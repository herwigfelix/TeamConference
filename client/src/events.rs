//! Verarbeitung von Server-Ereignissen auf dem UI-Thread und
//! Hilfsfunktionen zum Aufbau der Listenmodelle.

use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use slint::{ModelRc, SharedString, StandardListViewItem, VecModel};
use tokio::sync::mpsc;

use crate::protocol::{
    AudioUserState, AuthResponse, FileDownloadData, FileInfo, FileUploadAck, Message, RoomInfo,
    StreamFileStatus, UserInfo,
};
use crate::state::AppState;
use crate::MainWindow;

/// Append a line to the chat history (with timestamp) and keep it bounded.
pub fn append_chat(ui: &MainWindow, state: &Arc<AppState>, line: &str) {
    let log = {
        let mut inner = state.inner.lock();
        let ts = chrono::Local::now().format("%H:%M").to_string();
        inner.chat_log.push_str(&format!("[{}] {}\n", ts, line));
        if inner.chat_log.len() > 200_000 {
            // keep the newest part, cut at a char boundary
            let cut = inner.chat_log.len() - 150_000;
            let cut = inner
                .chat_log
                .char_indices()
                .map(|(i, _)| i)
                .find(|&i| i >= cut)
                .unwrap_or(0);
            inner.chat_log = inner.chat_log[cut..].to_string();
        }
        inner.chat_log.clone()
    };
    ui.set_chat_len(log.len() as i32);
    ui.set_chat_text(log.into());
}

pub fn set_status(ui: &MainWindow, text: &str) {
    ui.set_status_text(text.into());
}

fn list_item(text: String) -> StandardListViewItem {
    StandardListViewItem::from(SharedString::from(text))
}

/// Rebuild the room list model (tree flattened with indentation),
/// preserving the current selection by room id.
pub fn rebuild_rooms(ui: &MainWindow, state: &Arc<AppState>) {
    let (rooms, prev_selected) = {
        let inner = state.inner.lock();
        let prev = ui.get_rooms_current();
        let prev_id = if prev >= 0 {
            inner.ui_room_ids.get(prev as usize).copied()
        } else {
            None
        };
        (inner.rooms.clone(), prev_id)
    };

    let mut items: Vec<StandardListViewItem> = Vec::new();
    let mut ids: Vec<i64> = Vec::new();

    fn add_level(
        rooms: &[RoomInfo],
        parent: Option<i64>,
        depth: usize,
        items: &mut Vec<StandardListViewItem>,
        ids: &mut Vec<i64>,
    ) {
        let mut level: Vec<&RoomInfo> = rooms
            .iter()
            .filter(|r| r.parent_id == parent)
            .collect();
        level.sort_by(|a, b| a.name.cmp(&b.name));
        for room in level {
            let indent = "    ".repeat(depth);
            let lock = if room.has_password { ", Passwort" } else { "" };
            items.push(list_item(format!(
                "{}{} ({} Nutzer{})",
                indent,
                room.name,
                room.users.len(),
                lock
            )));
            ids.push(room.id);
            add_level(rooms, Some(room.id), depth + 1, items, ids);
        }
    }
    add_level(&rooms, None, 0, &mut items, &mut ids);

    let new_index = prev_selected
        .and_then(|id| ids.iter().position(|&r| r == id))
        .map(|i| i as i32)
        .unwrap_or(-1);

    {
        let mut inner = state.inner.lock();
        inner.ui_room_ids = ids;
    }
    ui.set_rooms_model(ModelRc::new(VecModel::from(items)));
    ui.set_rooms_current(new_index);
}

fn user_label(u: &UserInfo) -> String {
    let mut label = u.nickname.clone();
    if !u.role.is_empty() && u.role != "user" {
        label.push_str(&format!(" [{}]", u.role));
    }
    if u.muted {
        label.push_str(", stumm");
    }
    if u.deafened {
        label.push_str(", taub");
    }
    label
}

/// Rebuild the user list for the current room.
pub fn rebuild_users(ui: &MainWindow, state: &Arc<AppState>) {
    let (users, room_name) = {
        let inner = state.inner.lock();
        match inner.current_room_id {
            Some(rid) => {
                let room = inner.rooms.iter().find(|r| r.id == rid);
                (
                    room.map(|r| r.users.clone()).unwrap_or_default(),
                    room.map(|r| r.name.clone())
                        .unwrap_or_else(|| "kein Raum".into()),
                )
            }
            None => (Vec::new(), "kein Raum".into()),
        }
    };

    let mut items: Vec<StandardListViewItem> = Vec::new();
    let mut ids: Vec<i64> = Vec::new();
    for u in &users {
        items.push(list_item(user_label(u)));
        ids.push(u.id);
    }

    {
        let mut inner = state.inner.lock();
        inner.ui_user_ids = ids;
    }
    ui.set_current_room_name(room_name.into());
    ui.set_users_model(ModelRc::new(VecModel::from(items)));
}

/// Rebuild the file list for the current room.
pub fn rebuild_files(ui: &MainWindow, state: &Arc<AppState>) {
    let files = {
        let inner = state.inner.lock();
        inner.ui_files.clone()
    };
    let items: Vec<StandardListViewItem> = files
        .iter()
        .map(|f| {
            let kb = (f.size_bytes as f64 / 1024.0).ceil() as i64;
            list_item(format!("{} ({} KB)", f.filename, kb))
        })
        .collect();
    ui.set_files_model(ModelRc::new(VecModel::from(items)));
}

/// Composed status line shown at the bottom of the window.
pub fn refresh_status(ui: &MainWindow, state: &Arc<AppState>) {
    let inner = state.inner.lock();
    if !inner.connected {
        ui.set_status_text("Nicht verbunden".into());
        return;
    }
    let server = inner.server_name.clone().unwrap_or_else(|| "Server".into());
    let room = inner
        .current_room_id
        .map(|id| inner.room_name(id))
        .unwrap_or_else(|| "kein Raum".into());
    let mic = if inner.muted { "stumm" } else { "an" };
    let ton = if inner.deafened { "aus" } else { "an" };
    let mut status = format!(
        "Verbunden mit {} | Raum: {} | Mikrofon: {} | Ton: {}",
        server, room, mic, ton
    );
    if inner.loopback {
        status.push_str(" | Loopback an");
    }
    if inner.streaming_file {
        status.push_str(" | Streaming läuft");
    }
    ui.set_status_text(status.into());
}

/// Main dispatcher: handles a server (or synthetic client) message on the UI thread.
pub fn handle(
    ui: &MainWindow,
    state: &Arc<AppState>,
    rt: &tokio::runtime::Handle,
    ev_tx: &mpsc::UnboundedSender<Message>,
    msg: Message,
) {
    match msg.msg_type.as_str() {
        "auth_response" => {
            ui.set_connecting(false);
            match serde_json::from_value::<AuthResponse>(msg.data) {
                Ok(resp) if resp.success => {
                    let server = resp.server_name.unwrap_or_else(|| "Server".into());
                    ui.set_window_title(format!("TeamConference — {}", server).into());
                    ui.set_active_view(1);
                    append_chat(ui, state, &format!("Verbunden mit {}.", server));
                    rebuild_rooms(ui, state);
                    rebuild_users(ui, state);
                    refresh_status(ui, state);
                }
                Ok(resp) => {
                    let err = resp.error.unwrap_or_else(|| "Unbekannter Fehler".into());
                    set_status(ui, &format!("Anmeldung fehlgeschlagen: {}", err));
                    append_chat(ui, state, &format!("Anmeldung fehlgeschlagen: {}", err));
                    crate::actions::do_disconnect(ui, state);
                }
                Err(e) => {
                    set_status(ui, &format!("Ungültige Serverantwort: {}", e));
                }
            }
        }

        "connection_lost" => {
            crate::actions::do_disconnect(ui, state);
            set_status(ui, "Verbindung zum Server verloren");
            append_chat(ui, state, "Verbindung zum Server verloren.");
        }

        // synthetic, from our own async tasks
        "client_error" => {
            ui.set_connecting(false);
            let text = msg
                .data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unbekannter Fehler")
                .to_string();
            set_status(ui, &text);
            append_chat(ui, state, &text);
        }

        "client_stream_finished" => {
            {
                let mut inner = state.inner.lock();
                inner.streaming_file = false;
            }
            ui.set_streaming(false);
            append_chat(ui, state, "Datei-Streaming beendet.");
            refresh_status(ui, state);
        }

        "room_list" => {
            // state.rooms wurde bereits im Netzwerk-Task aktualisiert
            rebuild_rooms(ui, state);
            rebuild_users(ui, state);
        }

        "room_user_joined" => {
            let room_id = msg.data.get("room_id").and_then(|v| v.as_i64());
            let user: Option<UserInfo> = msg
                .data
                .get("user")
                .and_then(|u| serde_json::from_value(u.clone()).ok());
            if let (Some(rid), Some(user)) = (room_id, user) {
                let (announce, room_name) = {
                    let mut inner = state.inner.lock();
                    if let Some(room) = inner.rooms.iter_mut().find(|r| r.id == rid) {
                        room.users.retain(|u| u.id != user.id);
                        room.users.push(user.clone());
                    }
                    (
                        inner.current_room_id == Some(rid),
                        inner.room_name(rid),
                    )
                };
                rebuild_rooms(ui, state);
                rebuild_users(ui, state);
                if announce {
                    append_chat(
                        ui,
                        state,
                        &format!("* {} hat den Raum {} betreten.", user.nickname, room_name),
                    );
                }
            }
        }

        "room_user_left" => {
            let room_id = msg.data.get("room_id").and_then(|v| v.as_i64());
            let user_id = msg.data.get("user_id").and_then(|v| v.as_i64());
            if let (Some(rid), Some(uid)) = (room_id, user_id) {
                let (announce, nick, room_name) = {
                    let mut inner = state.inner.lock();
                    let nick = inner.nickname_of(uid);
                    if let Some(room) = inner.rooms.iter_mut().find(|r| r.id == rid) {
                        room.users.retain(|u| u.id != uid);
                    }
                    (inner.current_room_id == Some(rid), nick, inner.room_name(rid))
                };
                rebuild_rooms(ui, state);
                rebuild_users(ui, state);
                if announce {
                    append_chat(
                        ui,
                        state,
                        &format!("* {} hat den Raum {} verlassen.", nick, room_name),
                    );
                }
            }
        }

        "chat_room" => {
            let nick = msg
                .data
                .get("from_user")
                .and_then(|u| u.get("nickname"))
                .and_then(|n| n.as_str())
                .unwrap_or("Unbekannt");
            let text = msg
                .data
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("");
            let room = msg
                .data
                .get("room_id")
                .and_then(|v| v.as_i64())
                .map(|rid| state.inner.lock().room_name(rid))
                .unwrap_or_default();
            append_chat(ui, state, &format!("[{}] {}: {}", room, nick, text));
        }

        "chat_private" => {
            let nick = msg
                .data
                .get("from_user")
                .and_then(|u| u.get("nickname"))
                .and_then(|n| n.as_str())
                .unwrap_or("Unbekannt");
            let text = msg
                .data
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("");
            append_chat(ui, state, &format!("[Privat] {}: {}", nick, text));
        }

        "chat_server" => {
            let text = msg
                .data
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("");
            append_chat(ui, state, &format!("[Server] {}", text));
        }

        "audio_user_state" => {
            if let Ok(st) = serde_json::from_value::<AudioUserState>(msg.data) {
                {
                    let mut inner = state.inner.lock();
                    for room in inner.rooms.iter_mut() {
                        if let Some(u) = room.users.iter_mut().find(|u| u.id == st.user_id) {
                            u.muted = st.muted;
                            u.deafened = st.deafened;
                        }
                    }
                }
                rebuild_users(ui, state);
            }
        }

        "file_list" => {
            if let Some(files) = msg.data.get("files") {
                if let Ok(list) = serde_json::from_value::<Vec<FileInfo>>(files.clone()) {
                    {
                        let mut inner = state.inner.lock();
                        inner.ui_files = list;
                    }
                    rebuild_files(ui, state);
                }
            }
        }

        "file_upload_ack" => {
            if let Ok(ack) = serde_json::from_value::<FileUploadAck>(msg.data) {
                let pending = {
                    let mut inner = state.inner.lock();
                    inner.pending_upload.take()
                };
                if !ack.success {
                    append_chat(ui, state, "Upload vom Server abgelehnt.");
                    return;
                }
                if let Some(upload) = pending {
                    append_chat(
                        ui,
                        state,
                        &format!("Lade {} hoch…", upload.filename),
                    );
                    let state2 = state.clone();
                    let ev_tx2 = ev_tx.clone();
                    rt.spawn(async move {
                        // 48 KiB Rohdaten je Chunk (durch 3 teilbar → saubere Base64-Grenzen)
                        const CHUNK: usize = 48 * 1024;
                        let mut offset: i64 = 0;
                        for chunk in upload.data.chunks(CHUNK) {
                            let msg = Message::new(
                                "file_upload_chunk",
                                serde_json::json!({
                                    "upload_id": ack.upload_id,
                                    "data": BASE64.encode(chunk),
                                    "offset": offset,
                                }),
                            );
                            if state2.send_ws(msg).is_err() {
                                return;
                            }
                            offset += chunk.len() as i64;
                        }
                        let _ = state2.send_ws(Message::new(
                            "file_upload_complete",
                            serde_json::json!({ "upload_id": ack.upload_id }),
                        ));
                        // Dateiliste danach aktualisieren
                        let room = state2.inner.lock().current_room_id;
                        if let Some(rid) = room {
                            let _ = state2.send_ws(Message::new(
                                "file_list",
                                serde_json::json!({ "room_id": rid }),
                            ));
                        }
                        let _ = ev_tx2.send(Message::new(
                            "client_error",
                            serde_json::json!({ "message": format!("Upload von {} abgeschlossen.", upload.filename) }),
                        ));
                    });
                }
            }
        }

        "file_download_data" => {
            if let Ok(data) = serde_json::from_value::<FileDownloadData>(msg.data) {
                let decoded = match BASE64.decode(data.data.as_bytes()) {
                    Ok(d) => d,
                    Err(e) => {
                        append_chat(ui, state, &format!("Download-Fehler (Base64): {}", e));
                        return;
                    }
                };
                let finished = {
                    let mut inner = state.inner.lock();
                    if let Some((_path, buf)) = inner.download_targets.get_mut(&data.file_id) {
                        buf.extend_from_slice(&decoded);
                        if buf.len() as i64 >= data.total {
                            inner.download_targets.remove(&data.file_id)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                };
                if let Some((path, buf)) = finished {
                    match std::fs::write(&path, &buf) {
                        Ok(()) => append_chat(
                            ui,
                            state,
                            &format!("Datei gespeichert: {}", path.display()),
                        ),
                        Err(e) => append_chat(
                            ui,
                            state,
                            &format!("Datei konnte nicht gespeichert werden: {}", e),
                        ),
                    }
                }
            }
        }

        "stream_file_status" => {
            if let Ok(st) = serde_json::from_value::<StreamFileStatus>(msg.data) {
                let nick = state.inner.lock().nickname_of(st.user_id);
                let verb = if st.playing { "spielt" } else { "stoppte" };
                append_chat(ui, state, &format!("* {} {} {}", nick, verb, st.filename));
            }
        }

        "user_kicked" | "kicked" => {
            let reason = msg
                .data
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("kein Grund angegeben");
            append_chat(
                ui,
                state,
                &format!("Du wurdest vom Server geworfen: {}", reason),
            );
            set_status(ui, "Vom Server geworfen");
        }

        "user_banned" | "banned" => {
            let reason = msg
                .data
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("kein Grund angegeben");
            append_chat(ui, state, &format!("Du wurdest gebannt: {}", reason));
            set_status(ui, "Vom Server gebannt");
        }

        "user_moved" | "moved" => {
            let room_id = msg.data.get("room_id").and_then(|v| v.as_i64());
            let room_name = msg
                .data
                .get("room_name")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            if let Some(rid) = room_id {
                {
                    let mut inner = state.inner.lock();
                    inner.current_room_id = Some(rid);
                }
                let name = room_name.unwrap_or_else(|| state.inner.lock().room_name(rid));
                append_chat(
                    ui,
                    state,
                    &format!("* Du wurdest in den Raum {} verschoben.", name),
                );
                rebuild_users(ui, state);
                refresh_status(ui, state);
                let _ = state.send_ws(Message::new(
                    "file_list",
                    serde_json::json!({ "room_id": rid }),
                ));
            }
        }

        "error" => {
            let text = msg
                .data
                .get("message")
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| msg.data.to_string());
            set_status(ui, &format!("Fehler: {}", text));
            append_chat(ui, state, &format!("Fehler: {}", text));
        }

        other => {
            tracing::debug!("Unhandled server message type: {}", other);
        }
    }
}
