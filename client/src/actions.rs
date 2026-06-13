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
    let username = ctx.ui.user_in.get_value().trim().to_string();
    let password = ctx.ui.pass_in.get_value();
    let mut nickname = ctx.ui.nick_in.get_value().trim().to_string();
    if nickname.is_empty() {
        nickname = username.clone();
    }
    if host.is_empty() || username.is_empty() || password.is_empty() {
        notify(
            ctx,
            "Host, Benutzername und Passwort sind erforderlich.",
            "Verbinden",
        );
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

    set_menu_check(ctx, ui::ID_TOGGLE_MUTE, false);
    set_menu_check(ctx, ui::ID_TOGGLE_DEAFEN, false);
    set_menu_check(ctx, ui::ID_TOGGLE_LOOPBACK, false);
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
        ctx.ui.pass_in.set_value(&s.password);
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
    // Audio-Qualität wird vom Raum bestimmt: Werte aus der RoomInfo übernehmen.
    let (sr, bd, ch) = {
        let mut inner = ctx.app.inner.lock();
        inner.current_room_id = Some(room_id);
        let (mut sr, mut bd, mut ch) = inner
            .rooms
            .iter()
            .find(|r| r.id == room_id)
            .map(|r| (r.sample_rate, r.bit_depth, r.channels))
            .unwrap_or((48000, 16, 1));
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

/// Was der Raum-Dialog erstellt/bearbeitet.
enum RoomMode {
    Create { parent: Option<i64> },
    Edit { room_id: i64 },
}

/// Gemeinsamer Dialog zum Erstellen und Bearbeiten eines Raums:
/// Name, Passwort, max. Nutzer und die raumweite Audio-Qualität.
fn room_dialog(ctx: &Ctx, mode: RoomMode) {
    // Vorbelegung bei Bearbeiten
    let (title, name0, max0, sr0, bd0, ch0, had_pw) = match &mode {
        RoomMode::Create { parent } => {
            let t = if parent.is_some() {
                "Unterraum erstellen"
            } else {
                "Raum erstellen"
            };
            (t, String::new(), 0i64, 48000i64, 16i64, 1i64, false)
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
                    r.has_password,
                ),
                None => ("Raum bearbeiten", String::new(), 0, 48000, 16, 1, false),
            }
        }
    };

    let dialog = Dialog::builder(&ctx.ui.frame, title).build();
    let v = BoxSizer::builder(Orientation::Vertical).build();
    // generisch (Sizer::add braucht Sized), daher freie fn statt Closure
    fn add_row<W: WxWidget>(dialog: &Dialog, v: &BoxSizer, label: &str, ctrl: &W) {
        let r = BoxSizer::builder(Orientation::Horizontal).build();
        r.add(
            &StaticText::builder(dialog).with_label(label).build(),
            0,
            SizerFlag::AlignCenterVertical | SizerFlag::All,
            6,
        );
        r.add(ctrl, 1, SizerFlag::Expand | SizerFlag::All, 6);
        v.add_sizer(&r, 0, SizerFlag::Expand, 0);
    }

    let name_in = TextCtrl::builder(&dialog).build();
    name_in.set_value(&name0);
    ui::set_a11y_name(&name_in, "Raumname");
    add_row(&dialog, &v, "Name:", &name_in);

    // Passwort: Checkbox steuert, ob es gesetzt/geändert wird
    let pw_chk = CheckBox::builder(&dialog)
        .with_label("Passwort setzen/ändern")
        .build();
    pw_chk.set_value(false);
    ui::set_a11y_name(&pw_chk, "Passwort setzen oder ändern");
    v.add(&pw_chk, 0, SizerFlag::All, 6);
    let pw_in = TextCtrl::builder(&dialog)
        .with_style(TextCtrlStyle::Password)
        .build();
    ui::set_a11y_name(&pw_in, "Passwort");
    add_row(
        &dialog,
        &v,
        if had_pw {
            "Passwort (leer = entfernen):"
        } else {
            "Passwort:"
        },
        &pw_in,
    );

    let max_in = TextCtrl::builder(&dialog).build();
    max_in.set_value(&max0.to_string());
    ui::set_a11y_name(&max_in, "Maximale Nutzerzahl, 0 = unbegrenzt");
    add_row(&dialog, &v, "Max. Nutzer (0 = ∞):", &max_in);

    let rate_choice = Choice::builder(&dialog).build();
    for r in RATES {
        rate_choice.append(&r.to_string());
    }
    rate_choice.set_selection(RATES.iter().position(|&r| r == sr0).unwrap_or(0) as u32);
    ui::set_a11y_name(&rate_choice, "Samplerate in Hertz");
    add_row(&dialog, &v, "Samplerate (Hz):", &rate_choice);

    let depth_choice = Choice::builder(&dialog).build();
    for d in DEPTHS {
        depth_choice.append(&d.to_string());
    }
    depth_choice.set_selection(DEPTHS.iter().position(|&d| d == bd0).unwrap_or(0) as u32);
    ui::set_a11y_name(&depth_choice, "Bittiefe");
    add_row(&dialog, &v, "Bittiefe (Bit):", &depth_choice);

    let ch_choice = Choice::builder(&dialog).build();
    ch_choice.append("Mono");
    ch_choice.append("Stereo");
    ch_choice.set_selection(if ch0 >= 2 { 1 } else { 0 });
    ui::set_a11y_name(&ch_choice, "Kanäle Mono oder Stereo");
    add_row(&dialog, &v, "Kanäle:", &ch_choice);

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
    let sample_rate = RATES[rate_choice.get_selection().unwrap_or(0) as usize];
    let bit_depth = DEPTHS[depth_choice.get_selection().unwrap_or(0) as usize];
    let channels = if ch_choice.get_selection().unwrap_or(0) == 1 { 2 } else { 1 };
    let pw_change = pw_chk.is_checked();
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
            });
            if let Some(pid) = parent {
                data["parent_id"] = serde_json::json!(pid);
            }
            if pw_change && !pw_value.is_empty() {
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
            });
            // Passwort nur anfassen, wenn die Checkbox aktiv ist:
            // leeres Feld = entfernen (null), sonst setzen.
            if pw_change {
                data["password"] = if pw_value.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String(pw_value)
                };
            }
            send_or_status(ctx, Message::new("room_update", data));
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
    let _ = ctx.app.send_ws(Message::new(
        "audio_loopback",
        serde_json::json!({ "enabled": loopback }),
    ));
    set_menu_check(ctx, ui::ID_TOGGLE_LOOPBACK, loopback);
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
        {
            let mut inner = ctx.app.inner.lock();
            inner.input_device = input.clone();
            inner.output_device = output.clone();
        }
        let mut cfg = config::load_config();
        cfg.input_device = input;
        cfg.output_device = output;
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
        ui::ID_AUDIO_SETTINGS => audio_settings(ctx),
        ui::ID_STREAM_FILE => stream_file(ctx),
        ui::ID_PAUSE_STREAM => toggle_pause_stream(ctx),
        ui::ID_STOP_STREAM => stop_stream(ctx),
        ui::ID_JOIN_ROOM => join_selected(ctx),
        ui::ID_LEAVE_ROOM => leave_room(ctx),
        ui::ID_CREATE_ROOM => room_dialog(ctx, RoomMode::Create { parent: None }),
        ui::ID_CREATE_SUBROOM => {
            if let Some(parent) = selected_room(ctx) {
                room_dialog(ctx, RoomMode::Create { parent: Some(parent) });
            } else {
                notify(ctx, "Bitte zuerst den übergeordneten Raum auswählen.", "Unterraum");
            }
        }
        ui::ID_EDIT_ROOM => edit_selected_room(ctx),
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
