//! TeamConference — barrierefreier Slint-Client.
//!
//! Architektur:
//!   - Slint-Eventloop auf dem Hauptthread (UI)
//!   - Tokio-Runtime im Hintergrund (WebSocket, UDP-Audio, Datei-Streaming)
//!   - Server-Nachrichten laufen über einen Kanal und werden mit
//!     `slint::invoke_from_event_loop` auf dem UI-Thread verarbeitet

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod audio;
mod config;
mod events;
mod net;
mod protocol;
mod state;

use std::sync::Arc;

use slint::ComponentHandle;
use tokio::sync::mpsc;

use crate::protocol::Message;
use crate::state::AppState;

slint::include_modules!();

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

    let state = Arc::new(AppState::new());
    let ui = MainWindow::new().expect("Slint window");

    // Plattformabhängige Kurztasten-Beschriftung
    ui.set_mod_name(if cfg!(target_os = "macos") { "Cmd" } else { "Strg" }.into());

    // Gespeicherte Einstellungen vorbelegen
    let cfg = config::load_config();
    ui.set_conn_host(cfg.host.clone().into());
    ui.set_conn_port(cfg.port.to_string().into());
    ui.set_conn_ssl(cfg.ssl);
    ui.set_conn_username(cfg.username.clone().into());
    ui.set_conn_nickname(cfg.nickname.clone().into());
    state.set_volume(cfg.volume);
    ui.set_volume((cfg.volume * 100.0).clamp(0.0, 100.0));

    // Kanal: Netzwerk-Tasks → UI-Thread
    let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<Message>();
    {
        let weak = ui.as_weak();
        let state2 = state.clone();
        let rt2 = rt_handle.clone();
        let ev_tx2 = ev_tx.clone();
        rt.spawn(async move {
            while let Some(msg) = ev_rx.recv().await {
                let weak = weak.clone();
                let state3 = state2.clone();
                let rt3 = rt2.clone();
                let ev_tx3 = ev_tx2.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = weak.upgrade() {
                        events::handle(&ui, &state3, &rt3, &ev_tx3, msg);
                    }
                });
            }
        });
    }

    // ── UI-Callbacks ──

    {
        let weak = ui.as_weak();
        let state2 = state.clone();
        let rt2 = rt_handle.clone();
        let ev_tx2 = ev_tx.clone();
        ui.on_request_connect(move || {
            if let Some(ui) = weak.upgrade() {
                actions::connect_clicked(&ui, &state2, &rt2, &ev_tx2);
            }
        });
    }

    {
        let weak = ui.as_weak();
        let state2 = state.clone();
        let rt2 = rt_handle.clone();
        let ev_tx2 = ev_tx.clone();
        ui.on_menu_action(move |action| {
            if let Some(ui) = weak.upgrade() {
                actions::menu_action(&ui, &state2, &rt2, &ev_tx2, action.as_str());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let state2 = state.clone();
        ui.on_send_chat(move || {
            if let Some(ui) = weak.upgrade() {
                actions::send_chat(&ui, &state2);
            }
        });
    }

    {
        let state2 = state.clone();
        ui.on_volume_changed(move |value| {
            actions::volume_changed(&state2, value);
        });
    }

    {
        let weak = ui.as_weak();
        let state2 = state.clone();
        ui.on_dialog_accepted(move |mode, f1, f2| {
            if let Some(ui) = weak.upgrade() {
                actions::dialog_accepted(&ui, &state2, mode.as_str(), f1.as_str(), f2.as_str());
            }
        });
    }

    {
        let weak = ui.as_weak();
        let state2 = state.clone();
        ui.on_settings_accepted(move |input, output| {
            if let Some(ui) = weak.upgrade() {
                actions::settings_accepted(&ui, &state2, input.as_str(), output.as_str());
            }
        });
    }

    ui.run().expect("Slint event loop");

    // Lautstärke beim Beenden sichern
    actions::save_volume(&state);
}
