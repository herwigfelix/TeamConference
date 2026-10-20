//! Aktionen aus Menüs, Buttons, Tastatur und Dialogen.

use std::sync::Arc;

use wxdragon::prelude::*;

use crate::app::{AuthSpec, Ctx, Reconnect, SessionParams};
use crate::config::{self, ServerEntry};
use crate::handlers::{rebuild_files, rebuild_server_list, rebuild_tree, refresh_status};
use crate::protocol::Message;
use crate::state::{AppState, PendingUpload};
use crate::ui;

use tokio::sync::mpsc::UnboundedSender;

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

/// Im Baum ausgewählte Raum-ID (bei Nutzer-Knoten dessen Raum).
fn selected_room(ctx: &Ctx) -> Option<i64> {
    crate::roomtree::selected_room(&ctx.ui.rooms_tree, &ctx.st.borrow().tree_map)
}

/// Im Baum ausgewählte Nutzer-ID.
fn selected_user(ctx: &Ctx) -> Option<i64> {
    crate::roomtree::selected_user(&ctx.ui.rooms_tree, &ctx.st.borrow().tree_map)
}

fn selected_file(ctx: &Ctx) -> Option<crate::protocol::FileInfo> {
    let idx = ctx.ui.files.get_selection()? as usize;
    ctx.st.borrow().files.get(idx).cloned()
}

fn ask_text(ctx: &Ctx, message: &str, caption: &str, default: &str) -> Option<String> {
    ask_text_on(&ctx.ui.frame, message, caption, default)
}

/// Wie `ask_text`, aber nur mit dem Elternfenster — für Closures in Dialogen,
/// die kein `Ctx` einfangen können (Widgets sind `!Send`/`Copy`).
fn ask_text_on(parent: &Frame, message: &str, caption: &str, default: &str) -> Option<String> {
    let dlg = TextEntryDialog::builder(parent, message, caption)
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

/// Screenreader-taugliche Rückmeldung als modaler Dialog (die Statuszeile wird
/// von Screenreadern nicht vorgelesen). Setzt zusätzlich die Statuszeile.
pub fn notify(ctx: &Ctx, message: &str, caption: &str) {
    ctx.ui.set_status(message);
    let dlg = MessageDialog::builder(&ctx.ui.frame, message, caption).build();
    dlg.show_modal();
}

/// Meldung zu einem asynchronen Ereignis (aus Netzwerk-/Audio-Tasks, verarbeitet
/// im Nachrichten-Timer). Desktop: wie [`notify`] (modaler Dialog).
/// iOS/Android: nicht-modaler Hinweis, den VoiceOver/TalkBack ansagt. Ein
/// modaler Alert aus dem Timer heraus hielte unter MUi die Verarbeitung aller
/// weiteren Server-Nachrichten an, bis er geschlossen wird (der Timer-Handler
/// läuft nicht verschachtelt) — z. B. blieb die Dateiliste nach einem Upload
/// leer, solange „Upload abgeschlossen" offen war.
pub fn notify_async(ctx: &Ctx, message: &str, caption: &str) {
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    notify(ctx, message, caption);
    #[cfg(any(target_os = "ios", target_os = "android"))]
    {
        let _ = caption;
        ctx.ui.set_status(message);
        if let Ok(hint) = NotificationMessage::builder().with_message(message).build() {
            hint.show(TIMEOUT_AUTO);
        }
    }
}

/// Sprachausgabe einer Meldung über den Screenreader/TTS. `interrupt = true`
/// unterbricht laufende Ansagen (für direkte Aktionsrückmeldung wie Lautstärke);
/// `false` reiht ein (für aufeinanderfolgende Server-Ereignisse).
/// Nur auf dem UI-Thread aufrufen (TTS-Instanz ist nicht Send).
pub fn announce(ctx: &Ctx, text: &str, interrupt: bool) {
    let mut st = ctx.st.borrow_mut();
    if let Some(tts) = st.tts.as_mut() {
        let _ = tts.speak(text, interrupt);
    }
}

/// Ein Server-Ereignis ansagen — nur wenn der Toggle „Server-Ereignisse ansagen"
/// aktiv ist. Reiht die Ansagen ein, damit dichte Ereignisfolgen nicht verschluckt
/// werden.
pub fn announce_event(ctx: &Ctx, text: &str) {
    if ctx.st.borrow().announce_events {
        announce(ctx, text, false);
    }
}

// ── Verbindung ──

pub fn do_connect(ctx: &Ctx) {
    if ctx.app.inner.lock().connected {
        notify(ctx, "Bereits verbunden — bitte zuerst trennen.", "Verbinden");
        return;
    }
    #[cfg(any(target_os = "ios", target_os = "android"))]
    prepare_mobile_audio(ctx);
    let host = ctx.ui.host_in.get_value().trim().to_string();
    let port: u16 = match ctx.ui.port_in.get_value().trim().parse() {
        Ok(p) => p,
        Err(_) => {
            notify(ctx, "Ungültiger Port.", "Verbinden");
            return;
        }
    };
    let ssl = ctx.ui.ssl_chk.is_checked();
    let use_central = ctx.ui.use_central_chk.is_checked();
    // Unterserver-ID (Hub-Modus); vom Verzeichnis-Beitritt oder Lesezeichen gesetzt.
    let server_id = ctx.st.borrow().pending_server_id.clone().unwrap_or_default();
    // Audio-Port: optionales Feld (kann vom Steuerport abweichen, z. B. Hub);
    // leer/ungültig → Konvention Steuerport + 1.
    let udp_port = ctx
        .ui
        .audio_port_in
        .get_value()
        .trim()
        .parse::<u16>()
        .ok()
        .filter(|&p| p != 0)
        .unwrap_or(port + 1);
    let username = ctx.ui.user_in.get_value().trim().to_string();
    let password = ctx.ui.pass_in.get_value();
    let mut nickname = ctx.ui.nick_in.get_value().trim().to_string();

    if host.is_empty() {
        notify(ctx, "Host ist erforderlich.", "Verbinden");
        return;
    }

    // Anmeldemodus: Passwort (klassisch) oder zentrales Token.
    let auth = if use_central {
        match config::load_config().hub {
            Some(h) if !h.refresh_token.is_empty() => {
                if nickname.is_empty() {
                    nickname = if !h.display_name.is_empty() {
                        h.display_name.clone()
                    } else {
                        h.username.clone()
                    };
                }
                AuthSpec::Central { server_id }
            }
            _ => {
                notify(
                    ctx,
                    "Für zentrales Login bitte zuerst im Reiter „Server-Hub\" anmelden.",
                    "Verbinden",
                );
                return;
            }
        }
    } else {
        if username.is_empty() || password.is_empty() {
            notify(
                ctx,
                "Host, Benutzername und Passwort sind erforderlich.",
                "Verbinden",
            );
            return;
        }
        if nickname.is_empty() {
            nickname = username.clone();
        }
        AuthSpec::Password { username, password }
    };

    let (input_device, output_device) = {
        let cfg = config::load_config();
        (cfg.input_device.clone(), cfg.output_device.clone())
    };
    {
        let mut inner = ctx.app.inner.lock();
        inner.nickname = nickname.clone();
        inner.input_device = input_device.clone();
        inner.output_device = output_device.clone();
    }

    let params = SessionParams {
        host,
        port,
        ssl,
        udp_port,
        nickname,
        input_device,
        output_device,
        auth,
    };
    // Sitzung merken (ermöglicht Wiederverbindung); ein etwaiger früherer
    // Reconnect-Zustand wird verworfen.
    {
        let mut st = ctx.st.borrow_mut();
        st.session = Some(params.clone());
        st.reconnect = None;
    }

    status(ctx, "Verbinde…");

    // Neue Verbindungsgeneration → verwaist etwaige noch wartende Reconnect-Tasks.
    let gen = ctx
        .app
        .connect_gen
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        .wrapping_add(1);
    let app = ctx.app.clone();
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt
        .spawn(async move { establish_session(app, ev_tx, params, false, gen).await });
}

/// Verbindung + Audio + Anmeldung herstellen (gemeinsam von `do_connect` und der
/// automatischen Wiederverbindung genutzt). Läuft auf einem Tokio-Task.
///
/// Bei `is_reconnect` wird die Wiedergabe nicht erneut gestartet (der bestehende
/// Wiedergabe-Thread bleibt aktiv und wird weiter aus dem AppState gespeist),
/// damit sich Audiogeräte über mehrere Versuche nicht stapeln.
///
/// Ein nicht herstellbarer **WebSocket** (inkl. fehlgeschlagenem zentralem
/// Token-Refresh) meldet `connect_failed`; darauf entscheidet der UI-Handler
/// über erneuten Versuch (Reconnect) bzw. Fehleranzeige (Erstverbindung).
/// Spätere, nicht fatale Fehler (UDP) melden nur `client_error`.
async fn establish_session(
    app: Arc<AppState>,
    ev_tx: UnboundedSender<Message>,
    params: SessionParams,
    is_reconnect: bool,
    gen: u64,
) {
    // Wurde inzwischen neu verbunden oder getrennt? Dann ist dieser (evtl. nach
    // Backoff aufgewachte) Versuch veraltet → abbrechen, ohne etwas zu tun.
    if app.connect_gen.load(std::sync::atomic::Ordering::SeqCst) != gen {
        return;
    }
    let fail = |text: String| {
        let _ = ev_tx.send(Message::new("connect_failed", serde_json::json!({ "message": text })));
    };
    let warn = |text: String| {
        let _ = ev_tx.send(Message::new("client_error", serde_json::json!({ "message": text })));
    };

    // 1) Login-Daten zuerst bauen — beim zentralen Login ein frisches
    // Access-Token holen (Refresh-Rotation), BEVOR ein Socket geöffnet wird.
    // So lässt ein fehlgeschlagener Refresh keinen WebSocket zurück.
    let nickname = params.nickname.clone();
    let login_data = match &params.auth {
        AuthSpec::Password { username, password } => {
            serde_json::json!({ "username": username, "password": password, "nickname": nickname })
        }
        AuthSpec::Central { server_id } => {
            let refresh_token = match config::load_config().hub {
                Some(h) if !h.refresh_token.is_empty() => h.refresh_token,
                _ => {
                    fail("Zentrale Sitzung nicht mehr vorhanden.".into());
                    return;
                }
            };
            let res = tokio::task::spawn_blocking(move || crate::hub::refresh(&refresh_token)).await;
            match res {
                Ok(Ok(bundle)) => {
                    let mut cfg = config::load_config();
                    cfg.hub = Some(crate::config::HubSession {
                        central_uid: bundle.central_uid.clone(),
                        username: bundle.username.clone(),
                        display_name: bundle.display_name.clone(),
                        role: bundle.role.clone(),
                        refresh_token: bundle.refresh_token.clone(),
                        status: bundle.status.clone(),
                    });
                    let _ = config::save_config(&cfg);
                    serde_json::json!({ "central_token": bundle.access_token, "nickname": nickname, "server_id": server_id })
                }
                Ok(Err(e)) => {
                    fail(format!("Zentrales Login fehlgeschlagen: {}", e));
                    return;
                }
                Err(e) => {
                    fail(format!("Zentrales Login fehlgeschlagen: {}", e));
                    return;
                }
            }
        }
    };

    // 2) WebSocket herstellen.
    if let Err(e) =
        crate::net::ws_client::connect(&params.host, params.port, params.ssl, app.clone(), ev_tx.clone())
            .await
    {
        fail(format!("Verbindung fehlgeschlagen: {}", e));
        return;
    }

    // 3) UDP-Audio (nicht fatal — bei Fehler bleibt die Steuerverbindung).
    match crate::net::udp_client::start_udp_audio(&params.host, params.udp_port, app.clone()).await {
        Ok((send_shutdown, recv_shutdown)) => {
            let mut inner = app.inner.lock();
            inner.capture_shutdown = Some(send_shutdown);
            inner.playback_shutdown = Some(recv_shutdown);
        }
        Err(e) => warn(format!("UDP-Audio fehlgeschlagen: {}", e)),
    }

    // 4) Wiedergabe-Thread — nur bei Erstverbindung; bei Wiederverbindung läuft
    // der bestehende weiter (er liest aus dem AppState-Kanal).
    if !is_reconnect {
        let playback_state = app.clone();
        let output_device = params.output_device.clone();
        // C4: Owner-Thread hält den cpal-Stream und wartet auf ein Stop-Signal,
        // statt ewig zu parken. Bei Stop fällt `_stream` aus dem Scope → wird
        // gedroppt (kein Leak; alter Stream ist vor einem Restart wirklich weg).
        let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
        app.inner.lock().playback_stream_stop = Some(stop_tx);
        std::thread::spawn(move || {
            match crate::audio::playback::start_playback(playback_state, output_device) {
                Ok(_stream) => {
                    let _ = stop_rx.recv();
                }
                Err(e) => tracing::error!("Failed to start playback: {}", e),
            }
        });
    }

    // 5) Anmelden.
    if let Err(e) = app.send_ws(Message::new("auth_login", login_data)) {
        warn(format!("Anmeldung fehlgeschlagen: {}", e));
    }
}

/// Audio-Pipeline (Mikro-Aufnahme + UDP-Empfang) anhalten und Socket-bezogenen
/// Zustand löschen, OHNE die UI zu verstecken oder den Wiedergabe-Thread zu
/// beenden. Wird vor jedem Wiederverbindungsversuch genutzt, damit sich
/// Aufnahme-/Empfangs-Tasks nicht stapeln.
fn stop_audio_pipeline(app: &Arc<AppState>) {
    let mut inner = app.inner.lock();
    if let Some(tx) = inner.capture_shutdown.take() {
        let _ = tx.send(true);
    }
    // C1: auch den cpal-Aufnahme-Stream beenden (der Wiedergabe-Stream läuft über
    // Reconnects weiter und wird hier bewusst nicht angefasst).
    if let Some(tx) = inner.capture_stream_stop.take() {
        let _ = tx.send(());
    }
    if let Some(tx) = inner.playback_shutdown.take() {
        let _ = tx.send(true);
    }
    if let Some(tx) = inner.stream_shutdown.take() {
        let _ = tx.send(true);
    }
    inner.udp_socket = None;
    inner.server_udp_addr = None;
    inner.session_token = None;
    inner.audio_id = None;
    inner.capturing = false;
    inner.streaming_file = false;
    app.file_streaming
        .store(false, std::sync::atomic::Ordering::Relaxed);
}

/// Nach einem unerwarteten Abbruch die automatische Wiederverbindung einleiten
/// bzw. fortsetzen. Tut nichts, wenn keine Sitzung mehr gewünscht ist (der
/// Nutzer hat selbst getrennt → `session == None`).
pub fn begin_reconnect(ctx: &Ctx) {
    if ctx.st.borrow().session.is_none() {
        return;
    }
    // Reste der alten Verbindung abräumen.
    stop_audio_pipeline(&ctx.app);
    {
        let mut inner = ctx.app.inner.lock();
        inner.connected = false;
        inner.authenticated = false;
        inner.ws_tx = None;
    }
    // Beim ersten Abbruch den Reconnect-Zustand anlegen (Raum zum Wiederbetreten
    // merken). Ein Abbruch mitten in einem Versuch behält den Zustand bei.
    let fresh = ctx.st.borrow().reconnect.is_none();
    if fresh {
        let (room_id, room_password) = {
            let inner = ctx.app.inner.lock();
            (inner.current_room_id, inner.current_room_password.clone())
        };
        ctx.st.borrow_mut().reconnect = Some(Reconnect {
            attempt: 0,
            max_attempts: 8,
            room_id,
            room_password,
        });
    }
    schedule_reconnect(ctx);
}

/// Den nächsten Wiederverbindungsversuch planen (mit exponentiellem Backoff bis
/// 10 s) oder nach Erschöpfen der Versuche endgültig trennen.
pub fn schedule_reconnect(ctx: &Ctx) {
    let plan = {
        let mut st = ctx.st.borrow_mut();
        let Some(rc) = st.reconnect.as_mut() else {
            return;
        };
        rc.attempt += 1;
        let attempt = rc.attempt;
        let max = rc.max_attempts;
        if attempt > max {
            st.reconnect = None;
            None
        } else {
            // 1, 2, 4, 8, 10, 10, … Sekunden
            let delay = (1u64 << (attempt - 1).min(3)).min(10);
            st.session.clone().map(|p| (attempt, max, delay, p))
        }
    };

    let Some((attempt, max, delay, params)) = plan else {
        // Versuche erschöpft (oder keine Sitzung) → endgültig trennen.
        do_disconnect(ctx);
        ctx.ui
            .append_chat("Wiederverbindung fehlgeschlagen. Verbindung getrennt.");
        notify(
            ctx,
            "Wiederverbindung fehlgeschlagen. Verbindung zum Server verloren.",
            "Verbindung",
        );
        return;
    };

    let text = format!(
        "Verbindung verloren – Wiederverbindung… (Versuch {}/{})",
        attempt, max
    );
    status(ctx, &text);
    ctx.ui.append_chat(&text);

    let gen = ctx.app.connect_gen.load(std::sync::atomic::Ordering::SeqCst);
    let app = ctx.app.clone();
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
        establish_session(app, ev_tx, params, true, gen).await;
    });
}

/// Einem Raum nach erfolgreicher Wiederverbindung erneut beitreten — mit dem
/// beim ursprünglichen Betreten verwendeten Passwort (falls der Raum geschützt
/// war), sodass keine erneute Eingabe nötig ist.
pub fn rejoin_room(ctx: &Ctx, room_id: i64, password: Option<String>) {
    join_room(ctx, room_id, password);
}

/// iOS/Android: Mikrofon-Berechtigung erfragen (Systemdialog, blockiert wie ein
/// modaler Dialog) und die Audio-Session für Sprachchat aktivieren. Ohne
/// Session liefert iOS kein Eingabegerät; Android hält mit ihr die Verbindung
/// über einen Vordergrund-Dienst auch im Hintergrund.
#[cfg(any(target_os = "ios", target_os = "android"))]
fn prepare_mobile_audio(ctx: &Ctx) {
    use wxdragon::audio_session::{AudioMode, AudioSession};
    use wxdragon::permissions::{self, Permission};
    if !permissions::request(Permission::Microphone).is_granted() {
        ctx.ui.append_chat(
            "* Kein Mikrofonzugriff: Du kannst zuhören, aber nicht sprechen. Freigabe in den Systemeinstellungen.",
        );
    }
    if let Err(e) = AudioSession::new(AudioMode::VoiceChat)
        .with_speaker(true)
        .with_background_notice("TeamConference", "Verbunden")
        .activate()
    {
        tracing::warn!("Audio-Session konnte nicht aktiviert werden: {e}");
    }
    apply_mobile_input(config::load_config().input_device.as_deref());
}

pub fn do_disconnect(ctx: &Ctx) {
    #[cfg(any(target_os = "ios", target_os = "android"))]
    wxdragon::audio_session::deactivate();
    // Gewünschte Sitzung verwerfen → kein automatischer Reconnect mehr; die
    // erhöhte Generation verwaist zudem evtl. noch wartende Reconnect-Tasks.
    ctx.app
        .connect_gen
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    {
        let mut st = ctx.st.borrow_mut();
        st.session = None;
        st.reconnect = None;
    }
    {
        let mut inner = ctx.app.inner.lock();
        if let Some(ref tx) = inner.capture_shutdown {
            let _ = tx.send(true);
        }
        inner.capture_shutdown = None;
        // C1/C4: cpal-Aufnahme- und Wiedergabe-Stream sauber beenden (bei echtem
        // Disconnect auch die Wiedergabe, anders als beim Reconnect).
        if let Some(tx) = inner.capture_stream_stop.take() {
            let _ = tx.send(());
        }
        if let Some(tx) = inner.playback_stream_stop.take() {
            let _ = tx.send(());
        }
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
        inner.self_role = None;
        inner.session_token = None;
        inner.audio_id = None;
        inner.rooms.clear();
        inner.current_room_id = None;
        inner.current_room_password = None;
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

    set_menu_check(ctx, ui::ID_TOGGLE_MUTE, false);
    set_menu_check(ctx, ui::ID_TOGGLE_DEAFEN, false);
    set_menu_check(ctx, ui::ID_TOGGLE_LOOPBACK, false);
    ctx.ui.frame.set_title("TeamConference");
    ctx.ui.show_main(false);
    rebuild_tree(ctx);
    rebuild_files(ctx);
    // Admin-Menü zurücksetzen (kein angemeldeter Admin mehr).
    ctx.st.borrow_mut().account_dialog = None;
    update_account_menu(ctx);
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
        ctx.ui.pass_in.set_value(&s.password);
        ctx.ui.use_central_chk.set_value(s.use_central);
        let ap = if s.audio_port > 0 { s.audio_port.to_string() } else { String::new() };
        ctx.ui.audio_port_in.set_value(&ap);
        ctx.st.borrow_mut().pending_server_id =
            if s.server_id.is_empty() { None } else { Some(s.server_id.clone()) };
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
        password: ctx.ui.pass_in.get_value(),
        use_central: ctx.ui.use_central_chk.is_checked(),
        server_id: ctx.st.borrow().pending_server_id.clone().unwrap_or_default(),
        audio_port: ctx.ui.audio_port_in.get_value().trim().parse::<u16>().unwrap_or(0),
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

// ── Server-Hub (zentrales Login) ──

/// Hub-Statuszeile aus der gespeicherten Sitzung aktualisieren.
pub fn update_hub_status(ctx: &Ctx) {
    let label = match config::load_config().hub {
        Some(h) => {
            let name = if h.display_name.is_empty() { h.username.clone() } else { h.display_name.clone() };
            if h.status == "active" {
                let role = if h.role == "hub_admin" { " — Hub-Admin" } else { "" };
                format!("Angemeldet als {}{}.", name, role)
            } else {
                format!("Angemeldet als {} — das Konto wartet noch auf Freigabe.", name)
            }
        }
        None => "Nicht angemeldet. Mit dem zentralen Konto (Telefonnummer) siehst du das Server-Verzeichnis.".to_string(),
    };
    ctx.ui.hub_status.set_label(&label);
}

/// Die passende Hub-Seite zeigen: eingeloggt → Konto, sonst je nach Modus
/// Anmelden/Registrieren/Reset.
pub fn update_hub_view(ctx: &Ctx) {
    let idx = hub_page(ctx);
    if ctx.ui.hub_book.selection() != idx as i32 {
        ctx.ui.hub_book.set_selection(idx);
    }
    // Verwaltung nur für Hub-Admins ein-/ausblenden (Gruppe samt Knöpfen).
    let admin = hub_is_admin();
    ctx.ui.hub_admin_box.show(admin);
    ctx.ui.hub_admin_pending_btn.show(admin);
    ctx.ui.hub_admin_user_btn.show(admin);
    ctx.ui.hub_admin_servers_btn.show(admin);
    ctx.ui.hub_panel.layout();
    update_hub_status(ctx);
}

/// Index der Hub-Seite, die zum aktuellen Zustand gehört.
fn hub_page(ctx: &Ctx) -> usize {
    if config::load_config().hub.is_some() {
        return ui::HUB_PAGE_ACCOUNT;
    }
    match ctx.st.borrow().hub_view {
        crate::app::HubView::Login => ui::HUB_PAGE_LOGIN,
        crate::app::HubView::Register => ui::HUB_PAGE_REGISTER,
        crate::app::HubView::Reset => ui::HUB_PAGE_RESET,
    }
}

/// Seite wechseln und den Fokus mitnehmen. Ohne das bliebe er auf dem eben
/// ausgeblendeten Knopf („Passwort vergessen") hängen — Tastatur- und
/// Screenreader-Nutzer landeten im Nichts und fanden nicht zurück.
fn set_hub_view(ctx: &Ctx, view: crate::app::HubView) {
    use crate::app::HubView;
    let previous = std::mem::replace(&mut ctx.st.borrow_mut().hub_view, view);
    update_hub_view(ctx);
    if hub_page(ctx) == ui::HUB_PAGE_ACCOUNT {
        return;
    }
    match view {
        HubView::Register => ctx.ui.hub_phone_in.set_focus(),
        HubView::Reset => ctx.ui.hub_reset_phone_in.set_focus(),
        // Zurück: auf den Knopf, von dem man kam (gewohntes Verhalten).
        HubView::Login => match previous {
            HubView::Register => ctx.ui.hub_show_register_btn.set_focus(),
            HubView::Reset => ctx.ui.hub_show_reset_btn.set_focus(),
            HubView::Login => ctx.ui.hub_ident_in.set_focus(),
        },
    }
}

pub fn hub_show_register(ctx: &Ctx) {
    set_hub_view(ctx, crate::app::HubView::Register);
}
pub fn hub_show_reset(ctx: &Ctx) {
    set_hub_view(ctx, crate::app::HubView::Reset);
}
pub fn hub_show_login(ctx: &Ctx) {
    set_hub_view(ctx, crate::app::HubView::Login);
}

/// Escape auf einer Hub-Unterseite (Registrieren/Reset) führt zur Anmeldung zurück.
pub fn hub_escape(ctx: &Ctx) -> bool {
    if matches!(hub_page(ctx), ui::HUB_PAGE_REGISTER | ui::HUB_PAGE_RESET) {
        hub_show_login(ctx);
        true
    } else {
        false
    }
}

/// Modaler Formular-Dialog: mehrere Textfelder + optionales Häkchen, alles auf
/// einmal. `fields` = (Label, Vorbelegung, Passwortfeld?). Gibt bei OK die
/// Textwerte und den Häkchen-Zustand zurück.
fn form_dialog(
    ctx: &Ctx,
    title: &str,
    fields: &[(&str, String, bool)],
    checkbox: Option<(&str, bool)>,
) -> Option<(Vec<String>, bool)> {
    let dlg = Dialog::builder(&ctx.ui.frame, title).build();
    let v = BoxSizer::builder(Orientation::Vertical).build();
    let grid = ui::form_grid();
    let mut inputs: Vec<TextCtrl> = Vec::new();
    for (label, default, pw) in fields {
        let field = if *pw {
            TextCtrl::builder(&dlg).with_style(TextCtrlStyle::Password).build()
        } else {
            TextCtrl::builder(&dlg).build()
        };
        field.set_value(default);
        ui::form_row(&dlg, &grid, label, &field, label.trim_end_matches(':'));
        inputs.push(field);
    }
    if !ui::compact() {
        // Breite Felder statt gequetschter: Mindestbreite für die Feldspalte.
        if let Some(f) = inputs.first() {
            f.set_min_size(Size::new(300, -1));
        }
    }
    v.add_sizer(&grid, 0, SizerFlag::Expand | SizerFlag::All, 12);
    let chk = checkbox.map(|(label, val)| {
        let c = CheckBox::builder(&dlg).with_label(label).build();
        c.set_value(val);
        crate::ui::set_a11y_name(&c, label);
        v.add(&c, 0, SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 12);
        c
    });
    v.add_sizer(&ui::dialog_buttons(&dlg, "OK"), 0, SizerFlag::AlignRight | SizerFlag::Right | SizerFlag::Bottom, 6);
    dlg.set_sizer(v, true);
    dlg.fit();
    // Fokus ins erste Feld (Screenreader landen sonst auf „Abbrechen").
    if let Some(f) = inputs.first() {
        f.set_focus();
    }
    let result = dlg.show_modal();
    let out = if result == ID_OK {
        Some((
            inputs.iter().map(|f| f.get_value()).collect(),
            chk.map(|c| c.is_checked()).unwrap_or(false),
        ))
    } else {
        None
    };
    dlg.destroy();
    out
}

/// Verzeichnis nach einer Änderung neu laden (Event an den UI-Thread).
fn request_dir_reload(ev_tx: &tokio::sync::mpsc::UnboundedSender<Message>) {
    let _ = ev_tx.send(Message::new("hub_dir_reload", serde_json::json!({})));
}

/// Rolle aus der gespeicherten Hub-Sitzung („hub_admin" = Admin).
fn hub_is_admin() -> bool {
    matches!(config::load_config().hub, Some(h) if h.role == "hub_admin")
}

/// Token-Bündel als lokale Hub-Sitzung speichern (vom Hintergrund-Thread aus).
fn store_session(b: &crate::hub::TokenBundle) {
    let mut cfg = config::load_config();
    cfg.hub = Some(config::HubSession {
        central_uid: b.central_uid.clone(),
        username: b.username.clone(),
        display_name: b.display_name.clone(),
        role: b.role.clone(),
        refresh_token: b.refresh_token.clone(),
        status: b.status.clone(),
    });
    let _ = config::save_config(&cfg);
}

fn hub_msg(ev_tx: &tokio::sync::mpsc::UnboundedSender<Message>, message: String) {
    let _ = ev_tx.send(Message::new("hub_msg", serde_json::json!({ "message": message })));
}

pub fn hub_register(ctx: &Ctx) {
    let phone = ctx.ui.hub_phone_in.get_value().trim().to_string();
    let user = ctx.ui.hub_reg_user_in.get_value().trim().to_string();
    let display = ctx.ui.hub_reg_display_in.get_value().trim().to_string();
    let pass = ctx.ui.hub_reg_pass_in.get_value();
    if phone.is_empty() || user.is_empty() || pass.is_empty() {
        notify(ctx, "Telefon, Benutzername und Passwort sind erforderlich.", "Server-Hub");
        return;
    }
    ctx.ui.append_hub_log("Fordere Bestätigungscode an…");
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || crate::hub::register(&phone, &user, &pass, &display)).await;
        let m = match r {
            Ok(Ok(())) => "Code gesendet. Bitte Code eingeben und „Code bestätigen & anmelden\" wählen.".to_string(),
            Ok(Err(e)) => format!("Registrierung fehlgeschlagen: {}", e),
            Err(e) => format!("Fehler: {}", e),
        };
        hub_msg(&ev_tx, m);
    });
}

pub fn hub_verify(ctx: &Ctx) {
    let phone = ctx.ui.hub_phone_in.get_value().trim().to_string();
    let code = ctx.ui.hub_code_in.get_value().trim().to_string();
    if phone.is_empty() || code.is_empty() {
        notify(ctx, "Telefon und Bestätigungscode sind erforderlich.", "Server-Hub");
        return;
    }
    ctx.ui.append_hub_log("Bestätige Code…");
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || crate::hub::verify(&phone, &code)).await;
        let m = match r {
            Ok(Ok(b)) => {
                store_session(&b);
                if b.approved {
                    request_dir_reload(&ev_tx);
                    format!("Angemeldet als {}.", b.username)
                } else {
                    format!(
                        "Registriert als {}. Das Konto muss noch freigegeben werden. Bei Fragen: {} (Telefon/WhatsApp).",
                        b.username, b.team_contact
                    )
                }
            }
            Ok(Err(e)) => format!("Bestätigung fehlgeschlagen: {}", e),
            Err(e) => format!("Fehler: {}", e),
        };
        hub_msg(&ev_tx, m);
    });
}

pub fn hub_login(ctx: &Ctx) {
    let ident = ctx.ui.hub_ident_in.get_value().trim().to_string();
    let pass = ctx.ui.hub_login_pass_in.get_value();
    if ident.is_empty() || pass.is_empty() {
        notify(ctx, "Benutzername/Telefon und Passwort sind erforderlich.", "Server-Hub");
        return;
    }
    ctx.ui.append_hub_log("Melde am Hub an…");
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || crate::hub::login(&ident, &pass)).await;
        let m = match r {
            Ok(Ok(b)) => {
                store_session(&b);
                if b.approved {
                    request_dir_reload(&ev_tx);
                    format!("Angemeldet als {}.", b.username)
                } else {
                    format!("Angemeldet als {}. Konto wartet noch auf Freigabe.", b.username)
                }
            }
            Ok(Err(e)) => {
                if e.contains("phone_not_verified") {
                    "Telefonnummer noch nicht bestätigt — bitte zuerst registrieren/bestätigen.".to_string()
                } else {
                    format!("Anmeldung fehlgeschlagen: {}", e)
                }
            }
            Err(e) => format!("Fehler: {}", e),
        };
        hub_msg(&ev_tx, m);
    });
}

pub fn hub_logout(ctx: &Ctx) {
    let session = config::load_config().hub;
    let Some(h) = session else {
        notify(ctx, "Nicht angemeldet.", "Server-Hub");
        return;
    };
    // Sitzung lokal entfernen und sofort zur Anmelden-Seite wechseln.
    let mut cfg = config::load_config();
    cfg.hub = None;
    let _ = config::save_config(&cfg);
    ctx.st.borrow_mut().hub_view = crate::app::HubView::Login;
    // Verzeichnis-Liste leeren, damit die nächste Anmeldung frisch lädt.
    ctx.st.borrow_mut().hub_servers.clear();
    ctx.ui.hub_servers.clear();
    update_hub_view(ctx);
    let ev_tx = ctx.ev_tx.clone();
    let refresh = h.refresh_token.clone();
    ctx.rt.spawn(async move {
        let _ = tokio::task::spawn_blocking(move || crate::hub::logout(&refresh)).await;
        hub_msg(&ev_tx, "Abgemeldet.".to_string());
    });
}

pub fn hub_reset_start(ctx: &Ctx) {
    let phone = ctx.ui.hub_reset_phone_in.get_value().trim().to_string();
    if phone.is_empty() {
        notify(ctx, "Bitte Telefonnummer eingeben.", "Server-Hub");
        return;
    }
    ctx.ui.append_hub_log("Fordere Code für Passwort-Reset an…");
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || crate::hub::reset_start(&phone)).await;
        let m = match r {
            Ok(Ok(())) => "Falls die Nummer registriert ist, wurde ein Code gesendet. Neues Passwort eintragen und „Neues Passwort setzen\" wählen.".to_string(),
            Ok(Err(e)) => format!("Anfrage fehlgeschlagen: {}", e),
            Err(e) => format!("Fehler: {}", e),
        };
        hub_msg(&ev_tx, m);
    });
}

/// Frisches Access-Token aus der gespeicherten Sitzung holen (Refresh-Rotation,
/// neue Token werden gespeichert). Nur vom Hintergrund-Thread aus aufrufen.
fn fresh_access_token() -> Result<String, String> {
    let session = config::load_config().hub.ok_or_else(|| "Nicht im Hub angemeldet".to_string())?;
    let bundle = crate::hub::refresh(&session.refresh_token)?;
    store_session(&bundle);
    Ok(bundle.access_token)
}

pub fn hub_load_directory(ctx: &Ctx) {
    if config::load_config().hub.is_none() {
        notify(ctx, "Bitte zuerst im Server-Hub anmelden.", "Server-Hub");
        return;
    }
    let q = ctx.ui.hub_search_in.get_value().trim().to_string();
    // Ansicht 1 = „Meine Server": /servers/mine liefert eigene UND fremde
    // Server mit Mitgliedschaft, öffentlich wie privat. Das öffentliche
    // Verzeichnis (/servers) zeigt private Server grundsätzlich nicht.
    let mine = hub_scope_is_mine(ctx);
    ctx.ui.append_hub_log(if mine { "Lade meine Server…" } else { "Lade Verzeichnis…" });
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || -> Result<Vec<crate::hub::ServerInfo>, String> {
            let access = fresh_access_token()?;
            if mine {
                // /servers/mine kennt keinen Suchparameter → hier filtern.
                let all = crate::hub::list_my_servers(&access)?;
                let needle = q.to_lowercase();
                Ok(all
                    .into_iter()
                    .filter(|s| {
                        needle.is_empty()
                            || s.name.to_lowercase().contains(&needle)
                            || s.description.to_lowercase().contains(&needle)
                    })
                    .collect())
            } else {
                crate::hub::list_servers(&access, &q)
            }
        })
        .await;
        match r {
            Ok(Ok(servers)) => {
                let _ = ev_tx.send(Message::new(
                    "hub_servers",
                    serde_json::json!({
                        "servers": servers,
                        "scope": if mine { "mine" } else { "public" },
                    }),
                ));
            }
            Ok(Err(e)) => hub_msg(&ev_tx, format!("Serverliste konnte nicht geladen werden: {}", e)),
            Err(e) => hub_msg(&ev_tx, format!("Fehler: {}", e)),
        }
    });
}

/// Steht der Ansicht-Umschalter auf „Meine Server"?
fn hub_scope_is_mine(ctx: &Ctx) -> bool {
    ctx.ui.hub_scope.get_selection().unwrap_or(0) == 1
}

/// Auf „Meine Server" umschalten und neu laden (nach Anlegen/Annehmen sinnvoll,
/// weil private Server nur dort erscheinen).
pub fn hub_show_my_servers(ctx: &Ctx) {
    ctx.ui.hub_scope.set_selection(1);
    hub_load_directory(ctx);
}

pub fn hub_join_selected(ctx: &Ctx) {
    let Some(idx) = ctx.ui.hub_servers.get_selection() else {
        notify(ctx, "Bitte zuerst einen Server im Verzeichnis auswählen.", "Server-Hub");
        return;
    };
    let server = ctx.st.borrow().hub_servers.get(idx as usize).cloned();
    let Some(s) = server else { return };
    if s.host.trim().is_empty() {
        notify(ctx, "Für diesen Server ist keine Adresse hinterlegt.", "Server-Hub");
        return;
    }
    // Verbindungsformular (im Reiter „Serverliste") füllen, zentrales Login
    // wählen und verbinden. Der Reiter bleibt bewusst auf „Server-Hub": Nach
    // dem Trennen landet man so wieder im Verzeichnis, aus dem man kam.
    ctx.ui.host_in.set_value(&s.host);
    ctx.ui.port_in.set_value(&s.control_port.to_string());
    ctx.ui.ssl_chk.set_value(true);
    ctx.ui.use_central_chk.set_value(true);
    // Unterserver-ID merken (für auth_login) und den Audio-Port ins Feld
    // schreiben, damit das Audio den richtigen UDP-Port trifft (und sichtbar ist).
    ctx.st.borrow_mut().pending_server_id = Some(s.id.clone());
    let ap = if s.audio_port > 0 { s.audio_port.to_string() } else { String::new() };
    ctx.ui.audio_port_in.set_value(&ap);
    do_connect(ctx);
}

pub fn hub_create_server(ctx: &Ctx) {
    if config::load_config().hub.is_none() {
        notify(ctx, "Bitte zuerst im Server-Hub anmelden.", "Server-Hub");
        return;
    }
    let Some((vals, is_public)) = form_dialog(
        ctx,
        "Server anlegen",
        &[
            ("Name:", String::new(), false),
            ("Beschreibung:", String::new(), false),
            ("Adresse (leer = vom Hub gehostet):", String::new(), false),
            ("Steuerport (leer = 9500):", String::new(), false),
            ("Audio-Port (leer = Steuerport+1):", String::new(), false),
        ],
        Some(("Öffentlich im Verzeichnis", true)),
    ) else { return };
    let name = vals[0].trim().to_string();
    if name.is_empty() {
        notify(ctx, "Name ist erforderlich.", "Server anlegen");
        return;
    }
    let description = vals[1].clone();
    // Adresse leer → der Hub hostet den Server selbst und trägt seine eigene
    // TC-Adresse ein; Beitreten läuft dann über die server_id. Mit Adresse wird
    // ein selbst gehosteter Server nur ins Verzeichnis eingetragen.
    let host = vals[2].trim().to_string();
    let (control_port, audio_port) = match parse_ports(&vals[3], &vals[4], 9500) {
        Ok(p) => p,
        Err(e) => {
            notify(ctx, &e, "Server anlegen");
            return;
        }
    };
    let (control_port, audio_port) = if host.is_empty() { (0, 0) } else { (control_port, audio_port) };
    let private_hint = !is_public;
    ctx.ui.append_hub_log("Lege Server an…");
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || {
            let access = fresh_access_token()?;
            crate::hub::create_server(&access, &name, &description, is_public, &host, control_port, audio_port)
        })
        .await;
        match r {
            Ok(Ok(_id)) => {
                hub_msg(
                    &ev_tx,
                    if private_hint {
                        "Server angelegt (privat) — sichtbar unter „Meine Server\"."
                            .to_string()
                    } else {
                        "Server angelegt.".to_string()
                    },
                );
                // Private Server tauchen im öffentlichen Verzeichnis nicht auf,
                // deshalb direkt in die eigene Liste wechseln.
                let _ = ev_tx.send(Message::new(
                    "hub_dir_reload",
                    serde_json::json!({ "mine": private_hint }),
                ));
            }
            Ok(Err(e)) => hub_msg(&ev_tx, format!("Server konnte nicht angelegt werden: {}", e)),
            Err(e) => hub_msg(&ev_tx, format!("Fehler: {}", e)),
        }
    });
}

/// Testzugang zu `parse_ports` (die Funktion selbst braucht kein `Ctx`).
#[cfg(test)]
pub fn parse_ports_for_test(c: &str, a: &str, d: i64) -> Result<(i64, i64), String> {
    parse_ports(c, a, d)
}

/// Port-Felder auswerten: leer → Vorgabe, Audio-Port leer → Steuerport+1.
fn parse_ports(control: &str, audio: &str, default_control: i64) -> Result<(i64, i64), String> {
    let c = control.trim();
    let a = audio.trim();
    let cp = if c.is_empty() {
        default_control
    } else {
        c.parse::<i64>().map_err(|_| "Steuerport ist keine Zahl.".to_string())?
    };
    let ap = if a.is_empty() {
        cp + 1
    } else {
        a.parse::<i64>().map_err(|_| "Audio-Port ist keine Zahl.".to_string())?
    };
    if !(1..=65535).contains(&cp) || !(1..=65535).contains(&ap) {
        return Err("Ports müssen zwischen 1 und 65535 liegen.".into());
    }
    Ok((cp, ap))
}

fn selected_hub_server(ctx: &Ctx) -> Option<crate::hub::ServerInfo> {
    let idx = ctx.ui.hub_servers.get_selection()? as usize;
    ctx.st.borrow().hub_servers.get(idx).cloned()
}

pub fn hub_edit_server(ctx: &Ctx) {
    let Some(s) = selected_hub_server(ctx) else {
        notify(ctx, "Bitte zuerst einen Server in der Liste auswählen.", "Server-Hub");
        return;
    };
    let Some((vals, is_public)) = form_dialog(
        ctx,
        "Server bearbeiten",
        &[
            ("Name:", s.name.clone(), false),
            ("Beschreibung:", s.description.clone(), false),
            ("Adresse (leer = unverändert):", s.host.clone(), false),
            ("Steuerport:", if s.control_port > 0 { s.control_port.to_string() } else { String::new() }, false),
            ("Audio-Port:", if s.audio_port > 0 { s.audio_port.to_string() } else { String::new() }, false),
        ],
        Some(("Öffentlich im Verzeichnis", s.is_public)),
    ) else { return };
    let name = vals[0].trim().to_string();
    if name.is_empty() {
        notify(ctx, "Name ist erforderlich.", "Server bearbeiten");
        return;
    }
    let description = vals[1].clone();
    // Leere Adresse NICHT durchreichen: der Hub würde die hinterlegte Adresse
    // überschreiben und der Server wäre nicht mehr erreichbar.
    let new_host = vals[2].trim().to_string();
    let (new_cp, new_ap) = if new_host.is_empty() {
        (s.control_port, s.audio_port)
    } else {
        let default_control = if s.control_port > 0 { s.control_port } else { 9500 };
        match parse_ports(&vals[3], &vals[4], default_control) {
            Ok(p) => p,
            Err(e) => {
                notify(ctx, &e, "Server bearbeiten");
                return;
            }
        }
    };
    ctx.ui.append_hub_log("Speichere Server…");
    let ev_tx = ctx.ev_tx.clone();
    let (sid, host, cp, ap) = (
        s.id.clone(),
        if new_host.is_empty() { s.host.clone() } else { new_host },
        new_cp,
        new_ap,
    );
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || {
            let access = fresh_access_token()?;
            crate::hub::update_server(&access, &sid, &name, &description, is_public, &host, cp, ap)
        })
        .await;
        match r {
            Ok(Ok(())) => { hub_msg(&ev_tx, "Server gespeichert.".to_string()); request_dir_reload(&ev_tx); }
            Ok(Err(e)) => hub_msg(&ev_tx, format!("Speichern fehlgeschlagen: {}", e)),
            Err(e) => hub_msg(&ev_tx, format!("Fehler: {}", e)),
        }
    });
}

pub fn hub_delete_server(ctx: &Ctx) {
    let Some(s) = selected_hub_server(ctx) else {
        notify(ctx, "Bitte zuerst einen Server in der Liste auswählen.", "Server-Hub");
        return;
    };
    let confirm = {
        let dlg = MessageDialog::builder(
            &ctx.ui.frame,
            &format!("Server „{}\" wirklich löschen?", s.name),
            "Server löschen",
        )
        .with_style(MessageDialogStyle::YesNo)
        .build();
        dlg.show_modal() == ID_YES
    };
    if !confirm {
        return;
    }
    ctx.ui.append_hub_log("Lösche Server…");
    let ev_tx = ctx.ev_tx.clone();
    let sid = s.id.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || {
            let access = fresh_access_token()?;
            crate::hub::delete_server(&access, &sid)
        })
        .await;
        match r {
            Ok(Ok(())) => { hub_msg(&ev_tx, "Server gelöscht.".to_string()); request_dir_reload(&ev_tx); }
            Ok(Err(e)) => hub_msg(&ev_tx, format!("Löschen fehlgeschlagen: {}", e)),
            Err(e) => hub_msg(&ev_tx, format!("Fehler: {}", e)),
        }
    });
}

/// Admin: Liste der wartenden Konten als Dialog mit „Freigeben"/„Ablehnen".
pub fn hub_admin_pending(ctx: &Ctx) {
    if !hub_is_admin() {
        notify(ctx, "Nur für Hub-Admins.", "Admin");
        return;
    }
    let dlg = Dialog::builder(&ctx.ui.frame, "Wartende Freigaben").with_size(480, 420).build();
    let v = BoxSizer::builder(Orientation::Vertical).build();
    v.add(
        &StaticText::builder(&dlg)
            .with_label("Konto auswählen und freigeben oder ablehnen:")
            .build(),
        0,
        SizerFlag::All,
        6,
    );
    let list = ListBox::builder(&dlg).build();
    crate::ui::set_a11y_name(&list, "Wartende Konten");
    v.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 6);
    let approve_btn = Button::builder(&dlg).with_label("Freigeben").build();
    let reject_btn = Button::builder(&dlg).with_label("Ablehnen").build();
    let close_btn = Button::builder(&dlg).with_label("Schließen").build();
    #[cfg(any(target_os = "ios", target_os = "android"))]
    reject_btn.set_destructive(true);
    v.add_sizer(&ui::button_row(&[&approve_btn, &reject_btn, &close_btn]), 0, SizerFlag::Expand | SizerFlag::All, 6);
    dlg.set_sizer(v, true);

    // Daten + Nachlade-Funktion (Liste füllen). Blockierende Abrufe sind kurz
    // und laufen auf dem UI-Thread, wie sonst auch.
    let data: std::rc::Rc<std::cell::RefCell<Vec<crate::hub::UserSummary>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let fill: std::rc::Rc<dyn Fn()> = {
        let data = data.clone();
        std::rc::Rc::new(move || {
            let items = fresh_access_token()
                .and_then(|t| crate::hub::admin_pending(&t))
                .unwrap_or_default();
            list.clear();
            for u in &items {
                list.append(&format!("{} ({})", u.display_name, u.username));
            }
            *data.borrow_mut() = items;
        })
    };
    fill();

    {
        let data = data.clone();
        let fill = fill.clone();
        approve_btn.on_click(move |_| {
            let uid = list
                .get_selection()
                .and_then(|i| data.borrow().get(i as usize).map(|u| u.central_uid.clone()));
            if let Some(uid) = uid {
                let _ = fresh_access_token().and_then(|t| crate::hub::admin_approve(&t, &uid));
                fill();
            }
        });
    }
    {
        let data = data.clone();
        let fill = fill.clone();
        let frame = ctx.ui.frame;
        reject_btn.on_click(move |_| {
            let sel = list
                .get_selection()
                .and_then(|i| data.borrow().get(i as usize).cloned());
            if let Some(u) = sel {
                let ok = MessageDialog::builder(
                    &frame,
                    &format!("Registrierung von „{}\" ({}) ablehnen und löschen?", u.display_name, u.username),
                    "Ablehnen",
                )
                .with_style(MessageDialogStyle::YesNo)
                .build()
                .show_modal()
                    == ID_YES;
                if ok {
                    let _ = fresh_access_token().and_then(|t| crate::hub::admin_reject(&t, &u.central_uid));
                    fill();
                }
            }
        });
    }
    {
        let d = dlg;
        close_btn.on_click(move |_| d.end_modal(ID_OK));
    }
    dlg.show_modal();
    dlg.destroy();
}

/// Admin: Nutzer suchen und verwalten (bannen/entsperren/Passwort/Admin).
pub fn hub_admin_user(ctx: &Ctx) {
    if !hub_is_admin() {
        notify(ctx, "Nur für Hub-Admins.", "Admin");
        return;
    }
    let Some(q) = ask_text(ctx, "Benutzer suchen (Name, mind. 3 Zeichen):", "Admin — Nutzer", "") else { return };
    let results = match fresh_access_token().and_then(|t| crate::hub::search_users(&t, q.trim())) {
        Ok(v) => v,
        Err(e) => {
            notify(ctx, &format!("Suche fehlgeschlagen: {}", e), "Admin");
            return;
        }
    };
    if results.is_empty() {
        notify(ctx, "Kein Nutzer gefunden.", "Admin");
        return;
    }
    // Treffer auswählen.
    let labels: Vec<String> = results.iter().map(|u| format!("{} ({})", u.display_name, u.username)).collect();
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let pick = SingleChoiceDialog::builder(&ctx.ui.frame, "Nutzer wählen:", "Admin — Nutzer", &label_refs).build();
    if pick.show_modal() != ID_OK {
        return;
    }
    let sel = pick.get_selection();
    if sel < 0 {
        return;
    }
    let u = results[sel as usize].clone();
    // Aktion wählen.
    let action_refs: Vec<&str> = vec!["Bannen", "Entsperren", "Passwort zurücksetzen", "Zum Admin machen"];
    let adlg = SingleChoiceDialog::builder(&ctx.ui.frame, &format!("Aktion für {}:", u.username), "Admin — Aktion", &action_refs).build();
    if adlg.show_modal() != ID_OK {
        return;
    }
    let ai = adlg.get_selection();
    if ai < 0 {
        return;
    }
    let uid = u.central_uid.clone();
    let res = match ai {
        0 => {
            let reason = ask_text(ctx, "Grund (optional):", "Bannen", "").unwrap_or_default();
            fresh_access_token().and_then(|t| crate::hub::admin_ban(&t, &uid, &reason))
        }
        1 => fresh_access_token().and_then(|t| crate::hub::admin_unban(&t, &uid)),
        2 => {
            let Some(pw) = ask_text(ctx, "Neues Passwort (mind. 8 Zeichen):", "Passwort zurücksetzen", "") else { return };
            fresh_access_token().and_then(|t| crate::hub::admin_reset_password(&t, &uid, &pw))
        }
        3 => fresh_access_token().and_then(|t| crate::hub::admin_promote(&t, &uid)),
        _ => return,
    };
    match res {
        Ok(()) => notify(ctx, &format!("Aktion für {} ausgeführt.", u.username), "Admin"),
        Err(e) => notify(ctx, &format!("Aktion fehlgeschlagen: {}", e), "Admin"),
    }
}

/// Jemanden auf den ausgewählten Server einladen: Nutzer suchen, Treffer wählen,
/// Profil zur Kontrolle anzeigen, Einladung anlegen (POST /invites). Einladen
/// darf, wer Mitglied oder Eigentümer des Servers ist.
pub fn hub_invite_user(ctx: &Ctx) {
    if config::load_config().hub.is_none() {
        notify(ctx, "Bitte zuerst im Server-Hub anmelden.", "Server-Hub");
        return;
    }
    let Some(server) = selected_hub_server(ctx) else {
        notify(
            ctx,
            "Bitte zuerst den Server in der Liste auswählen, auf den eingeladen werden soll. Eigene private Server stehen unter „Meine Server\".",
            "Nutzer einladen",
        );
        return;
    };
    let Some(q) = ask_text(
        ctx,
        &format!("Wen auf „{}\" einladen? Name (mind. 3 Zeichen):", server.name),
        "Nutzer einladen",
        "",
    ) else { return };
    // Kurze blockierende Aufrufe wie in den anderen Hub-Dialogen.
    let results = match fresh_access_token().and_then(|t| crate::hub::search_users(&t, q.trim())) {
        Ok(v) => v,
        Err(e) => {
            notify(ctx, &format!("Suche fehlgeschlagen: {}", e), "Nutzer einladen");
            return;
        }
    };
    if results.is_empty() {
        notify(ctx, "Kein Nutzer gefunden.", "Nutzer einladen");
        return;
    }
    let labels: Vec<String> = results
        .iter()
        .map(|u| format!("{} ({})", u.display_name, u.username))
        .collect();
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let pick = SingleChoiceDialog::builder(&ctx.ui.frame, "Nutzer wählen:", "Nutzer einladen", &label_refs).build();
    if pick.show_modal() != ID_OK {
        return;
    }
    let sel = pick.get_selection();
    if sel < 0 {
        return;
    }
    let u = results[sel as usize].clone();
    // Profil dazuholen, damit man vor dem Einladen die richtige Person erwischt.
    let bio = fresh_access_token()
        .and_then(|t| crate::hub::get_user_profile(&t, &u.central_uid))
        .map(|p| p.bio)
        .unwrap_or_default();
    let question = if bio.trim().is_empty() {
        format!("{} ({}) zu „{}\" einladen?", u.display_name, u.username, server.name)
    } else {
        format!(
            "{} ({}) zu „{}\" einladen?\n\nÜber die Person: {}",
            u.display_name, u.username, server.name, bio
        )
    };
    let ok = MessageDialog::builder(&ctx.ui.frame, &question, "Nutzer einladen")
        .with_style(MessageDialogStyle::YesNo)
        .build()
        .show_modal()
        == ID_YES;
    if !ok {
        return;
    }
    match fresh_access_token().and_then(|t| crate::hub::create_invite(&t, &server.id, &u.central_uid)) {
        Ok(_) => {
            ctx.ui.append_hub_log(&format!(
                "{} ({}) zu „{}\" eingeladen.",
                u.display_name, u.username, server.name
            ));
            notify(
                ctx,
                &format!(
                    "{} wurde eingeladen. Die Einladung erscheint dort unter „Einladungen…\" und muss angenommen werden.",
                    u.display_name
                ),
                "Nutzer einladen",
            );
        }
        Err(e) => notify(ctx, &format!("Einladung fehlgeschlagen: {}", e), "Nutzer einladen"),
    }
}

/// Admin: alle Server des Hubs ansehen und das Datei-Limit setzen.
pub fn hub_admin_servers(ctx: &Ctx) {
    if !hub_is_admin() {
        notify(ctx, "Nur für Hub-Admins.", "Admin");
        return;
    }
    let dlg = Dialog::builder(&ctx.ui.frame, "Admin — Server").with_size(560, 440).build();
    let v = BoxSizer::builder(Orientation::Vertical).build();
    v.add(
        &StaticText::builder(&dlg)
            .with_label("Server auswählen und Datei-Limit setzen (private Server zeigen nur den Namen):")
            .build(),
        0,
        SizerFlag::All,
        6,
    );
    let list = ListBox::builder(&dlg).build();
    crate::ui::set_a11y_name(&list, "Alle Server im Hub");
    v.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 6);
    let quota_btn = Button::builder(&dlg).with_label("Datei-Limit setzen…").build();
    let refresh_btn = Button::builder(&dlg).with_label("Aktualisieren").build();
    let close_btn = Button::builder(&dlg).with_label("Schließen").build();
    v.add_sizer(&ui::button_row(&[&quota_btn, &refresh_btn, &close_btn]), 0, SizerFlag::Expand | SizerFlag::All, 6);
    dlg.set_sizer(v, true);

    let data: std::rc::Rc<std::cell::RefCell<Vec<crate::hub::ServerInfo>>> =
        std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let fill: std::rc::Rc<dyn Fn()> = {
        let data = data.clone();
        std::rc::Rc::new(move || {
            let items = fresh_access_token()
                .and_then(|t| crate::hub::admin_list_servers(&t))
                .unwrap_or_default();
            list.clear();
            for s in &items {
                list.append(&admin_server_label(s));
            }
            *data.borrow_mut() = items;
        })
    };
    fill();

    {
        let data = data.clone();
        let fill = fill.clone();
        let frame = ctx.ui.frame;
        quota_btn.on_click(move |_| {
            let sel = list
                .get_selection()
                .and_then(|i| data.borrow().get(i as usize).cloned());
            let Some(s) = sel else { return };
            let current_mb = if s.file_limit_bytes > 0 {
                (s.file_limit_bytes / (1024 * 1024)).to_string()
            } else {
                String::new()
            };
            let Some(txt) = ask_text_on(
                &frame,
                &format!("Datei-Limit für „{}\" in MB (0 = kein Limit):", s.name),
                "Datei-Limit",
                &current_mb,
            ) else { return };
            let Ok(mb) = txt.trim().parse::<i64>() else {
                MessageDialog::builder(&frame, "Bitte eine Zahl eingeben.", "Datei-Limit").build().show_modal();
                return;
            };
            match fresh_access_token()
                .and_then(|t| crate::hub::admin_set_quota(&t, &s.id, mb.max(0) * 1024 * 1024))
            {
                Ok(()) => fill(),
                Err(e) => {
                    MessageDialog::builder(&frame, &format!("Limit setzen fehlgeschlagen: {}", e), "Datei-Limit")
                        .build()
                        .show_modal();
                }
            }
        });
    }
    {
        let fill = fill.clone();
        refresh_btn.on_click(move |_| fill());
    }
    {
        let d = dlg;
        close_btn.on_click(move |_| d.end_modal(ID_OK));
    }
    dlg.show_modal();
    dlg.destroy();
}

/// Listeneintrag für die Admin-Serverliste (privat = ohne Details).
fn admin_server_label(s: &crate::hub::ServerInfo) -> String {
    if s.private || !s.is_public {
        return format!("{} — privat (Details verborgen)", s.name);
    }
    let limit = if s.file_limit_bytes > 0 {
        format!("{} MB", s.file_limit_bytes / (1024 * 1024))
    } else {
        "kein Limit".to_string()
    };
    format!(
        "{} — öffentlich · {}:{} · belegt {} MB von {}",
        s.name,
        s.host,
        s.control_port,
        s.storage_used_bytes / (1024 * 1024),
        limit
    )
}

/// Offene Einladungen laden und einzeln zum Annehmen/Ablehnen anbieten.
pub fn hub_invites(ctx: &Ctx) {
    if config::load_config().hub.is_none() {
        notify(ctx, "Bitte zuerst im Server-Hub anmelden.", "Server-Hub");
        return;
    }
    // Kurzer blockierender Aufruf; die Ja/Nein-Dialoge laufen ohnehin auf dem
    // UI-Thread.
    let invites = match fresh_access_token().and_then(|t| crate::hub::list_invites(&t)) {
        Ok(v) => v,
        Err(e) => {
            notify(ctx, &format!("Einladungen konnten nicht geladen werden: {}", e), "Server-Hub");
            return;
        }
    };
    if invites.is_empty() {
        notify(ctx, "Keine offenen Einladungen.", "Server-Hub");
        return;
    }
    for inv in invites {
        let dlg = MessageDialog::builder(
            &ctx.ui.frame,
            &format!("Einladung zu „{}\" annehmen?", inv.server_name),
            "Einladungen",
        )
        .with_style(MessageDialogStyle::YesNo)
        .build();
        let accept = dlg.show_modal() == ID_YES;
        match fresh_access_token().and_then(|t| crate::hub::respond_invite(&t, &inv.id, accept)) {
            Ok(()) => ctx.ui.append_hub_log(&format!(
                "Einladung zu „{}\" {}.",
                inv.server_name,
                if accept { "angenommen" } else { "abgelehnt" }
            )),
            Err(e) => ctx.ui.append_hub_log(&format!("Fehler bei Einladung: {}", e)),
        }
    }
    // Neue Mitgliedschaften stehen unter „Meine Server" — dorthin wechseln und
    // neu laden, sonst bliebe ein privater Server unsichtbar.
    hub_show_my_servers(ctx);
}

/// Anzeigename und Bio bearbeiten.
pub fn hub_edit_profile(ctx: &Ctx) {
    if config::load_config().hub.is_none() {
        notify(ctx, "Bitte zuerst im Server-Hub anmelden.", "Server-Hub");
        return;
    }
    // Aktuelles Profil laden (kurzer blockierender Aufruf) und vorausfüllen.
    let prof = match fresh_access_token().and_then(|t| crate::hub::get_my_profile(&t)) {
        Ok(p) => p,
        Err(e) => {
            notify(ctx, &format!("Profil konnte nicht geladen werden: {}", e), "Server-Hub");
            return;
        }
    };
    let Some((vals, _)) = form_dialog(
        ctx,
        "Profil bearbeiten",
        &[("Anzeigename:", prof.display_name, false), ("Über mich (Bio):", prof.bio, false)],
        None,
    ) else { return };
    let display = vals[0].clone();
    let bio = vals[1].clone();
    ctx.ui.append_hub_log("Speichere Profil…");
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || {
            let access = fresh_access_token()?;
            crate::hub::update_profile(&access, &display, &bio)
        })
        .await;
        let m = match r {
            Ok(Ok(())) => "Profil gespeichert.".to_string(),
            Ok(Err(e)) => format!("Profil konnte nicht gespeichert werden: {}", e),
            Err(e) => format!("Fehler: {}", e),
        };
        hub_msg(&ev_tx, m);
    });
}

pub fn hub_reset_confirm(ctx: &Ctx) {
    let phone = ctx.ui.hub_reset_phone_in.get_value().trim().to_string();
    let code = ctx.ui.hub_reset_code_in.get_value().trim().to_string();
    let new_pass = ctx.ui.hub_reset_pass_in.get_value();
    if phone.is_empty() || code.is_empty() || new_pass.is_empty() {
        notify(ctx, "Telefon, Code und neues Passwort sind erforderlich.", "Server-Hub");
        return;
    }
    ctx.ui.append_hub_log("Setze neues Passwort…");
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || crate::hub::reset_confirm(&phone, &code, &new_pass)).await;
        let m = match r {
            Ok(Ok(())) => "Passwort geändert. Bitte neu anmelden.".to_string(),
            Ok(Err(e)) => format!("Passwort-Reset fehlgeschlagen: {}", e),
            Err(e) => format!("Fehler: {}", e),
        };
        hub_msg(&ev_tx, m);
    });
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
    if let Some(pw) = &password {
        data["password"] = serde_json::Value::String(pw.clone());
    }
    if !send_or_status(ctx, Message::new("room_join", data)) {
        return;
    }
    // Audio-Qualität wird vom Raum bestimmt: Werte aus der RoomInfo übernehmen.
    let (sr, bd, ch) = {
        let mut inner = ctx.app.inner.lock();
        inner.current_room_id = Some(room_id);
        // Passwort für eine spätere automatische Wiederverbindung merken.
        inner.current_room_password = password;
        let (mut sr, mut bd, mut ch, br) = inner
            .rooms
            .iter()
            .find(|r| r.id == room_id)
            .map(|r| (r.sample_rate, r.bit_depth, r.channels, r.bitrate))
            .unwrap_or((48000, 16, 1, 0));
        if sr <= 0 {
            sr = 48000;
        }
        if bd <= 0 {
            bd = 16;
        }
        if ch <= 0 {
            ch = 1;
        }
        inner.audio_config.sample_rate = sr as u32;
        inner.audio_config.bit_depth = bd as u8;
        inner.audio_config.channels = ch as u8;
        // Bitrate des Raums (0 = automatisch) – der Encoder übernimmt sie laufend.
        inner.audio_config.bitrate = br.max(0) as u32;
        (sr, bd, ch)
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

/// Dem in der Raumliste ausgewählten Raum beitreten (Knopf, Strg+J, Doppelklick).
pub fn join_selected(ctx: &Ctx) {
    match selected_room(ctx) {
        Some(room_id) => join_room_checked(ctx, room_id),
        None => notify(ctx, "Bitte zuerst einen Raum auswählen.", "Beitreten"),
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
            inner.current_room_password = None;
            inner.current_files.clear();
        }
        ctx.ui.append_chat(&format!("* Raum {} verlassen.", room));
        rebuild_tree(ctx);
        rebuild_files(ctx);
        refresh_status(ctx);
    }
}

const RATES: [i64; 5] = [48000, 44100, 24000, 16000, 8000];
const DEPTHS: [i64; 3] = [16, 24, 32];

/// Raum-Vorlage: typische Audio-Einstellungen für einen Anwendungsfall.
struct RoomTemplate {
    label: &'static str,
    sample_rate: i64,
    bit_depth: i64,
    channels: i64,
    /// Opus-Bitrate in kbit/s
    bitrate_kbps: i64,
}

/// Auswählbare Vorlagen. Der letzte Eintrag „Erweitert" blendet alle Felder
/// zur freien Konfiguration ein.
const TEMPLATES: [RoomTemplate; 3] = [
    // Sprachchat in guter Qualität (wie ein Discord-Sprachkanal)
    RoomTemplate { label: "Discord-Raum (Sprache)", sample_rate: 48000, bit_depth: 16, channels: 1, bitrate_kbps: 64 },
    // Schmalbandige Sprache wie ein klassisches Telefonat
    RoomTemplate { label: "Klassisches Telefonat", sample_rate: 8000, bit_depth: 16, channels: 1, bitrate_kbps: 16 },
    // Musik/Übertragung in Stereo, hohe Qualität
    RoomTemplate { label: "Radio-Übertragung (Musik)", sample_rate: 48000, bit_depth: 16, channels: 2, bitrate_kbps: 256 },
];
/// Index der „Erweitert"-Auswahl (nach allen Vorlagen).
const ADVANCED_IDX: usize = TEMPLATES.len();

/// Was der Raum-Dialog erstellt/bearbeitet.
enum RoomMode {
    Create { parent: Option<i64> },
    Edit { room_id: i64 },
}

/// Gemeinsamer Dialog zum Erstellen und Bearbeiten eines Raums.
///
/// Aufbau: Name, Passwort (gilt, sobald etwas im Feld steht), max. Nutzer und
/// ein Vorlagen-Auswahlfeld. Vorlagen setzen die raumweite Audio-Qualität auf
/// typische Werte; „Erweitert" blendet Samplerate, Bittiefe, Kanäle und Bitrate
/// zur freien Einstellung ein.
fn room_dialog(ctx: &Ctx, mode: RoomMode) {
    // Vorbelegung bei Bearbeiten
    let (title, name0, max0, sr0, bd0, ch0, br0_kbps, edit) = match &mode {
        RoomMode::Create { parent } => {
            let t = if parent.is_some() {
                "Unterraum erstellen"
            } else {
                "Raum erstellen"
            };
            (t, String::new(), 0i64, 48000i64, 16i64, 1i64, 0i64, false)
        }
        RoomMode::Edit { room_id } => {
            let inner = ctx.app.inner.lock();
            match inner.rooms.iter().find(|r| r.id == *room_id) {
                Some(r) => (
                    "Raum bearbeiten",
                    r.name.clone(),
                    r.max_users,
                    if r.sample_rate > 0 { r.sample_rate } else { 48000 },
                    if r.bit_depth > 0 { r.bit_depth } else { 16 },
                    if r.channels > 0 { r.channels } else { 1 },
                    if r.bitrate > 0 { r.bitrate / 1000 } else { 0 },
                    true,
                ),
                None => ("Raum bearbeiten", String::new(), 0, 48000, 16, 1, 0, true),
            }
        }
    };

    let dialog = Dialog::builder(&ctx.ui.frame, title).build();
    let v = BoxSizer::builder(Orientation::Vertical).build();
    // Zwei Gruppen: Allgemeines und Audio-Qualität, je ein Formularraster.
    let general = ui::form_grid();
    let audio = ui::form_grid();
    // Beschriftete Zeile; gibt das Label zurück, damit erweiterte Zeilen
    // gemeinsam mit ihrem Control aus-/eingeblendet werden können.
    fn add_row<W: WxWidget>(dialog: &Dialog, grid: &FlexGridSizer, label: &str, ctrl: &W) -> StaticText {
        let lbl = StaticText::builder(dialog).with_label(label).build();
        if ui::compact() {
            grid.add(&lbl, 0, SizerFlag::Top, 6);
        } else {
            grid.add(&lbl, 0, SizerFlag::AlignCenterVertical | SizerFlag::AlignRight, 0);
        }
        grid.add(ctrl, 1, SizerFlag::Expand, 0);
        lbl
    }

    let name_in = TextCtrl::builder(&dialog).build();
    name_in.set_value(&name0);
    ui::set_a11y_name(&name_in, "Raumname");
    add_row(&dialog, &general, "Name:", &name_in);

    // Passwort ohne Checkbox: gilt, sobald etwas drinsteht.
    let pw_in = TextCtrl::builder(&dialog)
        .with_style(TextCtrlStyle::Password)
        .build();
    ui::set_a11y_name(&pw_in, "Passwort");
    add_row(
        &dialog,
        &general,
        if edit {
            "Passwort (leer = unverändert):"
        } else {
            "Passwort (leer = keines):"
        },
        &pw_in,
    );

    let max_in = TextCtrl::builder(&dialog).build();
    max_in.set_value(&max0.to_string());
    ui::set_a11y_name(&max_in, "Maximale Nutzerzahl, 0 = unbegrenzt");
    add_row(&dialog, &general, "Max. Nutzer (0 = ∞):", &max_in);

    // Vorlagen-Auswahl
    let template_choice = Choice::builder(&dialog).build();
    for t in &TEMPLATES {
        template_choice.append(t.label);
    }
    template_choice.append("Erweitert (frei einstellbar)");
    ui::set_a11y_name(&template_choice, "Vorlage für die Audio-Qualität");
    add_row(&dialog, &audio, "Vorlage:", &template_choice);

    // Erweiterte Audio-Felder (anfangs ggf. ausgeblendet)
    let rate_choice = Choice::builder(&dialog).build();
    for r in RATES {
        rate_choice.append(&r.to_string());
    }
    rate_choice.set_selection(RATES.iter().position(|&r| r == sr0).unwrap_or(0) as u32);
    ui::set_a11y_name(&rate_choice, "Samplerate in Hertz");
    let rate_lbl = add_row(&dialog, &audio, "Samplerate (Hz):", &rate_choice);

    let depth_choice = Choice::builder(&dialog).build();
    for d in DEPTHS {
        depth_choice.append(&d.to_string());
    }
    depth_choice.set_selection(DEPTHS.iter().position(|&d| d == bd0).unwrap_or(0) as u32);
    ui::set_a11y_name(&depth_choice, "Bittiefe");
    let depth_lbl = add_row(&dialog, &audio, "Bittiefe (Bit):", &depth_choice);

    let ch_choice = Choice::builder(&dialog).build();
    ch_choice.append("Mono");
    ch_choice.append("Stereo");
    ch_choice.set_selection(if ch0 >= 2 { 1 } else { 0 });
    ui::set_a11y_name(&ch_choice, "Kanäle Mono oder Stereo");
    let ch_lbl = add_row(&dialog, &audio, "Kanäle:", &ch_choice);

    let bitrate_in = TextCtrl::builder(&dialog).build();
    bitrate_in.set_value(&br0_kbps.to_string());
    ui::set_a11y_name(&bitrate_in, "Bitrate in kbit pro Sekunde, 0 = automatisch");
    let br_lbl = add_row(&dialog, &audio, "Bitrate (kbit/s, 0 = auto):", &bitrate_in);

    let gbox = ui::group(&dialog, "Allgemein");
    gbox.add_sizer(&general, 0, SizerFlag::Expand | SizerFlag::All, 8);
    v.add_sizer(&gbox, 0, SizerFlag::Expand | SizerFlag::All, 10);
    let abox = ui::group(&dialog, "Audio-Qualität");
    abox.add_sizer(&audio, 0, SizerFlag::Expand | SizerFlag::All, 8);
    v.add_sizer(&abox, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 10);

    // Ein-/Ausblenden der erweiterten Felder und Vorbelegen aus der Vorlage.
    let toggle: std::rc::Rc<dyn Fn(usize)> = std::rc::Rc::new(move |idx: usize| {
        let adv = idx >= ADVANCED_IDX;
        for w in [&rate_lbl as &dyn WxWidget, &depth_lbl, &ch_lbl, &br_lbl] {
            w.show(adv);
        }
        rate_choice.show(adv);
        depth_choice.show(adv);
        ch_choice.show(adv);
        bitrate_in.show(adv);
        if !adv {
            // Felder aus der Vorlage setzen (auf „Speichern" daraus gelesen).
            let t = &TEMPLATES[idx];
            rate_choice.set_selection(RATES.iter().position(|&r| r == t.sample_rate).unwrap_or(0) as u32);
            depth_choice.set_selection(DEPTHS.iter().position(|&d| d == t.bit_depth).unwrap_or(0) as u32);
            ch_choice.set_selection(if t.channels >= 2 { 1 } else { 0 });
            bitrate_in.set_value(&t.bitrate_kbps.to_string());
        }
        dialog.layout();
        dialog.fit();
    });

    // Startauswahl: beim Bearbeiten „Erweitert" (zeigt die echten Werte),
    // beim Erstellen die erste Vorlage.
    let initial_idx = if edit { ADVANCED_IDX } else { 0 };
    template_choice.set_selection(initial_idx as u32);
    toggle(initial_idx);
    {
        let toggle = toggle.clone();
        template_choice.on_selection_changed(move |_| {
            toggle(template_choice.get_selection().unwrap_or(0) as usize);
        });
    }

    v.add_sizer(&ui::dialog_buttons(&dialog, "Speichern"), 0, SizerFlag::AlignRight | SizerFlag::Right | SizerFlag::Bottom, 6);

    dialog.set_sizer(v, true);
    dialog.fit();

    let result = dialog.show_modal();
    if result != ID_OK {
        dialog.destroy();
        return;
    }

    let name = name_in.get_value().trim().to_string();
    let max_users: i64 = max_in.get_value().trim().parse().unwrap_or(0);
    // Audio-Werte immer aus den (ggf. von der Vorlage befüllten) Feldern lesen.
    let sample_rate = RATES[rate_choice.get_selection().unwrap_or(0) as usize];
    let bit_depth = DEPTHS[depth_choice.get_selection().unwrap_or(0) as usize];
    let channels = if ch_choice.get_selection().unwrap_or(0) == 1 { 2 } else { 1 };
    let bitrate_kbps: i64 = bitrate_in.get_value().trim().parse().unwrap_or(0);
    let bitrate = bitrate_kbps.max(0) * 1000;
    let pw_value = pw_in.get_value();
    dialog.destroy();

    if name.is_empty() {
        notify(ctx, "Raumname darf nicht leer sein.", "Raum");
        return;
    }

    match mode {
        RoomMode::Create { parent } => {
            let mut data = serde_json::json!({
                "name": name,
                "max_users": max_users,
                "sample_rate": sample_rate,
                "bit_depth": bit_depth,
                "channels": channels,
                "bitrate": bitrate,
            });
            if let Some(pid) = parent {
                data["parent_id"] = serde_json::json!(pid);
            }
            // Passwort gilt, sobald etwas im Feld steht.
            if !pw_value.is_empty() {
                data["password"] = serde_json::Value::String(pw_value);
            }
            send_or_status(ctx, Message::new("room_create", data));
        }
        RoomMode::Edit { room_id } => {
            let mut data = serde_json::json!({
                "room_id": room_id,
                "name": name,
                "max_users": max_users,
                "sample_rate": sample_rate,
                "bit_depth": bit_depth,
                "channels": channels,
                "bitrate": bitrate,
            });
            // Leeres Feld = Passwort unverändert lassen; sonst neu setzen.
            if !pw_value.is_empty() {
                data["password"] = serde_json::Value::String(pw_value);
            }
            send_or_status(ctx, Message::new("room_update", data));
        }
    }
}

/// Raum an der aktuellen Cursor-Position im Baum erstellen:
///   - kein Raum ausgewählt (Lobby) → Raum auf oberster Ebene,
///   - ein Top-Raum ausgewählt → Unterraum darin,
///   - ein Unterraum ausgewählt → abgelehnt (nicht tiefer als eine Ebene
///     unter der Lobby).
fn create_room_at_cursor(ctx: &Ctx) {
    match selected_room(ctx) {
        None => room_dialog(ctx, RoomMode::Create { parent: None }),
        Some(room_id) => {
            let parent_is_subroom = ctx
                .app
                .inner
                .lock()
                .rooms
                .iter()
                .find(|r| r.id == room_id)
                .map(|r| r.parent_id.is_some())
                .unwrap_or(false);
            if parent_is_subroom {
                notify(
                    ctx,
                    "Hier nicht möglich: Räume dürfen nur eine Ebene unter der Lobby liegen. Unter einem Unterraum lässt sich kein weiterer Raum erstellen.",
                    "Raum erstellen",
                );
                return;
            }
            room_dialog(ctx, RoomMode::Create { parent: Some(room_id) });
        }
    }
}

fn edit_selected_room(ctx: &Ctx) {
    match selected_room(ctx) {
        Some(room_id) => room_dialog(ctx, RoomMode::Edit { room_id }),
        None => notify(ctx, "Bitte zuerst einen Raum auswählen.", "Raum bearbeiten"),
    }
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

/// Häkchen eines Check-Menüpunkts setzen (zeigt An/Aus-Zustand an).
/// Häkchen im Audio-Menü setzen — und den passenden Schnellschalter der
/// Hauptansicht mitziehen, damit beide immer denselben Zustand zeigen.
pub fn set_menu_check(ctx: &Ctx, id: i32, checked: bool) {
    if let Some(mb) = ctx.ui.frame.get_menu_bar() {
        mb.check_item(id, checked);
    }
    match id {
        ui::ID_TOGGLE_MUTE => ctx.ui.mute_btn.set_value(checked),
        ui::ID_TOGGLE_DEAFEN => ctx.ui.deafen_btn.set_value(checked),
        _ => {}
    }
}

fn toggle_mute(ctx: &Ctx) {
    let muted = {
        let mut inner = ctx.app.inner.lock();
        inner.muted = !inner.muted;
        inner.muted
    };
    let _ = ctx
        .app
        .send_ws(Message::new("audio_mute", serde_json::json!({ "muted": muted })));
    set_menu_check(ctx, ui::ID_TOGGLE_MUTE, muted);
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
    set_menu_check(ctx, ui::ID_TOGGLE_DEAFEN, deafened);
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
    // Loopback ist ein rein lokaler Mithör-Monitor: die Aufnahme-Schleife
    // (capture.rs) speist das eigene Mikrofon direkt in den Empfangs-Mischer,
    // ohne Umweg über den Server. Der frühere Server-Reflection-Weg
    // („audio_loopback") wird nicht mehr genutzt — er hatte Latenz und
    // Rückkopplungsverstärkung. Sicherheitshalber dem Server mitteilen, dass
    // serverseitiges Loopback aus bleibt (kein doppeltes Echo).
    let _ = ctx.app.send_ws(Message::new(
        "audio_loopback",
        serde_json::json!({ "enabled": false }),
    ));
    set_menu_check(ctx, ui::ID_TOGGLE_LOOPBACK, loopback);
    ctx.ui.append_chat(if loopback {
        "* Loopback (Mithören) eingeschaltet."
    } else {
        "* Loopback (Mithören) ausgeschaltet."
    });
    refresh_status(ctx);
}

pub fn volume_changed(ctx: &Ctx) {
    let v = ctx.ui.volume.get_value();
    ctx.app.set_volume(v as f32 / 100.0);
    #[cfg(any(target_os = "ios", target_os = "android"))]
    save_volumes(ctx);
}

pub fn mic_volume_changed(ctx: &Ctx) {
    let v = ctx.ui.mic_volume.get_value();
    ctx.app.set_mic_volume(v as f32 / 100.0);
    #[cfg(any(target_os = "ios", target_os = "android"))]
    save_volumes(ctx);
}

/// iOS/Android: Apps werden vom System beendet, ohne dass `main` nach der
/// Eventloop weiterläuft — Lautstärken daher sofort sichern statt beim Beenden.
#[cfg(any(target_os = "ios", target_os = "android"))]
fn save_volumes(ctx: &Ctx) {
    let mut cfg = config::load_config();
    cfg.volume = ctx.app.volume();
    cfg.mic_volume = ctx.app.mic_volume();
    let _ = config::save_config(&cfg);
}

/// Dialog zur Auswahl von Mikrofon und Lautsprecher. Die Audio-Qualität
/// (Samplerate, Mono/Stereo) bestimmt dagegen der Raum (siehe Raum-Dialog).
/// Audio-Pipeline mit den aktuell gewählten Geräten neu starten — ohne
/// Reconnect. Stoppt Aufnahme/Wiedergabe und startet sie über dieselben Pfade
/// wie beim Verbinden neu (Geräte aus dem State). Nur sinnvoll, wenn verbunden.
fn restart_audio(ctx: &Ctx) {
    if !ctx.app.inner.lock().connected {
        return;
    }
    // Aufnahme: alten Sende-Task UND den alten cpal-Stream stoppen (C1), dann mit
    // neuem Mikrofon neu starten.
    {
        let mut inner = ctx.app.inner.lock();
        if let Some(tx) = inner.capture_shutdown.take() {
            let _ = tx.send(true);
        }
        if let Some(tx) = inner.capture_stream_stop.take() {
            let _ = tx.send(());
        }
        inner.capturing = false;
    }
    let cap_app = ctx.app.clone();
    let input_device = ctx.app.inner.lock().input_device.clone();
    let cap_rt = ctx.rt.clone();
    ctx.rt.spawn(async move {
        match crate::audio::capture::start_capture_owned(cap_app.clone(), input_device, cap_rt) {
            Ok((shutdown_tx, stop_tx)) => {
                let mut inner = cap_app.inner.lock();
                inner.capturing = true;
                inner.capture_shutdown = Some(shutdown_tx);
                inner.capture_stream_stop = Some(stop_tx);
            }
            Err(e) => tracing::error!("Mikrofon-Neustart fehlgeschlagen: {}", e),
        }
    });
    // Wiedergabe: alten Stream sauber stoppen (C4), dann neuen starten.
    {
        let mut inner = ctx.app.inner.lock();
        if let Some(tx) = inner.playback_stream_stop.take() {
            let _ = tx.send(());
        }
    }
    let output_device = ctx.app.inner.lock().output_device.clone();
    let playback_state = ctx.app.clone();
    let (stop_tx, stop_rx) = std::sync::mpsc::channel::<()>();
    ctx.app.inner.lock().playback_stream_stop = Some(stop_tx);
    std::thread::spawn(move || {
        match crate::audio::playback::start_playback(playback_state, output_device) {
            Ok(_stream) => {
                let _ = stop_rx.recv();
            }
            Err(e) => tracing::error!("Wiedergabe-Neustart fehlgeschlagen: {}", e),
        }
    });
}

/// Mikrofone zur Auswahl: (gespeicherter Wert, Anzeigename); der erste Eintrag
/// ist immer das Standardgerät (None). iOS: Eingänge der Audio-Session
/// (eingebautes Mikrofon unten/vorne/hinten, Headset, Bluetooth, USB) — cpal
/// kennt dort nur „Default Device". Sonst die Geräte von cpal.
fn input_choices() -> Vec<(Option<String>, String)> {
    let mut out = vec![(None, "Standardgerät".to_string())];
    #[cfg(target_os = "ios")]
    {
        for i in wxdragon::audio_session::inputs() {
            out.push((Some(i.id), i.name));
        }
        return out;
    }
    #[allow(unreachable_code)]
    {
        for d in crate::audio::device::list_devices() {
            if d.is_input {
                out.push((Some(d.name.clone()), d.name));
            }
        }
        out
    }
}

/// Lautsprecher zur Auswahl (wie [`input_choices`]).
fn output_choices() -> Vec<(Option<String>, String)> {
    let mut out = vec![(None, "Standardgerät".to_string())];
    for d in crate::audio::device::list_devices() {
        if d.is_output {
            out.push((Some(d.name.clone()), d.name));
        }
    }
    out
}

/// iOS: das gewählte Mikrofon der Audio-Session vorgeben (cpal nimmt dann das
/// Standardgerät, und das ist der bevorzugte Eingang). Anderswo nichts zu tun.
#[cfg(any(target_os = "ios", target_os = "android"))]
fn apply_mobile_input(input: Option<&str>) {
    #[cfg(target_os = "ios")]
    if let Err(e) = wxdragon::audio_session::set_preferred_input(input) {
        tracing::warn!("Mikrofon konnte nicht gewählt werden: {e}");
    }
    #[cfg(not(target_os = "ios"))]
    let _ = input;
}

/// Eine Geräteauswahl mit Einträgen füllen und den gespeicherten Wert wählen.
fn fill_device_choice(choice: &Choice, list: &[(Option<String>, String)], current: &Option<String>) {
    choice.clear();
    for (_, name) in list {
        choice.append(name);
    }
    let idx = list.iter().position(|(v, _)| v == current).unwrap_or(0);
    choice.set_selection(idx as u32);
}

/// Dialog „Audio-Einstellungen": Geräte, Sprachverarbeitung (Rausch- und
/// Echounterdrückung, Boost) und Ansagen. Die Audio-Qualität (Samplerate,
/// Mono/Stereo) bestimmt dagegen der Raum (siehe Raum-Dialog).
fn audio_settings(ctx: &Ctx) {
    let (cur_in, cur_out) = {
        let inner = ctx.app.inner.lock();
        (inner.input_device.clone(), inner.output_device.clone())
    };
    let lists = std::rc::Rc::new(std::cell::RefCell::new((input_choices(), output_choices())));

    let dialog = Dialog::builder(&ctx.ui.frame, "Audio-Einstellungen").build();
    let v = BoxSizer::builder(Orientation::Vertical).build();

    // ── Geräte ──
    let dev_box = StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, &dialog, "Geräte").build();
    let grid = FlexGridSizer::builder(0, 2).with_vgap(8).with_hgap(12).build();
    grid.add_growable_col(1, 1);
    let in_choice = Choice::builder(&dialog).build();
    let out_choice = Choice::builder(&dialog).build();
    fill_device_choice(&in_choice, &lists.borrow().0, &cur_in);
    fill_device_choice(&out_choice, &lists.borrow().1, &cur_out);
    ui::set_a11y_name(&in_choice, "Mikrofon");
    ui::set_a11y_name(&out_choice, "Lautsprecher");
    for (label, choice) in [("Mikrofon:", &in_choice), ("Lautsprecher:", &out_choice)] {
        grid.add(
            &StaticText::builder(&dialog).with_label(label).build(),
            0,
            SizerFlag::AlignCenterVertical,
            0,
        );
        grid.add(choice, 1, SizerFlag::Expand, 0);
    }
    dev_box.add_sizer(&grid, 0, SizerFlag::Expand | SizerFlag::All, 8);
    // „Aktualisieren" füllt die Auswahl neu (z. B. nach Ein-/Ausstöpseln) und
    // behält die aktuelle Wahl, falls das Gerät noch da ist.
    let refresh_btn = Button::builder(&dialog).with_label("Geräte aktualisieren").build();
    ui::set_a11y_name(&refresh_btn, "Geräteliste aktualisieren");
    {
        let lists = lists.clone();
        refresh_btn.on_click(move |_| {
            let pick = |sel: Option<u32>, list: &[(Option<String>, String)]| {
                sel.and_then(|i| list.get(i as usize)).and_then(|(v, _)| v.clone())
            };
            let (sel_in, sel_out) = {
                let g = lists.borrow();
                (pick(in_choice.get_selection(), &g.0), pick(out_choice.get_selection(), &g.1))
            };
            let fresh = (input_choices(), output_choices());
            fill_device_choice(&in_choice, &fresh.0, &sel_in);
            fill_device_choice(&out_choice, &fresh.1, &sel_out);
            *lists.borrow_mut() = fresh;
        });
    }
    dev_box.add(&refresh_btn, 0, SizerFlag::Left | SizerFlag::Bottom | SizerFlag::Right, 8);
    v.add_sizer(&dev_box, 0, SizerFlag::Expand | SizerFlag::All, 10);

    // ── Sprachverarbeitung ──
    let voice = ctx.app.voice_settings();
    let proc_box =
        StaticBoxSizerBuilder::new_with_label(Orientation::Vertical, &dialog, "Sprachverarbeitung").build();
    let prow = BoxSizer::builder(Orientation::Horizontal).build();
    let noise_choice = Choice::builder(&dialog).build();
    for l in crate::audio::voice::NOISE_LEVELS {
        noise_choice.append(l);
    }
    noise_choice.set_selection(voice.noise as u32);
    ui::set_a11y_name(&noise_choice, "Rauschunterdrückung");
    prow.add(
        &StaticText::builder(&dialog).with_label("Rauschunterdrückung:").build(),
        0,
        SizerFlag::AlignCenterVertical | SizerFlag::Right,
        12,
    );
    prow.add(&noise_choice, 1, SizerFlag::Expand, 0);
    proc_box.add_sizer(&prow, 0, SizerFlag::Expand | SizerFlag::All, 8);
    let echo_chk = CheckBox::builder(&dialog)
        .with_label("Echounterdrückung (wenn du ohne Kopfhörer sprichst)")
        .build();
    echo_chk.set_value(voice.echo);
    ui::set_a11y_name(&echo_chk, "Echounterdrückung");
    proc_box.add(&echo_chk, 0, SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 8);
    let boost_chk = CheckBox::builder(&dialog)
        .with_label("Mikrofon-Boost (doppelte Lautstärke)")
        .build();
    boost_chk.set_value(ctx.app.mic_boost());
    ui::set_a11y_name(&boost_chk, "Mikrofon-Boost, verdoppelt die Mikrofon-Lautstärke");
    proc_box.add(&boost_chk, 0, SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 8);
    v.add_sizer(&proc_box, 0, SizerFlag::Expand | SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 10);

    // ── Ansagen ──
    let announce_chk = CheckBox::builder(&dialog)
        .with_label("Server-Ereignisse per Sprachausgabe ansagen")
        .build();
    announce_chk.set_value(ctx.st.borrow().announce_events);
    ui::set_a11y_name(&announce_chk, "Server-Ereignisse per Sprachausgabe ansagen");
    v.add(&announce_chk, 0, SizerFlag::Left | SizerFlag::Right | SizerFlag::Bottom, 14);

    v.add_sizer(&ui::dialog_buttons(&dialog, "Speichern"), 0, SizerFlag::AlignRight | SizerFlag::Right | SizerFlag::Bottom, 6);

    dialog.set_sizer(v, true);
    dialog.fit();

    let result = dialog.show_modal();
    if result != ID_OK {
        dialog.destroy();
        return;
    }
    // Auswahl gegen die AKTUELLEN Listen auflösen (Index 0 = Standardgerät).
    let (input, output) = {
        let g = lists.borrow();
        let pick = |sel: Option<u32>, list: &[(Option<String>, String)]| {
            sel.and_then(|i| list.get(i as usize)).and_then(|(v, _)| v.clone())
        };
        (pick(in_choice.get_selection(), &g.0), pick(out_choice.get_selection(), &g.1))
    };
    let devices_changed = input != cur_in || output != cur_out;
    let announce_events = announce_chk.is_checked();
    let mic_boost = boost_chk.is_checked();
    let noise = noise_choice.get_selection().unwrap_or(2).min(4) as u8;
    let echo = echo_chk.is_checked();
    dialog.destroy();

    // Sprachverarbeitung und Boost wirken sofort (ohne Neustart des Mikrofons).
    ctx.app.set_mic_boost(mic_boost);
    ctx.app.set_voice_settings(noise, echo);
    {
        let mut inner = ctx.app.inner.lock();
        inner.input_device = input.clone();
        inner.output_device = output.clone();
    }
    ctx.st.borrow_mut().announce_events = announce_events;
    let mut cfg = config::load_config();
    cfg.input_device = input.clone();
    cfg.output_device = output;
    cfg.announce_events = announce_events;
    cfg.mic_boost = mic_boost;
    cfg.noise_suppression = noise;
    cfg.echo_cancellation = echo;
    let _ = config::save_config(&cfg);

    let connected = ctx.app.inner.lock().connected;
    if devices_changed && connected {
        #[cfg(any(target_os = "ios", target_os = "android"))]
        apply_mobile_input(input.as_deref());
        restart_audio(ctx);
        notify(ctx, "Audio-Einstellungen gespeichert, Geräte übernommen.", "Audio-Einstellungen");
    } else if devices_changed {
        notify(ctx, "Audio-Einstellungen gespeichert. Die Geräte gelten ab der nächsten Verbindung.", "Audio-Einstellungen");
    } else {
        notify(ctx, "Audio-Einstellungen gespeichert.", "Audio-Einstellungen");
    }
}

pub fn save_volume(ctx: &Ctx) {
    let mut cfg = config::load_config();
    cfg.volume = ctx.app.volume();
    cfg.mic_volume = ctx.app.mic_volume();
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
    set_menu_check(ctx, ui::ID_PAUSE_STREAM, false);
    // Stream-Lautstärke für den neuen Stream auf normal (100 %) zurücksetzen.
    ctx.app.set_stream_volume(1.0);

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    {
        let mut inner = ctx.app.inner.lock();
        inner.stream_shutdown = Some(shutdown_tx);
        inner.streaming_file = true;
    }
    // Kein Loopback mehr nötig: Das Datei-Audio wird clientseitig in den
    // Mikrofon-Sendestrom gemischt (andere hören Mikro+Datei) und lokal
    // abgespielt (der Streamer hört die Datei ohne Eigen-Echo).

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
    set_menu_check(ctx, ui::ID_PAUSE_STREAM, false);
    if !was {
        status(ctx, "Es läuft kein Streaming.");
    }
}

/// Gestreamte Datei pausieren bzw. fortsetzen (lokal, kein Server-Roundtrip).
fn toggle_pause_stream(ctx: &Ctx) {
    let streaming = ctx.app.inner.lock().stream_shutdown.is_some();
    if !streaming {
        set_menu_check(ctx, ui::ID_PAUSE_STREAM, false);
        status(ctx, "Es läuft kein Streaming.");
        return;
    }
    use std::sync::atomic::Ordering;
    let paused = !ctx.app.stream_paused.load(Ordering::Relaxed);
    ctx.app.stream_paused.store(paused, Ordering::Relaxed);
    set_menu_check(ctx, ui::ID_PAUSE_STREAM, paused);
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
    // Kapazität nur vorreservieren, nicht blind der Server-Angabe vertrauen: eine
    // bösartige/fehlerhafte size_bytes (untrusted i64) würde Vec::with_capacity zu
    // einer riesigen Allokation zwingen (Capacity-Overflow → Prozess-Abort). Auf
    // einen moderaten Deckel begrenzen; der Vec wächst bei Bedarf trotzdem mit.
    const PREALLOC_CAP: i64 = 64 * 1024 * 1024;
    let prealloc = file.size_bytes.clamp(0, PREALLOC_CAP) as usize;
    ctx.app.inner.lock().download_targets.insert(
        file.id,
        (
            std::path::PathBuf::from(path),
            Vec::with_capacity(prealloc),
        ),
    );
    if send_or_status(
        ctx,
        Message::new("file_download", serde_json::json!({ "file_id": file.id })),
    ) {
        ctx.ui.append_chat(&format!("Lade {} herunter…", file.filename));
    }
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

/// Aktuell im Konten-Dialog ausgewähltes Konto (Benutzername, Rolle).
fn selected_account(ctx: &Ctx) -> Option<(String, String)> {
    let st = ctx.st.borrow();
    let ad = st.account_dialog.as_ref()?;
    let idx = ad.list.get_selection()? as usize;
    ad.accounts.get(idx).cloned()
}

/// Zeigt anhand der eigenen Rolle den Menüpunkt „Benutzerkonten verwalten"
/// (nur für Admins) und merkt sich den Status. Für Nicht-Admins wird er
/// vollständig aus dem Menü entfernt.
pub fn update_account_menu(ctx: &Ctx) {
    let admin = ctx.app.inner.lock().is_self_admin();
    ctx.st.borrow_mut().is_admin = admin;
    let Some(mb) = ctx.ui.frame.get_menu_bar() else {
        return;
    };
    let present = mb.find_item(ui::ID_ACCOUNTS).is_some();
    if admin && !present {
        // Über ein immer vorhandenes Verwaltung-Item das richtige Menü finden.
        if let Some((_, menu)) = mb.find_item_and_menu(ui::ID_CHANGE_PW) {
            menu.insert(
                0,
                ui::ID_ACCOUNTS,
                "Benutzerkonten &verwalten…",
                "Konten anzeigen, Rollen ändern, Passwörter zurücksetzen, löschen",
                ItemKind::Normal,
            );
        }
    } else if !admin && present {
        if let Some((_, menu)) = mb.find_item_and_menu(ui::ID_ACCOUNTS) {
            menu.delete(ui::ID_ACCOUNTS);
        }
    }
}

/// Konten-Verwaltung (nur Admins): ein Dialog mit Kontenliste und Aktionen.
/// Die Liste aktualisiert sich live aus den Server-Antworten, weil der
/// UI-Timer auch während des modalen Dialogs weiterläuft.
fn manage_accounts(ctx: &Ctx) {
    let dialog = Dialog::builder(&ctx.ui.frame, "Benutzerkonten verwalten").build();
    let v = BoxSizer::builder(Orientation::Vertical).build();

    v.add(
        &StaticText::builder(&dialog)
            .with_label("Konten (Benutzername [Rolle]):")
            .build(),
        0,
        SizerFlag::All,
        6,
    );
    let list = ListBox::builder(&dialog).build();
    ui::set_a11y_name(&list, "Benutzerkonten");
    v.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 6);

    let btn_new = Button::builder(&dialog).with_label("Konto anlegen…").build();
    let btn_pw = Button::builder(&dialog)
        .with_label("Passwort zurücksetzen…")
        .build();
    let btn_role = Button::builder(&dialog).with_label("Rolle ändern…").build();
    let btn_del = Button::builder(&dialog).with_label("Konto löschen…").build();
    let btn_refresh = Button::builder(&dialog).with_label("Aktualisieren").build();
    #[cfg(any(target_os = "ios", target_os = "android"))]
    btn_del.set_destructive(true);
    v.add_sizer(
        &ui::button_row(&[&btn_new, &btn_pw, &btn_role, &btn_del, &btn_refresh]),
        0,
        SizerFlag::Expand | SizerFlag::All,
        4,
    );

    let reg_chk = CheckBox::builder(&dialog)
        .with_label("Selbstregistrierung erlauben")
        .build();
    reg_chk.set_value(ctx.st.borrow().registration_open);
    ui::set_a11y_name(&reg_chk, "Selbstregistrierung erlauben");
    v.add(&reg_chk, 0, SizerFlag::All, 6);

    let close = Button::builder(&dialog).with_label("Schließen").build();
    {
        let d = dialog;
        close.on_click(move |_| d.end_modal(ID_OK));
    }
    v.add(&close, 0, SizerFlag::AlignRight | SizerFlag::All, 6);

    // Live-Referenz hinterlegen, damit account_list_result die Liste füllt.
    ctx.st.borrow_mut().account_dialog = Some(crate::app::AccountDialogRef {
        list,
        reg_chk,
        accounts: Vec::new(),
    });

    {
        let ctx = ctx.clone();
        btn_new.on_click(move |_| account_create_new(&ctx));
    }
    {
        let ctx = ctx.clone();
        btn_pw.on_click(move |_| {
            let Some((name, _)) = selected_account(&ctx) else {
                notify(&ctx, "Bitte zuerst ein Konto auswählen.", "Passwort zurücksetzen");
                return;
            };
            if let Some(pw) = ask_secret(&ctx, &format!("Neues Passwort für „{}“:", name), "Passwort zurücksetzen") {
                send_or_status(
                    &ctx,
                    Message::new("account_set_password", serde_json::json!({ "username": name, "password": pw })),
                );
            }
        });
    }
    {
        let ctx = ctx.clone();
        btn_role.on_click(move |_| {
            let Some((name, role)) = selected_account(&ctx) else {
                notify(&ctx, "Bitte zuerst ein Konto auswählen.", "Rolle ändern");
                return;
            };
            let new_role = if role == "admin" { "user" } else { "admin" };
            let q = MessageDialog::builder(
                &ctx.ui.frame,
                &format!("Rolle von „{}“ auf „{}“ ändern?", name, new_role),
                "Rolle ändern",
            )
            .with_style(MessageDialogStyle::YesNo)
            .build();
            if q.show_modal() == ID_YES {
                send_or_status(
                    &ctx,
                    Message::new("account_set_role", serde_json::json!({ "username": name, "role": new_role })),
                );
            }
        });
    }
    {
        let ctx = ctx.clone();
        btn_del.on_click(move |_| {
            let Some((name, _)) = selected_account(&ctx) else {
                notify(&ctx, "Bitte zuerst ein Konto auswählen.", "Konto löschen");
                return;
            };
            let q = MessageDialog::builder(
                &ctx.ui.frame,
                &format!("Konto „{}“ wirklich löschen?", name),
                "Konto löschen",
            )
            .with_style(MessageDialogStyle::YesNo)
            .build();
            if q.show_modal() == ID_YES {
                send_or_status(
                    &ctx,
                    Message::new("account_delete", serde_json::json!({ "username": name })),
                );
            }
        });
    }
    {
        let ctx = ctx.clone();
        btn_refresh.on_click(move |_| {
            let _ = ctx.app.send_ws(Message::new("account_list", serde_json::json!({})));
        });
    }
    {
        let ctx = ctx.clone();
        reg_chk.on_toggled(move |_| {
            let open = reg_chk.is_checked();
            send_or_status(
                &ctx,
                Message::new("account_set_registration", serde_json::json!({ "open": open })),
            );
        });
    }

    dialog.set_sizer(v, true);
    dialog.fit();

    // Liste initial anfordern.
    let _ = ctx.app.send_ws(Message::new("account_list", serde_json::json!({})));

    let _ = dialog.show_modal();
    ctx.st.borrow_mut().account_dialog = None;
    dialog.destroy();
}

/// „Konto anlegen" aus dem Konten-Dialog.
fn account_create_new(ctx: &Ctx) {
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
Strg+Umschalt+P  Privatnachricht an ausgewählten Nutzer\n\
Strg+Q  Beenden\n\n\
Audio-Kurztasten:\n\
Strg+Umschalt+Pfeil hoch/runter  Eigenen Stream lauter/leiser (für alle)\n\
Strg+Umschalt+Pfeil rechts/links  Stream 10 s vor-/zurückspulen\n\
Strg+Pfeil hoch/runter  Ausgewählten Nutzer lokal lauter/leiser\n\n\
Im Baum: Pfeil rechts/links klappt auf/zu, Enter tritt einem Raum bei.";

// ── Audio-Kurztasten (Pfeiltasten) ──

// wxWidgets-Keycodes für die Pfeiltasten (in wxdragon nicht als Konstanten
// exportiert; die Werte sind seit jeher stabil).
const WXK_LEFT: i32 = 314;
const WXK_UP: i32 = 315;
const WXK_RIGHT: i32 = 316;
const WXK_DOWN: i32 = 317;

/// Schrittweite der Lautstärke-Kurztasten in Prozentpunkten.
const VOL_STEP: i32 = 10;
/// Schrittweite des Spulens in Sekunden.
const SEEK_STEP: i32 = 10;

/// Lautstärke des eigenen Datei-Streams ändern — gilt für ALLE Hörer
/// (skaliert das gesendete Datei-Audio) und das lokale Mithören.
fn stream_volume_step(ctx: &Ctx, delta_percent: i32) {
    if !ctx.app.inner.lock().streaming_file {
        announce(ctx, "Es läuft kein Streaming.", true);
        status(ctx, "Es läuft kein Streaming.");
        return;
    }
    let cur = (ctx.app.stream_volume() * 100.0).round() as i32;
    let next = (cur + delta_percent).clamp(0, 200);
    ctx.app.set_stream_volume(next as f32 / 100.0);
    let msg = format!("Stream-Lautstärke {} Prozent", next);
    announce(ctx, &msg, true);
    status(ctx, &msg);
}

/// Im laufenden Datei-Stream vor- oder zurückspulen (Sekunden).
fn stream_seek(ctx: &Ctx, secs: i32) {
    if !ctx.app.inner.lock().streaming_file {
        announce(ctx, "Es läuft kein Streaming.", true);
        status(ctx, "Es läuft kein Streaming.");
        return;
    }
    ctx.app.request_stream_seek(secs);
    let msg = if secs >= 0 {
        format!("{} Sekunden vor", secs)
    } else {
        format!("{} Sekunden zurück", -secs)
    };
    announce(ctx, &msg, true);
    status(ctx, &msg);
}

/// Lokale (nur für mich geltende) Lautstärke des im Baum fokussierten Nutzers
/// ändern.
fn user_volume_step(ctx: &Ctx, delta_percent: i32) {
    let Some(user_id) = selected_user(ctx) else {
        announce(ctx, "Kein Nutzer ausgewählt.", true);
        status(ctx, "Bitte zuerst einen Nutzer im Baum auswählen.");
        return;
    };
    let (nick, pct) = {
        let mut inner = ctx.app.inner.lock();
        let cur = (inner.user_volume(user_id) * 100.0).round() as i32;
        let next = (cur + delta_percent).clamp(0, 200);
        inner.user_volumes.insert(user_id, next as f32 / 100.0);
        (inner.nickname_of(user_id), next)
    };
    let msg = format!("Lautstärke {}: {} Prozent", nick, pct);
    announce(ctx, &msg, true);
    status(ctx, &msg);
}

/// Dispatcher für die Audio-Kurztasten (Pfeiltasten mit Strg bzw. Cmd auf macOS).
/// Liefert `true`, wenn die Taste verarbeitet wurde (dann nicht weiterreichen).
/// `cmd` entspricht Strg unter Windows/Linux und Cmd unter macOS.
pub fn on_hotkey(ctx: &Ctx, key: i32, cmd: bool, shift: bool) -> bool {
    if !cmd {
        return false;
    }
    match (shift, key) {
        // Strg/Cmd+Umschalt+Pfeil: eigenen Stream lauter/leiser bzw. vor/zurück.
        (true, WXK_UP) => stream_volume_step(ctx, VOL_STEP),
        (true, WXK_DOWN) => stream_volume_step(ctx, -VOL_STEP),
        (true, WXK_RIGHT) => stream_seek(ctx, SEEK_STEP),
        (true, WXK_LEFT) => stream_seek(ctx, -SEEK_STEP),
        // Strg/Cmd+Pfeil hoch/runter: fokussierten Nutzer lokal lauter/leiser.
        (false, WXK_UP) => user_volume_step(ctx, VOL_STEP),
        (false, WXK_DOWN) => user_volume_step(ctx, -VOL_STEP),
        _ => return false,
    }
    true
}

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
        ui::ID_AUDIO_SETTINGS => audio_settings(ctx),
        ui::ID_STREAM_FILE => stream_file(ctx),
        ui::ID_PAUSE_STREAM => toggle_pause_stream(ctx),
        ui::ID_STOP_STREAM => stop_stream(ctx),
        ui::ID_JOIN_ROOM => join_selected(ctx),
        ui::ID_LEAVE_ROOM => leave_room(ctx),
        ui::ID_CREATE_ROOM => create_room_at_cursor(ctx),
        ui::ID_EDIT_ROOM => edit_selected_room(ctx),
        ui::ID_DELETE_ROOM => delete_room(ctx),
        ui::ID_UPLOAD => upload_file(ctx),
        ui::ID_DOWNLOAD => download_file(ctx),
        ui::ID_PM => private_message(ctx),
        ui::ID_KICK => kick_user(ctx),
        ui::ID_BAN => ban_user(ctx),
        ui::ID_MOVE_USER => move_user(ctx),
        ui::ID_ADMIN_MUTE => admin_mute(ctx, true),
        ui::ID_ADMIN_UNMUTE => admin_mute(ctx, false),
        ui::ID_SERVER_MSG => server_message(ctx),
        ui::ID_ACCOUNTS => manage_accounts(ctx),
        ui::ID_CHANGE_PW => change_password(ctx),
        ui::ID_CHECK_UPDATE => crate::update::check_for_update(ctx, true),
        ui::ID_HELP_KEYS => info_box(ctx, HELP_TEXT, "Kurztasten"),
        ID_ABOUT => info_box(
            ctx,
            "TeamConference-Client\nNative Oberfläche mit wxWidgets (wxDragon).",
            "Über TeamConference",
        ),
        _ => {}
    }
}
