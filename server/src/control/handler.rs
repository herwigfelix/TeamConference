use std::sync::Arc;
use futures_util::{StreamExt, SinkExt};
use tokio::sync::mpsc;
use tokio_rusqlite::Connection;
use tokio_tungstenite::tungstenite;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

use crate::config::Config;
use crate::control::protocol::*;
use crate::control::auth;
use crate::db::queries;
use crate::chat::handler as chat_handler;
use crate::admin::handler as admin_handler;
use crate::files::handler::FileHandler;
use crate::user::manager::UserManager;
use crate::room::manager::RoomManager;
use crate::audio::file_stream::AudioFileStreamer;
use crate::audio::udp_server::UdpAudioServer;

/// True, wenn der angemeldete Nutzer Admin ist.
async fn is_admin(state: &SharedState, uid: i64) -> bool {
    matches!(state.users.get_user(uid).await, Some(u) if u.is_admin())
}

/// Standard-Antwort bei fehlenden Rechten.
fn deny() -> Message {
    Message::new("error", serde_json::json!({ "message": "Insufficient permissions" }))
}

/// Bestätigung/Fehler für Account-Operationen.
fn account_ack(success: bool, message: &str) -> Message {
    Message::new(
        "account_ack",
        serde_json::json!({ "success": success, "message": message }),
    )
}

/// Aktuelle Account-Liste + Registrierungsstatus als Nachricht.
async fn account_list_msg(state: &SharedState) -> Message {
    let users = queries::list_users(&state.db).await.unwrap_or_default();
    let open = queries::is_registration_open(&state.db).await;
    let accounts: Vec<serde_json::Value> = users
        .iter()
        .map(|u| serde_json::json!({ "username": u.username, "role": u.role }))
        .collect();
    Message::new(
        "account_list_result",
        serde_json::json!({ "accounts": accounts, "registration_open": open }),
    )
}

pub struct SharedState {
    pub config: Config,
    pub db: Arc<Connection>,
    pub users: Arc<UserManager>,
    pub rooms: Arc<RoomManager>,
    pub files: Arc<FileHandler>,
    pub udp_server: Option<Arc<UdpAudioServer>>,
}

pub async fn handle_connection<S>(
    ws_stream: S,
    peer_addr: String,
    state: Arc<SharedState>,
) where
    S: futures_util::Stream<Item = Result<tungstenite::Message, tungstenite::Error>>
        + futures_util::Sink<tungstenite::Message, Error = tungstenite::Error>
        + Unpin
        + Send
        + 'static,
{
    let (mut ws_tx, mut ws_rx) = ws_stream.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    let mut user_id: Option<i64> = None;
    let mut audio_streamer: Option<AudioFileStreamer> = None;

    // Task for sending messages from channel to WebSocket
    let send_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Ok(json) = serde_json::to_string(&msg) {
                if ws_tx.send(tungstenite::Message::Text(json.into())).await.is_err() {
                    break;
                }
            }
        }
    });

    // Main receive loop
    while let Some(Ok(msg)) = ws_rx.next().await {
        let text = match msg {
            tungstenite::Message::Text(t) => t.to_string(),
            tungstenite::Message::Close(_) => break,
            tungstenite::Message::Ping(_) => {
                let _ = tx.send(Message::new("pong", serde_json::Value::Null));
                continue;
            }
            _ => continue,
        };

        let parsed: Message = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                tracing::debug!("Invalid message from {}: {}", peer_addr, e);
                continue;
            }
        };

        match parsed.msg_type.as_str() {
            "auth_login" => {
                let login: AuthLogin = match serde_json::from_value(parsed.data) {
                    Ok(l) => l,
                    Err(_) => continue,
                };

                let response = auth::handle_login(
                    login,
                    peer_addr.clone(),
                    &state.config,
                    &state.db,
                    &state.users,
                    &state.rooms,
                    tx.clone(),
                ).await;

                if response.success {
                    user_id = response.user_id;

                    // Send welcome message
                    let welcome = Message::new("chat_server", serde_json::json!({
                        "message": state.config.server.welcome_message
                    }));
                    let _ = tx.send(welcome);

                    // Deliver offline messages
                    if let Some(uid) = user_id {
                        let _ = chat_handler::deliver_offline_messages(uid, &state.users, &state.db).await;
                    }
                }

                let msg = Message::new("auth_response", serde_json::to_value(&response).unwrap());
                let _ = tx.send(msg);
            }

            "room_join" => {
                let Some(uid) = user_id else { continue };
                let req: RoomJoin = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                // Leave current room first
                let current_user = state.users.get_user(uid).await;
                if let Some(ref cu) = current_user {
                    if let Some(old_room) = cu.room_id {
                        let leave_msg = Message::new("room_user_left", serde_json::json!({
                            "room_id": old_room,
                            "user_id": uid
                        }));
                        state.users.broadcast_to_room(old_room, leave_msg, Some(uid)).await;
                    }
                }

                match state.rooms.join_room(uid, req.room_id, req.password.as_deref()).await {
                    Ok(()) => {
                        // Notify new room
                        let user = state.users.get_user(uid).await.unwrap();
                        let join_msg = Message::new("room_user_joined", serde_json::json!({
                            "room_id": req.room_id,
                            "user": user.to_info()
                        }));
                        state.users.broadcast_to_room(req.room_id, join_msg, Some(uid)).await;

                        // Send updated room list
                        let room_list = state.rooms.get_room_list().await.unwrap_or_default();
                        let list_msg = Message::new("room_list", serde_json::json!({
                            "rooms": room_list
                        }));
                        let _ = tx.send(list_msg);
                    }
                    Err(e) => {
                        let err_msg = Message::new("error", serde_json::json!({
                            "message": e.to_string()
                        }));
                        let _ = tx.send(err_msg);
                    }
                }
            }

            "room_leave" => {
                let Some(uid) = user_id else { continue };
                let req: RoomLeave = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                let leave_msg = Message::new("room_user_left", serde_json::json!({
                    "room_id": req.room_id,
                    "user_id": uid
                }));
                state.users.broadcast_to_room(req.room_id, leave_msg, Some(uid)).await;
                state.rooms.leave_room(uid).await;
            }

            "room_create" => {
                let Some(uid) = user_id else { continue };
                let req: RoomCreate = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                match state.users.get_user(uid).await {
                    Some(u) if u.is_admin() => {}
                    _ => {
                        let _ = tx.send(Message::new("error", serde_json::json!({
                            "message": "Insufficient permissions"
                        })));
                        continue;
                    }
                };

                match state.rooms.create_room(
                    req.name,
                    req.parent_id,
                    req.password,
                    req.max_users.unwrap_or(0),
                    req.sample_rate.unwrap_or(48000),
                    req.bit_depth.unwrap_or(16),
                    req.channels.unwrap_or(1),
                    req.bitrate.unwrap_or(0),
                ).await {
                    Ok(_) => {
                        // Broadcast updated room list to all
                        let room_list = state.rooms.get_room_list().await.unwrap_or_default();
                        let list_msg = Message::new("room_list", serde_json::json!({
                            "rooms": room_list
                        }));
                        state.users.broadcast_all(list_msg).await;
                    }
                    Err(e) => {
                        let _ = tx.send(Message::new("error", serde_json::json!({
                            "message": e.to_string()
                        })));
                    }
                }
            }

            "room_delete" => {
                let Some(uid) = user_id else { continue };
                let req: RoomDelete = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                match state.users.get_user(uid).await {
                    Some(u) if u.is_admin() => {}
                    _ => {
                        let _ = tx.send(Message::new("error", serde_json::json!({
                            "message": "Insufficient permissions"
                        })));
                        continue;
                    }
                };

                match state.rooms.delete_room(req.room_id).await {
                    Ok(()) => {
                        let room_list = state.rooms.get_room_list().await.unwrap_or_default();
                        let list_msg = Message::new("room_list", serde_json::json!({
                            "rooms": room_list
                        }));
                        state.users.broadcast_all(list_msg).await;
                    }
                    Err(e) => {
                        let _ = tx.send(Message::new("error", serde_json::json!({
                            "message": e.to_string()
                        })));
                    }
                }
            }

            "room_update" => {
                let Some(uid) = user_id else { continue };
                let req: RoomUpdate = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                match state.users.get_user(uid).await {
                    Some(u) if u.is_admin() => {}
                    _ => {
                        let _ = tx.send(Message::new("error", serde_json::json!({
                            "message": "Insufficient permissions"
                        })));
                        continue;
                    }
                };

                match state.rooms.update_room(req.room_id, req.name, req.password, req.max_users, req.sample_rate, req.bit_depth, req.channels, req.bitrate).await {
                    Ok(()) => {
                        let room_list = state.rooms.get_room_list().await.unwrap_or_default();
                        let list_msg = Message::new("room_list", serde_json::json!({
                            "rooms": room_list
                        }));
                        state.users.broadcast_all(list_msg).await;
                    }
                    Err(e) => {
                        let _ = tx.send(Message::new("error", serde_json::json!({
                            "message": e.to_string()
                        })));
                    }
                }
            }

            "chat_room" => {
                let Some(uid) = user_id else { continue };
                let chat: ChatRoom = match serde_json::from_value(parsed.data) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let _ = chat_handler::handle_room_chat(uid, chat, &state.users).await;
            }

            "chat_private" => {
                let Some(uid) = user_id else { continue };
                let chat: ChatPrivate = match serde_json::from_value(parsed.data) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let _ = chat_handler::handle_private_chat(uid, chat, &state.users, &state.db).await;
            }

            "audio_config" => {
                let Some(uid) = user_id else { continue };
                let req: AudioConfigRequest = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                // Validate settings
                let valid = req.sample_rate <= state.config.audio.max_sample_rate
                    && req.bit_depth <= state.config.audio.max_bit_depth
                    && (req.channels == 1 || req.channels == 2);

                if valid {
                    state.users.set_audio_config(uid, req.sample_rate, req.bit_depth, req.channels, req.enabled).await;
                    let user = state.users.get_user(uid).await.unwrap();
                    let ack = Message::new("audio_config_ack", serde_json::to_value(AudioConfigAck {
                        success: true,
                        udp_token: Some(user.session_token),
                    }).unwrap());
                    let _ = tx.send(ack);
                } else {
                    let ack = Message::new("audio_config_ack", serde_json::to_value(AudioConfigAck {
                        success: false,
                        udp_token: None,
                    }).unwrap());
                    let _ = tx.send(ack);
                }
            }

            "audio_mute" => {
                let Some(uid) = user_id else { continue };
                let req: AudioMute = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                state.users.set_muted(uid, req.muted).await;

                let user = state.users.get_user(uid).await.unwrap();
                if let Some(room_id) = user.room_id {
                    let state_msg = Message::new("audio_user_state", serde_json::to_value(AudioUserState {
                        user_id: uid,
                        muted: user.muted || user.admin_muted,
                        deafened: user.deafened,
                    }).unwrap());
                    state.users.broadcast_to_room(room_id, state_msg, None).await;
                }
            }

            "audio_deafen" => {
                let Some(uid) = user_id else { continue };
                let req: AudioDeafen = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                state.users.set_deafened(uid, req.deafened).await;

                let user = state.users.get_user(uid).await.unwrap();
                if let Some(room_id) = user.room_id {
                    let state_msg = Message::new("audio_user_state", serde_json::to_value(AudioUserState {
                        user_id: uid,
                        muted: user.muted || user.admin_muted,
                        deafened: user.deafened,
                    }).unwrap());
                    state.users.broadcast_to_room(room_id, state_msg, None).await;
                }
            }

            "audio_loopback" => {
                let Some(uid) = user_id else { continue };
                let req: AudioLoopback = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                state.users.set_loopback(uid, req.enabled).await;
                tracing::info!("User {} loopback: {}", uid, req.enabled);
            }

            "file_upload_start" => {
                let Some(uid) = user_id else { continue };
                let req: FileUploadStart = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                match state.files.start_upload(uid, req).await {
                    Ok(ack) => {
                        let _ = tx.send(Message::new("file_upload_ack", serde_json::to_value(ack).unwrap()));
                    }
                    Err(e) => {
                        let _ = tx.send(Message::new("file_upload_ack", serde_json::json!({
                            "upload_id": "",
                            "success": false,
                            "error": e.to_string()
                        })));
                    }
                }
            }

            "file_upload_chunk" => {
                let Some(_uid) = user_id else { continue };
                let chunk: FileUploadChunk = match serde_json::from_value(parsed.data) {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                let _ = state.files.write_chunk(chunk).await;
            }

            "file_upload_complete" => {
                let Some(uid) = user_id else { continue };
                let req: FileUploadComplete = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                if let Ok(file_id) = state.files.complete_upload(req).await {
                    let _ = tx.send(Message::new("file_upload_done", serde_json::json!({
                        "file_id": file_id,
                        "success": true
                    })));
                    // Aktualisierte Dateiliste an alle im Raum senden, damit der
                    // Client ohne manuelles Aktualisieren die neue Datei sieht.
                    if let Some(u) = state.users.get_user(uid).await {
                        if let Some(room_id) = u.room_id {
                            if let Ok(files) = state.files.get_file_list(room_id).await {
                                let list_msg = Message::new("file_list", serde_json::json!({
                                    "room_id": room_id,
                                    "files": files
                                }));
                                state.users.broadcast_to_room(room_id, list_msg, None).await;
                            }
                        }
                    }
                }
            }

            "file_list" => {
                let Some(_uid) = user_id else { continue };
                let req: FileListRequest = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                if let Ok(files) = state.files.get_file_list(req.room_id).await {
                    let _ = tx.send(Message::new("file_list", serde_json::json!({
                        "room_id": req.room_id,
                        "files": files
                    })));
                }
            }

            "file_download" => {
                let Some(_uid) = user_id else { continue };
                let req: FileDownloadRequest = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                match state.files.download_file(req.file_id).await {
                    Ok((_info, data)) => {
                        let encoded = BASE64.encode(&data);
                        let chunk_size = 64 * 1024; // 64KB chunks
                        let total = encoded.len() as i64;
                        let mut offset = 0i64;

                        for chunk in encoded.as_bytes().chunks(chunk_size) {
                            let chunk_str = String::from_utf8_lossy(chunk).to_string();
                            let _ = tx.send(Message::new("file_download_data", serde_json::json!({
                                "file_id": req.file_id,
                                "data": chunk_str,
                                "offset": offset,
                                "total": total
                            })));
                            offset += chunk.len() as i64;
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Message::new("error", serde_json::json!({
                            "message": format!("Download failed: {}", e)
                        })));
                    }
                }
            }

            "admin_kick" => {
                let Some(uid) = user_id else { continue };
                let req: AdminKick = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                if let Err(e) = admin_handler::handle_kick(uid, req, &state.users, &state.rooms).await {
                    let _ = tx.send(Message::new("error", serde_json::json!({
                        "message": e.to_string()
                    })));
                }
            }

            "admin_ban" => {
                let Some(uid) = user_id else { continue };
                let req: AdminBan = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                if let Err(e) = admin_handler::handle_ban(uid, req, &state.users, &state.rooms, &state.db).await {
                    let _ = tx.send(Message::new("error", serde_json::json!({
                        "message": e.to_string()
                    })));
                }
            }

            "admin_move" => {
                let Some(uid) = user_id else { continue };
                let req: AdminMove = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                if let Err(e) = admin_handler::handle_move(uid, req, &state.users, &state.rooms).await {
                    let _ = tx.send(Message::new("error", serde_json::json!({
                        "message": e.to_string()
                    })));
                }
            }

            "admin_mute" => {
                let Some(uid) = user_id else { continue };
                let req: AdminMute = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                if let Err(e) = admin_handler::handle_admin_mute(uid, req, &state.users).await {
                    let _ = tx.send(Message::new("error", serde_json::json!({
                        "message": e.to_string()
                    })));
                }
            }

            "admin_server_message" => {
                let Some(uid) = user_id else { continue };
                let req: AdminServerMessage = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                match state.users.get_user(uid).await {
                    Some(u) if u.is_admin() => {}
                    _ => continue,
                };
                chat_handler::send_server_message(req.message, &state.users).await;
            }

            "stream_file_start" => {
                let Some(uid) = user_id else { continue };
                let req: StreamFileStart = match serde_json::from_value(parsed.data) {
                    Ok(r) => r,
                    Err(_) => continue,
                };

                // Stop any existing stream
                if let Some(ref streamer) = audio_streamer {
                    streamer.stop();
                }

                if let Some(ref udp) = state.udp_server {
                    let streamer = AudioFileStreamer::new();
                    let udp_clone = udp.clone();
                    let users_clone = state.users.clone();
                    let path = std::path::PathBuf::from(&req.filename);

                    let _ = streamer.stream_file(&path, req.room_id, uid, udp_clone, users_clone).await;
                    audio_streamer = Some(streamer);
                }
            }

            "stream_file_stop" => {
                if let Some(ref streamer) = audio_streamer {
                    streamer.stop();
                }
                audio_streamer = None;
            }

            // ── Account-Verwaltung (admin-only, außer password_change) ──

            "account_list" => {
                let Some(uid) = user_id else { continue };
                if !is_admin(&state, uid).await {
                    let _ = tx.send(deny());
                    continue;
                }
                let _ = tx.send(account_list_msg(&state).await);
            }

            "account_create" => {
                let Some(uid) = user_id else { continue };
                if !is_admin(&state, uid).await {
                    let _ = tx.send(deny());
                    continue;
                }
                let username = parsed.data.get("username").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                let password = parsed.data.get("password").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let role = match parsed.data.get("role").and_then(|v| v.as_str()) {
                    Some("admin") => "admin",
                    _ => "user",
                }.to_string();
                if username.is_empty() || password.is_empty() {
                    let _ = tx.send(account_ack(false, "Benutzername und Passwort sind erforderlich"));
                    continue;
                }
                if queries::find_user_by_username(&state.db, username.clone()).await.ok().flatten().is_some() {
                    let _ = tx.send(account_ack(false, "Benutzername existiert bereits"));
                    continue;
                }
                match queries::create_user(&state.db, username.clone(), password, role).await {
                    Ok(_) => {
                        let _ = tx.send(account_ack(true, &format!("Konto '{}' angelegt", username)));
                        let _ = tx.send(account_list_msg(&state).await);
                    }
                    Err(e) => { let _ = tx.send(account_ack(false, &format!("Fehler: {}", e))); }
                }
            }

            "account_delete" => {
                let Some(uid) = user_id else { continue };
                if !is_admin(&state, uid).await {
                    let _ = tx.send(deny());
                    continue;
                }
                let username = parsed.data.get("username").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                match queries::find_user_by_username(&state.db, username.clone()).await.ok().flatten() {
                    Some(target) if target.id == uid => {
                        let _ = tx.send(account_ack(false, "Das eigene Konto kann nicht gelöscht werden"));
                    }
                    Some(target) => match queries::delete_user(&state.db, target.id).await {
                        Ok(()) => {
                            let _ = tx.send(account_ack(true, &format!("Konto '{}' gelöscht", username)));
                            let _ = tx.send(account_list_msg(&state).await);
                        }
                        Err(e) => { let _ = tx.send(account_ack(false, &format!("Fehler: {}", e))); }
                    },
                    None => { let _ = tx.send(account_ack(false, "Konto nicht gefunden")); }
                }
            }

            "account_set_password" => {
                let Some(uid) = user_id else { continue };
                if !is_admin(&state, uid).await {
                    let _ = tx.send(deny());
                    continue;
                }
                let username = parsed.data.get("username").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                let password = parsed.data.get("password").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if password.is_empty() {
                    let _ = tx.send(account_ack(false, "Passwort darf nicht leer sein"));
                    continue;
                }
                match queries::find_user_by_username(&state.db, username.clone()).await.ok().flatten() {
                    Some(target) => match queries::update_password(&state.db, target.id, password).await {
                        Ok(()) => { let _ = tx.send(account_ack(true, &format!("Passwort für '{}' geändert", username))); }
                        Err(e) => { let _ = tx.send(account_ack(false, &format!("Fehler: {}", e))); }
                    },
                    None => { let _ = tx.send(account_ack(false, "Konto nicht gefunden")); }
                }
            }

            "account_set_role" => {
                let Some(uid) = user_id else { continue };
                if !is_admin(&state, uid).await {
                    let _ = tx.send(deny());
                    continue;
                }
                let username = parsed.data.get("username").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
                let role = match parsed.data.get("role").and_then(|v| v.as_str()) {
                    Some("admin") => "admin",
                    _ => "user",
                }.to_string();
                match queries::find_user_by_username(&state.db, username.clone()).await.ok().flatten() {
                    Some(target) => match queries::update_role(&state.db, target.id, role.clone()).await {
                        Ok(()) => {
                            let _ = tx.send(account_ack(true, &format!("Rolle von '{}' = {}", username, role)));
                            let _ = tx.send(account_list_msg(&state).await);
                        }
                        Err(e) => { let _ = tx.send(account_ack(false, &format!("Fehler: {}", e))); }
                    },
                    None => { let _ = tx.send(account_ack(false, "Konto nicht gefunden")); }
                }
            }

            "account_set_registration" => {
                let Some(uid) = user_id else { continue };
                if !is_admin(&state, uid).await {
                    let _ = tx.send(deny());
                    continue;
                }
                let open = parsed.data.get("open").and_then(|v| v.as_bool()).unwrap_or(false);
                match queries::set_registration(&state.db, open).await {
                    Ok(()) => {
                        let _ = tx.send(account_ack(true, if open { "Registrierung aktiviert" } else { "Registrierung deaktiviert" }));
                        let _ = tx.send(account_list_msg(&state).await);
                    }
                    Err(e) => { let _ = tx.send(account_ack(false, &format!("Fehler: {}", e))); }
                }
            }

            "password_change" => {
                // Self-Service: jeder angemeldete Nutzer ändert sein eigenes Passwort
                let Some(uid) = user_id else { continue };
                let old = parsed.data.get("old_password").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let new = parsed.data.get("new_password").and_then(|v| v.as_str()).unwrap_or("").to_string();
                if new.is_empty() {
                    let _ = tx.send(account_ack(false, "Neues Passwort darf nicht leer sein"));
                    continue;
                }
                let username = match state.users.get_user(uid).await {
                    Some(u) => u.username.clone(),
                    None => continue,
                };
                // Altes Passwort prüfen
                match queries::authenticate_user(&state.db, username, old).await {
                    Ok(Some(_)) => match queries::update_password(&state.db, uid, new).await {
                        Ok(()) => { let _ = tx.send(account_ack(true, "Dein Passwort wurde geändert")); }
                        Err(e) => { let _ = tx.send(account_ack(false, &format!("Fehler: {}", e))); }
                    },
                    _ => { let _ = tx.send(account_ack(false, "Altes Passwort ist falsch")); }
                }
            }

            _ => {
                tracing::debug!("Unknown message type: {}", parsed.msg_type);
            }
        }
    }

    // Cleanup on disconnect
    if let Some(uid) = user_id {
        if let Some(ref streamer) = audio_streamer {
            streamer.stop();
        }

        if let Some(user) = state.users.get_user(uid).await {
            if let Some(room_id) = user.room_id {
                let leave_msg = Message::new("room_user_left", serde_json::json!({
                    "room_id": room_id,
                    "user_id": uid
                }));
                state.users.broadcast_to_room(room_id, leave_msg, Some(uid)).await;
            }
        }

        state.users.remove_user(uid).await;
        tracing::info!("User {} disconnected", uid);
    }

    send_task.abort();
}
