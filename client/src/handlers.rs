//! Verarbeitung eingehender Server-Nachrichten auf dem UI-Thread sowie
//! Aufbau der Raum-, Nutzer-, Datei- und Serverliste.

use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
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
        let kb = (f.size_bytes + 1023) / 1024;
        ctx.ui.files.append(&format!("{} ({} KB)", f.filename, kb));
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
        ctx.ui.set_status("Nicht verbunden");
        return;
    }
    let server = inner.server_name.clone().unwrap_or_else(|| "Server".into());
    let room = inner
        .current_room_id
        .map(|id| inner.room_name(id))
        .unwrap_or_else(|| "kein Raum".into());
    let mic = if inner.muted { "stumm" } else { "an" };
    let ton = if inner.deafened { "aus" } else { "an" };
    let mut s = format!(
        "Verbunden mit {} | Raum: {} | Mikrofon: {} | Ton: {}",
        server, room, mic, ton
    );
    if inner.loopback {
        s.push_str(" | Loopback an");
    }
    if inner.streaming_file {
        s.push_str(" | Streaming läuft");
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
                let server = resp.server_name.unwrap_or_else(|| "Server".into());
                ui.frame.set_title(&format!("TeamConference — {}", server));
                ui.show_main(true);
                ui.append_chat(&format!("Verbunden mit {}.", server));
                rebuild_tree(ctx);
                rebuild_files(ctx);
                refresh_status(ctx);
                // Admin-only-Menüpunkte je nach eigener Rolle ein-/ausblenden
                crate::actions::update_account_menu(ctx);
                // Fokus in die Hauptansicht setzen, damit der Screenreader mitwandert
                ui.rooms_tree.set_focus();
            }
            Ok(resp) => {
                let err = resp.error.unwrap_or_else(|| "Unbekannter Fehler".into());
                crate::actions::do_disconnect(ctx);
                crate::actions::notify(ctx, &format!("Anmeldung fehlgeschlagen: {}", err), "Verbinden");
            }
            Err(e) => crate::actions::notify(ctx, &format!("Ungültige Serverantwort: {}", e), "Verbinden"),
        },

        "connection_lost" => {
            crate::actions::do_disconnect(ctx);
            ui.append_chat("Verbindung zum Server verloren.");
            crate::actions::notify(ctx, "Verbindung zum Server verloren.", "Verbindung");
        }

        // synthetisch aus eigenen Tokio-Tasks (Verbindungs-/Streaming-Fehler, Hinweise)
        "client_error" => {
            let text = msg
                .data
                .get("message")
                .and_then(|v| v.as_str())
                .unwrap_or("Unbekannter Fehler")
                .to_string();
            ui.append_chat(&text);
            crate::actions::notify(ctx, &text, "TeamConference");
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
            crate::actions::update_hub_status(ctx);
            crate::actions::notify(ctx, &text, "Server-Hub");
        }

        // Auto-Updater: neue Version verfügbar → nachfragen und ggf. laden.
        "client_update" => {
            let version = msg.data.get("version").and_then(|v| v.as_str()).unwrap_or("?").to_string();
            let url = msg.data.get("url").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let filename = msg.data.get("filename").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let q = MessageDialog::builder(
                &ui.frame,
                &format!(
                    "Eine neue Version ({}) ist verfügbar. Du nutzt {}. Jetzt herunterladen?",
                    version,
                    crate::update::current_version()
                ),
                "Aktualisierung verfügbar",
            )
            .with_style(MessageDialogStyle::YesNo)
            .build();
            if q.show_modal() == ID_YES {
                if url.is_empty() {
                    // Kein passendes Paket gefunden → Release-Seite öffnen.
                    crate::update::open_path(&crate::update::releases_page());
                } else {
                    ui.append_chat("Lade Aktualisierung herunter…");
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
                            "Die Aktualisierung wird jetzt eingespielt. TeamConference \
                             schließt sich kurz und startet danach automatisch neu.",
                            "Aktualisierung",
                        );
                        // Prozess beenden, damit der Updater die Dateien ersetzen kann.
                        std::process::exit(0);
                    }
                    Err(e) => {
                        // Fallback: Paket nur öffnen, manuell installieren.
                        crate::actions::notify(
                            ctx,
                            &format!(
                                "Automatisches Einspielen fehlgeschlagen ({}).\n\nDas \
                                 heruntergeladene Paket wird zum manuellen Installieren geöffnet:\n{}",
                                e, path
                            ),
                            "Aktualisierung",
                        );
                        crate::update::open_path(&path);
                    }
                }
            }

            #[cfg(not(target_os = "windows"))]
            {
                crate::actions::notify(
                    ctx,
                    &format!(
                        "Aktualisierung heruntergeladen:\n{}\n\nDie Datei wird zum Installieren geöffnet.",
                        path
                    ),
                    "Aktualisierung",
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
            ui.append_chat("Datei-Streaming beendet.");
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
                    let line = format!("{} hat den Raum {} betreten.", user.nickname, room_name);
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
                    let line = format!("{} hat den Raum {} verlassen.", nick, room_name);
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
                .unwrap_or("Jemand");
            let line = format!("{} hat den Server betreten.", nick);
            ui.append_chat(&format!("* {}", line));
            crate::actions::announce_event(ctx, &line);
        }

        "user_disconnected" => {
            let nick = msg
                .data
                .get("nickname")
                .and_then(|v| v.as_str())
                .unwrap_or("Jemand");
            let line = format!("{} hat den Server verlassen.", nick);
            ui.append_chat(&format!("* {}", line));
            crate::actions::announce_event(ctx, &line);
        }

        "chat_room" => {
            let nick = msg
                .data
                .get("from_user")
                .and_then(|u| u.get("nickname"))
                .and_then(|n| n.as_str())
                .unwrap_or("Unbekannt");
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
                .unwrap_or("Unbekannt");
            let text = msg.data.get("message").and_then(|m| m.as_str()).unwrap_or("");
            ui.append_chat(&format!("[Privat] {}: {}", nick, text));
        }

        "chat_server" => {
            let text = msg.data.get("message").and_then(|m| m.as_str()).unwrap_or("");
            ui.append_chat(&format!("[Server] {}", text));
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
            if let Ok(ack) = serde_json::from_value::<FileUploadAck>(msg.data) {
                let pending = ctx.app.inner.lock().pending_upload.take();
                if !ack.success {
                    ui.append_chat("Upload vom Server abgelehnt.");
                    return;
                }
                if let Some(upload) = pending {
                    ui.append_chat(&format!("Lade {} hoch…", upload.filename));
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
                        ui.append_chat(&format!("Download-Fehler (Base64): {}", e));
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
                        Ok(()) => ui.append_chat(&format!("Datei gespeichert: {}", path.display())),
                        Err(e) => {
                            ui.append_chat(&format!("Datei konnte nicht gespeichert werden: {}", e))
                        }
                    }
                }
            }
        }

        "stream_file_status" => {
            if let Ok(st) = serde_json::from_value::<StreamFileStatus>(msg.data) {
                let nick = ctx.app.inner.lock().nickname_of(st.user_id);
                let verb = if st.playing { "spielt" } else { "stoppte" };
                ui.append_chat(&format!("* {} {} {}", nick, verb, st.filename));
            }
        }

        "user_kicked" | "kicked" => {
            let reason = msg
                .data
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("kein Grund angegeben");
            ui.append_chat(&format!("Du wurdest vom Server geworfen: {}", reason));
            ui.set_status("Vom Server geworfen");
        }

        "user_banned" | "banned" => {
            let reason = msg
                .data
                .get("reason")
                .and_then(|r| r.as_str())
                .unwrap_or("kein Grund angegeben");
            ui.append_chat(&format!("Du wurdest gebannt: {}", reason));
            ui.set_status("Vom Server gebannt");
        }

        "user_moved" | "moved" => {
            let room_id = msg.data.get("room_id").and_then(|v| v.as_i64());
            if let Some(rid) = room_id {
                ctx.app.inner.lock().current_room_id = Some(rid);
                let name = ctx.app.inner.lock().room_name(rid);
                ui.append_chat(&format!("* Du wurdest in den Raum {} verschoben.", name));
                rebuild_tree(ctx);
                refresh_status(ctx);
                let _ = ctx
                    .app
                    .send_ws(Message::new("file_list", serde_json::json!({ "room_id": rid })));
            }
        }

        "account_ack" => {
            let success = msg.data.get("success").and_then(|v| v.as_bool()).unwrap_or(false);
            let text = msg
                .data
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string();
            let prefix = if success { "Konten" } else { "Konten-Fehler" };
            ui.set_status(&format!("{}: {}", prefix, text));
            ui.append_chat(&format!("[{}] {}", prefix, text));
            // Bei offenem Konten-Dialog für Screenreader-Nutzer rückmelden.
            if ctx.st.borrow().account_dialog.is_some() {
                crate::actions::notify(ctx, &text, prefix);
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
            let text = msg
                .data
                .get("message")
                .and_then(|m| m.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| msg.data.to_string());
            ui.set_status(&format!("Fehler: {}", text));
            ui.append_chat(&format!("Fehler: {}", text));
        }

        other => tracing::debug!("Unbehandelter Servernachrichtentyp: {}", other),
    }
}
