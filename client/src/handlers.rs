//! Verarbeitung eingehender Server-Nachrichten auf dem UI-Thread sowie
//! Aufbau der Raum-, Nutzer-, Datei- und Serverliste.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use rust_i18n::t;
use wxdragon::prelude::*;

use crate::app::Ctx;
use crate::protocol::{
    AudioUserState, AuthResponse, FileDownloadData, FileInfo, FileUploadAck, Message,
    StreamFileStatus, UserInfo,
};

/// Räume-und-Nutzer-Baum neu aufbauen.
pub fn rebuild_tree(ctx: &Ctx) {
    let (rooms, current) = {
        let inner = ctx.app.inner.lock();
        (inner.rooms.clone(), inner.current_room_id)
    };
    let mut st = ctx.st.borrow_mut();
    crate::roomtree::rebuild(&ctx.ui.rooms_tree, &rooms, current, &mut st.tree_map);
}

/// Dateiliste neu aufbauen.
pub fn rebuild_files(ctx: &Ctx) {
    let files: Vec<FileInfo> = ctx.app.inner.lock().current_files.clone();
    ctx.ui.files.clear();
    for f in &files {
        let kb = f.size_bytes.saturating_add(1023) / 1024;
        ctx.ui.files.append(&t!("files.entry", file = f.filename, kb = kb));
    }
    ctx.st.borrow_mut().files = files;
}

/// Serverliste (Verbindungsansicht) neu aufbauen.
pub fn rebuild_server_list(ctx: &Ctx) {
    let servers = ctx.st.borrow().servers.clone();
    ctx.ui.server_list.clear();
    for s in &servers {
        ctx.ui.server_list.append(&s.label());
    }
}

/// Statuszeile aus dem aktuellen Zustand zusammensetzen.
pub fn refresh_status(ctx: &Ctx) {
    let inner = ctx.app.inner.lock();
    if !inner.connected {
        ctx.ui.set_status(&t!("status.not_connected"));
        return;
    }
    let server = inner.server_name.clone().unwrap_or_else(|| t!("status.server").to_string());
    let room = inner
        .current_room_id
        .map(|id| inner.room_name(id))
        .unwrap_or_else(|| t!("status.no_room").to_string());
    let mic = if inner.muted { t!("status.mic_muted") } else { t!("status.state_on") };
    let ton = if inner.deafened { t!("status.state_off") } else { t!("status.state_on") };
    let mut s = t!("status.connected", server = server, room = room, mic = mic, sound = ton).to_string();
    if inner.loopback {
        s.push_str(&t!("status.loopback_on"));
    }
    if inner.streaming_file {
        s.push_str(&t!("status.streaming"));
    }
    drop(inner);
    ctx.ui.set_status(&s);
}

/// Eine eingehende (oder synthetische) Nachricht auf dem UI-Thread verarbeiten.
pub fn handle(ctx: &Ctx, msg: Message) {
    let ui = &ctx.ui;
    match msg.msg_type.as_str() {
        "auth_response" => match serde_json::from_value::<AuthResponse>(msg.data) {
            Ok(resp) if resp.success => {
                let server = resp.server_name.unwrap_or_else(|| t!("status.server").to_string());
                // Mobil steht der Titel groß in der Navigationsleiste — dort nur
                // der Servername, sonst wird er abgeschnitten.
                #[cfg(any(target_os = "ios", target_os = "android"))]
                ui.frame.set_title(&server);
                #[cfg(not(any(target_os = "ios", target_os = "android")))]
                ui.frame.set_title(&format!("TeamConference — {}", server));
                ui.show_main(true);
                // War dies eine erfolgreiche Wiederverbindung? Dann Reconnect-
                // Zustand beenden und ggf. den vorherigen Raum erneut betreten.
                let reconnect = ctx.st.borrow_mut().reconnect.take();
                if let Some(rc) = reconnect {
                    ui.append_chat(&t!("reconnect.restored"));
                    if let Some(rid) = rc.room_id {
                        crate::actions::rejoin_room(ctx, rid, rc.room_password);
                    }
                } else {
                    ui.append_chat(&t!("connect.connected_to", server = server));
                }
                rebuild_tree(ctx);
                rebuild_files(ctx);
                refresh_status(ctx);
                // Admin-only-Menüpunkte je nach eigener Rolle ein-/ausblenden
                crate::actions::update_account_menu(ctx);
                // Fokus in die Hauptansicht setzen, damit der Screenreader mitwandert
                ui.rooms_tree.set_focus();
            }
            Ok(resp) => {
                let err = crate::i18n::server_error(&resp.error_code, &resp.error_params, resp.error.as_deref());
                crate::actions::do_disconnect(ctx);
                crate::actions::notify(ctx, &t!("connect.login_failed", error = err), &t!("connect.caption"));
            }
            Err(e) => crate::actions::notify(ctx, &t!("connect.invalid_response", error = e), &t!("connect.caption")),
        },

        "connection_lost" => {
            // Bei gewünschter Sitzung erst Wiederverbindung versuchen; nur ein
            // manuelles Trennen (session == None) ist endgültig.
            if ctx.st.borrow().session.is_some() {
                crate::actions::begin_reconnect(ctx);
            }
        }

        // synthetisch: ein Verbindungsaufbau (Erst- oder Wiederverbindung) ist
        // gescheitert, bevor eine WebSocket stand.
        "connect_failed" => {
            let text = msg
                .data
                .get("message")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| t!("connect.failed").to_string());
            if ctx.st.borrow().reconnect.is_some() {
                // Laufender Wiederverbindungsversuch → nächsten Versuch planen.
                ui.append_chat(&t!("reconnect.failed_with", error = text));
                crate::actions::schedule_reconnect(ctx);
            } else {
                // Erstverbindung gescheitert → sauber trennen und melden.
                crate::actions::do_disconnect(ctx);
                ui.append_chat(&text);
                crate::actions::notify(ctx, &text, &t!("connect.caption"));
            }
        }

        // synthetisch aus eigenen Tokio-Tasks (Verbindungs-/Streaming-Fehler, Hinweise)
        "client_error" => {
            let text = msg
                .data
                .get("message")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| t!("common.unknown_error").to_string());
            ui.append_chat(&text);
            crate::actions::notify_async(ctx, &text, "TeamConference");
        }

        // Server-Hub: Ergebnis einer Hub-Aktion (Login/Registrierung/Reset).
        "hub_msg" => {
            let text = msg
                .data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            ui.append_hub_log(&text);
            crate::actions::update_hub_view(ctx);
            crate::actions::notify_async(ctx, &text, &t!("hub.caption"));
        }

        // Server-Hub: Verzeichnis neu laden (nach Login/Anlegen/Bearbeiten/Löschen).
        // `mine = true` schaltet zusätzlich auf „Meine Server" um — private
        // Server erscheinen im öffentlichen Verzeichnis nicht.
        "hub_dir_reload" => {
            if msg.data.get("mine").and_then(|v| v.as_bool()).unwrap_or(false) {
                crate::actions::hub_show_my_servers(ctx);
            } else {
                crate::actions::hub_load_directory(ctx);
            }
        }

        // Server-Hub: aktualisierte Serverliste → Liste füllen. Host/Port werden
        // NICHT angezeigt (immer derselbe Hub); dafür sagt der Eintrag, ob der
        // Server öffentlich oder privat ist und in welcher Rolle man ihn sieht —
        // sonst ist einem Screenreader-Nutzer nicht erkennbar, warum ein Server
        // nur in einer der beiden Ansichten auftaucht.
        "hub_servers" => {
            let servers: Vec<crate::hub::ServerInfo> = serde_json::from_value(
                msg.data.get("servers").cloned().unwrap_or_default(),
            )
            .unwrap_or_default();
            let mine_view = msg.data.get("scope").and_then(|v| v.as_str()) == Some("mine");
            let my_uid = crate::config::load_config()
                .hub
                .map(|h| h.central_uid)
                .unwrap_or_default();
            ui.hub_servers.clear();
            for s in &servers {
                let mut label = if s.description.trim().is_empty() {
                    s.name.clone()
                } else {
                    format!("{} — {}", s.name, s.description)
                };
                label.push_str(&if s.is_public { t!("hub_list.public") } else { t!("hub_list.private") });
                if !my_uid.is_empty() && s.owner_uid == my_uid {
                    label.push_str(&t!("hub_list.own"));
                } else if mine_view {
                    label.push_str(&t!("hub_list.member"));
                }
                ui.hub_servers.append(&label);
            }
            if servers.is_empty() {
                ui.append_hub_log(&if mine_view {
                    t!("hub_list.none_mine")
                } else {
                    t!("hub_list.none_public")
                });
            } else {
                ui.append_hub_log(&if mine_view {
                    t!("hub_list.loaded_mine", count = servers.len())
                } else {
                    t!("hub_list.loaded_public", count = servers.len())
                });
            }
            ctx.st.borrow_mut().hub_servers = servers;
        }

        // Auto-Updater: neue Version verfügbar → nachfragen und ggf. laden.
        "client_update" => {
            let version = msg.data.get("version").and_then(|v| v.as_str()).unwrap_or("?").to_string();
            let url = msg.data.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let filename = msg.data.get("filename").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let q = MessageDialog::builder(
                &ui.frame,
                &t!(
                    "update.available",
                    version = version,
                    current = crate::update::current_version()
                ),
                &t!("update.available_title"),
            )
            .with_style(MessageDialogStyle::YesNo)
            .build();
            if q.show_modal() == ID_YES {
                if url.is_empty() {
                    // Kein passendes Paket gefunden → Release-Seite öffnen.
                    crate::update::open_path(&crate::update::releases_page());
                } else if cfg!(target_os = "android") {
                    // Android: die APK im Browser laden lassen; Android bietet
                    // danach die Installation an (keine eigene Install-Berechtigung).
                    let _ = &filename;
                    crate::update::open_path(&url);
                    ui.append_chat(&t!("update.android_browser_download"));
                } else {
                    ui.append_chat(&t!("update.downloading"));
                    crate::update::download_update(ctx.ev_tx.clone(), url, filename);
                }
            }
        }

        "client_update_done" => {
            let path = msg.data.get("path").and_then(|v| v.as_str()).unwrap_or("").to_string();

            // Windows: das ZIP automatisch entpacken, die laufende Installation
            // ersetzen und neu starten. Klappt das, beendet sich der Client,
            // damit die gesperrte EXE überschrieben werden kann.
            #[cfg(target_os = "windows")]
            {
                match crate::update::apply_update_windows(&path) {
                    Ok(()) => {
                        crate::actions::notify(
                            ctx,
                            &t!("update.applying"),
                            &t!("update.title"),
                        );
                        // Prozess beenden, damit der Updater die Dateien ersetzen kann.
                        std::process::exit(0);
                    }
                    Err(e) => {
                        // Fallback: Paket nur öffnen, manuell installieren.
                        crate::actions::notify(
                            ctx,
                            &t!("update.apply_failed", error = e, path = path),
                            &t!("update.title"),
                        );
                        crate::update::open_path(&path);
                    }
                }
            }

            #[cfg(not(target_os = "windows"))]
            {
                crate::actions::notify(
                    ctx,
                    &t!("update.downloaded", path = path),
                    &t!("update.title"),
                );
                crate::update::open_path(&path);
            }
        }

        "client_stream_finished" => {
            ctx.app.inner.lock().streaming_file = false;
            ctx.app
                .stream_paused
                .store(false, std::sync::atomic::Ordering::Relaxed);
            crate::actions::set_menu_check(ctx, crate::ui::ID_PAUSE_STREAM, false);
            ui.append_chat(&t!("stream.finished"));
            refresh_status(ctx);
        }

        "room_list" => {
            // inner.rooms wurde bereits im Netzwerk-Task aktualisiert
            rebuild_tree(ctx);
            // Eigene Rolle kann sich geändert haben → Admin-Menü anpassen.
            crate::actions::update_account_menu(ctx);
        }

        "room_user_joined" => {
            let room_id = msg.data.get("room_id").and_then(|v| v.as_i64());
            let user: Option<UserInfo> = msg
                .data
                .get("user")
                .and_then(|u| serde_json::from_value(u.clone()).ok());
            if let (Some(rid), Some(user)) = (room_id, user) {
                let (announce, room_name) = {
                    let mut inner = ctx.app.inner.lock();
                    if let Some(room) = inner.rooms.iter_mut().find(|r| r.id == rid) {
                        room.users.retain(|u| u.id != user.id);
                        room.users.push(user.clone());
                    }
                    // Neuer Nutzer bringt sein Token mit → Zuordnung aktualisieren.
                    inner.rebuild_token_map();
                    (inner.current_room_id == Some(rid), inner.room_name(rid))
                };
                rebuild_tree(ctx);
                if announce {
                    let line = t!("events.room_joined", name = user.nickname, room = room_name).to_string();
                    ui.append_chat(&format!("* {}", line));
                    crate::actions::announce_event(ctx, &line);
                }
            }
        }

        "room_user_left" => {
            let room_id = msg.data.get("room_id").and_then(|v| v.as_i64());
            let user_id = msg.data.get("user_id").and_then(|v| v.as_i64());
            if let (Some(rid), Some(uid)) = (room_id, user_id) {
                let (announce, nick, room_name) = {
                    let mut inner = ctx.app.inner.lock();
                    let nick = inner.nickname_of(uid);
                    if let Some(room) = inner.rooms.iter_mut().find(|r| r.id == rid) {
                        room.users.retain(|u| u.id != uid);
                    }
                    inner.rebuild_token_map();
                    (inner.current_room_id == Some(rid), nick, inner.room_name(rid))
                };
                rebuild_tree(ctx);
                if announce {
                    let line = t!("events.room_left", name = nick, room = room_name).to_string();
                    ui.append_chat(&format!("* {}", line));
                    crate::actions::announce_event(ctx, &line);
                }
            }
        }

        // Serverweite Presence-Events (jemand meldet sich an bzw. ab). Diese
        // betreffen nicht zwingend den eigenen Raum, daher eigene Ansage.
        "user_connected" => {
            let nick = msg
                .data
                .get("nickname")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| t!("events.someone").to_string());
            let line = t!("events.server_joined", name = nick).to_string();
            ui.append_chat(&format!("* {}", line));
            crate::actions::announce_event(ctx, &line);
        }

        "user_disconnected" => {
            let nick = msg
                .data
                .get("nickname")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| t!("events.someone").to_string());
            let line = t!("events.server_left", name = nick).to_string();
            ui.append_chat(&format!("* {}", line));
            crate::actions::announce_event(ctx, &line);
        }

        "chat_room" => {
            let nick = msg
                .data
                .get("from_user")
                .and_then(|u| u.get("nickname"))
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| t!("events.unknown").to_string());
            let text = msg.data.get("message").and_then(|m| m.as_str()).unwrap_or("");
            let room = msg
                .data
                .get("room_id")
                .and_then(|v| v.as_i64())
                .map(|rid| ctx.app.inner.lock().room_name(rid))
                .unwrap_or_default();
            ui.append_chat(&format!("[{}] {}: {}", room, nick, text));
            // Raumnachrichten im aktuellen Raum ansagen (der Server stellt
            // chat_room nur an Mitglieder des Raums zu).
            crate::actions::announce_event(ctx, &format!("{}: {}", nick, text));
        }

        "chat_private" => {
            let nick = msg
                .data
                .get("from_user")
                .and_then(|u| u.get("nickname"))
                .and_then(|n| n.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| t!("events.unknown").to_string());
            let text = msg.data.get("message").and_then(|m| m.as_str()).unwrap_or("");
            ui.append_chat(&t!("chat.private_from", name = nick, text = text));
        }

        "chat_server" => {
            let text = msg.data.get("message").and_then(|m| m.as_str()).unwrap_or("");
            ui.append_chat(&t!("chat.server", text = text));
        }

        "audio_user_state" => {
            if let Ok(st) = serde_json::from_value::<AudioUserState>(msg.data) {
                {
                    let mut inner = ctx.app.inner.lock();
                    for room in inner.rooms.iter_mut() {
                        if let Some(u) = room.users.iter_mut().find(|u| u.id == st.user_id) {
                            u.muted = st.muted;
                            u.deafened = st.deafened;
                        }
                    }
                }
                rebuild_tree(ctx);
            }
        }

        "file_list" => {
            if let Some(files) = msg.data.get("files") {
                if let Ok(list) = serde_json::from_value::<Vec<FileInfo>>(files.clone()) {
                    ctx.app.inner.lock().current_files = list;
                    rebuild_files(ctx);
                }
            }
        }

        "file_upload_ack" => {
            // Grund einer Ablehnung: Code → übersetzt, sonst Klartext des Servers.
            let reason = if msg.data.get("code").is_some() {
                crate::i18n::server_text(&msg.data)
            } else {
                msg.data.get("error").and_then(|v| v.as_str()).unwrap_or("").to_string()
            };
            if let Ok(ack) = serde_json::from_value::<FileUploadAck>(msg.data) {
                let pending = ctx.app.inner.lock().pending_upload.take();
                if !ack.success {
                    if reason.is_empty() {
                        ui.append_chat(&t!("files.upload_rejected"));
                    } else {
                        ui.append_chat(&t!("files.upload_rejected_reason", reason = reason));
                    }
                    return;
                }
                if let Some(upload) = pending {
                    ui.append_chat(&t!("files.uploading", file = upload.filename));
                    let app = ctx.app.clone();
                    let ev_tx = ctx.ev_tx.clone();
                    ctx.rt.spawn(async move {
                        const CHUNK: usize = 48 * 1024; // durch 3 teilbar → saubere Base64-Grenzen
                        let mut offset: i64 = 0;
                        for chunk in upload.data.chunks(CHUNK) {
                            let m = Message::new(
                                "file_upload_chunk",
                                serde_json::json!({
                                    "upload_id": ack.upload_id,
                                    "data": BASE64.encode(chunk),
                                    "offset": offset,
                                }),
                            );
                            if app.send_ws(m).is_err() {
                                return;
                            }
                            offset += chunk.len() as i64;
                        }
                        let _ = app.send_ws(Message::new(
                            "file_upload_complete",
                            serde_json::json!({ "upload_id": ack.upload_id }),
                        ));
                        let room = app.inner.lock().current_room_id;
                        if let Some(rid) = room {
                            let _ = app.send_ws(Message::new(
                                "file_list",
                                serde_json::json!({ "room_id": rid }),
                            ));
                        }
                        let _ = ev_tx.send(Message::new(
                            "client_error",
                            serde_json::json!({ "message": t!("files.upload_done", file = upload.filename) }),
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
                        ui.append_chat(&t!("files.download_error", error = e));
                        return;
                    }
                };
                let finished = {
                    let mut inner = ctx.app.inner.lock();
                    if let Some((_p, buf)) = inner.download_targets.get_mut(&data.file_id) {
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
                        // iOS/Android: Der Sandbox-Pfad (…/Containers/Data/Application/<UUID>/…)
                        // hilft niemandem; sagen, wo die Datei in der Dateien-App liegt.
                        #[cfg(target_os = "ios")]
                        Ok(()) => {
                            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let text = t!("files.saved_ios", name = name).to_string();
                            ui.append_chat(&text);
                            crate::actions::notify_async(ctx, &text, &t!("files.download_caption"));
                        }
                        #[cfg(target_os = "android")]
                        Ok(()) => {
                            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                            let text = t!("files.saved", path = name).to_string();
                            ui.append_chat(&text);
                            crate::actions::notify_async(ctx, &text, &t!("files.download_caption"));
                        }
                        #[cfg(not(any(target_os = "ios", target_os = "android")))]
                        Ok(()) => ui.append_chat(&t!("files.saved", path = path.display())),
                        Err(e) => {
                            ui.append_chat(&t!("files.save_failed", error = e))
                        }
                    }
                }
            }
        }

        "stream_file_status" => {
            if let Ok(st) = serde_json::from_value::<StreamFileStatus>(msg.data) {
                let nick = ctx.app.inner.lock().nickname_of(st.user_id);
                ui.append_chat(&if st.playing {
                    t!("events.stream_playing", name = nick, file = st.filename)
                } else {
                    t!("events.stream_stopped", name = nick, file = st.filename)
                });
            }
        }

        "user_kicked" | "kicked" => {
            let reason = msg
                .data
                .get("reason")
                .and_then(|r| r.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| t!("events.no_reason").to_string());
            ui.append_chat(&t!("events.kicked", reason = reason));
            ui.set_status(&t!("events.kicked_status"));
        }

        "user_banned" | "banned" => {
            let reason = msg
                .data
                .get("reason")
                .and_then(|r| r.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| t!("events.no_reason").to_string());
            ui.append_chat(&t!("events.banned", reason = reason));
            ui.set_status(&t!("events.banned_status"));
        }

        "user_moved" | "moved" => {
            let room_id = msg.data.get("room_id").and_then(|v| v.as_i64());
            if let Some(rid) = room_id {
                {
                    let mut inner = ctx.app.inner.lock();
                    inner.current_room_id = Some(rid);
                    // Serverseitiger Wechsel ohne Passwort → kein Raum-Passwort merken.
                    inner.current_room_password = None;
                }
                let name = ctx.app.inner.lock().room_name(rid);
                ui.append_chat(&t!("events.moved", room = name));
                rebuild_tree(ctx);
                refresh_status(ctx);
                let _ = ctx
                    .app
                    .send_ws(Message::new("file_list", serde_json::json!({ "room_id": rid })));
            }
        }

        "account_ack" => {
            let success = msg.data.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
            let text = crate::i18n::server_text(&msg.data);
            let prefix = if success { t!("accounts.prefix") } else { t!("accounts.prefix_error") };
            ui.set_status(&format!("{}: {}", prefix, text));
            ui.append_chat(&format!("[{}] {}", prefix, text));
            // Bei offenem Konten-Dialog für Screenreader-Nutzer rückmelden.
            if ctx.st.borrow().account_dialog.is_some() {
                crate::actions::notify(ctx, &text, &prefix);
            }
        }

        "account_list_result" => {
            let open = msg
                .data
                .get("registration_open")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            ctx.st.borrow_mut().registration_open = open;

            // (Benutzername, Rolle) sammeln.
            let mut accounts: Vec<(String, String)> = Vec::new();
            if let Some(arr) = msg.data.get("accounts").and_then(|v| v.as_array()) {
                for a in arr {
                    let name = a.get("username").and_then(|v| v.as_str()).unwrap_or("?");
                    let role = a.get("role").and_then(|v| v.as_str()).unwrap_or("user");
                    accounts.push((name.to_string(), role.to_string()));
                }
            }

            // Offenen Konten-Dialog live aktualisieren.
            let mut st = ctx.st.borrow_mut();
            if let Some(ad) = st.account_dialog.as_mut() {
                ad.list.clear();
                for (name, role) in &accounts {
                    ad.list.append(&format!("{} [{}]", name, role));
                }
                ad.reg_chk.set_value(open);
                ad.accounts = accounts;
            }
        }

        "error" => {
            let text = if msg.data.get("message").is_some() || msg.data.get("code").is_some() {
                crate::i18n::server_text(&msg.data)
            } else {
                msg.data.to_string()
            };
            ui.set_status(&t!("common.error", error = text));
            ui.append_chat(&t!("common.error", error = text));
        }

        other => tracing::debug!("Unbehandelter Servernachrichtentyp: {}", other),
    }
}
