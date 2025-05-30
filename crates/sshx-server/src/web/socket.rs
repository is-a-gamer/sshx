use std::collections::HashSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::{
    ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade},
    Path, State,
};
use axum::extract::multipart::Multipart;
use axum::response::IntoResponse;
use axum::Json;
use axum::body::Body;
use bytes::Bytes;
use futures_util::SinkExt;
use http::HeaderMap;
use sshx_core::proto::{
    server_update::ServerMessage, ListDirectoryRequest, NewShell, TerminalInput, TerminalSize,
    FileUploadRequest, FileDownloadRequest,
};
use sshx_core::Sid;
use subtle::ConstantTimeEq;
use tokio::sync::mpsc;
use tokio_stream::StreamExt;
use tracing::{error, info, info_span, warn, Instrument};
use axum::http::StatusCode;
use futures_util::stream::Stream;
use std::pin::Pin;

use crate::session::Session;
use crate::web::protocol::{WsClient, WsServer};
use crate::ServerState;

pub async fn get_session_ws(
    Path(name): Path<String>,
    ws: WebSocketUpgrade,
    State(state): State<Arc<ServerState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |mut socket| {
        let span = info_span!("ws", %name);
        async move {
            match state.frontend_connect(&name).await {
                Ok(Ok(session)) => {
                    if let Err(err) = handle_socket(&mut socket, session).await {
                        warn!(?err, "websocket exiting early");
                    } else {
                        socket.close().await.ok();
                    }
                }
                Ok(Err(Some(host))) => {
                    if let Err(err) = proxy_redirect(&mut socket, &host, &name).await {
                        error!(?err, "failed to proxy websocket");
                        let frame = CloseFrame {
                            code: 4500,
                            reason: format!("proxy redirect: {err}").into(),
                        };
                        socket.send(Message::Close(Some(frame))).await.ok();
                    } else {
                        socket.close().await.ok();
                    }
                }
                Ok(Err(None)) => {
                    let frame = CloseFrame {
                        code: 4404,
                        reason: "could not find the requested session".into(),
                    };
                    socket.send(Message::Close(Some(frame))).await.ok();
                }
                Err(err) => {
                    error!(?err, "failed to connect to frontend session");
                    let frame = CloseFrame {
                        code: 4500,
                        reason: format!("session connect: {err}").into(),
                    };
                    socket.send(Message::Close(Some(frame))).await.ok();
                }
            }
        }
        .instrument(span)
    })
}

/// Handle an incoming live WebSocket connection to a given session.
async fn handle_socket(socket: &mut WebSocket, session: Arc<Session>) -> Result<()> {
    /// Send a message to the client over WebSocket.
    async fn send(socket: &mut WebSocket, msg: WsServer) -> Result<()> {
        let mut buf = Vec::new();
        ciborium::ser::into_writer(&msg, &mut buf)?;
        socket.send(Message::Binary(Bytes::from(buf))).await?;
        Ok(())
    }

    /// Receive a message from the client over WebSocket.
    async fn recv(socket: &mut WebSocket) -> Result<Option<WsClient>> {
        Ok(loop {
            match socket.recv().await.transpose()? {
                Some(Message::Text(_)) => warn!("ignoring text message over WebSocket"),
                Some(Message::Binary(msg)) => break Some(ciborium::de::from_reader(&*msg)?),
                Some(_) => (), // ignore other message types, keep looping
                None => break None,
            }
        })
    }

    let metadata = session.metadata();
    let user_id = session.counter().next_uid();
    session.sync_now();
    send(socket, WsServer::Hello(user_id, metadata.name.clone())).await?;

    let can_write = match recv(socket).await? {
        Some(WsClient::Authenticate(bytes, write_password_bytes)) => {
            // Constant-time comparison of bytes, converting Choice to bool
            if !bool::from(bytes.ct_eq(metadata.encrypted_zeros.as_ref())) {
                send(socket, WsServer::InvalidAuth()).await?;
                return Ok(());
            }

            match (write_password_bytes, &metadata.write_password_hash) {
                // No password needed, so all users can write (default).
                (_, None) => true,

                // Password stored but not provided, user is read-only.
                (None, Some(_)) => false,

                // Password stored and provided, compare them.
                (Some(provided), Some(stored)) => {
                    if !bool::from(provided.ct_eq(stored)) {
                        send(socket, WsServer::InvalidAuth()).await?;
                        return Ok(());
                    }
                    true
                }
            }
        }
        _ => {
            send(socket, WsServer::InvalidAuth()).await?;
            return Ok(());
        }
    };

    let _user_guard = session.user_scope(user_id, can_write)?;

    let update_tx = session.update_tx(); // start listening for updates before any state reads
    let mut broadcast_stream = session.subscribe_broadcast();
    send(socket, WsServer::Users(session.list_users())).await?;

    let mut subscribed = HashSet::new(); // prevent duplicate subscriptions
    let (chunks_tx, mut chunks_rx) = mpsc::channel::<(Sid, u64, Vec<Bytes>)>(1);

    let mut shells_stream = session.subscribe_shells();
    loop {
        let msg = tokio::select! {
            _ = session.terminated() => break,
            Some(result) = broadcast_stream.next() => {
                let msg = result.context("client fell behind on broadcast stream")?;
                send(socket, msg).await?;
                continue;
            }
            Some(shells) = shells_stream.next() => {
                send(socket, WsServer::Shells(shells)).await?;
                continue;
            }
            Some((id, seqnum, chunks)) = chunks_rx.recv() => {
                send(socket, WsServer::Chunks(id, seqnum, chunks)).await?;
                continue;
            }
            result = recv(socket) => {
                match result? {
                    Some(msg) => msg,
                    None => break,
                }
            }
        };

        match msg {
            WsClient::Authenticate(_, _) => {}
            WsClient::SetName(name) => {
                if !name.is_empty() {
                    session.update_user(user_id, |user| user.name = name)?;
                }
            }
            WsClient::SetCursor(cursor) => {
                session.update_user(user_id, |user| user.cursor = cursor)?;
            }
            WsClient::SetFocus(id) => {
                session.update_user(user_id, |user| user.focus = id)?;
            }
            WsClient::Create(x, y) => {
                if let Err(e) = session.check_write_permission(user_id) {
                    send(socket, WsServer::Error(e.to_string())).await?;
                    continue;
                }
                let id = session.counter().next_sid();
                session.sync_now();
                let new_shell = NewShell { id: id.0, x, y };
                update_tx
                    .send(ServerMessage::CreateShell(new_shell))
                    .await?;
            }
            WsClient::Close(id) => {
                if let Err(e) = session.check_write_permission(user_id) {
                    send(socket, WsServer::Error(e.to_string())).await?;
                    continue;
                }
                update_tx.send(ServerMessage::CloseShell(id.0)).await?;
            }
            WsClient::Move(id, winsize) => {
                if let Err(e) = session.check_write_permission(user_id) {
                    send(socket, WsServer::Error(e.to_string())).await?;
                    continue;
                }
                if let Err(err) = session.move_shell(id, winsize) {
                    send(socket, WsServer::Error(err.to_string())).await?;
                    continue;
                }
                if let Some(winsize) = winsize {
                    let msg = ServerMessage::Resize(TerminalSize {
                        id: id.0,
                        rows: winsize.rows as u32,
                        cols: winsize.cols as u32,
                    });
                    session.update_tx().send(msg).await?;
                }
            }
            WsClient::Data(id, data, offset) => {
                if let Err(e) = session.check_write_permission(user_id) {
                    send(socket, WsServer::Error(e.to_string())).await?;
                    continue;
                }
                let input = TerminalInput {
                    id: id.0,
                    data,
                    offset,
                };
                update_tx.send(ServerMessage::Input(input)).await?;
            }
            WsClient::Subscribe(id, chunknum) => {
                if subscribed.contains(&id) {
                    continue;
                }
                subscribed.insert(id);
                let session = Arc::clone(&session);
                let chunks_tx = chunks_tx.clone();
                tokio::spawn(async move {
                    let stream = session.subscribe_chunks(id, chunknum);
                    tokio::pin!(stream);
                    while let Some((seqnum, chunks)) = stream.next().await {
                        if chunks_tx.send((id, seqnum, chunks)).await.is_err() {
                            break;
                        }
                    }
                });
            }
            WsClient::Chat(msg) => {
                session.send_chat(user_id, &msg)?;
            }
            WsClient::Ping(ts) => {
                send(socket, WsServer::Pong(ts)).await?;
            }
            WsClient::Kick(_) => {
                info!("kicked session {}", session.metadata().name.clone());
                update_tx.send(ServerMessage::Kick(session.metadata().name.clone())).await?;
                socket.close().await?;
            }
        }
    }
    Ok(())
}

/// Transparently reverse-proxy a WebSocket connection to a different host.
async fn proxy_redirect(socket: &mut WebSocket, host: &str, name: &str) -> Result<()> {
    use tokio_tungstenite::{
        connect_async,
        tungstenite::protocol::{CloseFrame as TCloseFrame, Message as TMessage},
    };

    let (mut upstream, _) = connect_async(format!("ws://{host}/api/s/{name}")).await?;
    loop {
        // Due to axum having its own WebSocket API types, we need to manually translate
        // between it and tungstenite's message type.
        tokio::select! {
            Some(client_msg) = socket.recv() => {
                let msg = match client_msg {
                    Ok(Message::Text(s)) => Some(TMessage::Text(s.as_str().into())),
                    Ok(Message::Binary(b)) => Some(TMessage::Binary(b)),
                    Ok(Message::Close(frame)) => {
                        let frame = frame.map(|frame| TCloseFrame {
                            code: frame.code.into(),
                            reason: frame.reason.as_str().into(),
                        });
                        Some(TMessage::Close(frame))
                    }
                    Ok(_) => None,
                    Err(_) => break,
                };
                if let Some(msg) = msg {
                    if upstream.send(msg).await.is_err() {
                        break;
                    }
                }
            }
            Some(server_msg) = upstream.next() => {
                let msg = match server_msg {
                    Ok(TMessage::Text(s)) => Some(Message::Text(s.as_str().into())),
                    Ok(TMessage::Binary(b)) => Some(Message::Binary(b)),
                    Ok(TMessage::Close(frame)) => {
                        let frame = frame.map(|frame| CloseFrame {
                            code: frame.code.into(),
                            reason: frame.reason.as_str().into(),
                        });
                        Some(Message::Close(frame))
                    }
                    Ok(_) => None,
                    Err(_) => break,
                };
                if let Some(msg) = msg {
                    if socket.send(msg).await.is_err() {
                        break;
                    }
                }
            }
            else => break,
        }
    }

    Ok(())
}
pub async fn get_session_list(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    let auth_str = headers.get("Authorization");
    // let s = Vec::<String>::new();
    if auth_str.unwrap().is_empty() {
        return Json(Vec::<String>::new());
    }
    if auth_str.unwrap().to_str().unwrap_or_default() != state.secret {
        return Json(Vec::<String>::new());
    }
    let session_names = state.get_all_session_names();
    Json(session_names)
}
// 跳转到session 中的 list_directory 方法
pub async fn list_directory(
    Path(path): Path<String>,
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // TODO modify token
    let session_name = headers.get("Session");
    if session_name.is_none() {
        return (StatusCode::BAD_REQUEST, "Session name is required").into_response();
    }
    if session_name.unwrap().is_empty() {
        return Json(Vec::<String>::new()).into_response();
    }
    // 最后的是 会话名称|随机UUID
    let token = session_name.unwrap().to_str().unwrap().to_string() + &"|" + & uuid::Uuid::new_v4().to_string();
    info!("token: {}", token);
    let result = if let Some(session) = state.lookup(&session_name.unwrap().to_str().unwrap()) {
        let request = ListDirectoryRequest {
            path,
            token: token.clone(),
        };
        match session.list_directory(request).await {
            Ok(response) => Ok(response),
            Err(e) => Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string())),
        }
    } else {
        Err((StatusCode::BAD_REQUEST, "Session not found".to_string()))
    };
    
    match result {
        Ok(response) => Json(response).into_response(),
        Err((status, message)) => (status, message).into_response(),
    }
}

pub async fn upload_file(
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> impl IntoResponse {
    let session_name = match headers.get("Session") {
        Some(name) => name.to_str().unwrap_or_default(),
        None => return (StatusCode::BAD_REQUEST, "Session name is required").into_response(),
    };

    if session_name.is_empty() {
        return (StatusCode::BAD_REQUEST, "Session name cannot be empty").into_response();
    }
    let session = match state.lookup(session_name) {
        Some(session) => session,
        None => return (StatusCode::BAD_REQUEST, "Session not found").into_response(),
    };
    let token = format!("{}|{}", session_name, uuid::Uuid::new_v4().to_string());

    // const _CHUNK_SIZE: usize = 8 * 1024 * 1024; // 8MB chunks
    let mut target_path = String::new();

    // 读取multipart表单字段
    while let Some(field) = multipart.next_field().await.unwrap_or(None) {
        let name = field.name().unwrap_or_default();
        match name {
            "path" => {
                target_path = field.text().await.unwrap_or_default();
            }
            "file" => {
                let mut _is_first = true;
                let mut _total_size = 0;
                let mut field = field;

                while let Ok(Some(chunk)) = field.chunk().await {
                    let chunk_size = chunk.len();
                    _total_size += chunk_size;

                    // 创建上传请求
                    let request = FileUploadRequest {
                        path: target_path.clone(),
                        chunk: Bytes::from(chunk),
                        token: token.clone(),
                        is_last: false,
                    };

                    // 发送到客户端并等待响应
                    if let Err(e) = session.handle_upload_file_chunk(request).await {
                        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
                    }

                    _is_first = false;
                }

                // 发送最后一块,标记为结束
                let final_request = FileUploadRequest {
                    path: target_path.clone(),
                    chunk: Bytes::new(),
                    token: token.clone(),
                    is_last: true,
                };

                match session.upload_file(final_request).await {
                    Ok(response) => {
                        if response.success {
                            return (StatusCode::OK, "File uploaded successfully").into_response();
                        } else {
                            return (StatusCode::INTERNAL_SERVER_ERROR, response.message).into_response();
                        }
                    }
                    Err(e) => {
                        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
                    }
                }
            }
            _ => continue,
        }
    }

    (StatusCode::BAD_REQUEST, "Missing file or path field").into_response()
}

pub async fn download_file(
    Path(path): Path<String>,
    State(state): State<Arc<ServerState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // 克隆path用于后续使用
    let filename = path.split('/').last().unwrap_or("download").to_string();
    
    // 获取会话名称
    let session_name = match headers.get("Session") {
        Some(name) => name.to_str().unwrap_or_default(),
        None => return (StatusCode::BAD_REQUEST, "Session name is required").into_response(),
    };

    if session_name.is_empty() {
        return (StatusCode::BAD_REQUEST, "Session name cannot be empty").into_response();
    }

    // 获取会话实例
    let session = match state.lookup(session_name) {
        Some(session) => session,
        None => return (StatusCode::BAD_REQUEST, "Session not found").into_response(),
    };

    // 生成请求token
    let token = format!("{}|{}", session_name, uuid::Uuid::new_v4().to_string());

    // 创建下载请求
    let request = FileDownloadRequest {
        path,
        token: token.clone(),
    };

    // 发送下载请求并获取响应流
    match session.download_file(request).await {
        Ok(stream) => {
            // 将响应流转换为字节流
            let byte_stream = stream.map(|result| {
                result.and_then(|response| Ok(response.chunk))
            });

            // 创建Body响应
            let body = Body::from_stream(byte_stream);
            
            // 设置响应头
            let mut headers = HeaderMap::new();
            headers.insert(
                axum::http::header::CONTENT_TYPE,
                "application/octet-stream".parse().unwrap(),
            );
            headers.insert(
                axum::http::header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", filename)
                    .parse()
                    .unwrap(),
            );

            (StatusCode::OK, headers, body).into_response()
        }
        Err(e) => {
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}