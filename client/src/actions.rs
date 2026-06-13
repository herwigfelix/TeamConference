//! Aktionen, die von der UI (Menü, Kurztasten, Buttons, Dialoge) ausgelöst werden.

use std::sync::Arc;

use tokio::sync::mpsc;

use crate::events::{append_chat, rebuild_files, rebuild_rooms, rebuild_users, refresh_status, set_status};
use crate::protocol::Message;
use crate::state::{AppState, PendingUpload};
use crate::MainWindow;

const HELP_TEXT: &str = "Kurztasten (Strg unter Windows/Linux, Cmd unter macOS):\n\
Strg+M — Mikrofon stumm/laut\n\
Strg+D — Ton aus/an (taub)\n\
Strg+L — Loopback an/aus\n\
Strg+S — Audiodatei streamen\n\
Strg+Umschalt+S — Streaming stoppen\n\
Strg+J — Ausgewähltem Raum beitreten\n\
Strg+U — Datei hochladen\n\
Strg+H — Ausgewählte Datei herunterladen\n\
Strg+R — Dateiliste aktualisieren\n\
Strg+P — Privatnachricht an ausgewählten Nutzer\n\
Strg+Q — Beenden\n\
Tab/Umschalt+Tab — zwischen Bedienelementen wechseln";

fn show_dialog(
    ui: &MainWindow,
    mode: &str,
    title: &str,
    text: &str,
    label1: &str,
    show1: bool,
    label2: &str,
    show2: bool,
    password: bool,
) {
    ui.invoke_show_dialog(
        mode.into(),
        title.into(),
        text.into(),
        label1.into(),
        show1,
        label2.into(),
        show2,
        password,
    );
}

fn selected_room(ui: &MainWindow, state: &Arc<AppState>) -> Option<i64> {
    let idx = ui.get_rooms_current();
    if idx < 0 {
        return None;
    }
    state.inner.lock().ui_room_ids.get(idx as usize).copied()
}

fn selected_user(ui: &MainWindow, state: &Arc<AppState>) -> Option<i64> {
    let idx = ui.get_users_current();
    if idx < 0 {
        return None;
    }
    state.inner.lock().ui_user_ids.get(idx as usize).copied()
}

fn selected_file(ui: &MainWindow, state: &Arc<AppState>) -> Option<crate::protocol::FileInfo> {
    let idx = ui.get_files_current();
    if idx < 0 {
        return None;
    }
    state.inner.lock().ui_files.get(idx as usize).cloned()
}

fn send_or_status(ui: &MainWindow, state: &Arc<AppState>, msg: Message) -> bool {
    match state.send_ws(msg) {
        Ok(()) => true,
        Err(e) => {
            set_status(ui, &e);
            false
        }
    }
}

// ── Verbindung ──

pub fn connect_clicked(
    ui: &MainWindow,
    state: &Arc<AppState>,
    rt: &tokio::runtime::Handle,
    ev_tx: &mpsc::UnboundedSender<Message>,
) {
    if state.inner.lock().connected {
        set_status(ui, "Bereits verbunden — bitte zuerst trennen.");
        return;
    }

    let host = ui.get_conn_host().to_string().trim().to_string();
    let port: u16 = match ui.get_conn_port().to_string().trim().parse() {
        Ok(p) => p,
        Err(_) => {
            set_status(ui, "Ungültiger Port.");
            return;
        }
    };
    let ssl = ui.get_conn_ssl();
    let username = ui.get_conn_username().to_string().trim().to_string();
    let password = ui.get_conn_password().to_string();
    let mut nickname = ui.get_conn_nickname().to_string().trim().to_string();
    if nickname.is_empty() {
        nickname = username.clone();
    }

    if host.is_empty() || username.is_empty() || password.is_empty() {
        set_status(ui, "Host, Benutzername und Passwort sind erforderlich.");
        return;
    }

    // Einstellungen merken (ohne Passwort)
    let mut cfg = crate::config::load_config();
    cfg.host = host.clone();
    cfg.port = port;
    cfg.ssl = ssl;
    cfg.username = username.clone();
    cfg.nickname = nickname.clone();
    let _ = crate::config::save_config(&cfg);

    {
        let mut inner = state.inner.lock();
        inner.nickname = nickname.clone();
        inner.input_device = cfg.input_device.clone();
        inner.output_device = cfg.output_device.clone();
    }

    ui.set_connecting(true);
    set_status(ui, "Verbinde…");

    let state2 = state.clone();
    let ev_tx2 = ev_tx.clone();
    let output_device = cfg.output_device.clone();
    rt.spawn(async move {
        let fail = |text: String| {
            let _ = ev_tx2.send(Message::new(
                "client_error",
                serde_json::json!({ "message": text }),
            ));
        };

        if let Err(e) =
            crate::net::ws_client::connect(&host, port, ssl, state2.clone(), ev_tx2.clone()).await
        {
            fail(format!("Verbindung fehlgeschlagen: {}", e));
            return;
        }

        // UDP-Audio (Audioport = Steuerport + 1, wie Server-Standard)
        match crate::net::udp_client::start_udp_audio(&host, port + 1, state2.clone()).await {
            Ok((send_shutdown, recv_shutdown)) => {
                let mut inner = state2.inner.lock();
                inner.capture_shutdown = Some(send_shutdown);
                inner.playback_shutdown = Some(recv_shutdown);
            }
            Err(e) => {
                fail(format!("UDP-Audio fehlgeschlagen: {}", e));
            }
        }

        // Audio-Wiedergabe starten (Stream muss am Leben bleiben)
        let playback_state = state2.clone();
        std::thread::spawn(move || match crate::audio::playback::start_playback(
            playback_state,
            output_device,
        ) {
            Ok(_stream) => loop {
                std::thread::park();
            },
            Err(e) => {
                tracing::error!("Failed to start playback: {}", e);
            }
        });

        // Anmeldung
        let login = Message::new(
            "auth_login",
            serde_json::json!({
                "username": username,
                "password": password,
                "nickname": nickname,
            }),
        );
        if let Err(e) = state2.send_ws(login) {
            fail(format!("Anmeldung fehlgeschlagen: {}", e));
        }
    });
}

/// Tear down the connection and reset state + UI to the connect view.
pub fn do_disconnect(ui: &MainWindow, state: &Arc<AppState>) {
    {
        let mut inner = state.inner.lock();

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
        inner.ui_room_ids.clear();
        inner.ui_user_ids.clear();
        inner.ui_files.clear();
        inner.pending_upload = None;
        inner.download_targets.clear();
        inner.muted = false;
        inner.deafened = false;
        inner.loopback = false;
    }
    state
        .file_streaming
        .store(false, std::sync::atomic::Ordering::Relaxed);

    ui.set_active_view(0);
    ui.set_connecting(false);
    ui.set_muted(false);
    ui.set_deafened(false);
    ui.set_loopback(false);
    ui.set_streaming(false);
    ui.set_window_title("TeamConference".into());
    rebuild_rooms(ui, state);
    rebuild_users(ui, state);
    rebuild_files(ui, state);
    set_status(ui, "Nicht verbunden");
}

// ── Chat ──

pub fn send_chat(ui: &MainWindow, state: &Arc<AppState>) {
    let text = ui.get_chat_input().to_string();
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    let (room_id, nickname) = {
        let inner = state.inner.lock();
        (inner.current_room_id, inner.nickname.clone())
    };
    let Some(room_id) = room_id else {
        set_status(ui, "Bitte zuerst einem Raum beitreten.");
        return;
    };
    let msg = Message::new(
        "chat_room",
        serde_json::json!({ "room_id": room_id, "message": text }),
    );
    if send_or_status(ui, state, msg) {
        let room = state.inner.lock().room_name(room_id);
        append_chat(ui, state, &format!("[{}] {}: {}", room, nickname, text));
        ui.set_chat_input("".into());
    }
}

fn send_private_message(ui: &MainWindow, state: &Arc<AppState>) {
    let Some(user_id) = selected_user(ui, state) else {
        set_status(ui, "Bitte zuerst einen Nutzer in der Liste auswählen.");
        return;
    };
    let text = ui.get_chat_input().to_string();
    let text = text.trim().to_string();
    if text.is_empty() {
        set_status(
            ui,
            "Privatnachricht: Text zuerst ins Chateingabefeld schreiben, dann Strg+P.",
        );
        return;
    }
    let msg = Message::new(
        "chat_private",
        serde_json::json!({ "to_user_id": user_id, "message": text }),
    );
    if send_or_status(ui, state, msg) {
        let nick = state.inner.lock().nickname_of(user_id);
        append_chat(ui, state, &format!("[Privat an {}] {}", nick, text));
        ui.set_chat_input("".into());
    }
}

// ── Räume ──

pub fn join_room(ui: &MainWindow, state: &Arc<AppState>, room_id: i64, password: Option<String>) {
    let mut data = serde_json::json!({ "room_id": room_id });
    if let Some(pw) = password {
        data["password"] = serde_json::Value::String(pw);
    }
    if !send_or_status(ui, state, Message::new("room_join", data)) {
        return;
    }

    let (sr, bd, ch) = {
        let mut inner = state.inner.lock();
        inner.current_room_id = Some(room_id);
        (
            inner.audio_config.sample_rate,
            inner.audio_config.bit_depth,
            inner.audio_config.channels,
        )
    };

    // Audio-Konfiguration anmelden (Server antwortet mit UDP-Token)
    let _ = state.send_ws(Message::new(
        "audio_config",
        serde_json::json!({
            "sample_rate": sr,
            "bit_depth": bd,
            "channels": ch,
            "enabled": true,
        }),
    ));

    // Dateiliste anfordern
    let _ = state.send_ws(Message::new(
        "file_list",
        serde_json::json!({ "room_id": room_id }),
    ));

    let room = state.inner.lock().room_name(room_id);
    append_chat(ui, state, &format!("* Raum {} beigetreten.", room));
    rebuild_users(ui, state);
    refresh_status(ui, state);
}

fn join_selected(ui: &MainWindow, state: &Arc<AppState>) {
    let Some(room_id) = selected_room(ui, state) else {
        set_status(ui, "Bitte zuerst einen Raum in der Liste auswählen.");
        return;
    };
    let has_password = state
        .inner
        .lock()
        .rooms
        .iter()
        .find(|r| r.id == room_id)
        .map(|r| r.has_password)
        .unwrap_or(false);
    if has_password {
        {
            let mut inner = state.inner.lock();
            inner.pending_join_room = Some(room_id);
        }
        show_dialog(
            ui,
            "room-password",
            "Raum-Passwort",
            "Dieser Raum ist passwortgeschützt.",
            "Passwort:",
            true,
            "",
            false,
            true,
        );
    } else {
        join_room(ui, state, room_id, None);
    }
}

fn leave_room(ui: &MainWindow, state: &Arc<AppState>) {
    let Some(room_id) = state.inner.lock().current_room_id else {
        set_status(ui, "Du bist in keinem Raum.");
        return;
    };
    if send_or_status(
        ui,
        state,
        Message::new("room_leave", serde_json::json!({ "room_id": room_id })),
    ) {
        let room = state.inner.lock().room_name(room_id);
        {
            let mut inner = state.inner.lock();
            inner.current_room_id = None;
            inner.ui_files.clear();
        }
        append_chat(ui, state, &format!("* Raum {} verlassen.", room));
        rebuild_users(ui, state);
        rebuild_files(ui, state);
        refresh_status(ui, state);
    }
}

// ── Audio ──

fn toggle_mute(ui: &MainWindow, state: &Arc<AppState>) {
    let muted = {
        let mut inner = state.inner.lock();
        inner.muted = !inner.muted;
        inner.muted
    };
    let _ = state.send_ws(Message::new(
        "audio_mute",
        serde_json::json!({ "muted": muted }),
    ));
    ui.set_muted(muted);
    append_chat(
        ui,
        state,
        if muted {
            "* Mikrofon stummgeschaltet."
        } else {
            "* Mikrofon eingeschaltet."
        },
    );
    refresh_status(ui, state);
}

fn toggle_deafen(ui: &MainWindow, state: &Arc<AppState>) {
    let deafened = {
        let mut inner = state.inner.lock();
        inner.deafened = !inner.deafened;
        inner.deafened
    };
    let _ = state.send_ws(Message::new(
        "audio_deafen",
        serde_json::json!({ "deafened": deafened }),
    ));
    ui.set_deafened(deafened);
    append_chat(
        ui,
        state,
        if deafened {
            "* Ton ausgeschaltet (taub)."
        } else {
            "* Ton eingeschaltet."
        },
    );
    refresh_status(ui, state);
}

fn toggle_loopback(ui: &MainWindow, state: &Arc<AppState>) {
    let loopback = {
        let mut inner = state.inner.lock();
        inner.loopback = !inner.loopback;
        inner.loopback
    };
    let _ = state.send_ws(Message::new(
        "audio_loopback",
        serde_json::json!({ "enabled": loopback }),
    ));
    ui.set_loopback(loopback);
    append_chat(
        ui,
        state,
        if loopback {
            "* Loopback eingeschaltet."
        } else {
            "* Loopback ausgeschaltet."
        },
    );
    refresh_status(ui, state);
}

pub fn volume_changed(state: &Arc<AppState>, value: f32) {
    state.set_volume(value / 100.0);
}

pub fn save_volume(state: &Arc<AppState>) {
    let mut cfg = crate::config::load_config();
    cfg.volume = state.volume();
    let _ = crate::config::save_config(&cfg);
}

// ── Datei-Streaming ──

fn stream_file(
    ui: &MainWindow,
    state: &Arc<AppState>,
    rt: &tokio::runtime::Handle,
    ev_tx: &mpsc::UnboundedSender<Message>,
) {
    if !state.inner.lock().connected {
        set_status(ui, "Nicht verbunden.");
        return;
    }
    let Some(path) = rfd::FileDialog::new()
        .set_title("Audiodatei zum Streamen wählen")
        .add_filter(
            "Audiodateien",
            &["mp3", "wav", "flac", "ogg", "m4a", "aac", "opus", "aiff"],
        )
        .pick_file()
    else {
        return;
    };

    // Laufenden Stream stoppen
    {
        let inner = state.inner.lock();
        if let Some(ref tx) = inner.stream_shutdown {
            let _ = tx.send(true);
        }
    }

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let was_loopback = {
        let mut inner = state.inner.lock();
        inner.stream_shutdown = Some(shutdown_tx);
        inner.streaming_file = true;
        inner.loopback
    };

    // Loopback aktivieren, damit der Stream auch lokal hörbar ist
    if !was_loopback {
        let _ = state.send_ws(Message::new(
            "audio_loopback",
            serde_json::json!({ "enabled": true }),
        ));
    }

    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    ui.set_streaming(true);
    append_chat(ui, state, &format!("* Streame Datei: {}", filename));
    refresh_status(ui, state);

    let state2 = state.clone();
    let ev_tx2 = ev_tx.clone();
    rt.spawn(async move {
        let result =
            crate::audio::file_stream::stream_audio_file(&path, state2.clone(), shutdown_rx).await;
        if let Err(e) = result {
            tracing::error!("Audio file stream error: {}", e);
            let _ = ev_tx2.send(Message::new(
                "client_error",
                serde_json::json!({ "message": format!("Streaming-Fehler: {}", e) }),
            ));
        }
        // Loopback zurücksetzen
        if !was_loopback {
            let _ = state2.send_ws(Message::new(
                "audio_loopback",
                serde_json::json!({ "enabled": false }),
            ));
        }
        {
            let mut inner = state2.inner.lock();
            inner.stream_shutdown = None;
        }
        let _ = ev_tx2.send(Message::new(
            "client_stream_finished",
            serde_json::json!({}),
        ));
    });
}

fn stop_stream(ui: &MainWindow, state: &Arc<AppState>) {
    let was_streaming = {
        let mut inner = state.inner.lock();
        let was = inner.stream_shutdown.is_some();
        if let Some(ref tx) = inner.stream_shutdown {
            let _ = tx.send(true);
        }
        inner.stream_shutdown = None;
        was
    };
    if !was_streaming {
        set_status(ui, "Es läuft kein Streaming.");
    }
    // Der Stream-Task meldet sich mit client_stream_finished und räumt auf.
}

// ── Dateien ──

fn upload_file(ui: &MainWindow, state: &Arc<AppState>) {
    let Some(room_id) = state.inner.lock().current_room_id else {
        set_status(ui, "Bitte zuerst einem Raum beitreten.");
        return;
    };
    let Some(path) = rfd::FileDialog::new()
        .set_title("Datei zum Hochladen wählen")
        .pick_file()
    else {
        return;
    };
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let data = match std::fs::read(&path) {
        Ok(d) => d,
        Err(e) => {
            set_status(ui, &format!("Datei konnte nicht gelesen werden: {}", e));
            return;
        }
    };
    let size = data.len() as i64;
    {
        let mut inner = state.inner.lock();
        inner.pending_upload = Some(PendingUpload {
            filename: filename.clone(),
            data,
        });
    }
    // Der Server antwortet mit file_upload_ack und der upload_id;
    // die Chunks werden im Event-Handler gesendet.
    send_or_status(
        ui,
        state,
        Message::new(
            "file_upload_start",
            serde_json::json!({
                "room_id": room_id,
                "filename": filename,
                "size": size,
            }),
        ),
    );
}

fn download_file(ui: &MainWindow, state: &Arc<AppState>) {
    let Some(file) = selected_file(ui, state) else {
        set_status(ui, "Bitte zuerst eine Datei in der Liste auswählen.");
        return;
    };
    let Some(path) = rfd::FileDialog::new()
        .set_title("Speicherort wählen")
        .set_file_name(&file.filename)
        .save_file()
    else {
        return;
    };
    {
        let mut inner = state.inner.lock();
        inner
            .download_targets
            .insert(file.id, (path, Vec::with_capacity(file.size_bytes as usize)));
    }
    if send_or_status(
        ui,
        state,
        Message::new("file_download", serde_json::json!({ "file_id": file.id })),
    ) {
        append_chat(ui, state, &format!("Lade {} herunter…", file.filename));
    }
}

fn refresh_files(ui: &MainWindow, state: &Arc<AppState>) {
    let Some(room_id) = state.inner.lock().current_room_id else {
        set_status(ui, "Bitte zuerst einem Raum beitreten.");
        return;
    };
    send_or_status(
        ui,
        state,
        Message::new("file_list", serde_json::json!({ "room_id": room_id })),
    );
}

// ── Einstellungen ──

fn open_settings(ui: &MainWindow) {
    let cfg = crate::config::load_config();
    let devices = crate::audio::device::list_devices();

    let mut inputs: Vec<slint::SharedString> = vec!["Standardgerät".into()];
    let mut outputs: Vec<slint::SharedString> = vec!["Standardgerät".into()];
    for d in &devices {
        if d.is_input {
            inputs.push(d.name.clone().into());
        }
        if d.is_output {
            outputs.push(d.name.clone().into());
        }
    }
    ui.set_input_devices(slint::ModelRc::new(slint::VecModel::from(inputs)));
    ui.set_output_devices(slint::ModelRc::new(slint::VecModel::from(outputs)));
    ui.set_selected_input(
        cfg.input_device
            .unwrap_or_else(|| "Standardgerät".into())
            .into(),
    );
    ui.set_selected_output(
        cfg.output_device
            .unwrap_or_else(|| "Standardgerät".into())
            .into(),
    );
    ui.set_settings_visible(true);
}

pub fn settings_accepted(ui: &MainWindow, state: &Arc<AppState>, input: &str, output: &str) {
    let to_opt = |s: &str| {
        if s.is_empty() || s == "Standardgerät" {
            None
        } else {
            Some(s.to_string())
        }
    };
    let mut cfg = crate::config::load_config();
    cfg.input_device = to_opt(input);
    cfg.output_device = to_opt(output);
    let _ = crate::config::save_config(&cfg);
    {
        let mut inner = state.inner.lock();
        inner.input_device = cfg.input_device.clone();
        inner.output_device = cfg.output_device.clone();
    }
    set_status(ui, "Einstellungen gespeichert (gelten ab nächster Verbindung).");
}

// ── Dialog-Ergebnisse ──

pub fn dialog_accepted(ui: &MainWindow, state: &Arc<AppState>, mode: &str, f1: &str, f2: &str) {
    let (kind, arg) = match mode.split_once(':') {
        Some((k, a)) => (k, Some(a)),
        None => (mode, None),
    };
    let arg_id: Option<i64> = arg.and_then(|a| a.parse().ok());

    match kind {
        "room-password" => {
            let pending = {
                let mut inner = state.inner.lock();
                inner.pending_join_room.take()
            };
            if let Some(room_id) = pending {
                join_room(ui, state, room_id, Some(f1.to_string()));
            }
        }
        "create-room" | "create-subroom" => {
            if f1.trim().is_empty() {
                set_status(ui, "Raumname darf nicht leer sein.");
                return;
            }
            let mut data = serde_json::json!({ "name": f1.trim() });
            if kind == "create-subroom" {
                if let Some(pid) = arg_id {
                    data["parent_id"] = serde_json::json!(pid);
                }
            }
            if !f2.trim().is_empty() {
                data["password"] = serde_json::Value::String(f2.trim().to_string());
            }
            send_or_status(ui, state, Message::new("room_create", data));
        }
        "delete-room" => {
            if let Some(room_id) = arg_id {
                send_or_status(
                    ui,
                    state,
                    Message::new("room_delete", serde_json::json!({ "room_id": room_id })),
                );
            }
        }
        "kick" => {
            if let Some(user_id) = arg_id {
                let mut data = serde_json::json!({ "user_id": user_id });
                if !f1.trim().is_empty() {
                    data["reason"] = serde_json::Value::String(f1.trim().to_string());
                }
                send_or_status(ui, state, Message::new("admin_kick", data));
            }
        }
        "ban" => {
            if let Some(user_id) = arg_id {
                let mut data = serde_json::json!({ "user_id": user_id });
                if !f1.trim().is_empty() {
                    data["reason"] = serde_json::Value::String(f1.trim().to_string());
                }
                if let Ok(minutes) = f2.trim().parse::<i64>() {
                    data["duration_minutes"] = serde_json::json!(minutes);
                }
                send_or_status(ui, state, Message::new("admin_ban", data));
            }
        }
        "server-message" => {
            if !f1.trim().is_empty() {
                send_or_status(
                    ui,
                    state,
                    Message::new(
                        "admin_server_message",
                        serde_json::json!({ "message": f1.trim() }),
                    ),
                );
            }
        }
        _ => {}
    }
}

// ── Menü-Dispatcher ──

pub fn menu_action(
    ui: &MainWindow,
    state: &Arc<AppState>,
    rt: &tokio::runtime::Handle,
    ev_tx: &mpsc::UnboundedSender<Message>,
    action: &str,
) {
    match action {
        "quit" => {
            save_volume(state);
            do_disconnect(ui, state);
            let _ = slint::quit_event_loop();
        }
        "disconnect" => {
            do_disconnect(ui, state);
            append_chat(ui, state, "Verbindung getrennt.");
        }
        "settings" => open_settings(ui),

        "toggle-mute" => toggle_mute(ui, state),
        "toggle-deafen" => toggle_deafen(ui, state),
        "toggle-loopback" => toggle_loopback(ui, state),
        "stream-file" => stream_file(ui, state, rt, ev_tx),
        "stop-stream" => stop_stream(ui, state),

        "join-room" => join_selected(ui, state),
        "leave-room" => leave_room(ui, state),
        "create-room" => show_dialog(
            ui,
            "create-room",
            "Raum erstellen",
            "",
            "Raumname:",
            true,
            "Passwort (optional):",
            true,
            false,
        ),
        "create-subroom" => {
            let Some(parent) = selected_room(ui, state) else {
                set_status(ui, "Bitte zuerst den übergeordneten Raum auswählen.");
                return;
            };
            let parent_name = state.inner.lock().room_name(parent);
            show_dialog(
                ui,
                &format!("create-subroom:{}", parent),
                "Unterraum erstellen",
                &format!("Übergeordneter Raum: {}", parent_name),
                "Raumname:",
                true,
                "Passwort (optional):",
                true,
                false,
            );
        }
        "delete-room" => {
            let Some(room_id) = selected_room(ui, state) else {
                set_status(ui, "Bitte zuerst einen Raum auswählen.");
                return;
            };
            let name = state.inner.lock().room_name(room_id);
            show_dialog(
                ui,
                &format!("delete-room:{}", room_id),
                "Raum löschen",
                &format!("Raum „{}“ wirklich löschen?", name),
                "",
                false,
                "",
                false,
                false,
            );
        }

        "upload-file" => upload_file(ui, state),
        "download-file" => download_file(ui, state),
        "refresh-files" => refresh_files(ui, state),

        "private-message" => send_private_message(ui, state),
        "kick" => {
            let Some(user_id) = selected_user(ui, state) else {
                set_status(ui, "Bitte zuerst einen Nutzer auswählen.");
                return;
            };
            let nick = state.inner.lock().nickname_of(user_id);
            show_dialog(
                ui,
                &format!("kick:{}", user_id),
                "Nutzer kicken",
                &format!("Nutzer: {}", nick),
                "Grund (optional):",
                true,
                "",
                false,
                false,
            );
        }
        "ban" => {
            let Some(user_id) = selected_user(ui, state) else {
                set_status(ui, "Bitte zuerst einen Nutzer auswählen.");
                return;
            };
            let nick = state.inner.lock().nickname_of(user_id);
            show_dialog(
                ui,
                &format!("ban:{}", user_id),
                "Nutzer bannen",
                &format!("Nutzer: {}", nick),
                "Grund (optional):",
                true,
                "Dauer in Minuten (leer = unbegrenzt):",
                true,
                false,
            );
        }
        "move-user" => {
            let Some(user_id) = selected_user(ui, state) else {
                set_status(ui, "Bitte zuerst einen Nutzer auswählen.");
                return;
            };
            let Some(room_id) = selected_room(ui, state) else {
                set_status(ui, "Bitte zuerst den Zielraum in der Raumliste auswählen.");
                return;
            };
            send_or_status(
                ui,
                state,
                Message::new(
                    "admin_move",
                    serde_json::json!({ "user_id": user_id, "room_id": room_id }),
                ),
            );
        }
        "admin-mute" | "admin-unmute" => {
            let Some(user_id) = selected_user(ui, state) else {
                set_status(ui, "Bitte zuerst einen Nutzer auswählen.");
                return;
            };
            send_or_status(
                ui,
                state,
                Message::new(
                    "admin_mute",
                    serde_json::json!({ "user_id": user_id, "muted": action == "admin-mute" }),
                ),
            );
        }
        "server-message" => show_dialog(
            ui,
            "server-message",
            "Servernachricht senden",
            "",
            "Nachricht:",
            true,
            "",
            false,
            false,
        ),

        "help" => show_dialog(ui, "help", "Kurztasten", HELP_TEXT, "", false, "", false, false),

        other => tracing::warn!("Unbekannte Menü-Aktion: {}", other),
    }
}
