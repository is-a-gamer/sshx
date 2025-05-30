//! sshx会话的核心逻辑，独立于消息传输。

use std::collections::HashMap;
use std::ops::DerefMut;
use std::time::Duration;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use bytes::Bytes;
use parking_lot::{Mutex, RwLock, RwLockWriteGuard};
use sshx_core::{
    proto::{server_update::ServerMessage, ListDirectoryRequest, ListDirectoryResponse, SequenceNumbers, ServerUpdate, FileUploadRequest, FileUploadResponse, FileDownloadRequest, FileDownloadResponse},
    IdCounter, Sid, Uid,
};
use tokio::sync::{broadcast, watch, Notify, oneshot, mpsc};
use tokio_stream::{wrappers::{errors::BroadcastStreamRecvError, BroadcastStream, WatchStream, ReceiverStream}, Stream};
use tracing::{debug, error, info, warn};
use tokio::time::{Instant, timeout};
use async_channel;

use crate::utils::Shutdown;
use crate::web::protocol::{WsServer, WsUser, WsWinsize};

mod snapshot;

/// 每个shell存储的最大输出缓冲区大小。
const SHELL_STORED_BYTES: u64 = 1 << 21; // 2 MiB

/// 会话的静态元数据。
#[derive(Debug, Clone)]
pub struct Metadata {
    /// 用于验证客户端是否有正确的加密密钥。
    pub encrypted_zeros: Bytes,

    /// 会话名称（人类可读）。
    pub name: String,

    /// 会话写入权限的密码哈希。
    pub write_password_hash: Option<Bytes>,
}

/// 单个sshx会话的内存状态。
#[derive(Debug)]
pub struct Session {
    /// 会话的静态元数据。
    metadata: Metadata,

    /// 会话的内存状态。
    shells: RwLock<HashMap<Sid, State>>,

    /// 当前连接用户的元数据。
    users: RwLock<HashMap<Uid, WsUser>>,

    /// 用于获取新的唯一ID的原子计数器。
    counter: IdCounter,

    /// 最后一次活动连接的后端客户端消息的时间戳。
    last_accessed: Mutex<Instant>,

    /// 已打开shell的有序列表和大小的watch通道源。
    source: watch::Sender<Vec<(Sid, WsWinsize)>>,

    /// 向所有WebSocket客户端广播更新。
    ///
    /// 此通道中的每个更新必须是幂等形式，因为消息可能在当前会话状态的任何快照之前或之后到达。
    /// 重复的事件应保持一致。
    broadcast: broadcast::Sender<WsServer>,

    /// 缓冲客户端消息的通道发送端。
    update_tx: async_channel::Sender<ServerMessage>,

    /// 缓冲客户端消息的通道接收端。
    update_rx: async_channel::Receiver<ServerMessage>,

    /// 当需要立即快照时从元数据事件触发。
    sync_notify: Notify,

    /// 当此会话已关闭并移除时设置。
    shutdown: Shutdown,

    /// 等待中的目录列表请求
    pending_list_directory: RwLock<HashMap<String, oneshot::Sender<Result<ListDirectoryResponse>>>>,

    /// 等待中的文件上传请求
    pending_upload_file: RwLock<HashMap<String, oneshot::Sender<Result<FileUploadResponse>>>>,

    /// 等待中的文件下载请求
    pending_download_file: RwLock<HashMap<String, mpsc::Sender<Result<FileDownloadResponse>>>>,
}

/// 每个shell的内部状态。
#[derive(Default, Debug)]
struct State {
    /// 序列号，表示已接收的字节数。
    seqnum: u64,

    /// 终端数据块。
    data: Vec<Bytes>,

    /// data[0]之前被修剪的数据块数量。
    chunk_offset: u64,

    /// 被修剪的数据块中的字节数。
    byte_offset: u64,

    /// 当此shell终止时设置。
    closed: bool,

    /// 当上述任何字段更改时更新。
    notify: Arc<Notify>,
}

impl Session {
    /// 构造一个新会话。
    pub fn new(metadata: Metadata) -> Self {
        let now = Instant::now();
        let (update_tx, update_rx) = async_channel::bounded(256);
        Session {
            metadata,
            shells: RwLock::new(HashMap::new()),
            users: RwLock::new(HashMap::new()),
            counter: IdCounter::default(),
            last_accessed: Mutex::new(now),
            source: watch::channel(Vec::new()).0,
            broadcast: broadcast::channel(64).0,
            update_tx,
            update_rx,
            sync_notify: Notify::new(),
            shutdown: Shutdown::new(),
            pending_list_directory: RwLock::new(HashMap::new()),
            pending_upload_file: RwLock::new(HashMap::new()),
            pending_download_file: RwLock::new(HashMap::new()),
        }
    }

    /// 返回此会话的元数据。
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }

    /// 提供对ID计数器的访问以获取新ID。
    pub fn counter(&self) -> &IdCounter {
        &self.counter
    }

    /// 返回当前shell的序列号。
    pub fn sequence_numbers(&self) -> SequenceNumbers {
        let shells = self.shells.read();
        let mut map = HashMap::with_capacity(shells.len());
        for (key, value) in &*shells {
            if !value.closed {
                map.insert(key.0, value.seqnum);
            }
        }
        SequenceNumbers { map }
    }

    /// 接收广播消息事件的通知。
    pub fn subscribe_broadcast(
        &self,
    ) -> impl Stream<Item = Result<WsServer, BroadcastStreamRecvError>> + Unpin {
        BroadcastStream::new(self.broadcast.subscribe())
    }

    /// 每次shell集合更改时接收通知。
    pub fn subscribe_shells(&self) -> impl Stream<Item = Vec<(Sid, WsWinsize)>> + Unpin {
        WatchStream::new(self.source.subscribe())
    }

    /// 订阅shell的数据块，直到它关闭。
    pub fn subscribe_chunks(
        &self,
        id: Sid,
        mut chunknum: u64,
    ) -> impl Stream<Item = (u64, Vec<Bytes>)> + '_ {
        async_stream::stream! {
            while !self.shutdown.is_terminated() {
                // 我们绝对不能在await点持有`shells`，
                // 因为这会导致死锁。
                let (seqnum, chunks, notified) = {
                    let shells = self.shells.read();
                    let shell = match shells.get(&id) {
                        Some(shell) if !shell.closed => shell,
                        _ => return,
                    };
                    let notify = Arc::clone(&shell.notify);
                    let notified = async move { notify.notified().await };
                    let mut seqnum = shell.byte_offset;
                    let mut chunks = Vec::new();
                    let current_chunks = shell.chunk_offset + shell.data.len() as u64;
                    if chunknum < current_chunks {
                        let start = chunknum.saturating_sub(shell.chunk_offset) as usize;
                        seqnum += shell.data[..start].iter().map(|x| x.len() as u64).sum::<u64>();
                        chunks = shell.data[start..].to_vec();
                        chunknum = current_chunks;
                    }
                    (seqnum, chunks, notified)
                };

                if !chunks.is_empty() {
                    yield (seqnum, chunks);
                }
                tokio::select! {
                    _ = notified => (),
                    _ = self.terminated() => return,
                }
            }
        }
    }

    /// 向会话添加新shell。
    pub fn add_shell(&self, id: Sid, center: (i32, i32)) -> Result<()> {
        use std::collections::hash_map::Entry::*;
        let _guard = match self.shells.write().entry(id) {
            Occupied(_) => bail!("shell already exists with id={id}"),
            Vacant(v) => v.insert(State::default()),
        };
        self.source.send_modify(|source| {
            let winsize = WsWinsize {
                x: center.0,
                y: center.1,
                ..Default::default()
            };
            source.push((id, winsize));
        });
        self.sync_now();
        Ok(())
    }

    /// 终止现有shell。
    pub fn close_shell(&self, id: Sid) -> Result<()> {
        match self.shells.write().get_mut(&id) {
            Some(shell) if !shell.closed => {
                shell.closed = true;
                shell.notify.notify_waiters();
            }
            Some(_) => return Ok(()),
            None => bail!("cannot close shell with id={id}, does not exist"),
        }
        self.source.send_modify(|source| {
            source.retain(|&(x, _)| x != id);
        });
        self.sync_now();
        Ok(())
    }

    fn get_shell_mut(&self, id: Sid) -> Result<impl DerefMut<Target = State> + '_> {
        let shells = self.shells.write();
        match shells.get(&id) {
            Some(shell) if !shell.closed => {
                Ok(RwLockWriteGuard::map(shells, |s| s.get_mut(&id).unwrap()))
            }
            Some(_) => bail!("cannot update shell with id={id}, already closed"),
            None => bail!("cannot update shell with id={id}, does not exist"),
        }
    }

    /// 更改终端大小，必要时通知客户端。
    pub fn move_shell(&self, id: Sid, winsize: Option<WsWinsize>) -> Result<()> {
        let _guard = self.get_shell_mut(id)?; // 确保互斥
        self.source.send_modify(|source| {
            if let Some(idx) = source.iter().position(|&(sid, _)| sid == id) {
                let (_, oldsize) = source.remove(idx);
                source.push((id, winsize.unwrap_or(oldsize)));
            }
        });
        Ok(())
    }

    /// 接收新数据到会话。
    pub fn add_data(&self, id: Sid, data: Bytes, seq: u64) -> Result<()> {
        let mut shell = self.get_shell_mut(id)?;

        if seq <= shell.seqnum && seq + data.len() as u64 > shell.seqnum {
            let start = shell.seqnum - seq;
            let segment = data.slice(start as usize..);
            debug!(%id, bytes = segment.len(), "adding data to shell");
            shell.seqnum += segment.len() as u64;
            shell.data.push(segment);

            // 如果超过最大存储字节数，修剪旧的数据块
            let mut stored_bytes = shell.seqnum - shell.byte_offset;
            if stored_bytes > SHELL_STORED_BYTES {
                let mut offset = 0;
                while offset < shell.data.len() && stored_bytes > SHELL_STORED_BYTES {
                    let bytes = shell.data[offset].len() as u64;
                    stored_bytes -= bytes;
                    shell.chunk_offset += 1;
                    shell.byte_offset += bytes;
                    offset += 1;
                }
                shell.data.drain(..offset);
            }

            shell.notify.notify_waiters();
        }

        Ok(())
    }

    /// 列出会话中的所有用户。
    pub fn list_users(&self) -> Vec<(Uid, WsUser)> {
        self.users
            .read()
            .iter()
            .map(|(k, v)| (*k, v.clone()))
            .collect()
    }

    /// 通过ID就地更新用户，对对象应用回调。
    pub fn update_user(&self, id: Uid, f: impl FnOnce(&mut WsUser)) -> Result<()> {
        let updated_user = {
            let mut users = self.users.write();
            let user = users.get_mut(&id).context("user not found")?;
            f(user);
            user.clone()
        };
        self.broadcast
            .send(WsServer::UserDiff(id, Some(updated_user)))
            .ok();
        Ok(())
    }

    /// 添加新用户，并返回一个在删除时移除用户的守卫。
    pub fn user_scope(&self, id: Uid, can_write: bool) -> Result<impl Drop + '_> {
        use std::collections::hash_map::Entry::*;

        #[must_use]
        struct UserGuard<'a>(&'a Session, Uid);
        impl Drop for UserGuard<'_> {
            fn drop(&mut self) {
                self.0.remove_user(self.1);
            }
        }

        match self.users.write().entry(id) {
            Occupied(_) => bail!("user already exists with id={id}"),
            Vacant(v) => {
                let user = WsUser {
                    name: format!("User {id}"),
                    cursor: None,
                    focus: None,
                    can_write,
                };
                v.insert(user.clone());
                self.broadcast.send(WsServer::UserDiff(id, Some(user))).ok();
                Ok(UserGuard(self, id))
            }
        }
    }

    /// 移除现有用户。
    fn remove_user(&self, id: Uid) {
        if self.users.write().remove(&id).is_none() {
            warn!(%id, "invariant violation: removed user that does not exist");
        }
        self.broadcast.send(WsServer::UserDiff(id, None)).ok();
    }

    /// 检查用户是否在会话中有写入权限。
    pub fn check_write_permission(&self, user_id: Uid) -> Result<()> {
        let users = self.users.read();
        let user = users.get(&user_id).context("user not found")?;
        if !user.can_write {
            bail!("No write permission");
        }
        Ok(())
    }

    /// 在房间中发送聊天消息。
    pub fn send_chat(&self, id: Uid, msg: &str) -> Result<()> {
        // 用当前名称填充消息，以防稍后不知道
        let name = {
            let users = self.users.read();
            users.get(&id).context("user not found")?.name.clone()
        };
        self.broadcast
            .send(WsServer::Hear(id, name, msg.into()))
            .ok();
        Ok(())
    }

    /// 发送shell延迟的测量结果。
    pub fn send_latency_measurement(&self, latency: u64) {
        self.broadcast.send(WsServer::ShellLatency(latency)).ok();
    }

    /// 注册后端客户端心跳，刷新时间戳。
    pub fn access(&self) {
        *self.last_accessed.lock() = Instant::now();
    }

    /// 返回最后一次后端客户端活动的时间戳。
    pub fn last_accessed(&self) -> Instant {
        *self.last_accessed.lock()
    }

    /// 访问此会话的客户端消息通道的发送端。
    pub fn update_tx(&self) -> &async_channel::Sender<ServerMessage> {
        &self.update_tx
    }

    /// 访问此会话的客户端消息通道的接收端。
    pub fn update_rx(&self) -> &async_channel::Receiver<ServerMessage> {
        &self.update_rx
    }

    /// 将会话标记为需要立即存储同步。
    ///
    /// 这对于在创建新shell、删除旧shell或更新ID计数器时的一致性是必需的。
    /// 如果这些操作在服务器重启时丢失，那么包含它们的快照相对于当前后端客户端状态将无效。
    ///
    /// 注意，不需要一直这样做，因为这会给数据库带来太大压力。
    /// 丢失的终端数据已经定期重新同步。
    pub fn sync_now(&self) {
        self.sync_notify.notify_one();
    }

    /// 当会话被标记为需要立即同步时解析。
    pub async fn sync_now_wait(&self) {
        self.sync_notify.notified().await
    }

    /// 发送终止信号以退出此会话。
    pub fn shutdown(&self) {
        self.shutdown.shutdown()
    }

    /// 当会话收到关闭信号时解析。
    pub async fn terminated(&self) {
        self.shutdown.wait().await
    }

    /// 获取会话的token
    pub fn token(&self) -> String {
        String::from("token")
    }

    /// 列出目录内容
    pub async fn list_directory(&self, request: ListDirectoryRequest) -> Result<ListDirectoryResponse> {
        debug!("发送目录列表请求到客户端");
        let request_id = request.token.clone();  // 使用token作为request_id
        debug!("使用的request_id: {}", request_id);
        
        // 创建一个oneshot channel来等待响应
        let (tx, rx) = oneshot::channel();
        
        // 存储发送端
        {
            let mut pending = self.pending_list_directory.write();
            pending.insert(request_id.clone(), tx);
            debug!("存储的pending请求数量: {}", pending.len());
        }
        
        // 发送请求到客户端
        let sve_msg = ServerUpdate {
            request_id: request_id.clone(),
            server_message: Some(ServerMessage::ListDirectory(request.clone())),
        };
        
        info!("发送目录列表请求到客户端: {:?}", request);
        match self.update_tx.send(sve_msg.server_message.unwrap()).await {
            Ok(_) => debug!("成功发送目录列表请求, request_id: {}", request_id),
            Err(e) => {
                error!("发送目录列表请求失败: {:?}", e);
                // 清理pending请求
                self.pending_list_directory.write().remove(&request_id);
                bail!("发送请求失败: {}", e);
            }
        }

        // 等待响应,设置超时时间为10秒
        match timeout(Duration::from_secs(10), rx).await {
            Ok(result) => {
                debug!("收到目录列表响应, request_id: {}, result: {:?}", request_id, result);
                // 清理pending请求
                self.pending_list_directory.write().remove(&request_id);
                // 处理结果
                match result {
                    Ok(response) => response,
                    Err(e) => bail!("接收响应失败: {}", e),
                }
            }
            Err(_) => {
                // 超时,清理pending请求
                self.pending_list_directory.write().remove(&request_id);
                bail!("请求超时")
            }
        }
    }

    /// 处理目录列表响应
    pub fn handle_list_directory_result(&self, request_id: String, error: Option<String>, response: Option<ListDirectoryResponse>) {
        debug!("开始处理目录列表响应, request_id: {}", request_id);
        {
            let pending = self.pending_list_directory.read();
            debug!("获取列表 当前pending请求数量: {}, 包含当前request_id: {}", pending.len(), pending.contains_key(&request_id));
        }
        
        if let Some(sender) = self.pending_list_directory.write().remove(&request_id) {
            let result = match (error, response) {
                (Some(error), _) => Err(anyhow::anyhow!(error)),
                (None, Some(response)) => Ok(response),
                (None, None) => Err(anyhow::anyhow!("无效的响应: 既没有错误也没有结果")),
            };
            debug!("发送响应结果到等待的channel, request_id: {}, result: {:?}", request_id, result);
            sender.send(result).ok();
        } else {
            warn!("未找到对应的pending请求, request_id: {}", request_id);
        }
    }

    /// 处理文件上传请求
    pub async fn upload_file(&self, request: FileUploadRequest) -> Result<FileUploadResponse> {
        debug!("准备发送文件上传请求到客户端");
        let request_id = request.token.clone();
        debug!("使用的request_id: {}", request_id);
        
        // 创建一个oneshot channel来等待响应
        let (tx, rx) = oneshot::channel();
        
        // 存储发送端
        {
            let mut pending = self.pending_upload_file.write();
            pending.insert(request_id.clone(), tx);
            debug!("上传文件的 pending上传请求数量: {}", pending.len());
        }
        
        // 发送请求到客户端
        let sve_msg = ServerUpdate {
            request_id: request_id.clone(),
            server_message: Some(ServerMessage::UploadFile(request.clone())),
        };
        
        info!("发送文件上传请求到客户端: {}", request.token);
        match self.update_tx.send(sve_msg.server_message.unwrap()).await {
            Ok(_) => debug!("成功发送文件上传请求, request_id: {}", request_id),
            Err(e) => {
                error!("发送文件上传请求失败: {:?}", e);
                // 清理pending请求
                self.pending_upload_file.write().remove(&request_id);
                bail!("发送请求失败: {}", e);
            }
        }

        // 等待响应,设置超时时间为30秒
        match timeout(Duration::from_secs(30), rx).await {
            Ok(result) => {
                debug!("收到文件上传响应, request_id: {}, result: {:?}", request_id, result);
                // 清理pending请求
                self.pending_upload_file.write().remove(&request_id);
                // 处理结果
                match result {
                    Ok(response) => response,
                    Err(e) => bail!("接收响应失败: {}", e),
                }
            }
            Err(_) => {
                // 超时,清理pending请求
                self.pending_upload_file.write().remove(&request_id);
                bail!("请求超时")
            }
        }
    }

    /// 处理文件上传响应
    pub fn handle_upload_file_result(&self, request_id: String, error: Option<String>, response: Option<FileUploadResponse>) {
        debug!("开始处理文件上传响应, request_id: {}", request_id);
        {
            let pending = self.pending_upload_file.read();
            debug!("上传文件的 当前pending上传请求数量: {}, 包含当前request_id: {}", pending.len(), pending.contains_key(&request_id));
        }
        
        if let Some(sender) = self.pending_upload_file.write().remove(&request_id) {
            let result = match (error, response) {
                (Some(error), _) => Err(anyhow::anyhow!(error)),
                (None, Some(response)) => Ok(response),
                (None, None) => Err(anyhow::anyhow!("无效的响应: 既没有错误也没有结果")),
            };
            debug!("发送响应结果到等待的channel, request_id: {}, result: {:?}", request_id, result);
            sender.send(result).ok();
        } else {
            warn!("未找到对应的pending上传请求, request_id: {}", request_id);
        }
    }

    /// 处理文件上传数据块
    pub async fn handle_upload_file_chunk(&self, request: FileUploadRequest) -> Result<()> {
        debug!("处理文件上传数据块: path={}, size={}, is_last={}", 
            request.path, request.chunk.len(), request.is_last);

        // 发送请求到客户端
        let sve_msg = ServerUpdate {
            request_id: request.token.clone(),
            server_message: Some(ServerMessage::UploadFile(request)),
        };
        
        match self.update_tx.send(sve_msg.server_message.unwrap()).await {
            Ok(_) => {
                debug!("成功发送文件块到客户端");
                Ok(())
            }
            Err(e) => {
                error!("发送文件块失败: {:?}", e);
                bail!("发送文件块失败: {}", e)
            }
        }
    }

    /// 处理文件下载请求
    pub async fn download_file(&self, request: FileDownloadRequest) -> Result<impl Stream<Item = Result<FileDownloadResponse>>> {
        debug!("准备发送文件下载请求到客户端");
        let request_id = request.token.clone();
        debug!("使用的request_id: {}", request_id);
        
        // 创建一个mpsc channel来传输文件块，增加缓冲区大小到128
        let (tx, rx) = mpsc::channel(128);
        
        // 存储发送端
        {
            let mut pending = self.pending_download_file.write();
            pending.insert(request_id.clone(), tx);
            debug!("下载的 存储的pending下载请求数量: {}", pending.len());
        }
        
        // 发送请求到客户端
        let sve_msg = ServerUpdate {
            request_id: request_id.clone(),
            server_message: Some(ServerMessage::DownloadFile(request.clone())),
        };
        
        debug!("发送文件下载请求到客户端: {}", request.path);
        match self.update_tx.send(sve_msg.server_message.unwrap()).await {
            Ok(_) => debug!("成功发送文件下载请求, request_id: {}", request_id),
            Err(e) => {
                error!("发送文件下载请求失败: {:?}", e);
                // 清理pending请求
                self.pending_download_file.write().remove(&request_id);
                bail!("发送请求失败: {}", e);
            }
        }

        // 返回接收流
        Ok(ReceiverStream::new(rx))
    }

    /// 处理文件下载响应
    pub fn handle_download_file_chunk(&self, request_id: String, response: FileDownloadResponse) {
        debug!("开始处理文件下载响应块, request_id: {}", request_id);
        
        let mut pending = self.pending_download_file.write();
        if let Some(sender) = pending.get(&request_id) {
            debug!("下载文件的 发送文件块到等待的channel, request_id: {}, is_last: {}", request_id, response.is_last);
            
            // 使用 try_send 而不是 send，避免阻塞
            match sender.try_send(Ok(response.clone())) {
                Ok(_) => {
                    // 如果是最后一块,清理pending状态
                    if response.is_last {
                        debug!("这是最后一块,清理pending状态");
                        pending.remove(&request_id);
                        
                        // 通知客户端下载完成，使用 try_send 避免阻塞
                        if let Err(e) = self.update_tx.try_send(ServerMessage::DownloadFile(FileDownloadRequest {
                            path: String::new(),
                            token: request_id.clone(),
                        })) {
                            warn!("无法发送下载完成通知到客户端: {}", e);
                        }
                    }
                }
                Err(e) => {
                    warn!("无法发送文件块到接收端: {}", e);
                    // 如果发送失败,移除pending状态
                    pending.remove(&request_id);
                }
            }
        } else {
            warn!("未找到对应的pending下载请求, request_id: {}", request_id);
        }
    }
}
