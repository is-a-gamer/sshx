//! Network gRPC client allowing server control of terminals.

use std::collections::HashMap;
use std::pin::pin;
use std::process::exit;
use std::io::Write;
use anyhow::{Context, Result};
use sshx_core::proto::FileInfo;
use sshx_core::proto::{
    client_update::ClientMessage, server_update::ServerMessage,
    sshx_service_client::SshxServiceClient, ClientUpdate, CloseRequest, NewShell, OpenRequest,
    ListDirectoryResult, ListDirectoryResponse, FileUploadRequest, FileUploadResponse, UploadFileResult,
    FileDownloadRequest, FileDownloadResponse, DownloadFileResult,
};
use sshx_core::Sid;
use tokio::sync::mpsc;
use tokio::task;
use tokio::time::{self, Duration, Instant, MissedTickBehavior};
use tokio_stream::{wrappers::ReceiverStream, StreamExt};
use tonic::transport::Channel;
use tracing::{debug, error, info, warn};
use std::collections::HashSet;
use std::sync::Arc;
use parking_lot;
use tokio::io::AsyncReadExt;

use crate::encrypt::Encrypt;
use crate::runner::{Runner, ShellData};

/// Interval for sending empty heartbeat messages to the server.
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(2);

/// Interval to automatically reestablish connections.
const RECONNECT_INTERVAL: Duration = Duration::from_secs(60);

/// Handles a single session's communication with the remote server.
pub struct Controller {
    enable_reconnect: bool,
    origin: String,
    runner: Runner,
    encrypt: Encrypt,
    encryption_key: String,
    name: String,
    password: String,
    token: String,
    url: String,
    write_url: Option<String>,
    /// Channels with backpressure routing messages to each shell task.
    shells_tx: HashMap<Sid, mpsc::Sender<ShellData>>,
    /// Channel shared with tasks to allow them to output client messages.
    output_tx: mpsc::Sender<ClientMessage>,
    /// Owned receiving end of the `output_tx` channel.
    output_rx: mpsc::Receiver<ClientMessage>,
    webfile_path: String,
    uploading_files: Arc<parking_lot::Mutex<HashSet<String>>>,
}

impl Controller {
    /// Construct a new controller, connecting to the remote server.
    pub async fn new(
        origin: &str,
        name: &str,
        password: &str,
        runner: Runner,
        enable_reconnect: bool,
    ) -> Result<Self> {
        debug!(%origin, "connecting to server");
        let encryption_key = "view".to_string(); // 83.3 bits of entropy

        let kdf_task = {
            let encryption_key = encryption_key.clone();
            task::spawn_blocking(move || Encrypt::new(&encryption_key))
        };

        let (write_password, kdf_write_password_task) = if !password.is_empty() {
            let write_password = password.to_string(); // 83.3 bits of entropy
            let task = {
                let write_password = write_password.clone();
                task::spawn_blocking(move || Encrypt::new(&write_password))
            };
            (Some(write_password), Some(task))
        } else {
            (None, None)
        };

        let mut client = Self::connect(origin).await?;
        let encrypt = kdf_task.await?;
        let write_password_hash = if let Some(task) = kdf_write_password_task {
            Some(task.await?.zeros().into())
        } else {
            None
        };

        let req = OpenRequest {
            origin: origin.into(),
            encrypted_zeros: encrypt.zeros().into(),
            name: name.into(),
            write_password_hash,
        };
        let mut resp = client.open(req).await?.into_inner();
        resp.url = resp.url + "#" + &encryption_key;

        let write_url = if let Some(write_password) = write_password {
            Some(resp.url.clone() + "," + &write_password)
        } else {
            None
        };
        let webfile_path = std::env::current_dir().unwrap().to_str().unwrap().to_string();
        let uploading_files = Arc::new(parking_lot::Mutex::new(HashSet::new()));
        let (output_tx, output_rx) = mpsc::channel(64);
        Ok(Self {
            enable_reconnect,
            origin: origin.into(),
            runner,
            encrypt,
            encryption_key,
            name: resp.name,
            password: password.into(),
            token: resp.token,
            url: resp.url,
            write_url,
            shells_tx: HashMap::new(),
            output_tx,
            output_rx,
            webfile_path,
            uploading_files,
        })
    }

    /// Create a new gRPC client to the HTTP(S) origin.
    ///
    /// This is used on reconnection to the server, since some replicas may be
    /// gracefully shutting down, which means connected clients need to start a
    /// new TCP handshake.
    async fn connect(origin: &str) -> Result<SshxServiceClient<Channel>, tonic::transport::Error> {
        SshxServiceClient::connect(String::from(origin)).await
    }

    /// Returns the name of the session.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the URL of the session.
    pub fn url(&self) -> &str {
        &self.url
    }

    /// Returns the write URL of the session, if it exists.
    pub fn write_url(&self) -> Option<&str> {
        self.write_url.as_deref()
    }

    /// Returns the encryption key for this session, hidden from the server.
    pub fn encryption_key(&self) -> &str {
        &self.encryption_key
    }

    async fn list_directory(&self, path: &str, _: &mpsc::Sender<ClientUpdate>) -> Result<Vec<FileInfo>> {
        let path = std::path::Path::new(&self.webfile_path).join(path);
        let entries = std::fs::read_dir(path)?;
        let mut files = Vec::new();
        
        for entry in entries {
            if let Ok(entry) = entry {
                if let Ok(metadata) = entry.metadata() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    let is_directory = metadata.is_dir();
                    let size = metadata.len();
                    let modified_time = metadata.modified()
                        .map(|t| t.duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default().as_secs())
                        .unwrap_or(0);
                    
                    files.push(FileInfo {
                        name,
                        is_directory,
                        size,
                        modified_time,
                        permissions: "".to_string(),
                    });
                }
            }
        }
        
        Ok(files)
    }

    /// 处理文件上传
    async fn handle_upload_file(&self, request: &FileUploadRequest, tx: &mpsc::Sender<ClientUpdate>) -> Result<()> {
        let target_path = std::path::Path::new(&self.webfile_path).join(&request.path);
        
        // 确保目标目录存在
        if let Some(parent) = target_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let mut uploading = self.uploading_files.lock();
        let is_new_upload = uploading.insert(request.token.clone());

        // 如果是新的上传请求且不是续传,则删除已存在的文件
        if is_new_upload {
            if target_path.exists() {
                std::fs::remove_file(&target_path)?;
            }
        }

        // 写入文件 - 使用append模式
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .append(true)
            .open(&target_path)?;

        // 写入当前块
        file.write_all(&request.chunk)?;
        file.flush()?;

        info!("写入文件块成功, 大小: {} bytes, 是否为最后一块: {}", request.chunk.len(), request.is_last);

        // 只在最后一块时发送成功响应并清理状态
        if request.is_last {
            uploading.remove(&request.token);
            send_msg(&tx, ClientMessage::UploadFileResult(
                UploadFileResult {
                    request_id: request.token.clone(),
                    error: None,
                    response: Some(FileUploadResponse {
                        success: true,
                        message: "文件上传成功".to_string(),
                    }),
                }
            )).await?;
        }

        Ok(())
    }

    /// 处理文件下载请求
    async fn handle_download_file(&self, request: &FileDownloadRequest, tx: &mpsc::Sender<ClientUpdate>) -> Result<()> {
        let target_path = std::path::Path::new(&self.webfile_path).join(&request.path);
        
        // 检查文件是否存在
        if !target_path.exists() {
            send_msg(tx, ClientMessage::DownloadFileResult(DownloadFileResult {
                request_id: request.token.clone(),
                error: Some(format!("文件不存在: {}", request.path)),
                response: None,
            })).await?;
            return Ok(());
        }

        // 打开文件
        let mut file = match tokio::fs::File::open(&target_path).await {
            Ok(file) => file,
            Err(e) => {
                send_msg(tx, ClientMessage::DownloadFileResult(DownloadFileResult {
                    request_id: request.token.clone(),
                    error: Some(format!("无法打开文件: {}", e)),
                    response: None,
                })).await?;
                return Ok(());
            }
        };

        // 读取文件并分块发送
        let mut buffer = vec![0; 8 * 1024 * 1024]; // 8MB 缓冲区
        loop {
            match file.read(&mut buffer).await {
                Ok(n) if n > 0 => {
                    // 发送数据块
                    let response = FileDownloadResponse {
                        chunk: buffer[..n].to_vec().into(),
                        is_last: false,
                    };
                    send_msg(tx, ClientMessage::DownloadFileResult(DownloadFileResult {
                        request_id: request.token.clone(),
                        error: None,
                        response: Some(response),
                    })).await?;
                }
                Ok(_) => {
                    // 发送最后一个空块表示结束
                    let response = FileDownloadResponse {
                        chunk: Vec::new().into(),
                        is_last: true,
                    };
                    send_msg(tx, ClientMessage::DownloadFileResult(DownloadFileResult {
                        request_id: request.token.clone(),
                        error: None,
                        response: Some(response),
                    })).await?;
                    break;
                }
                Err(e) => {
                    send_msg(tx, ClientMessage::DownloadFileResult(DownloadFileResult {
                        request_id: request.token.clone(),
                        error: Some(format!("读取文件失败: {}", e)),
                        response: None,
                    })).await?;
                    break;
                }
            }
        }

        Ok(())
    }

    /// Run the controller forever, listening for requests from the server.
    pub async fn run(&mut self) -> ! {
        let mut last_retry = Instant::now();
        let mut retries = 0;
        loop {
            if let Err(err) = self.try_channel().await {
                if let Some(status) = err.downcast_ref::<tonic::Status>() {
                    // server not found this session id
                    if status.code() == tonic::Code::NotFound
                        && status.message() == "session not found"
                    {
                        match Controller::new(
                            &self.origin,
                            &self.name,
                            &self.password,
                            self.runner.clone(),
                            self.enable_reconnect,
                            // self.write_url.is_some(),
                        )
                        .await
                        {
                            Ok(new_controller) => {
                                // successfully rebuilt the connection
                                // replaced controller
                                *self = new_controller;
                                info!("recreate session success");
                                continue;
                            }
                            Err(e) => {
                                error!(error = ?e, "failed to recreate session");
                            }
                        }
                    }
                }
                if !self.enable_reconnect{
                    info!("connect close,client not enable reconnect");
                    exit(0)
                }
                if last_retry.elapsed() >= Duration::from_secs(10) {
                    retries = 0;
                }
                let secs = 2_u64.pow(retries.min(4));
                error!(%err, "disconnected, retrying in {secs}s...");
                time::sleep(Duration::from_secs(secs)).await;
                retries += 1;
            }
            last_retry = Instant::now();
        }
    }

    /// Helper function used by `run()` that can return errors.
    async fn try_channel(&mut self) -> Result<()> {
        let (tx, rx) = mpsc::channel(16);

        let hello = ClientMessage::Hello(format!("{},{}", self.name, self.token));
        send_msg(&tx, hello).await?;

        let mut client = Self::connect(&self.origin).await?;
        let resp = client.channel(ReceiverStream::new(rx)).await?;
        let mut messages = resp.into_inner(); // A stream of server messages.

        let mut interval = time::interval(HEARTBEAT_INTERVAL);
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut reconnect = pin!(time::sleep(RECONNECT_INTERVAL));
        loop {
            let message = tokio::select! {
                _ = interval.tick() => {
                    tx.send(ClientUpdate::default()).await?;
                    continue;
                }
                msg = self.output_rx.recv() => {
                    let msg = msg.context("unreachable: output_tx was closed?")?;
                    send_msg(&tx, msg).await?;
                    continue;
                }
                item = messages.next() => {
                    item.context("server closed connection")??
                        .server_message
                        .context("server message is missing")?
                }
                _ = &mut reconnect => {
                    return Ok(()); // Reconnect to the server.
                }
            };

            match message {
                ServerMessage::Input(input) => {
                    let data = self.encrypt.segment(0x200000000, input.offset, &input.data);
                    if let Some(sender) = self.shells_tx.get(&Sid(input.id)) {
                        // This line applies backpressure if the shell task is overloaded.
                        sender.send(ShellData::Data(data)).await.ok();
                    } else {
                        warn!(%input.id, "received data for non-existing shell");
                    }
                }
                ServerMessage::CreateShell(new_shell) => {
                    let id = Sid(new_shell.id);
                    let center = (new_shell.x, new_shell.y);
                    if !self.shells_tx.contains_key(&id) {
                        self.spawn_shell_task(id, center);
                    } else {
                        warn!(%id, "server asked to create duplicate shell");
                    }
                }
                ServerMessage::CloseShell(id) => {
                    // Closes the channel when it is dropped, notifying the task to shut down.
                    self.shells_tx.remove(&Sid(id));
                    send_msg(&tx, ClientMessage::ClosedShell(id)).await?;
                }
                ServerMessage::Sync(seqnums) => {
                    for (id, seq) in seqnums.map {
                        if let Some(sender) = self.shells_tx.get(&Sid(id)) {
                            sender.send(ShellData::Sync(seq)).await.ok();
                        } else {
                            warn!(%id, "received sequence number for non-existing shell");
                            send_msg(&tx, ClientMessage::ClosedShell(id)).await?;
                        }
                    }
                }
                ServerMessage::Resize(msg) => {
                    if let Some(sender) = self.shells_tx.get(&Sid(msg.id)) {
                        sender.send(ShellData::Size(msg.rows, msg.cols)).await.ok();
                    } else {
                        warn!(%msg.id, "received resize for non-existing shell");
                    }
                }
                ServerMessage::Ping(ts) => {
                    // Echo back the timestamp, for stateless latency measurement.
                    // info!("this ping msg");
                    send_msg(&tx, ClientMessage::Pong(ts)).await?;
                }
                ServerMessage::Error(err) => {
                    error!(?err, "error received from server");
                }
                ServerMessage::Kick(_) => {
                    info!("server kick this session");
                    let req = CloseRequest {
                        name: self.name.clone(),
                        token: self.token.clone(),
                    };
                    let mut client = Self::connect(&self.origin).await?;
                    client.close(req).await?;
                    exit(0)
                }
                ServerMessage::ListDirectory(req) => {
                    info!("收到服务端的目录列表请求: {:?}", req);
                    match self.list_directory(&req.path, &tx).await {
                        Ok(files) => {
                            send_msg(&tx, ClientMessage::ListDirectoryResult(
                                ListDirectoryResult {
                                    request_id: req.token.clone(),
                                    error: None,
                                    response: Some(ListDirectoryResponse {
                                        files,
                                    }),
                                }
                            )).await?;
                        },
                        Err(e) => {
                            send_msg(&tx, ClientMessage::ListDirectoryResult(
                                ListDirectoryResult {
                                    request_id: req.token.clone(),
                                    error: Some(format!("无法读取目录: {}", e)),
                                    response: None,
                                }
                            )).await?;
                        }
                    }
                }
                ServerMessage::UploadFile(req) => {
                    // info!("收到服务端的上传请求: {},{}", req.token,req.path);
                    // let target_path = std::path::Path::new(&self.webfile_path).join(&req.path);
                    match self.handle_upload_file(&req, &tx).await {
                        Ok(_) => {
                            // info!("文件上传处理成功");
                        }
                        Err(e) => {
                            error!("文件上传处理失败: {}", e);
                            send_msg(&tx, ClientMessage::UploadFileResult(
                                UploadFileResult {
                                    request_id: req.token.clone(),
                                    error: Some(format!("文件上传失败: {}", e)),
                                    response: None,
                                }
                            )).await?;
                        }
                    }
                }
                ServerMessage::DownloadFile(req) => {
                    // 如果path为空，这是一个完成通知
                    if req.path.is_empty() {
                        debug!("收到下载完成通知: token={}", req.token);
                        continue;
                    }
                    
                    info!("收到服务端的下载请求: {:?}", req);
                    match self.handle_download_file(&req, &tx).await {
                        Ok(_) => {
                            debug!("文件下载处理成功");
                        }
                        Err(e) => {
                            error!("文件下载处理失败: {}", e);
                            send_msg(&tx, ClientMessage::DownloadFileResult(
                                DownloadFileResult {
                                    request_id: req.token.clone(),
                                    error: Some(format!("文件下载失败: {}", e)),
                                    response: None,
                                }
                            )).await?;
                        }
                    }
                }
                ServerMessage::DeleteFile(req) => {
                    info!("收到服务端的删除请求: {:?}", req);
                }
                ServerMessage::CreateDirectory(req) => {
                    info!("收到服务端的创建目录请求: {:?}", req);
                }
            }
        }
    }

    /// Entry point to start a new terminal task on the client.
    fn spawn_shell_task(&mut self, id: Sid, center: (i32, i32)) {
        let (shell_tx, shell_rx) = mpsc::channel(16);
        let opt = self.shells_tx.insert(id, shell_tx);
        debug_assert!(opt.is_none(), "shell ID cannot be in existing tasks");

        let runner = self.runner.clone();
        let encrypt = self.encrypt.clone();
        let output_tx = self.output_tx.clone();
        tokio::spawn(async move {
            debug!(%id, "spawning new shell");
            let new_shell = NewShell {
                id: id.0,
                x: center.0,
                y: center.1,
            };
            if let Err(err) = output_tx.send(ClientMessage::CreatedShell(new_shell)).await {
                error!(%id, ?err, "failed to send shell creation message");
                return;
            }
            if let Err(err) = runner.run(id, encrypt, shell_rx, output_tx.clone()).await {
                let err = ClientMessage::Error(err.to_string());
                output_tx.send(err).await.ok();
            }
            output_tx.send(ClientMessage::ClosedShell(id.0)).await.ok();
        });
    }

    /// Terminate this session gracefully.
    pub async fn close(&self) -> Result<()> {
        debug!("closing session");
        let req = CloseRequest {
            name: self.name.clone(),
            token: self.token.clone(),
        };
        let mut client = Self::connect(&self.origin).await?;
        client.close(req).await?;
        Ok(())
    }
}

/// Attempt to send a client message over an update channel.
async fn send_msg(tx: &mpsc::Sender<ClientUpdate>, message: ClientMessage) -> Result<()> {
    let update = ClientUpdate {
        request_id: "".to_string(),
        client_message: Some(message),
    };
    tx.send(update)
        .await
        .context("failed to send message to server")
}
