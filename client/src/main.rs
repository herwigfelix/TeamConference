//! TeamConference — barrierefreier Client mit nativer wxWidgets-Oberfläche (wxDragon).
//!
//! Architektur:
//!   - wxWidgets-Eventloop auf dem Hauptthread (UI, native Accessibility)
//!   - Tokio-Runtime im Hintergrund (WebSocket, UDP-Audio, Datei-Streaming)
//!   - Server-Nachrichten laufen über einen Kanal und werden von einem
//!     UI-Timer (alle 30 ms) auf dem Hauptthread verarbeitet. Widgets sind
//!     nicht Send, daher überqueren nur reine Daten den Thread.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod app;
mod audio;
mod config;
mod handlers;
mod net;
mod protocol;
mod roomtree;
mod state;
mod ui;
mod update;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use wxdragon::prelude::*;

use crate::app::{Ctx, UiState};
use crate::protocol::Message;
use crate::state::AppState;
use crate::ui::Ui;

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Tokio runtime");
    let rt_handle = rt.handle().clone();

    let app_state = Arc::new(AppState::new());
    let cfg = config::load_config();
    app_state.set_volume(cfg.volume);
    // Gespeicherte Audio-Einstellungen übernehmen
    {
        let mut inner = app_state.inner.lock();
        inner.audio_config.sample_rate = cfg.sample_rate;
        inner.audio_config.bit_depth = cfg.bit_depth;
        inner.audio_config.channels = cfg.channels;
    }
    // Klon für das Speichern der Lautstärke nach dem Event-Loop (app_state wird in die Closure verschoben)
    let app_state_exit = app_state.clone();

    // Kanal: Netzwerk-Tasks → UI-Thread
    let (ev_tx, ev_rx) = tokio::sync::mpsc::unbounded_channel::<Message>();
    let ev_rx = Rc::new(RefCell::new(ev_rx));

    let _ = wxdragon::main(move |_| {
        let frame = Frame::builder()
            .with_title("TeamConference")
            .with_size(Size::new(1080, 720))
            .build();
        frame.centre();

        let ui = Ui::build(frame);

        let ctx = Ctx {
            ui,
            app: app_state.clone(),
            rt: rt_handle.clone(),
            ev_tx: ev_tx.clone(),
            st: Rc::new(RefCell::new(UiState {
                servers: cfg.servers.clone(),
                files: Vec::new(),
                tree_map: std::collections::HashMap::new(),
                registration_open: false,
                ..Default::default()
            })),
        };

        // Serverliste füllen und ggf. Formular mit erstem Eintrag vorbelegen
        handlers::rebuild_server_list(&ctx);
        ui.volume
            .set_value((cfg.volume * 100.0).clamp(0.0, 200.0) as i32);
        if let Some(first) = ctx.st.borrow().servers.first().cloned() {
            ui.host_in.set_value(&first.host);
            ui.port_in.set_value(&first.port.to_string());
            ui.ssl_chk.set_value(first.ssl);
            ui.user_in.set_value(&first.username);
            ui.nick_in.set_value(&first.nickname);
            ui.pass_in.set_value(&first.password);
        }

        wire_events(&ctx);

        // Beim Start still nach einer neueren Version suchen (fragt nur nach,
        // wenn tatsächlich ein Update vorliegt).
        update::check_for_update(&ctx, false);

        // UI-Timer: Server-Nachrichten vom Kanal abholen und verarbeiten
        {
            let ctx = ctx.clone();
            let ev_rx = ev_rx.clone();
            let timer = Timer::new(&ui.frame);
            timer.on_tick(move |_| {
                let mut rx = ev_rx.borrow_mut();
                for _ in 0..50 {
                    match rx.try_recv() {
                        Ok(msg) => handlers::handle(&ctx, msg),
                        Err(_) => break,
                    }
                }
            });
            timer.start(30, false);
            // Timer am Leben halten (lokale Variable würde sonst gedroppt)
            std::mem::forget(timer);
        }

        ui.frame.show(true);
    });

    // Beim Beenden Lautstärke sichern
    let mut saved = config::load_config();
    saved.volume = app_state_exit.volume();
    let _ = config::save_config(&saved);
}

/// Verbindet alle Buttons, Menüs und Listen mit ihren Aktionen.
fn wire_events(ctx: &Ctx) {
    let ui = ctx.ui;

    // Menüleiste (ein Handler für alle IDs)
    {
        let ctx = ctx.clone();
        ui.frame
            .on_menu(move |event| actions::handle_menu(&ctx, event.get_id()));
    }

    // Verbindungsansicht
    {
        let ctx = ctx.clone();
        ui.connect_btn.on_click(move |_| actions::do_connect(&ctx));
    }
    {
        let ctx = ctx.clone();
        ui.bookmark_btn.on_click(move |_| actions::save_bookmark(&ctx));
    }
    {
        let ctx = ctx.clone();
        ui.remove_btn.on_click(move |_| actions::remove_server(&ctx));
    }
    {
        let ctx = ctx.clone();
        ui.server_list
            .on_selection_changed(move |_| actions::fill_form_from_server(&ctx));
    }

    // Hauptansicht
    {
        let ctx = ctx.clone();
        ui.send_btn.on_click(move |_| actions::send_chat(&ctx));
    }
    {
        let ctx = ctx.clone();
        ui.chat_in.on_text_enter(move |_| actions::send_chat(&ctx));
    }
    // Beitreten-Knopf nur auf macOS/Linux
    #[cfg(not(target_os = "windows"))]
    {
        let ctx = ctx.clone();
        ui.join_btn.on_click(move |_| actions::join_selected(&ctx));
    }
    {
        let ctx = ctx.clone();
        ui.download_btn
            .on_click(move |_| actions::handle_menu(&ctx, ui::ID_DOWNLOAD));
    }
    {
        let ctx = ctx.clone();
        ui.volume.on_slider(move |_| actions::volume_changed(&ctx));
    }
    // Enter/Doppelklick auf einen Baumeintrag tritt dem Raum bei
    {
        let ctx = ctx.clone();
        ui.rooms_tree
            .on_item_activated(move |_| actions::join_selected(&ctx));
    }
}
