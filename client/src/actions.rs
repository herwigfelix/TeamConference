//! Aktionen aus Menüs, Buttons, Tastatur und Dialogen.

use wxdragon::prelude::*;

use crate::app::Ctx;
use crate::config::{self, ServerEntry};
use crate::handlers::{rebuild_files, rebuild_server_list, rebuild_tree, refresh_status};
use crate::protocol::Message;
use crate::state::PendingUpload;
use crate::ui;

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
        notify(ctx, "Bereits verbunden — bitte zuerst trennen.", "Verbinden");
        return;
    }
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
    let username = ctx.ui.user_in.get_value().trim().to_string();
    let password = ctx.ui.pass_in.get_value();
    let mut nickname = ctx.ui.nick_in.get_value().trim().to_string();

    if host.is_empty() {
        notify(ctx, "Host ist erforderlich.", "Verbinden");
        return;
    }

    // Anmeldemodus: Passwort (klassisch) oder zentrales Token.
    enum Auth {
        Password { username: String, password: String },
        Central { refresh_token: String },
    }
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
                Auth::Central { refresh_token: h.refresh_token }
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
        Auth::Password { username, password }
    };

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

        // Login-Daten je nach Modus zusammenbauen. Beim zentralen Login holen
        // wir ein frisches Access-Token (Refresh-Rotation) und senden es als
        // `central_token`; das neue Refresh-Token wird lokal gespeichert.
        let login_data = match auth {
            Auth::Password { username, password } => {
                serde_json::json!({ "username": username, "password": password, "nickname": nickname })
            }
            Auth::Central { refresh_token } => {
                let res = tokio::task::spawn_blocking(move || crate::hub::refresh(&refresh_token)).await;
                match res {
                    Ok(Ok(bundle)) => {
                        let mut cfg = crate::config::load_config();
                        cfg.hub = Some(crate::config::HubSession {
                            central_uid: bundle.central_uid.clone(),
                            username: bundle.username.clone(),
                            display_name: bundle.display_name.clone(),
                            role: bundle.role.clone(),
                            refresh_token: bundle.refresh_token.clone(),
                            status: bundle.status.clone(),
                        });
                        let _ = crate::config::save_config(&cfg);
                        serde_json::json!({ "central_token": bundle.access_token, "nickname": nickname })
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
        let login = Message::new("auth_login", login_data);
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
        inner.self_role = None;
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
                format!("Status: angemeldet als {}{}.", name, role)
            } else {
                format!("Status: {} — Konto wartet auf Freigabe.", name)
            }
        }
        None => "Status: nicht angemeldet.".to_string(),
    };
    ctx.ui.hub_status.set_label(&label);
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
    // Sitzung lokal entfernen.
    let mut cfg = config::load_config();
    cfg.hub = None;
    let _ = config::save_config(&cfg);
    let ev_tx = ctx.ev_tx.clone();
    let refresh = h.refresh_token.clone();
    ctx.rt.spawn(async move {
        let _ = tokio::task::spawn_blocking(move || crate::hub::logout(&refresh)).await;
        hub_msg(&ev_tx, "Abgemeldet.".to_string());
    });
}

pub fn hub_reset_start(ctx: &Ctx) {
    let phone = ctx.ui.hub_phone_in.get_value().trim().to_string();
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
    ctx.ui.append_hub_log("Lade Verzeichnis…");
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || {
            let access = fresh_access_token()?;
            crate::hub::list_servers(&access, &q)
        })
        .await;
        match r {
            Ok(Ok(servers)) => {
                let _ = ev_tx.send(Message::new(
                    "hub_servers",
                    serde_json::json!({ "servers": servers }),
                ));
            }
            Ok(Err(e)) => hub_msg(&ev_tx, format!("Verzeichnis konnte nicht geladen werden: {}", e)),
            Err(e) => hub_msg(&ev_tx, format!("Fehler: {}", e)),
        }
    });
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
    // Verbindungsformular füllen, zentrales Login wählen, zur Serverliste
    // wechseln und verbinden.
    ctx.ui.host_in.set_value(&s.host);
    ctx.ui.port_in.set_value(&s.control_port.to_string());
    ctx.ui.ssl_chk.set_value(true);
    ctx.ui.use_central_chk.set_value(true);
    ctx.ui.notebook.set_selection(0);
    do_connect(ctx);
}

pub fn hub_create_server(ctx: &Ctx) {
    if config::load_config().hub.is_none() {
        notify(ctx, "Bitte zuerst im Server-Hub anmelden.", "Server-Hub");
        return;
    }
    let Some(name) = ask_text(ctx, "Name des Servers:", "Server anlegen", "") else { return };
    let description = ask_text(ctx, "Beschreibung (optional):", "Server anlegen", "").unwrap_or_default();
    let host = ask_text(ctx, "Adresse/Host des TeamConference-Servers:", "Server anlegen", "").unwrap_or_default();
    let control_port: i64 = ask_text(ctx, "Steuer-Port:", "Server anlegen", "10001")
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(10001);
    let audio_port = control_port + 1;
    let is_public = {
        let dlg = MessageDialog::builder(&ctx.ui.frame, "Soll der Server öffentlich im Verzeichnis erscheinen?", "Server anlegen")
            .with_style(MessageDialogStyle::YesNo)
            .build();
        dlg.show_modal() == ID_YES
    };
    ctx.ui.append_hub_log("Lege Server an…");
    let ev_tx = ctx.ev_tx.clone();
    ctx.rt.spawn(async move {
        let r = tokio::task::spawn_blocking(move || {
            let access = fresh_access_token()?;
            crate::hub::create_server(&access, &name, &description, is_public, &host, control_port, audio_port)
        })
        .await;
        let m = match r {
            Ok(Ok(_id)) => "Server angelegt. „Verzeichnis laden\" aktualisiert die Liste.".to_string(),
            Ok(Err(e)) => format!("Server konnte nicht angelegt werden: {}", e),
            Err(e) => format!("Fehler: {}", e),
        };
        hub_msg(&ev_tx, m);
    });
}

pub fn hub_reset_confirm(ctx: &Ctx) {
    let phone = ctx.ui.hub_phone_in.get_value().trim().to_string();
    let code = ctx.ui.hub_code_in.get_value().trim().to_string();
    let new_pass = ctx.ui.hub_reg_pass_in.get_value();
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
    if let Some(pw) = password {
        data["password"] = serde_json::Value::String(pw);
    }
    if !send_or_status(ctx, Message::new("room_join", data)) {
        return;
    }
    // Audio-Qualität wird vom Raum bestimmt: Werte aus der RoomInfo übernehmen.
    let (sr, bd, ch) = {
        let mut inner = ctx.app.inner.lock();
        inner.current_room_id = Some(room_id);
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
    ui::set_a11y_name(&name_in, "Raumname");
    add_row(&dialog, &v, "Name:", &name_in);

    // Passwort ohne Checkbox: gilt, sobald etwas drinsteht.
    let pw_in = TextCtrl::builder(&dialog)
        .with_style(TextCtrlStyle::Password)
        .build();
    ui::set_a11y_name(&pw_in, "Passwort");
    add_row(
        &dialog,
        &v,
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
    add_row(&dialog, &v, "Max. Nutzer (0 = ∞):", &max_in);

    // Vorlagen-Auswahl
    let template_choice = Choice::builder(&dialog).build();
    for t in &TEMPLATES {
        template_choice.append(t.label);
    }
    template_choice.append("Erweitert (frei einstellbar)");
    ui::set_a11y_name(&template_choice, "Vorlage für die Audio-Qualität");
    add_row(&dialog, &v, "Vorlage:", &template_choice);

    // Erweiterte Audio-Felder (anfangs ggf. ausgeblendet)
    let rate_choice = Choice::builder(&dialog).build();
    for r in RATES {
        rate_choice.append(&r.to_string());
    }
    rate_choice.set_selection(RATES.iter().position(|&r| r == sr0).unwrap_or(0) as u32);
    ui::set_a11y_name(&rate_choice, "Samplerate in Hertz");
    let rate_lbl = add_row(&dialog, &v, "Samplerate (Hz):", &rate_choice);

    let depth_choice = Choice::builder(&dialog).build();
    for d in DEPTHS {
        depth_choice.append(&d.to_string());
    }
    depth_choice.set_selection(DEPTHS.iter().position(|&d| d == bd0).unwrap_or(0) as u32);
    ui::set_a11y_name(&depth_choice, "Bittiefe");
    let depth_lbl = add_row(&dialog, &v, "Bittiefe (Bit):", &depth_choice);

    let ch_choice = Choice::builder(&dialog).build();
    ch_choice.append("Mono");
    ch_choice.append("Stereo");
    ch_choice.set_selection(if ch0 >= 2 { 1 } else { 0 });
    ui::set_a11y_name(&ch_choice, "Kanäle Mono oder Stereo");
    let ch_lbl = add_row(&dialog, &v, "Kanäle:", &ch_choice);

    let bitrate_in = TextCtrl::builder(&dialog).build();
    bitrate_in.set_value(&br0_kbps.to_string());
    ui::set_a11y_name(&bitrate_in, "Bitrate in kbit pro Sekunde, 0 = automatisch");
    let br_lbl = add_row(&dialog, &v, "Bitrate (kbit/s, 0 = auto):", &bitrate_in);

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
    let cancel = Button::builder(&dialog).with_label("Abbrechen").build();
    let ok = Button::builder(&dialog).with_label("Speichern").build();
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
}

/// Dialog zur Auswahl von Mikrofon und Lautsprecher. Die Audio-Qualität
/// (Samplerate, Mono/Stereo) bestimmt dagegen der Raum (siehe Raum-Dialog).
fn audio_settings(ctx: &Ctx) {
    let devices = crate::audio::device::list_devices();
    let mut inputs: Vec<String> = vec!["Standardgerät".to_string()];
    let mut outputs: Vec<String> = vec!["Standardgerät".to_string()];
    for d in &devices {
        if d.is_input {
            inputs.push(d.name.clone());
        }
        if d.is_output {
            outputs.push(d.name.clone());
        }
    }

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

    let dialog = Dialog::builder(&ctx.ui.frame, "Audiogeräte").build();
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
    for d in &inputs {
        in_choice.append(d);
    }
    in_choice.set_selection(in_idx as u32);
    ui::set_a11y_name(&in_choice, "Mikrofon");
    row("Mikrofon:", &in_choice);

    let out_choice = Choice::builder(&dialog).build();
    for d in &outputs {
        out_choice.append(d);
    }
    out_choice.set_selection(out_idx as u32);
    ui::set_a11y_name(&out_choice, "Lautsprecher");
    row("Lautsprecher:", &out_choice);

    // Toggle: Server-Ereignisse per Sprachausgabe ansagen (Standard an).
    let announce_chk = CheckBox::builder(&dialog)
        .with_label("Server-Ereignisse ansagen")
        .build();
    announce_chk.set_value(ctx.st.borrow().announce_events);
    ui::set_a11y_name(&announce_chk, "Server-Ereignisse per Sprachausgabe ansagen");
    v.add(&announce_chk, 0, SizerFlag::All, 6);

    let btns = BoxSizer::builder(Orientation::Horizontal).build();
    let cancel = Button::builder(&dialog).with_label("Abbrechen").build();
    let ok = Button::builder(&dialog).with_label("Speichern").build();
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
        // Index 0 = Standardgerät = None
        let pick = |sel: Option<u32>, list: &[String]| -> Option<String> {
            match sel {
                Some(0) | None => None,
                Some(i) => list.get(i as usize).cloned(),
            }
        };
        let input = pick(in_choice.get_selection(), &inputs);
        let output = pick(out_choice.get_selection(), &outputs);
        let announce_events = announce_chk.is_checked();
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
        let _ = config::save_config(&cfg);
        dialog.destroy();
        notify(
            ctx,
            "Audiogeräte gespeichert. Gilt ab der nächsten Verbindung.",
            "Audiogeräte",
        );
    } else {
        dialog.destroy();
    }
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

    let row = BoxSizer::builder(Orientation::Horizontal).build();
    let btn_new = Button::builder(&dialog).with_label("Konto anlegen…").build();
    let btn_pw = Button::builder(&dialog)
        .with_label("Passwort zurücksetzen…")
        .build();
    let btn_role = Button::builder(&dialog).with_label("Rolle ändern…").build();
    let btn_del = Button::builder(&dialog).with_label("Konto löschen…").build();
    let btn_refresh = Button::builder(&dialog).with_label("Aktualisieren").build();
    for b in [&btn_new, &btn_pw, &btn_role, &btn_del, &btn_refresh] {
        row.add(b, 0, SizerFlag::All, 4);
    }
    v.add_sizer(&row, 0, SizerFlag::All, 4);

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
