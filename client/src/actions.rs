//! Aktionen aus Menüs, Buttons, Tastatur und Dialogen.

use wxdragon::prelude::*;

use crate::app::Ctx;
use crate::config::{self, ServerEntry};
use crate::handlers::{rebuild_files, rebuild_server_list, rebuild_tree, refresh_status};
use crate::protocol::Message;
use crate::state::PendingUpload;
use crate::ui::{self, NodeRef};

// ── kleine Helfer ──

fn status(ctx: &Ctx, text: &str) {
    ctx.ui.set_status(text);
}

fn send_or_status(ctx: &Ctx, msg: Message) -> bool {
    match ctx.app.send_ws(msg) {
        Ok(()) => true,
        Err(e) => {
            status(ctx, &e);
            false
        }
    }
}

/// Aktuell im Baum ausgewählter Knoten (über die DataViewItem→NodeRef-Map).
fn selected_node(ctx: &Ctx) -> Option<NodeRef> {
    let item = ctx.ui.tree.get_selection()?;
    let key = item.get_id::<u8>().map(|p| p as usize)?;
    ctx.st.borrow().tree_map.get(&key).cloned()
}

fn selected_room(ctx: &Ctx) -> Option<i64> {
    match selected_node(ctx)? {
        NodeRef::Room(id) => Some(id),
        NodeRef::User { room, .. } => Some(room),
    }
}

fn selected_user(ctx: &Ctx) -> Option<i64> {
    match selected_node(ctx)? {
        NodeRef::User { id, .. } => Some(id),
        NodeRef::Room(_) => None,
    }
}

fn selected_file(ctx: &Ctx) -> Option<crate::protocol::FileInfo> {
    let idx = ctx.ui.files.get_selection()? as usize;
    ctx.st.borrow().files.get(idx).cloned()
}

fn ask_text(ctx: &Ctx, message: &str, caption: &str, default: &str) -> Option<String> {
    let dlg = TextEntryDialog::builder(&ctx.ui.frame, message, caption)
        .with_default_value(default)
        .build();
    if dlg.show_modal() == ID_OK {
        dlg.get_value().filter(|s| !s.is_empty())
    } else {
        None
    }
}

fn info_box(ctx: &Ctx, message: &str, caption: &str) {
    let dlg = MessageDialog::builder(&ctx.ui.frame, message, caption).build();
    dlg.show_modal();
}

// ── Verbindung ──

pub fn do_connect(ctx: &Ctx) {
    if ctx.app.inner.lock().connected {
        status(ctx, "Bereits verbunden — bitte zuerst trennen.");
        return;
    }
    let host = ctx.ui.host_in.get_value().trim().to_string();
    let port: u16 = match ctx.ui.port_in.get_value().trim().parse() {
        Ok(p) => p,
        Err(_) => {
            status(ctx, "Ungültiger Port.");
            return;
        }
    };
    let ssl = ctx.ui.ssl_chk.is_checked();
    let username = ctx.ui.user_in.get_value().trim().to_string();
    let password = ctx.ui.pass_in.get_value();
    let mut nickname = ctx.ui.nick_in.get_value().trim().to_string();
    if nickname.is_empty() {
        nickname = username.clone();
    }
    if host.is_empty() || username.is_empty() || password.is_empty() {
        status(ctx, "Host, Benutzername und Passwort sind erforderlich.");
        return;
    }

    let (input_device, output_device) = {
        let cfg = config::load_config();
        (cfg.input_device.clone(), cfg.output_device.clone())
    };
    {
        let mut inner = ctx.app.inner.lock();
        inner.nickname = nickname.clone();
        inner.input_device = input_device;
        inner.output_device = output_device.clone();
    }

    status(ctx, "Verbinde…");

    let app = ctx.app.clone();
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let fail = |text: String| {
            let _ = ev_tx.send(Message::new("client_error", serde_json::json!({ "message": text })));
        };

        if let Err(e) =
            crate::net::ws_client::connect(&host, port, ssl, app.clone(), ev_tx.clone()).await
        {
            fail(format!("Verbindung fehlgeschlagen: {}", e));
            return;
        }
        match crate::net::udp_client::start_udp_audio(&host, port + 1, app.clone()).await {
            Ok((send_shutdown, recv_shutdown)) => {
                let mut inner = app.inner.lock();
                inner.capture_shutdown = Some(send_shutdown);
                inner.playback_shutdown = Some(recv_shutdown);
            }
            Err(e) => fail(format!("UDP-Audio fehlgeschlagen: {}", e)),
        }

        let playback_state = app.clone();
        std::thread::spawn(move || {
            match crate::audio::playback::start_playback(playback_state, output_device) {
                Ok(_stream) => loop {
                    std::thread::park();
                },
                Err(e) => tracing::error!("Failed to start playback: {}", e),
            }
        });

        let login = Message::new(
            "auth_login",
            serde_json::json!({ "username": username, "password": password, "nickname": nickname }),
        );
        if let Err(e) = app.send_ws(login) {
            fail(format!("Anmeldung fehlgeschlagen: {}", e));
        }
    });
}

pub fn do_disconnect(ctx: &Ctx) {
    {
        let mut inner = ctx.app.inner.lock();
        if let Some(ref tx) = inner.capture_shutdown {
            let _ = tx.send(true);
        }
        inner.capture_shutdown = None;
        if let Some(ref tx) = inner.playback_shutdown {
            let _ = tx.send(true);
        }
        inner.playback_shutdown = None;
        if let Some(ref tx) = inner.stream_shutdown {
            let _ = tx.send(true);
        }
        inner.stream_shutdown = None;
        inner.ws_tx = None;
        inner.connected = false;
        inner.authenticated = false;
        inner.user_id = None;
        inner.session_token = None;
        inner.rooms.clear();
        inner.current_room_id = None;
        inner.udp_socket = None;
        inner.server_udp_addr = None;
        inner.capturing = false;
        inner.streaming_file = false;
        inner.current_files.clear();
        inner.pending_upload = None;
        inner.download_targets.clear();
        inner.muted = false;
        inner.deafened = false;
        inner.loopback = false;
    }
    ctx.app
        .file_streaming
        .store(false, std::sync::atomic::Ordering::Relaxed);

    ctx.ui.frame.set_title("TeamConference");
    ctx.ui.show_main(false);
    rebuild_tree(ctx);
    rebuild_files(ctx);
    status(ctx, "Nicht verbunden");
}

// ── Serverliste ──

pub fn fill_form_from_server(ctx: &Ctx) {
    let Some(idx) = ctx.ui.server_list.get_selection() else {
        return;
    };
    let entry = ctx.st.borrow().servers.get(idx as usize).cloned();
    if let Some(s) = entry {
        ctx.ui.host_in.set_value(&s.host);
        ctx.ui.port_in.set_value(&s.port.to_string());
        ctx.ui.ssl_chk.set_value(s.ssl);
        ctx.ui.user_in.set_value(&s.username);
        ctx.ui.nick_in.set_value(&s.nickname);
    }
}

pub fn save_bookmark(ctx: &Ctx) {
    let host = ctx.ui.host_in.get_value().trim().to_string();
    if host.is_empty() {
        status(ctx, "Host darf nicht leer sein.");
        return;
    }
    let port: u16 = ctx.ui.port_in.get_value().trim().parse().unwrap_or(9500);
    let entry = ServerEntry {
        name: host.clone(),
        host,
        port,
        ssl: ctx.ui.ssl_chk.is_checked(),
        username: ctx.ui.user_in.get_value().trim().to_string(),
        nickname: ctx.ui.nick_in.get_value().trim().to_string(),
    };
    ctx.st.borrow_mut().servers.push(entry);
    persist_servers(ctx);
    rebuild_server_list(ctx);
    status(ctx, "Server als Lesezeichen gespeichert.");
}

pub fn remove_server(ctx: &Ctx) {
    let Some(idx) = ctx.ui.server_list.get_selection() else {
        status(ctx, "Bitte zuerst einen Server auswählen.");
        return;
    };
    {
        let mut st = ctx.st.borrow_mut();
        if (idx as usize) < st.servers.len() {
            st.servers.remove(idx as usize);
        }
    }
    persist_servers(ctx);
    rebuild_server_list(ctx);
    status(ctx, "Server entfernt.");
}

fn persist_servers(ctx: &Ctx) {
    let mut cfg = config::load_config();
    cfg.servers = ctx.st.borrow().servers.clone();
    let _ = config::save_config(&cfg);
}

// ── Chat ──

pub fn send_chat(ctx: &Ctx) {
    let text = ctx.ui.chat_in.get_value();
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let (room_id, nickname) = {
        let inner = ctx.app.inner.lock();
        (inner.current_room_id, inner.nickname.clone())
    };
    let Some(room_id) = room_id else {
        status(ctx, "Bitte zuerst einem Raum beitreten.");
        return;
    };
    let msg = Message::new(
        "chat_room",
        serde_json::json!({ "room_id": room_id, "message": text }),
    );
    if send_or_status(ctx, msg) {
        let room = ctx.app.inner.lock().room_name(room_id);
        ctx.ui.append_chat(&format!("[{}] {}: {}", room, nickname, text));
        ctx.ui.chat_in.clear();
    }
}

fn private_message(ctx: &Ctx) {
    let Some(user_id) = selected_user(ctx) else {
        status(ctx, "Bitte zuerst einen Nutzer im Baum auswählen.");
        return;
    };
    let text = ctx.ui.chat_in.get_value().trim().to_string();
    if text.is_empty() {
        status(ctx, "Privatnachricht: Text zuerst ins Eingabefeld schreiben, dann Strg+P.");
        return;
    }
    let msg = Message::new(
        "chat_private",
        serde_json::json!({ "to_user_id": user_id, "message": text }),
    );
    if send_or_status(ctx, msg) {
        let nick = ctx.app.inner.lock().nickname_of(user_id);
        ctx.ui.append_chat(&format!("[Privat an {}] {}", nick, text));
        ctx.ui.chat_in.clear();
    }
}

// ── Räume ──

fn join_room(ctx: &Ctx, room_id: i64, password: Option<String>) {
    let mut data = serde_json::json!({ "room_id": room_id });
    if let Some(pw) = password {
        data["password"] = serde_json::Value::String(pw);
    }
    if !send_or_status(ctx, Message::new("room_join", data)) {
        return;
    }
    let (sr, bd, ch) = {
        let mut inner = ctx.app.inner.lock();
        inner.current_room_id = Some(room_id);
        (
            inner.audio_config.sample_rate,
            inner.audio_config.bit_depth,
            inner.audio_config.channels,
        )
    };
    let _ = ctx.app.send_ws(Message::new(
        "audio_config",
        serde_json::json!({ "sample_rate": sr, "bit_depth": bd, "channels": ch, "enabled": true }),
    ));
    let _ = ctx
        .app
        .send_ws(Message::new("file_list", serde_json::json!({ "room_id": room_id })));

    let room = ctx.app.inner.lock().room_name(room_id);
    ctx.ui.append_chat(&format!("* Raum {} beigetreten.", room));
    rebuild_tree(ctx);
    refresh_status(ctx);
}

fn join_room_checked(ctx: &Ctx, room_id: i64) {
    let has_password = ctx
        .app
        .inner
        .lock()
        .rooms
        .iter()
        .find(|r| r.id == room_id)
        .map(|r| r.has_password)
        .unwrap_or(false);
    if has_password {
        if let Some(pw) = ask_text(ctx, "Passwort für diesen Raum:", "Raum-Passwort", "") {
            join_room(ctx, room_id, Some(pw));
        }
    } else {
        join_room(ctx, room_id, None);
    }
}

fn join_selected(ctx: &Ctx) {
    match selected_room(ctx) {
        Some(room_id) => join_room_checked(ctx, room_id),
        None => status(ctx, "Bitte zuerst einen Raum im Baum auswählen."),
    }
}

/// Doppelklick/Enter auf einen Baumeintrag: Raum → beitreten.
pub fn tree_activate(ctx: &Ctx) {
    match selected_node(ctx) {
        Some(NodeRef::Room(id)) => join_room_checked(ctx, id),
        _ => {}
    }
}

fn leave_room(ctx: &Ctx) {
    let Some(room_id) = ctx.app.inner.lock().current_room_id else {
        status(ctx, "Du bist in keinem Raum.");
        return;
    };
    if send_or_status(
        ctx,
        Message::new("room_leave", serde_json::json!({ "room_id": room_id })),
    ) {
        let room = ctx.app.inner.lock().room_name(room_id);
        {
            let mut inner = ctx.app.inner.lock();
            inner.current_room_id = None;
            inner.current_files.clear();
        }
        ctx.ui.append_chat(&format!("* Raum {} verlassen.", room));
        rebuild_tree(ctx);
        rebuild_files(ctx);
        refresh_status(ctx);
    }
}

fn create_room(ctx: &Ctx, parent: Option<i64>) {
    let title = if parent.is_some() {
        "Unterraum erstellen"
    } else {
        "Raum erstellen"
    };
    let Some(name) = ask_text(ctx, "Name des Raums:", title, "") else {
        return;
    };
    let mut data = serde_json::json!({ "name": name });
    if let Some(pid) = parent {
        data["parent_id"] = serde_json::json!(pid);
    }
    if let Some(pw) = ask_text(ctx, "Passwort (leer = keins):", title, "") {
        data["password"] = serde_json::Value::String(pw);
    }
    send_or_status(ctx, Message::new("room_create", data));
}

fn delete_room(ctx: &Ctx) {
    let Some(room_id) = selected_room(ctx) else {
        status(ctx, "Bitte zuerst einen Raum auswählen.");
        return;
    };
    let name = ctx.app.inner.lock().room_name(room_id);
    let dlg = MessageDialog::builder(
        &ctx.ui.frame,
        &format!("Raum „{}“ wirklich löschen?", name),
        "Raum löschen",
    )
    .with_style(MessageDialogStyle::YesNo)
    .build();
    if dlg.show_modal() == ID_YES {
        send_or_status(
            ctx,
            Message::new("room_delete", serde_json::json!({ "room_id": room_id })),
        );
    }
}

// ── Audio ──

fn toggle_mute(ctx: &Ctx) {
    let muted = {
        let mut inner = ctx.app.inner.lock();
        inner.muted = !inner.muted;
        inner.muted
    };
    let _ = ctx
        .app
        .send_ws(Message::new("audio_mute", serde_json::json!({ "muted": muted })));
    ctx.ui.append_chat(if muted {
        "* Mikrofon stummgeschaltet."
    } else {
        "* Mikrofon eingeschaltet."
    });
    refresh_status(ctx);
}

fn toggle_deafen(ctx: &Ctx) {
    let deafened = {
        let mut inner = ctx.app.inner.lock();
        inner.deafened = !inner.deafened;
        inner.deafened
    };
    let _ = ctx.app.send_ws(Message::new(
        "audio_deafen",
        serde_json::json!({ "deafened": deafened }),
    ));
    ctx.ui.append_chat(if deafened {
        "* Ton ausgeschaltet (taub)."
    } else {
        "* Ton eingeschaltet."
    });
    refresh_status(ctx);
}

fn toggle_loopback(ctx: &Ctx) {
    let loopback = {
        let mut inner = ctx.app.inner.lock();
        inner.loopback = !inner.loopback;
        inner.loopback
    };
    let _ = ctx.app.send_ws(Message::new(
        "audio_loopback",
        serde_json::json!({ "enabled": loopback }),
    ));
    ctx.ui.append_chat(if loopback {
        "* Loopback eingeschaltet."
    } else {
        "* Loopback ausgeschaltet."
    });
    refresh_status(ctx);
}

pub fn volume_changed(ctx: &Ctx) {
    let v = ctx.ui.volume.get_value();
    ctx.app.set_volume(v as f32 / 100.0);
}

pub fn save_volume(ctx: &Ctx) {
    let mut cfg = config::load_config();
    cfg.volume = ctx.app.volume();
    let _ = config::save_config(&cfg);
}

// ── Datei-Streaming ──

fn stream_file(ctx: &Ctx) {
    if !ctx.app.inner.lock().connected {
        status(ctx, "Nicht verbunden.");
        return;
    }
    let dlg = FileDialog::builder(&ctx.ui.frame)
        .with_message("Audiodatei zum Streamen wählen")
        .with_wildcard("Audiodateien|*.mp3;*.wav;*.flac;*.ogg;*.m4a;*.aac;*.opus;*.aiff|Alle Dateien|*.*")
        .with_style(FileDialogStyle::Open)
        .build();
    if dlg.show_modal() != ID_OK {
        return;
    }
    let Some(path) = dlg.get_path() else { return };
    let path = std::path::PathBuf::from(path);

    {
        let inner = ctx.app.inner.lock();
        if let Some(ref tx) = inner.stream_shutdown {
            let _ = tx.send(true);
        }
    }
    // Pause-Flag für den neuen Stream zurücksetzen
    ctx.app
        .stream_paused
        .store(false, std::sync::atomic::Ordering::Relaxed);

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let was_loopback = {
        let mut inner = ctx.app.inner.lock();
        inner.stream_shutdown = Some(shutdown_tx);
        inner.streaming_file = true;
        inner.loopback
    };
    if !was_loopback {
        let _ = ctx.app.send_ws(Message::new(
            "audio_loopback",
            serde_json::json!({ "enabled": true }),
        ));
    }

    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    ctx.ui.append_chat(&format!("* Streame Datei: {}", filename));
    refresh_status(ctx);

    let app = ctx.app.clone();
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let result =
            crate::audio::file_stream::stream_audio_file(&path, app.clone(), shutdown_rx).await;
        if let Err(e) = result {
            tracing::error!("Audio file stream error: {}", e);
            let _ = ev_tx.send(Message::new(
                "client_error",
                serde_json::json!({ "message": format!("Streaming-Fehler: {}", e) }),
            ));
        }
        if !was_loopback {
            let _ = app.send_ws(Message::new(
                "audio_loopback",
                serde_json::json!({ "enabled": false }),
            ));
        }
        app.inner.lock().stream_shutdown = None;
        let _ = ev_tx.send(Message::new("client_stream_finished", serde_json::json!({})));
    });
}

fn stop_stream(ctx: &Ctx) {
    let was = {
        let mut inner = ctx.app.inner.lock();
        let was = inner.stream_shutdown.is_some();
        if let Some(ref tx) = inner.stream_shutdown {
            let _ = tx.send(true);
        }
        inner.stream_shutdown = None;
        was
    };
    // Pause-Flag zurücksetzen, damit der nächste Stream nicht pausiert startet
    ctx.app
        .stream_paused
        .store(false, std::sync::atomic::Ordering::Relaxed);
    if !was {
        status(ctx, "Es läuft kein Streaming.");
    }
}

/// Gestreamte Datei pausieren bzw. fortsetzen (lokal, kein Server-Roundtrip).
fn toggle_pause_stream(ctx: &Ctx) {
    let streaming = ctx.app.inner.lock().stream_shutdown.is_some();
    if !streaming {
        status(ctx, "Es läuft kein Streaming.");
        return;
    }
    use std::sync::atomic::Ordering;
    let paused = !ctx.app.stream_paused.load(Ordering::Relaxed);
    ctx.app.stream_paused.store(paused, Ordering::Relaxed);
    ctx.ui.append_chat(if paused {
        "* Streaming pausiert."
    } else {
        "* Streaming fortgesetzt."
    });
    status(ctx, if paused { "Streaming pausiert" } else { "Streaming fortgesetzt" });
}

// ── Dateien ──

fn upload_file(ctx: &Ctx) {
    let Some(room_id) = ctx.app.inner.lock().current_room_id else {
        status(ctx, "Bitte zuerst einem Raum beitreten.");
        return;
    };
    let dlg = FileDialog::builder(&ctx.ui.frame)
        .with_message("Datei zum Hochladen wählen")
        .with_style(FileDialogStyle::Open)
        .build();
    if dlg.show_modal() != ID_OK {
        return;
    }
    let Some(path) = dlg.get_path() else { return };
    let path = std::path::PathBuf::from(path);
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            status(ctx, &format!("Datei konnte nicht gelesen werden: {}", e));
            return;
        }
    };
    let size = data.len() as i64;
    ctx.app.inner.lock().pending_upload = Some(PendingUpload {
        filename: filename.clone(),
        data,
    });
    send_or_status(
        ctx,
        Message::new(
            "file_upload_start",
            serde_json::json!({ "room_id": room_id, "filename": filename, "size": size }),
        ),
    );
}

fn download_file(ctx: &Ctx) {
    let Some(file) = selected_file(ctx) else {
        status(ctx, "Bitte zuerst eine Datei auswählen.");
        return;
    };
    let dlg = FileDialog::builder(&ctx.ui.frame)
        .with_message("Speicherort wählen")
        .with_default_file(&file.filename)
        .with_style(FileDialogStyle::Save)
        .build();
    if dlg.show_modal() != ID_OK {
        return;
    }
    let Some(path) = dlg.get_path() else { return };
    ctx.app.inner.lock().download_targets.insert(
        file.id,
        (
            std::path::PathBuf::from(path),
            Vec::with_capacity(file.size_bytes.max(0) as usize),
        ),
    );
    if send_or_status(
        ctx,
        Message::new("file_download", serde_json::json!({ "file_id": file.id })),
    ) {
        ctx.ui.append_chat(&format!("Lade {} herunter…", file.filename));
    }
}

fn refresh_files(ctx: &Ctx) {
    let Some(room_id) = ctx.app.inner.lock().current_room_id else {
        status(ctx, "Bitte zuerst einem Raum beitreten.");
        return;
    };
    send_or_status(
        ctx,
        Message::new("file_list", serde_json::json!({ "room_id": room_id })),
    );
}

// ── Verwaltung ──

fn kick_user(ctx: &Ctx) {
    let Some(user_id) = selected_user(ctx) else {
        status(ctx, "Bitte zuerst einen Nutzer auswählen.");
        return;
    };
    let reason = ask_text(ctx, "Grund (optional):", "Nutzer kicken", "");
    let mut data = serde_json::json!({ "user_id": user_id });
    if let Some(r) = reason {
        data["reason"] = serde_json::Value::String(r);
    }
    send_or_status(ctx, Message::new("admin_kick", data));
}

fn ban_user(ctx: &Ctx) {
    let Some(user_id) = selected_user(ctx) else {
        status(ctx, "Bitte zuerst einen Nutzer auswählen.");
        return;
    };
    let reason = ask_text(ctx, "Grund (optional):", "Nutzer bannen", "");
    let duration = ask_text(ctx, "Dauer in Minuten (leer = dauerhaft):", "Nutzer bannen", "");
    let mut data = serde_json::json!({ "user_id": user_id });
    if let Some(r) = reason {
        data["reason"] = serde_json::Value::String(r);
    }
    if let Some(d) = duration.and_then(|s| s.trim().parse::<i64>().ok()) {
        data["duration_minutes"] = serde_json::json!(d);
    }
    send_or_status(ctx, Message::new("admin_ban", data));
}

fn move_user(ctx: &Ctx) {
    let Some(user_id) = selected_user(ctx) else {
        status(ctx, "Bitte zuerst einen Nutzer auswählen.");
        return;
    };
    let Some(room_id) = selected_room(ctx) else {
        status(ctx, "Bitte zuerst den Zielraum auswählen.");
        return;
    };
    send_or_status(
        ctx,
        Message::new(
            "admin_move",
            serde_json::json!({ "user_id": user_id, "room_id": room_id }),
        ),
    );
}

fn admin_mute(ctx: &Ctx, muted: bool) {
    let Some(user_id) = selected_user(ctx) else {
        status(ctx, "Bitte zuerst einen Nutzer auswählen.");
        return;
    };
    send_or_status(
        ctx,
        Message::new(
            "admin_mute",
            serde_json::json!({ "user_id": user_id, "muted": muted }),
        ),
    );
}

fn server_message(ctx: &Ctx) {
    if let Some(text) = ask_text(ctx, "Nachricht an alle:", "Servernachricht", "") {
        send_or_status(
            ctx,
            Message::new("admin_server_message", serde_json::json!({ "message": text })),
        );
    }
}

// ── Account-Verwaltung ──

/// Passwort-Eingabe (maskiert).
fn ask_secret(ctx: &Ctx, message: &str, caption: &str) -> Option<String> {
    let dlg = TextEntryDialog::builder(&ctx.ui.frame, message, caption)
        .password()
        .build();
    if dlg.show_modal() == ID_OK {
        dlg.get_value().filter(|s| !s.is_empty())
    } else {
        None
    }
}

/// Ja/Nein-Frage, ob das Konto Administrator sein soll.
fn ask_admin(ctx: &Ctx, caption: &str) -> bool {
    let dlg = MessageDialog::builder(&ctx.ui.frame, "Soll dieses Konto Administrator sein?", caption)
        .with_style(MessageDialogStyle::YesNo)
        .build();
    dlg.show_modal() == ID_YES
}

/// Account-Liste vom Server anfordern (Antwort: account_list_result).
fn request_accounts(ctx: &Ctx) {
    send_or_status(ctx, Message::new("account_list", serde_json::json!({})));
}

/// Vom Handler aufgerufen, um die empfangene Liste anzuzeigen.
pub fn show_account_list(ctx: &Ctx, text: &str) {
    info_box(ctx, text, "Konten");
}

fn account_create(ctx: &Ctx) {
    let Some(name) = ask_text(ctx, "Benutzername:", "Konto anlegen", "") else {
        return;
    };
    let Some(pw) = ask_secret(ctx, "Passwort:", "Konto anlegen") else {
        return;
    };
    let role = if ask_admin(ctx, "Konto anlegen") { "admin" } else { "user" };
    send_or_status(
        ctx,
        Message::new(
            "account_create",
            serde_json::json!({ "username": name, "password": pw, "role": role }),
        ),
    );
}

fn account_password(ctx: &Ctx) {
    let Some(name) = ask_text(ctx, "Benutzername:", "Passwort zurücksetzen", "") else {
        return;
    };
    let Some(pw) = ask_secret(ctx, "Neues Passwort:", "Passwort zurücksetzen") else {
        return;
    };
    send_or_status(
        ctx,
        Message::new(
            "account_set_password",
            serde_json::json!({ "username": name, "password": pw }),
        ),
    );
}

fn account_role(ctx: &Ctx) {
    let Some(name) = ask_text(ctx, "Benutzername:", "Rolle ändern", "") else {
        return;
    };
    let role = if ask_admin(ctx, "Rolle ändern") { "admin" } else { "user" };
    send_or_status(
        ctx,
        Message::new(
            "account_set_role",
            serde_json::json!({ "username": name, "role": role }),
        ),
    );
}

fn account_delete(ctx: &Ctx) {
    let Some(name) = ask_text(ctx, "Benutzername:", "Konto löschen", "") else {
        return;
    };
    let dlg = MessageDialog::builder(
        &ctx.ui.frame,
        &format!("Konto „{}“ wirklich löschen?", name),
        "Konto löschen",
    )
    .with_style(MessageDialogStyle::YesNo)
    .build();
    if dlg.show_modal() == ID_YES {
        send_or_status(
            ctx,
            Message::new("account_delete", serde_json::json!({ "username": name })),
        );
    }
}

fn toggle_registration(ctx: &Ctx) {
    let open = !ctx.st.borrow().registration_open;
    send_or_status(
        ctx,
        Message::new("account_set_registration", serde_json::json!({ "open": open })),
    );
}

fn change_password(ctx: &Ctx) {
    let Some(old) = ask_secret(ctx, "Aktuelles Passwort:", "Passwort ändern") else {
        return;
    };
    let Some(new) = ask_secret(ctx, "Neues Passwort:", "Passwort ändern") else {
        return;
    };
    send_or_status(
        ctx,
        Message::new(
            "password_change",
            serde_json::json!({ "old_password": old, "new_password": new }),
        ),
    );
}

const HELP_TEXT: &str = "Kurztasten (unter macOS Cmd statt Strg):\n\n\
Strg+M  Mikrofon stumm/laut\n\
Strg+D  Ton aus/an (taub)\n\
Strg+L  Loopback an/aus\n\
Strg+S  Audiodatei streamen\n\
Strg+P  Streaming pausieren/fortsetzen\n\
Strg+Umschalt+S  Streaming stoppen\n\
Strg+J  Ausgewähltem Raum beitreten\n\
Strg+U  Datei hochladen\n\
Strg+H  Datei herunterladen\n\
Strg+R  Dateiliste aktualisieren\n\
Strg+Umschalt+P  Privatnachricht an ausgewählten Nutzer\n\
Strg+Q  Beenden\n\n\
Im Baum: Pfeil rechts/links klappt auf/zu, Enter tritt einem Raum bei.";

// ── Menü-Dispatcher ──

pub fn handle_menu(ctx: &Ctx, id: i32) {
    match id {
        ID_EXIT => {
            save_volume(ctx);
            ctx.ui.frame.close(true);
        }
        ui::ID_DISCONNECT => {
            do_disconnect(ctx);
            ctx.ui.append_chat("Verbindung getrennt.");
        }
        ui::ID_TOGGLE_MUTE => toggle_mute(ctx),
        ui::ID_TOGGLE_DEAFEN => toggle_deafen(ctx),
        ui::ID_TOGGLE_LOOPBACK => toggle_loopback(ctx),
        ui::ID_STREAM_FILE => stream_file(ctx),
        ui::ID_PAUSE_STREAM => toggle_pause_stream(ctx),
        ui::ID_STOP_STREAM => stop_stream(ctx),
        ui::ID_JOIN_ROOM => join_selected(ctx),
        ui::ID_LEAVE_ROOM => leave_room(ctx),
        ui::ID_CREATE_ROOM => create_room(ctx, None),
        ui::ID_CREATE_SUBROOM => {
            if let Some(parent) = selected_room(ctx) {
                create_room(ctx, Some(parent));
            } else {
                status(ctx, "Bitte zuerst den übergeordneten Raum auswählen.");
            }
        }
        ui::ID_DELETE_ROOM => delete_room(ctx),
        ui::ID_UPLOAD => upload_file(ctx),
        ui::ID_DOWNLOAD => download_file(ctx),
        ui::ID_REFRESH_FILES => refresh_files(ctx),
        ui::ID_PM => private_message(ctx),
        ui::ID_KICK => kick_user(ctx),
        ui::ID_BAN => ban_user(ctx),
        ui::ID_MOVE_USER => move_user(ctx),
        ui::ID_ADMIN_MUTE => admin_mute(ctx, true),
        ui::ID_ADMIN_UNMUTE => admin_mute(ctx, false),
        ui::ID_SERVER_MSG => server_message(ctx),
        ui::ID_ACCOUNTS => request_accounts(ctx),
        ui::ID_ACCOUNT_CREATE => account_create(ctx),
        ui::ID_ACCOUNT_PASSWORD => account_password(ctx),
        ui::ID_ACCOUNT_ROLE => account_role(ctx),
        ui::ID_ACCOUNT_DELETE => account_delete(ctx),
        ui::ID_REGISTRATION => toggle_registration(ctx),
        ui::ID_CHANGE_PW => change_password(ctx),
        ui::ID_HELP_KEYS => info_box(ctx, HELP_TEXT, "Kurztasten"),
        ID_ABOUT => info_box(
            ctx,
            "TeamConference-Client\nNative Oberfläche mit wxWidgets (wxDragon).",
            "Über TeamConference",
        ),
        _ => {}
    }
}
