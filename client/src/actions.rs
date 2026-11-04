//! Aktionen aus Menüs, Buttons, Tastatur und Dialogen.

use std::sync::Arc;

use wxdragon::prelude::*;

use crate::app::{AuthSpec, Ctx, Reconnect, SessionParams};
use crate::config::{self, ServerEntry};
use crate::handlers::{rebuild_files, rebuild_server_list, rebuild_tree, refresh_status};
use crate::protocol::Message;
use crate::state::{AppState, PendingUpload};
use crate::ui;

use rust_i18n::t;
use tokio::sync::mpsc::UnboundedSender;

// ── kleine Helfer ──

fn status(ctx: &Ctx, text: &str) {
    ctx.ui.set_status(text);
}

fn send_or_status(ctx: &Ctx, msg: Message) -> bool {
    match ctx.app.send_ws(msg) {
        Ok(()) => true,
        Err(e) => {
            // state.rs wird auch von lib/ genutzt und liefert daher keinen
            // übersetzten Text; ohne Verbindung die eigene Meldung zeigen.
            let text = if ctx.app.inner.lock().ws_tx.is_none() {
                t!("status.not_connected_dot").to_string()
            } else {
                e
            };
            status(ctx, &text);
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
        notify(ctx, &t!("connect.already_connected"), &t!("connect.caption"));
        return;
    }
    let host = ctx.ui.host_in.get_value().trim().to_string();
    let port: u16 = match ctx.ui.port_in.get_value().trim().parse() {
        Ok(p) => p,
        Err(_) => {
            notify(ctx, &t!("connect.invalid_port"), &t!("connect.caption"));
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
        notify(ctx, &t!("connect.host_required"), &t!("connect.caption"));
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
                    &t!("connect.central_needs_hub_login"),
                    &t!("connect.caption"),
                );
                return;
            }
        }
    } else {
        if username.is_empty() || password.is_empty() {
            notify(
                ctx,
                &t!("connect.credentials_required"),
                &t!("connect.caption"),
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

    status(ctx, &t!("connect.connecting"));

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
            if config::load_config().hub.map_or(true, |h| h.refresh_token.is_empty()) {
                fail(t!("connect.central_session_gone").into());
                return;
            }
            let res = tokio::task::spawn_blocking(crate::hubsession::access_token).await;
            match res {
                Ok(Ok(access_token)) => {
                    serde_json::json!({ "central_token": access_token, "nickname": nickname, "server_id": server_id })
                }
                Ok(Err(e)) => {
                    fail(t!("connect.central_login_failed", error = e).into());
                    return;
                }
                Err(e) => {
                    fail(t!("connect.central_login_failed", error = e).into());
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
        fail(t!("connect.failed_with", error = e).into());
        return;
    }

    // 3) UDP-Audio (nicht fatal — bei Fehler bleibt die Steuerverbindung).
    match crate::net::udp_client::start_udp_audio(&params.host, params.udp_port, app.clone()).await {
        Ok((send_shutdown, recv_shutdown)) => {
            let mut inner = app.inner.lock();
            inner.capture_shutdown = Some(send_shutdown);
            inner.playback_shutdown = Some(recv_shutdown);
        }
        Err(e) => warn(t!("connect.udp_failed", error = e).into()),
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
        warn(t!("connect.login_failed", error = e).into());
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
            .append_chat(&t!("reconnect.gave_up_chat"));
        notify(ctx, &t!("reconnect.gave_up"), &t!("reconnect.caption"));
        return;
    };

    let text = t!("reconnect.attempt", attempt = attempt, max = max).to_string();
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

pub fn do_disconnect(ctx: &Ctx) {
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
    status(ctx, &t!("status.not_connected"));
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
        status(ctx, &t!("bookmark.host_empty"));
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
    status(ctx, &t!("bookmark.saved"));
}

pub fn remove_server(ctx: &Ctx) {
    let Some(idx) = ctx.ui.server_list.get_selection() else {
        status(ctx, &t!("bookmark.select_first"));
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
    status(ctx, &t!("bookmark.removed"));
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
                if h.role == "hub_admin" {
                    t!("hub_status.logged_in_admin", name = name).to_string()
                } else {
                    t!("hub_status.logged_in", name = name).to_string()
                }
            } else {
                t!("hub_status.pending", name = name).to_string()
            }
        }
        None => t!("hub_status.logged_out").to_string(),
    };
    ctx.ui.hub_status.set_label(&label);
}

/// Die passende Hub-Seite zeigen: eingeloggt → Konto, sonst je nach Modus
/// Anmelden/Registrieren/Reset.
pub fn update_hub_view(ctx: &Ctx) {
    let logged_in = config::load_config().hub.is_some();
    let idx: usize = if logged_in {
        3
    } else {
        match ctx.st.borrow().hub_view {
            crate::app::HubView::Login => 0,
            crate::app::HubView::Register => 1,
            crate::app::HubView::Reset => 2,
        }
    };
    ctx.ui.hub_book.set_selection(idx);
    // Admin-Knöpfe nur für Hub-Admins ein-/ausblenden.
    let admin = hub_is_admin();
    ctx.ui.hub_admin_pending_btn.show(admin);
    ctx.ui.hub_admin_user_btn.show(admin);
    ctx.ui.hub_admin_servers_btn.show(admin);
    ctx.ui.hub_panel.layout();
    update_hub_status(ctx);
}

fn set_hub_view(ctx: &Ctx, view: crate::app::HubView) {
    ctx.st.borrow_mut().hub_view = view;
    update_hub_view(ctx);
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

/// Modaler Formular-Dialog: mehrere Textfelder + optionales Häkchen, alles auf
/// einmal. `fields` = (Label, Vorbelegung, Passwortfeld?). Gibt bei OK die
/// Textwerte und den Häkchen-Zustand zurück.
fn form_dialog(
    ctx: &Ctx,
    title: &str,
    fields: &[(&str, String, bool)],
    checkbox: Option<(&str, bool)>,
) -> Option<(Vec<String>, bool)> {
    let dlg = Dialog::builder(&ctx.ui.frame, title).with_size(480, 360).build();
    let v = BoxSizer::builder(Orientation::Vertical).build();
    let mut inputs: Vec<TextCtrl> = Vec::new();
    for (label, default, pw) in fields {
        let row = BoxSizer::builder(Orientation::Horizontal).build();
        let lbl = StaticText::builder(&dlg).with_label(label).build();
        let field = if *pw {
            TextCtrl::builder(&dlg).with_style(TextCtrlStyle::Password).build()
        } else {
            TextCtrl::builder(&dlg).build()
        };
        field.set_value(default);
        crate::ui::set_a11y_name(&field, label);
        row.add(&lbl, 0, SizerFlag::AlignCenterVertical | SizerFlag::All, 6);
        row.add(&field, 1, SizerFlag::Expand | SizerFlag::All, 6);
        v.add_sizer(&row, 0, SizerFlag::Expand, 0);
        inputs.push(field);
    }
    let chk = checkbox.map(|(label, val)| {
        let c = CheckBox::builder(&dlg).with_label(label).build();
        c.set_value(val);
        crate::ui::set_a11y_name(&c, label);
        v.add(&c, 0, SizerFlag::All, 6);
        c
    });
    let brow = BoxSizer::builder(Orientation::Horizontal).build();
    let ok = Button::builder(&dlg).with_label(&t!("common.ok")).build();
    let cancel = Button::builder(&dlg).with_label(&t!("common.cancel")).build();
    brow.add(&ok, 0, SizerFlag::All, 6);
    brow.add(&cancel, 0, SizerFlag::All, 6);
    v.add_sizer(&brow, 0, SizerFlag::All, 6);
    {
        let d = dlg;
        ok.on_click(move |_| d.end_modal(ID_OK));
    }
    {
        let d = dlg;
        cancel.on_click(move |_| d.end_modal(ID_CANCEL));
    }
    dlg.set_sizer(v, true);
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
    crate::hubsession::store(b);
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
        notify(ctx, &t!("hub.register_fields_required"), &t!("hub.caption"));
        return;
    }
    ctx.ui.append_hub_log(&t!("hub.requesting_code"));
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || crate::hub::register(&phone, &user, &pass, &display)).await;
        let m = match r {
            Ok(Ok(())) => t!("hub.code_sent").to_string(),
            Ok(Err(e)) => t!("hub.register_failed", error = e).to_string(),
            Err(e) => t!("common.error", error = e).to_string(),
        };
        hub_msg(&ev_tx, m);
    });
}

pub fn hub_verify(ctx: &Ctx) {
    let phone = ctx.ui.hub_phone_in.get_value().trim().to_string();
    let code = ctx.ui.hub_code_in.get_value().trim().to_string();
    if phone.is_empty() || code.is_empty() {
        notify(ctx, &t!("hub.verify_fields_required"), &t!("hub.caption"));
        return;
    }
    ctx.ui.append_hub_log(&t!("hub.verifying_code"));
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || crate::hub::verify(&phone, &code)).await;
        let m = match r {
            Ok(Ok(b)) => {
                store_session(&b);
                if b.approved {
                    request_dir_reload(&ev_tx);
                    t!("hub.logged_in_as", name = b.username).to_string()
                } else {
                    t!("hub.registered_pending", name = b.username, contact = b.team_contact).to_string()
                }
            }
            Ok(Err(e)) => t!("hub.verify_failed", error = e).to_string(),
            Err(e) => t!("common.error", error = e).to_string(),
        };
        hub_msg(&ev_tx, m);
    });
}

pub fn hub_login(ctx: &Ctx) {
    let ident = ctx.ui.hub_ident_in.get_value().trim().to_string();
    let pass = ctx.ui.hub_login_pass_in.get_value();
    if ident.is_empty() || pass.is_empty() {
        notify(ctx, &t!("hub.login_fields_required"), &t!("hub.caption"));
        return;
    }
    ctx.ui.append_hub_log(&t!("hub.logging_in"));
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || crate::hub::login(&ident, &pass)).await;
        let m = match r {
            Ok(Ok(b)) => {
                store_session(&b);
                if b.approved {
                    request_dir_reload(&ev_tx);
                    t!("hub.logged_in_as", name = b.username).to_string()
                } else {
                    t!("hub.logged_in_pending", name = b.username).to_string()
                }
            }
            Ok(Err(e)) => {
                if e.contains("phone_not_verified") {
                    t!("hub.phone_not_verified").to_string()
                } else {
                    t!("hub.login_failed", error = e).to_string()
                }
            }
            Err(e) => t!("common.error", error = e).to_string(),
        };
        hub_msg(&ev_tx, m);
    });
}

pub fn hub_logout(ctx: &Ctx) {
    let session = config::load_config().hub;
    let Some(h) = session else {
        notify(ctx, &t!("hub.not_logged_in"), &t!("hub.caption"));
        return;
    };
    // Sitzung lokal entfernen und sofort zur Anmelden-Seite wechseln.
    let mut cfg = config::load_config();
    cfg.hub = None;
    let _ = config::save_config(&cfg);
    crate::hubsession::forget();
    ctx.st.borrow_mut().hub_view = crate::app::HubView::Login;
    // Verzeichnis-Liste leeren, damit die nächste Anmeldung frisch lädt.
    ctx.st.borrow_mut().hub_servers.clear();
    ctx.ui.hub_servers.clear();
    update_hub_view(ctx);
    let ev_tx = ctx.ev_tx.clone();
    let refresh = h.refresh_token.clone();
    ctx.rt.spawn(async move {
        let _ = tokio::task::spawn_blocking(move || crate::hub::logout(&refresh)).await;
        hub_msg(&ev_tx, t!("hub.logged_out").to_string());
    });
}

pub fn hub_reset_start(ctx: &Ctx) {
    let phone = ctx.ui.hub_reset_phone_in.get_value().trim().to_string();
    if phone.is_empty() {
        notify(ctx, &t!("hub.enter_phone"), &t!("hub.caption"));
        return;
    }
    ctx.ui.append_hub_log(&t!("hub.requesting_reset_code"));
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || crate::hub::reset_start(&phone)).await;
        let m = match r {
            Ok(Ok(())) => t!("hub.reset_code_sent").to_string(),
            Ok(Err(e)) => t!("hub.request_failed", error = e).to_string(),
            Err(e) => t!("common.error", error = e).to_string(),
        };
        hub_msg(&ev_tx, m);
    });
}

/// Gültiges Access-Token der gespeicherten Sitzung (erneuert sie bei Bedarf,
/// serialisiert über alle Instanzen, siehe hubsession). Nur vom
/// Hintergrund-Thread aus aufrufen.
fn fresh_access_token() -> Result<String, String> {
    crate::hubsession::access_token()
}

/// Der Hub hat die Sitzung abgelehnt (abgelaufen/gesperrt); sie ist lokal
/// bereits gelöscht. Oberfläche auf die Anmeldeseite umstellen. Kein eigener
/// Dialog — die auslösende Aktion meldet ihren Fehler (mit Grund) selbst.
pub fn hub_session_lost(ctx: &Ctx, reason: &str) {
    ctx.st.borrow_mut().hub_view = crate::app::HubView::Login;
    ctx.st.borrow_mut().hub_servers.clear();
    ctx.ui.hub_servers.clear();
    update_hub_view(ctx);
    let text = t!("hub.session_lost", reason = reason);
    ctx.ui.append_hub_log(&text);
    status(ctx, &text);
}

pub fn hub_load_directory(ctx: &Ctx) {
    if config::load_config().hub.is_none() {
        notify(ctx, &t!("hub.login_first"), &t!("hub.caption"));
        return;
    }
    let q = ctx.ui.hub_search_in.get_value().trim().to_string();
    // Ansicht 1 = „Meine Server": /servers/mine liefert eigene UND fremde
    // Server mit Mitgliedschaft, öffentlich wie privat. Das öffentliche
    // Verzeichnis (/servers) zeigt private Server grundsätzlich nicht.
    let mine = hub_scope_is_mine(ctx);
    ctx.ui.append_hub_log(&if mine { t!("hub.loading_mine") } else { t!("hub.loading_directory") });
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
            Ok(Err(e)) => hub_msg(&ev_tx, t!("hub.list_failed", error = e).to_string()),
            Err(e) => hub_msg(&ev_tx, t!("common.error", error = e).to_string()),
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
        notify(ctx, &t!("hub.select_directory_server"), &t!("hub.caption"));
        return;
    };
    let server = ctx.st.borrow().hub_servers.get(idx as usize).cloned();
    let Some(s) = server else { return };
    if s.host.trim().is_empty() {
        notify(ctx, &t!("hub.no_address"), &t!("hub.caption"));
        return;
    }
    // Verbindungsformular füllen, zentrales Login wählen, zur Serverliste
    // wechseln und verbinden.
    ctx.ui.host_in.set_value(&s.host);
    ctx.ui.port_in.set_value(&s.control_port.to_string());
    ctx.ui.ssl_chk.set_value(true);
    ctx.ui.use_central_chk.set_value(true);
    // Unterserver-ID merken (für auth_login) und den Audio-Port ins Feld
    // schreiben, damit das Audio den richtigen UDP-Port trifft (und sichtbar ist).
    ctx.st.borrow_mut().pending_server_id = Some(s.id.clone());
    let ap = if s.audio_port > 0 { s.audio_port.to_string() } else { String::new() };
    ctx.ui.audio_port_in.set_value(&ap);
    // Zur „Serverliste" wechseln (Reiter 1; Server-Hub ist jetzt Reiter 0).
    ctx.ui.notebook.set_selection(1);
    do_connect(ctx);
}

pub fn hub_create_server(ctx: &Ctx) {
    if config::load_config().hub.is_none() {
        notify(ctx, &t!("hub.login_first"), &t!("hub.caption"));
        return;
    }
    let Some((vals, is_public)) = form_dialog(
        ctx,
        &t!("hub_server.create_title"),
        &[
            (&*t!("hub_server.name"), String::new(), false),
            (&*t!("hub_server.description"), String::new(), false),
            (&*t!("hub_server.address_create"), String::new(), false),
            (&*t!("hub_server.control_port_create"), String::new(), false),
            (&*t!("hub_server.audio_port_create"), String::new(), false),
        ],
        Some((&*t!("hub_server.public"), true)),
    ) else { return };
    let name = vals[0].trim().to_string();
    if name.is_empty() {
        notify(ctx, &t!("hub_server.name_required"), &t!("hub_server.create_title"));
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
            notify(ctx, &e, &t!("hub_server.create_title"));
            return;
        }
    };
    let (control_port, audio_port) = if host.is_empty() { (0, 0) } else { (control_port, audio_port) };
    let private_hint = !is_public;
    ctx.ui.append_hub_log(&*t!("hub_server.creating"));
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
                        t!("hub_server.created_private").to_string()
                    } else {
                        t!("hub_server.created").to_string()
                    },
                );
                // Private Server tauchen im öffentlichen Verzeichnis nicht auf,
                // deshalb direkt in die eigene Liste wechseln.
                let _ = ev_tx.send(Message::new(
                    "hub_dir_reload",
                    serde_json::json!({ "mine": private_hint }),
                ));
            }
            Ok(Err(e)) => hub_msg(&ev_tx, t!("hub_server.create_failed", error = e).to_string()),
            Err(e) => hub_msg(&ev_tx, t!("common.error", error = e).to_string()),
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
        c.parse::<i64>().map_err(|_| t!("hub_server.control_port_nan").to_string())?
    };
    let ap = if a.is_empty() {
        cp + 1
    } else {
        a.parse::<i64>().map_err(|_| t!("hub_server.audio_port_nan").to_string())?
    };
    if !(1..=65535).contains(&cp) || !(1..=65535).contains(&ap) {
        return Err(t!("hub_server.port_range").into());
    }
    Ok((cp, ap))
}

fn selected_hub_server(ctx: &Ctx) -> Option<crate::hub::ServerInfo> {
    let idx = ctx.ui.hub_servers.get_selection()? as usize;
    ctx.st.borrow().hub_servers.get(idx).cloned()
}

pub fn hub_edit_server(ctx: &Ctx) {
    let Some(s) = selected_hub_server(ctx) else {
        notify(ctx, &t!("hub.select_list_server"), &t!("hub.caption"));
        return;
    };
    let Some((vals, is_public)) = form_dialog(
        ctx,
        &t!("hub_server.edit_title"),
        &[
            (&*t!("hub_server.name"), s.name.clone(), false),
            (&*t!("hub_server.description"), s.description.clone(), false),
            (&*t!("hub_server.address_edit"), s.host.clone(), false),
            (&*t!("hub_server.control_port"), if s.control_port > 0 { s.control_port.to_string() } else { String::new() }, false),
            (&*t!("hub_server.audio_port"), if s.audio_port > 0 { s.audio_port.to_string() } else { String::new() }, false),
        ],
        Some((&*t!("hub_server.public"), s.is_public)),
    ) else { return };
    let name = vals[0].trim().to_string();
    if name.is_empty() {
        notify(ctx, &t!("hub_server.name_required"), &t!("hub_server.edit_title"));
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
                notify(ctx, &e, &t!("hub_server.edit_title"));
                return;
            }
        }
    };
    ctx.ui.append_hub_log(&*t!("hub_server.saving"));
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
            Ok(Ok(())) => { hub_msg(&ev_tx, t!("hub_server.saved").to_string()); request_dir_reload(&ev_tx); }
            Ok(Err(e)) => hub_msg(&ev_tx, t!("hub_server.save_failed", error = e).to_string()),
            Err(e) => hub_msg(&ev_tx, t!("common.error", error = e).to_string()),
        }
    });
}

pub fn hub_delete_server(ctx: &Ctx) {
    let Some(s) = selected_hub_server(ctx) else {
        notify(ctx, &t!("hub.select_list_server"), &t!("hub.caption"));
        return;
    };
    let confirm = {
        let dlg = MessageDialog::builder(
            &ctx.ui.frame,
            &t!("hub_server.delete_confirm", name = s.name),
            &t!("hub_server.delete_title"),
        )
        .with_style(MessageDialogStyle::YesNo)
        .build();
        dlg.show_modal() == ID_YES
    };
    if !confirm {
        return;
    }
    ctx.ui.append_hub_log(&*t!("hub_server.deleting"));
    let ev_tx = ctx.ev_tx.clone();
    let sid = s.id.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || {
            let access = fresh_access_token()?;
            crate::hub::delete_server(&access, &sid)
        })
        .await;
        match r {
            Ok(Ok(())) => { hub_msg(&ev_tx, t!("hub_server.deleted").to_string()); request_dir_reload(&ev_tx); }
            Ok(Err(e)) => hub_msg(&ev_tx, t!("hub_server.delete_failed", error = e).to_string()),
            Err(e) => hub_msg(&ev_tx, t!("common.error", error = e).to_string()),
        }
    });
}

/// Admin: Liste der wartenden Konten als Dialog mit „Freigeben"/„Ablehnen".
pub fn hub_admin_pending(ctx: &Ctx) {
    if !hub_is_admin() {
        notify(ctx, &t!("hub_admin.only_admins"), &t!("hub_admin.caption"));
        return;
    }
    let dlg = Dialog::builder(&ctx.ui.frame, &t!("hub_admin.pending_title")).with_size(480, 420).build();
    let v = BoxSizer::builder(Orientation::Vertical).build();
    v.add(
        &StaticText::builder(&dlg)
            .with_label(&t!("hub_admin.pending_hint"))
            .build(),
        0,
        SizerFlag::All,
        6,
    );
    let list = ListBox::builder(&dlg).build();
    crate::ui::set_a11y_name(&list, &t!("hub_admin.pending_list"));
    v.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 6);
    let brow = BoxSizer::builder(Orientation::Horizontal).build();
    let approve_btn = Button::builder(&dlg).with_label(&t!("hub_admin.approve")).build();
    let reject_btn = Button::builder(&dlg).with_label(&t!("hub_admin.reject")).build();
    let close_btn = Button::builder(&dlg).with_label(&t!("common.close")).build();
    brow.add(&approve_btn, 0, SizerFlag::All, 4);
    brow.add(&reject_btn, 0, SizerFlag::All, 4);
    brow.add(&close_btn, 0, SizerFlag::All, 4);
    v.add_sizer(&brow, 0, SizerFlag::All, 6);
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
                    &t!("hub_admin.reject_confirm", name = u.display_name, user = u.username),
                    &t!("hub_admin.reject"),
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
        notify(ctx, &t!("hub_admin.only_admins"), &t!("hub_admin.caption"));
        return;
    }
    let Some(q) = ask_text(ctx, &t!("hub_admin.search_prompt"), &t!("hub_admin.user_title"), "") else { return };
    let results = match fresh_access_token().and_then(|t| crate::hub::search_users(&t, q.trim())) {
        Ok(v) => v,
        Err(e) => {
            notify(ctx, &t!("hub.search_failed", error = e), &t!("hub_admin.caption"));
            return;
        }
    };
    if results.is_empty() {
        notify(ctx, &t!("hub.no_user_found"), &t!("hub_admin.caption"));
        return;
    }
    // Treffer auswählen.
    let labels: Vec<String> = results.iter().map(|u| format!("{} ({})", u.display_name, u.username)).collect();
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let pick = SingleChoiceDialog::builder(&ctx.ui.frame, &t!("hub_admin.pick_user"), &t!("hub_admin.user_title"), &label_refs).build();
    if pick.show_modal() != ID_OK {
        return;
    }
    let sel = pick.get_selection();
    if sel < 0 {
        return;
    }
    let u = results[sel as usize].clone();
    // Aktion wählen.
    let actions: Vec<String> = ["hub_admin.action_ban", "hub_admin.action_unban", "hub_admin.action_reset_pw", "hub_admin.action_promote"]
        .iter()
        .map(|k| t!(*k).to_string())
        .collect();
    let action_refs: Vec<&str> = actions.iter().map(|s| s.as_str()).collect();
    let adlg = SingleChoiceDialog::builder(&ctx.ui.frame, &t!("hub_admin.action_for", user = u.username), &t!("hub_admin.action_title"), &action_refs).build();
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
            let reason = ask_text(ctx, &t!("hub_admin.reason_prompt"), &t!("hub_admin.action_ban"), "").unwrap_or_default();
            fresh_access_token().and_then(|t| crate::hub::admin_ban(&t, &uid, &reason))
        }
        1 => fresh_access_token().and_then(|t| crate::hub::admin_unban(&t, &uid)),
        2 => {
            let Some(pw) = ask_text(ctx, &t!("hub_admin.new_pw_prompt"), &t!("hub_admin.action_reset_pw"), "") else { return };
            fresh_access_token().and_then(|t| crate::hub::admin_reset_password(&t, &uid, &pw))
        }
        3 => fresh_access_token().and_then(|t| crate::hub::admin_promote(&t, &uid)),
        _ => return,
    };
    match res {
        Ok(()) => notify(ctx, &t!("hub_admin.action_done", user = u.username), &t!("hub_admin.caption")),
        Err(e) => notify(ctx, &t!("hub_admin.action_failed", error = e), &t!("hub_admin.caption")),
    }
}

/// Jemanden auf den ausgewählten Server einladen: Nutzer suchen, Treffer wählen,
/// Profil zur Kontrolle anzeigen, Einladung anlegen (POST /invites). Einladen
/// darf, wer Mitglied oder Eigentümer des Servers ist.
pub fn hub_invite_user(ctx: &Ctx) {
    if config::load_config().hub.is_none() {
        notify(ctx, &t!("hub.login_first"), &t!("hub.caption"));
        return;
    }
    let Some(server) = selected_hub_server(ctx) else {
        notify(
            ctx,
            &t!("invite.select_server"),
            &t!("invite.caption"),
        );
        return;
    };
    let Some(q) = ask_text(
        ctx,
        &t!("invite.search_prompt", server = server.name),
        &t!("invite.caption"),
        "",
    ) else { return };
    // Kurze blockierende Aufrufe wie in den anderen Hub-Dialogen.
    let results = match fresh_access_token().and_then(|t| crate::hub::search_users(&t, q.trim())) {
        Ok(v) => v,
        Err(e) => {
            notify(ctx, &t!("hub.search_failed", error = e), &t!("invite.caption"));
            return;
        }
    };
    if results.is_empty() {
        notify(ctx, &t!("hub.no_user_found"), &t!("invite.caption"));
        return;
    }
    let labels: Vec<String> = results
        .iter()
        .map(|u| format!("{} ({})", u.display_name, u.username))
        .collect();
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let pick = SingleChoiceDialog::builder(&ctx.ui.frame, &t!("hub_admin.pick_user"), &t!("invite.caption"), &label_refs).build();
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
        t!("invite.confirm", name = u.display_name, user = u.username, server = server.name).to_string()
    } else {
        t!("invite.confirm_bio", name = u.display_name, user = u.username, server = server.name, bio = bio).to_string()
    };
    let ok = MessageDialog::builder(&ctx.ui.frame, &question, &t!("invite.caption"))
        .with_style(MessageDialogStyle::YesNo)
        .build()
        .show_modal()
        == ID_YES;
    if !ok {
        return;
    }
    match fresh_access_token().and_then(|t| crate::hub::create_invite(&t, &server.id, &u.central_uid)) {
        Ok(_) => {
            ctx.ui.append_hub_log(&t!("invite.sent_log", name = u.display_name, user = u.username, server = server.name));
            notify(ctx, &t!("invite.sent", name = u.display_name), &t!("invite.caption"));
        }
        Err(e) => notify(ctx, &t!("invite.failed", error = e), &t!("invite.caption")),
    }
}

/// Admin: alle Server des Hubs ansehen und das Datei-Limit setzen.
pub fn hub_admin_servers(ctx: &Ctx) {
    if !hub_is_admin() {
        notify(ctx, &t!("hub_admin.only_admins"), &t!("hub_admin.caption"));
        return;
    }
    let dlg = Dialog::builder(&ctx.ui.frame, &t!("hub_admin.servers_title")).with_size(560, 440).build();
    let v = BoxSizer::builder(Orientation::Vertical).build();
    v.add(
        &StaticText::builder(&dlg)
            .with_label(&t!("hub_admin.servers_hint"))
            .build(),
        0,
        SizerFlag::All,
        6,
    );
    let list = ListBox::builder(&dlg).build();
    crate::ui::set_a11y_name(&list, &t!("hub_admin.servers_list"));
    v.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 6);
    let brow = BoxSizer::builder(Orientation::Horizontal).build();
    let quota_btn = Button::builder(&dlg).with_label(&t!("hub_admin.set_quota")).build();
    let refresh_btn = Button::builder(&dlg).with_label(&t!("common.refresh")).build();
    let close_btn = Button::builder(&dlg).with_label(&t!("common.close")).build();
    brow.add(&quota_btn, 0, SizerFlag::All, 4);
    brow.add(&refresh_btn, 0, SizerFlag::All, 4);
    brow.add(&close_btn, 0, SizerFlag::All, 4);
    v.add_sizer(&brow, 0, SizerFlag::All, 6);
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
                &t!("hub_admin.quota_prompt", name = s.name),
                &t!("hub_admin.quota_title"),
                &current_mb,
            ) else { return };
            let Ok(mb) = txt.trim().parse::<i64>() else {
                MessageDialog::builder(&frame, &t!("common.enter_number"), &t!("hub_admin.quota_title")).build().show_modal();
                return;
            };
            match fresh_access_token()
                .and_then(|t| crate::hub::admin_set_quota(&t, &s.id, mb.max(0) * 1024 * 1024))
            {
                Ok(()) => fill(),
                Err(e) => {
                    MessageDialog::builder(&frame, &t!("hub_admin.quota_failed", error = e), &t!("hub_admin.quota_title"))
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
        return t!("hub_admin.server_private", name = s.name).to_string();
    }
    let limit = if s.file_limit_bytes > 0 {
        format!("{} MB", s.file_limit_bytes / (1024 * 1024))
    } else {
        t!("hub_admin.no_limit").to_string()
    };
    t!(
        "hub_admin.server_public",
        name = s.name,
        host = s.host,
        port = s.control_port,
        used = s.storage_used_bytes / (1024 * 1024),
        limit = limit
    )
    .to_string()
}

/// Offene Einladungen laden und einzeln zum Annehmen/Ablehnen anbieten.
pub fn hub_invites(ctx: &Ctx) {
    if config::load_config().hub.is_none() {
        notify(ctx, &t!("hub.login_first"), &t!("hub.caption"));
        return;
    }
    // Kurzer blockierender Aufruf; die Ja/Nein-Dialoge laufen ohnehin auf dem
    // UI-Thread.
    let invites = match fresh_access_token().and_then(|t| crate::hub::list_invites(&t)) {
        Ok(v) => v,
        Err(e) => {
            notify(ctx, &t!("invites.load_failed", error = e), &t!("hub.caption"));
            return;
        }
    };
    if invites.is_empty() {
        notify(ctx, &t!("invites.none"), &t!("hub.caption"));
        return;
    }
    for inv in invites {
        let dlg = MessageDialog::builder(
            &ctx.ui.frame,
            &t!("invites.accept_prompt", server = inv.server_name),
            &t!("invites.caption"),
        )
        .with_style(MessageDialogStyle::YesNo)
        .build();
        let accept = dlg.show_modal() == ID_YES;
        match fresh_access_token().and_then(|t| crate::hub::respond_invite(&t, &inv.id, accept)) {
            Ok(()) => ctx.ui.append_hub_log(&if accept {
                t!("invites.accepted", server = inv.server_name)
            } else {
                t!("invites.declined", server = inv.server_name)
            }),
            Err(e) => ctx.ui.append_hub_log(&t!("invites.error", error = e)),
        }
    }
    // Neue Mitgliedschaften stehen unter „Meine Server" — dorthin wechseln und
    // neu laden, sonst bliebe ein privater Server unsichtbar.
    hub_show_my_servers(ctx);
}

/// Anzeigename und Bio bearbeiten.
pub fn hub_edit_profile(ctx: &Ctx) {
    if config::load_config().hub.is_none() {
        notify(ctx, &t!("hub.login_first"), &t!("hub.caption"));
        return;
    }
    // Aktuelles Profil laden (kurzer blockierender Aufruf) und vorausfüllen.
    let prof = match fresh_access_token().and_then(|t| crate::hub::get_my_profile(&t)) {
        Ok(p) => p,
        Err(e) => {
            notify(ctx, &t!("profile.load_failed", error = e), &t!("hub.caption"));
            return;
        }
    };
    let Some((vals, _)) = form_dialog(
        ctx,
        &t!("profile.title"),
        &[(&*t!("profile.display_name"), prof.display_name, false), (&*t!("profile.bio"), prof.bio, false)],
        None,
    ) else { return };
    let display = vals[0].clone();
    let bio = vals[1].clone();
    ctx.ui.append_hub_log(&t!("profile.saving"));
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || {
            let access = fresh_access_token()?;
            crate::hub::update_profile(&access, &display, &bio)
        })
        .await;
        let m = match r {
            Ok(Ok(())) => t!("profile.saved").to_string(),
            Ok(Err(e)) => t!("profile.save_failed", error = e).to_string(),
            Err(e) => t!("common.error", error = e).to_string(),
        };
        hub_msg(&ev_tx, m);
    });
}

pub fn hub_reset_confirm(ctx: &Ctx) {
    let phone = ctx.ui.hub_reset_phone_in.get_value().trim().to_string();
    let code = ctx.ui.hub_reset_code_in.get_value().trim().to_string();
    let new_pass = ctx.ui.hub_reset_pass_in.get_value();
    if phone.is_empty() || code.is_empty() || new_pass.is_empty() {
        notify(ctx, &t!("hub.reset_fields_required"), &t!("hub.caption"));
        return;
    }
    ctx.ui.append_hub_log(&t!("hub.setting_password"));
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || crate::hub::reset_confirm(&phone, &code, &new_pass)).await;
        let m = match r {
            Ok(Ok(())) => t!("hub.password_changed").to_string(),
            Ok(Err(e)) => t!("hub.reset_failed", error = e).to_string(),
            Err(e) => t!("common.error", error = e).to_string(),
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
        status(ctx, &t!("chat.join_room_first"));
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
        status(ctx, &t!("chat.select_user_first"));
        return;
    };
    let text = ctx.ui.chat_in.get_value().trim().to_string();
    if text.is_empty() {
        status(ctx, &t!("chat.pm_hint"));
        return;
    }
    let msg = Message::new(
        "chat_private",
        serde_json::json!({ "to_user_id": user_id, "message": text }),
    );
    if send_or_status(ctx, msg) {
        let nick = ctx.app.inner.lock().nickname_of(user_id);
        ctx.ui.append_chat(&t!("chat.private_to", name = nick, text = text));
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
    ctx.ui.append_chat(&t!("room.joined", room = room));
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
        if let Some(pw) = ask_text(ctx, &t!("room.password_prompt"), &t!("room.password_title"), "") {
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
        None => notify(ctx, &t!("room.select_first"), &t!("room.join_caption")),
    }
}

fn leave_room(ctx: &Ctx) {
    let Some(room_id) = ctx.app.inner.lock().current_room_id else {
        status(ctx, &t!("room.not_in_room"));
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
        ctx.ui.append_chat(&t!("room.left", room = room));
        rebuild_tree(ctx);
        rebuild_files(ctx);
        refresh_status(ctx);
    }
}

const RATES: [i64; 5] = [48000, 44100, 24000, 16000, 8000];
const DEPTHS: [i64; 3] = [16, 24, 32];

/// Raum-Vorlage: typische Audio-Einstellungen für einen Anwendungsfall.
struct RoomTemplate {
    /// Übersetzungsschlüssel des Vorlagennamens
    label_key: &'static str,
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
    RoomTemplate { label_key: "room.template_voice", sample_rate: 48000, bit_depth: 16, channels: 1, bitrate_kbps: 64 },
    // Schmalbandige Sprache wie ein klassisches Telefonat
    RoomTemplate { label_key: "room.template_phone", sample_rate: 8000, bit_depth: 16, channels: 1, bitrate_kbps: 16 },
    // Musik/Übertragung in Stereo, hohe Qualität
    RoomTemplate { label_key: "room.template_radio", sample_rate: 48000, bit_depth: 16, channels: 2, bitrate_kbps: 256 },
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
                t!("room.create_sub_title")
            } else {
                t!("room.create_title")
            };
            (t, String::new(), 0i64, 48000i64, 16i64, 1i64, 0i64, false)
        }
        RoomMode::Edit { room_id } => {
            let inner = ctx.app.inner.lock();
            match inner.rooms.iter().find(|r| r.id == *room_id) {
                Some(r) => (
                    t!("room.edit_title"),
                    r.name.clone(),
                    r.max_users,
                    if r.sample_rate > 0 { r.sample_rate } else { 48000 },
                    if r.bit_depth > 0 { r.bit_depth } else { 16 },
                    if r.channels > 0 { r.channels } else { 1 },
                    if r.bitrate > 0 { r.bitrate / 1000 } else { 0 },
                    true,
                ),
                None => (t!("room.edit_title"), String::new(), 0, 48000, 16, 1, 0, true),
            }
        }
    };

    let dialog = Dialog::builder(&ctx.ui.frame, &title).build();
    let v = BoxSizer::builder(Orientation::Vertical).build();
    // Beschriftete Zeile; gibt das Label zurück, damit erweiterte Zeilen
    // gemeinsam mit ihrem Control aus-/eingeblendet werden können.
    fn add_row<W: WxWidget>(dialog: &Dialog, v: &BoxSizer, label: &str, ctrl: &W) -> StaticText {
        let r = BoxSizer::builder(Orientation::Horizontal).build();
        let lbl = StaticText::builder(dialog).with_label(label).build();
        r.add(&lbl, 0, SizerFlag::AlignCenterVertical | SizerFlag::All, 6);
        r.add(ctrl, 1, SizerFlag::Expand | SizerFlag::All, 6);
        v.add_sizer(&r, 0, SizerFlag::Expand, 0);
        lbl
    }

    let name_in = TextCtrl::builder(&dialog).build();
    name_in.set_value(&name0);
    ui::set_a11y_name(&name_in, &t!("room.name_a11y"));
    add_row(&dialog, &v, &t!("room.name"), &name_in);

    // Passwort ohne Checkbox: gilt, sobald etwas drinsteht.
    let pw_in = TextCtrl::builder(&dialog)
        .with_style(TextCtrlStyle::Password)
        .build();
    ui::set_a11y_name(&pw_in, &t!("room.password_a11y"));
    add_row(
        &dialog,
        &v,
        &if edit {
            t!("room.password_edit")
        } else {
            t!("room.password_create")
        },
        &pw_in,
    );

    let max_in = TextCtrl::builder(&dialog).build();
    max_in.set_value(&max0.to_string());
    ui::set_a11y_name(&max_in, &t!("room.max_users_a11y"));
    add_row(&dialog, &v, &t!("room.max_users"), &max_in);

    // Vorlagen-Auswahl
    let template_choice = Choice::builder(&dialog).build();
    ui::ignore_mouse_wheel(&template_choice);
    for t in &TEMPLATES {
        template_choice.append(&t!(t.label_key));
    }
    template_choice.append(&t!("room.template_advanced"));
    ui::set_a11y_name(&template_choice, &t!("room.template_a11y"));
    add_row(&dialog, &v, &t!("room.template"), &template_choice);

    // Erweiterte Audio-Felder (anfangs ggf. ausgeblendet)
    let rate_choice = Choice::builder(&dialog).build();
    ui::ignore_mouse_wheel(&rate_choice);
    for r in RATES {
        rate_choice.append(&r.to_string());
    }
    rate_choice.set_selection(RATES.iter().position(|&r| r == sr0).unwrap_or(0) as u32);
    ui::set_a11y_name(&rate_choice, &t!("audio.sample_rate_a11y"));
    let rate_lbl = add_row(&dialog, &v, &t!("audio.sample_rate"), &rate_choice);

    let depth_choice = Choice::builder(&dialog).build();
    ui::ignore_mouse_wheel(&depth_choice);
    for d in DEPTHS {
        depth_choice.append(&d.to_string());
    }
    depth_choice.set_selection(DEPTHS.iter().position(|&d| d == bd0).unwrap_or(0) as u32);
    ui::set_a11y_name(&depth_choice, &t!("audio.bit_depth_a11y"));
    let depth_lbl = add_row(&dialog, &v, &t!("audio.bit_depth"), &depth_choice);

    let ch_choice = Choice::builder(&dialog).build();
    ui::ignore_mouse_wheel(&ch_choice);
    ch_choice.append(&t!("audio.mono"));
    ch_choice.append(&t!("audio.stereo"));
    ch_choice.set_selection(if ch0 >= 2 { 1 } else { 0 });
    ui::set_a11y_name(&ch_choice, &t!("audio.channels_a11y"));
    let ch_lbl = add_row(&dialog, &v, &t!("audio.channels"), &ch_choice);

    let bitrate_in = TextCtrl::builder(&dialog).build();
    bitrate_in.set_value(&br0_kbps.to_string());
    ui::set_a11y_name(&bitrate_in, &t!("audio.bitrate_a11y"));
    let br_lbl = add_row(&dialog, &v, &t!("audio.bitrate"), &bitrate_in);

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

    let btns = BoxSizer::builder(Orientation::Horizontal).build();
    let cancel = Button::builder(&dialog).with_label(&t!("common.cancel")).build();
    let ok = Button::builder(&dialog).with_label(&t!("common.save")).build();
    {
        let d = dialog;
        cancel.on_click(move |_| d.end_modal(ID_CANCEL));
    }
    {
        let d = dialog;
        ok.on_click(move |_| d.end_modal(ID_OK));
    }
    btns.add(&cancel, 0, SizerFlag::All, 6);
    btns.add(&ok, 0, SizerFlag::All, 6);
    v.add_sizer(&btns, 0, SizerFlag::AlignRight, 0);

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
        notify(ctx, &t!("room.name_empty"), &t!("room.caption"));
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
                    &t!("room.too_deep"),
                    &t!("room.create_title"),
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
        None => notify(ctx, &t!("room.select_first"), &t!("room.edit_title")),
    }
}

fn delete_room(ctx: &Ctx) {
    let Some(room_id) = selected_room(ctx) else {
        status(ctx, &t!("room.select_first"));
        return;
    };
    let name = ctx.app.inner.lock().room_name(room_id);
    let dlg = MessageDialog::builder(
        &ctx.ui.frame,
        &t!("room.delete_confirm", name = name),
        &t!("room.delete_title"),
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
pub fn set_menu_check(ctx: &Ctx, id: i32, checked: bool) {
    if let Some(mb) = ctx.ui.frame.get_menu_bar() {
        mb.check_item(id, checked);
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
    ctx.ui.append_chat(&if muted {
        t!("audio_state.mic_muted")
    } else {
        t!("audio_state.mic_on")
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
    ctx.ui.append_chat(&if deafened {
        t!("audio_state.deafened")
    } else {
        t!("audio_state.sound_on")
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
    ctx.ui.append_chat(&if loopback {
        t!("audio_state.loopback_on")
    } else {
        t!("audio_state.loopback_off")
    });
    refresh_status(ctx);
}

pub fn volume_changed(ctx: &Ctx) {
    let v = ctx.ui.volume.get_value();
    ctx.app.set_volume(v as f32 / 100.0);
}

pub fn mic_volume_changed(ctx: &Ctx) {
    let v = ctx.ui.mic_volume.get_value();
    ctx.app.set_mic_volume(v as f32 / 100.0);
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

fn audio_settings(ctx: &Ctx) {
    let enumerate = || -> (Vec<String>, Vec<String>) {
        let devices = crate::audio::device::list_devices();
        let mut inputs: Vec<String> = vec![t!("devices.default").to_string()];
        let mut outputs: Vec<String> = vec![t!("devices.default").to_string()];
        for d in &devices {
            if d.is_input {
                inputs.push(d.name.clone());
            }
            if d.is_output {
                outputs.push(d.name.clone());
            }
        }
        (inputs, outputs)
    };
    let (inputs, outputs) = enumerate();

    let (cur_in, cur_out) = {
        let inner = ctx.app.inner.lock();
        (inner.input_device.clone(), inner.output_device.clone())
    };
    let in_idx = cur_in
        .as_ref()
        .and_then(|n| inputs.iter().position(|d| d == n))
        .unwrap_or(0);
    let out_idx = cur_out
        .as_ref()
        .and_then(|n| outputs.iter().position(|d| d == n))
        .unwrap_or(0);

    let dialog = Dialog::builder(&ctx.ui.frame, &t!("devices.title")).build();
    let v = BoxSizer::builder(Orientation::Vertical).build();

    let row = |label: &str, choice: &Choice| {
        let r = BoxSizer::builder(Orientation::Horizontal).build();
        r.add(
            &StaticText::builder(&dialog).with_label(label).build(),
            0,
            SizerFlag::AlignCenterVertical | SizerFlag::All,
            6,
        );
        r.add(choice, 1, SizerFlag::Expand | SizerFlag::All, 6);
        v.add_sizer(&r, 0, SizerFlag::Expand, 0);
    };

    let in_choice = Choice::builder(&dialog).build();
    ui::ignore_mouse_wheel(&in_choice);
    for d in &inputs {
        in_choice.append(d);
    }
    in_choice.set_selection(in_idx as u32);
    ui::set_a11y_name(&in_choice, &t!("devices.mic_a11y"));
    row(&*t!("devices.mic"), &in_choice);

    let out_choice = Choice::builder(&dialog).build();
    ui::ignore_mouse_wheel(&out_choice);
    for d in &outputs {
        out_choice.append(d);
    }
    out_choice.set_selection(out_idx as u32);
    ui::set_a11y_name(&out_choice, &t!("devices.speaker_a11y"));
    row(&*t!("devices.speaker"), &out_choice);

    // Aktuelle Gerätelisten (für die Auswahl-Zuordnung). Der „Aktualisieren"-
    // Knopf füllt die Auswahlfelder neu (z. B. nach Ein-/Ausstöpseln eines Geräts).
    let lists = std::rc::Rc::new(std::cell::RefCell::new((inputs, outputs)));
    let refresh_btn = Button::builder(&dialog).with_label(&t!("devices.refresh")).build();
    ui::set_a11y_name(&refresh_btn, &t!("devices.refresh_a11y"));
    {
        let lists = lists.clone();
        refresh_btn.on_click(move |_| {
            let devices = crate::audio::device::list_devices();
            let mut ins: Vec<String> = vec![t!("devices.default").to_string()];
            let mut outs: Vec<String> = vec![t!("devices.default").to_string()];
            for d in &devices {
                if d.is_input {
                    ins.push(d.name.clone());
                }
                if d.is_output {
                    outs.push(d.name.clone());
                }
            }
            // Aktuelle Auswahl per Name beibehalten, falls noch vorhanden.
            let sel_in = in_choice
                .get_selection()
                .and_then(|i| lists.borrow().0.get(i as usize).cloned());
            let sel_out = out_choice
                .get_selection()
                .and_then(|i| lists.borrow().1.get(i as usize).cloned());
            in_choice.clear();
            for d in &ins {
                in_choice.append(d);
            }
            out_choice.clear();
            for d in &outs {
                out_choice.append(d);
            }
            let ii = sel_in.and_then(|n| ins.iter().position(|d| *d == n)).unwrap_or(0);
            let oi = sel_out.and_then(|n| outs.iter().position(|d| *d == n)).unwrap_or(0);
            in_choice.set_selection(ii as u32);
            out_choice.set_selection(oi as u32);
            *lists.borrow_mut() = (ins, outs);
        });
    }
    v.add(&refresh_btn, 0, SizerFlag::All, 6);

    // Toggle: Server-Ereignisse per Sprachausgabe ansagen (Standard an).
    let announce_chk = CheckBox::builder(&dialog)
        .with_label(&t!("devices.announce"))
        .build();
    announce_chk.set_value(ctx.st.borrow().announce_events);
    ui::set_a11y_name(&announce_chk, &t!("devices.announce_a11y"));
    v.add(&announce_chk, 0, SizerFlag::All, 6);

    // Toggle: Mikrofon-Boost (+100% / 2x Lautstärke).
    let boost_chk = CheckBox::builder(&dialog)
        .with_label(&t!("devices.boost"))
        .build();
    boost_chk.set_value(ctx.app.mic_boost());
    ui::set_a11y_name(&boost_chk, &t!("devices.boost_a11y"));
    v.add(&boost_chk, 0, SizerFlag::All, 6);

    let btns = BoxSizer::builder(Orientation::Horizontal).build();
    let cancel = Button::builder(&dialog).with_label(&t!("common.cancel")).build();
    let ok = Button::builder(&dialog).with_label(&t!("common.save")).build();
    {
        let d = dialog;
        cancel.on_click(move |_| d.end_modal(ID_CANCEL));
    }
    {
        let d = dialog;
        ok.on_click(move |_| d.end_modal(ID_OK));
    }
    btns.add(&cancel, 0, SizerFlag::All, 6);
    btns.add(&ok, 0, SizerFlag::All, 6);
    v.add_sizer(&btns, 0, SizerFlag::AlignRight, 0);

    dialog.set_sizer(v, true);
    dialog.fit();

    let result = dialog.show_modal();
    if result == ID_OK {
        // Index 0 = Standardgerät = None. Auswahl gegen die AKTUELLEN Listen.
        let (input, output) = {
            let g = lists.borrow();
            let pick = |sel: Option<u32>, list: &[String]| -> Option<String> {
                match sel {
                    Some(0) | None => None,
                    Some(i) => list.get(i as usize).cloned(),
                }
            };
            (
                pick(in_choice.get_selection(), &g.0),
                pick(out_choice.get_selection(), &g.1),
            )
        };
        let announce_events = announce_chk.is_checked();
        let mic_boost = boost_chk.is_checked();
        ctx.app.set_mic_boost(mic_boost);
        {
            let mut inner = ctx.app.inner.lock();
            inner.input_device = input.clone();
            inner.output_device = output.clone();
        }
        ctx.st.borrow_mut().announce_events = announce_events;
        let mut cfg = config::load_config();
        cfg.input_device = input;
        cfg.output_device = output;
        cfg.announce_events = announce_events;
        cfg.mic_boost = mic_boost;
        let _ = config::save_config(&cfg);
        let connected = ctx.app.inner.lock().connected;
        dialog.destroy();
        // Geräte sofort übernehmen (ohne Reconnect), wenn verbunden.
        if connected {
            restart_audio(ctx);
            notify(ctx, &t!("devices.applied"), &t!("devices.title"));
        } else {
            notify(ctx, &t!("devices.saved"), &t!("devices.title"));
        }
    } else {
        dialog.destroy();
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
        status(ctx, &t!("status.not_connected_dot"));
        return;
    }
    let dlg = FileDialog::builder(&ctx.ui.frame)
        .with_message(&t!("stream.choose_file"))
        .with_wildcard(&format!(
            "{}|*.mp3;*.wav;*.flac;*.ogg;*.m4a;*.aac;*.opus;*.aiff|{}|*.*",
            t!("stream.audio_files"),
            t!("files.all_files")
        ))
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
    ctx.ui.append_chat(&t!("stream.streaming", file = filename));
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
                serde_json::json!({ "message": t!("stream.error", error = e) }),
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
        status(ctx, &t!("stream.none_running"));
    }
}

/// Gestreamte Datei pausieren bzw. fortsetzen (lokal, kein Server-Roundtrip).
fn toggle_pause_stream(ctx: &Ctx) {
    let streaming = ctx.app.inner.lock().stream_shutdown.is_some();
    if !streaming {
        set_menu_check(ctx, ui::ID_PAUSE_STREAM, false);
        status(ctx, &t!("stream.none_running"));
        return;
    }
    use std::sync::atomic::Ordering;
    let paused = !ctx.app.stream_paused.load(Ordering::Relaxed);
    ctx.app.stream_paused.store(paused, Ordering::Relaxed);
    set_menu_check(ctx, ui::ID_PAUSE_STREAM, paused);
    ctx.ui.append_chat(&if paused {
        t!("stream.paused_chat")
    } else {
        t!("stream.resumed_chat")
    });
    status(ctx, &if paused { t!("stream.paused") } else { t!("stream.resumed") });
}

// ── Dateien ──

fn upload_file(ctx: &Ctx) {
    let Some(room_id) = ctx.app.inner.lock().current_room_id else {
        status(ctx, &t!("chat.join_room_first"));
        return;
    };
    let dlg = FileDialog::builder(&ctx.ui.frame)
        .with_message(&t!("files.choose_upload"))
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
            status(ctx, &t!("files.read_failed", error = e));
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
        status(ctx, &t!("files.select_first"));
        return;
    };
    let dlg = FileDialog::builder(&ctx.ui.frame)
        .with_message(&t!("files.choose_target"))
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
        ctx.ui.append_chat(&t!("files.downloading", file = file.filename));
    }
}

// ── Verwaltung ──

fn kick_user(ctx: &Ctx) {
    let Some(user_id) = selected_user(ctx) else {
        status(ctx, &t!("admin.select_user_first"));
        return;
    };
    let reason = ask_text(ctx, &t!("hub_admin.reason_prompt"), &t!("admin.kick_title"), "");
    let mut data = serde_json::json!({ "user_id": user_id });
    if let Some(r) = reason {
        data["reason"] = serde_json::Value::String(r);
    }
    send_or_status(ctx, Message::new("admin_kick", data));
}

fn ban_user(ctx: &Ctx) {
    let Some(user_id) = selected_user(ctx) else {
        status(ctx, &t!("admin.select_user_first"));
        return;
    };
    let reason = ask_text(ctx, &t!("hub_admin.reason_prompt"), &t!("admin.ban_title"), "");
    let duration = ask_text(ctx, &t!("admin.ban_duration"), &t!("admin.ban_title"), "");
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
        status(ctx, &t!("admin.select_user_first"));
        return;
    };
    let Some(room_id) = selected_room(ctx) else {
        status(ctx, &t!("admin.select_target_room"));
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
        status(ctx, &t!("admin.select_user_first"));
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
    if let Some(text) = ask_text(ctx, &t!("admin.server_msg_prompt"), &t!("admin.server_msg_title"), "") {
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
    let dlg = MessageDialog::builder(&ctx.ui.frame, &t!("accounts.ask_admin"), caption)
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
                &t!("menu.accounts"),
                &t!("menu.accounts_help"),
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
    let dialog = Dialog::builder(&ctx.ui.frame, &t!("accounts.title")).build();
    let v = BoxSizer::builder(Orientation::Vertical).build();

    v.add(
        &StaticText::builder(&dialog)
            .with_label(&t!("accounts.list_label"))
            .build(),
        0,
        SizerFlag::All,
        6,
    );
    let list = ListBox::builder(&dialog).build();
    ui::set_a11y_name(&list, &t!("accounts.list_a11y"));
    v.add(&list, 1, SizerFlag::Expand | SizerFlag::All, 6);

    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let btn_new = Button::builder(&dialog).with_label(&t!("accounts.create_btn")).build();
    let btn_pw = Button::builder(&dialog)
        .with_label(&t!("accounts.reset_pw_btn"))
        .build();
    let btn_role = Button::builder(&dialog).with_label(&t!("accounts.role_btn")).build();
    let btn_del = Button::builder(&dialog).with_label(&t!("accounts.delete_btn")).build();
    let btn_refresh = Button::builder(&dialog).with_label(&t!("common.refresh")).build();
    for b in [&btn_new, &btn_pw, &btn_role, &btn_del, &btn_refresh] {
        row.add(b, 0, SizerFlag::All, 4);
    }
    v.add_sizer(&row, 0, SizerFlag::All, 4);

    let reg_chk = CheckBox::builder(&dialog)
        .with_label(&t!("accounts.allow_registration"))
        .build();
    reg_chk.set_value(ctx.st.borrow().registration_open);
    ui::set_a11y_name(&reg_chk, &t!("accounts.allow_registration"));
    v.add(&reg_chk, 0, SizerFlag::All, 6);

    let close = Button::builder(&dialog).with_label(&t!("common.close")).build();
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
                notify(&ctx, &t!("accounts.select_first"), &t!("hub_admin.action_reset_pw"));
                return;
            };
            if let Some(pw) = ask_secret(&ctx, &t!("accounts.new_pw_for", name = name), &t!("hub_admin.action_reset_pw")) {
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
                notify(&ctx, &t!("accounts.select_first"), &t!("accounts.role_title"));
                return;
            };
            let new_role = if role == "admin" { "user" } else { "admin" };
            let q = MessageDialog::builder(
                &ctx.ui.frame,
                &t!("accounts.role_confirm", name = name, role = new_role),
                &t!("accounts.role_title"),
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
                notify(&ctx, &t!("accounts.select_first"), &t!("accounts.delete_title"));
                return;
            };
            let q = MessageDialog::builder(
                &ctx.ui.frame,
                &t!("accounts.delete_confirm", name = name),
                &t!("accounts.delete_title"),
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
    let Some(name) = ask_text(ctx, &t!("accounts.username"), &t!("accounts.create_title"), "") else {
        return;
    };
    let Some(pw) = ask_secret(ctx, &t!("accounts.password"), &t!("accounts.create_title")) else {
        return;
    };
    let role = if ask_admin(ctx, &t!("accounts.create_title")) { "admin" } else { "user" };
    send_or_status(
        ctx,
        Message::new(
            "account_create",
            serde_json::json!({ "username": name, "password": pw, "role": role }),
        ),
    );
}

fn change_password(ctx: &Ctx) {
    let Some(old) = ask_secret(ctx, &t!("accounts.current_pw"), &t!("accounts.change_pw_title")) else {
        return;
    };
    let Some(new) = ask_secret(ctx, &t!("accounts.new_pw"), &t!("accounts.change_pw_title")) else {
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
        announce(ctx, &t!("stream.none_running"), true);
        status(ctx, &t!("stream.none_running"));
        return;
    }
    let cur = (ctx.app.stream_volume() * 100.0).round() as i32;
    let next = (cur + delta_percent).clamp(0, 200);
    ctx.app.set_stream_volume(next as f32 / 100.0);
    let msg = t!("hotkeys.stream_volume", percent = next).to_string();
    announce(ctx, &msg, true);
    status(ctx, &msg);
}

/// Im laufenden Datei-Stream vor- oder zurückspulen (Sekunden).
fn stream_seek(ctx: &Ctx, secs: i32) {
    if !ctx.app.inner.lock().streaming_file {
        announce(ctx, &t!("stream.none_running"), true);
        status(ctx, &t!("stream.none_running"));
        return;
    }
    ctx.app.request_stream_seek(secs);
    let msg = if secs >= 0 {
        t!("hotkeys.seek_forward", secs = secs).to_string()
    } else {
        t!("hotkeys.seek_back", secs = -secs).to_string()
    };
    announce(ctx, &msg, true);
    status(ctx, &msg);
}

/// Lokale (nur für mich geltende) Lautstärke des im Baum fokussierten Nutzers
/// ändern.
fn user_volume_step(ctx: &Ctx, delta_percent: i32) {
    let Some(user_id) = selected_user(ctx) else {
        announce(ctx, &t!("hotkeys.no_user"), true);
        status(ctx, &t!("chat.select_user_first"));
        return;
    };
    let (nick, pct) = {
        let mut inner = ctx.app.inner.lock();
        let cur = (inner.user_volume(user_id) * 100.0).round() as i32;
        let next = (cur + delta_percent).clamp(0, 200);
        inner.user_volumes.insert(user_id, next as f32 / 100.0);
        (inner.nickname_of(user_id), next)
    };
    let msg = t!("hotkeys.user_volume", name = nick, percent = pct).to_string();
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

// ── Sprache ──

/// Sprache der Oberfläche wählen und speichern. Neue Dialoge und Meldungen
/// nutzen sie sofort; Menüs und feste Beschriftungen erst nach einem Neustart.
fn choose_language(ctx: &Ctx) {
    let current = config::load_config().language;
    let mut labels: Vec<String> = vec![t!("language.system").to_string()];
    labels.extend(crate::i18n::LANGUAGES.iter().map(|(_, name)| name.to_string()));
    let label_refs: Vec<&str> = labels.iter().map(|s| s.as_str()).collect();
    let dlg = SingleChoiceDialog::builder(&ctx.ui.frame, &t!("language.prompt"), &t!("language.title"), &label_refs).build();
    let preselect = crate::i18n::LANGUAGES
        .iter()
        .position(|(code, _)| *code == current)
        .map(|i| i as i32 + 1)
        .unwrap_or(0);
    dlg.set_selection(preselect);
    if dlg.show_modal() != ID_OK {
        return;
    }
    let sel = dlg.get_selection();
    if sel < 0 {
        return;
    }
    let language = if sel == 0 {
        String::new()
    } else {
        crate::i18n::LANGUAGES[(sel - 1) as usize].0.to_string()
    };
    let mut cfg = config::load_config();
    cfg.language = language.clone();
    let _ = config::save_config(&cfg);
    crate::i18n::init(&language);
    notify(ctx, &t!("language.restart"), &t!("language.title"));
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
            ctx.ui.append_chat(&t!("connect.disconnected"));
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
        ui::ID_LANGUAGE => choose_language(ctx),
        ui::ID_CHECK_UPDATE => crate::update::check_for_update(ctx, true),
        ui::ID_HELP_KEYS => info_box(ctx, &t!("help.keys_text"), &t!("help.keys_title")),
        ID_ABOUT => info_box(ctx, &t!("help.about_text"), &t!("help.about_title")),
        _ => {}
    }
}
