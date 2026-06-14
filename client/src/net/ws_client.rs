use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite;

use crate::protocol::Message;
use crate::state::AppState;

/// Dangerous TLS verifier that accepts all certificates (for self-signed certs).
#[derive(Debug)]
struct NoVerifier;

impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls_pki_types::CertificateDer<'_>,
        _intermediates: &[rustls_pki_types::CertificateDer<'_>],
        _server_name: &rustls_pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls_pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &rustls_pki_types::CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Connect to the server via WebSocket (optionally with TLS).
/// Spawns background tasks that:
///   - Forward outgoing messages from the channel to the WebSocket
///   - Update shared state from incoming messages and forward every
///     message (plus a synthetic `connection_lost`) to `event_tx` for the UI
pub async fn connect(
    host: &str,
    port: u16,
    ssl: bool,
    state: Arc<AppState>,
    event_tx: mpsc::UnboundedSender<Message>,
) -> Result<mpsc::UnboundedSender<Message>, String> {
    let scheme = if ssl { "wss" } else { "ws" };
    let url = format!("{}://{}:{}", scheme, host, port);

    let ws_stream = if ssl {
        // Build a TLS config that accepts self-signed certs
        let tls_config = rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("TLS config error: {}", e))?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(NoVerifier))
        .with_no_client_auth();

        let connector = tokio_tungstenite::Connector::Rustls(Arc::new(tls_config));
        let (stream, _) =
            tokio_tungstenite::connect_async_tls_with_config(&url, None, false, Some(connector))
                .await
                .map_err(|e| format!("WSS connection failed: {}", e))?;
        stream
    } else {
        let (stream, _) = tokio_tungstenite::connect_async(&url)
            .await
            .map_err(|e| format!("WS connection failed: {}", e))?;
        stream
    };

    let (mut ws_tx, mut ws_rx) = ws_stream.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();

    // Store sender in state
    {
        let mut inner = state.inner.lock();
        inner.ws_tx = Some(tx.clone());
        inner.connected = true;
    }

    // Task: forward outgoing messages from channel → WebSocket
    let state_clone = state.clone();
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if let Ok(json) = serde_json::to_string(&msg) {
                if ws_tx
                    .send(tungstenite::Message::Text(json.into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
        }
        // Connection lost on send side
        let mut inner = state_clone.inner.lock();
        inner.connected = false;
        inner.ws_tx = None;
    });

    // Task: receive incoming messages from WebSocket → state + UI events
    let state_clone2 = state.clone();
    tokio::spawn(async move {
        while let Some(Ok(msg)) = ws_rx.next().await {
            let text = match msg {
                tungstenite::Message::Text(t) => t.to_string(),
                tungstenite::Message::Close(_) => break,
                _ => continue,
            };

            let parsed: Message = match serde_json::from_str(&text) {
                Ok(m) => m,
                Err(_) => continue,
            };

            pre_handle_message(&parsed, &state_clone2);

            if parsed.msg_type != "pong" {
                let _ = event_tx.send(parsed);
            }
        }

        // Connection lost
        {
            let mut inner = state_clone2.inner.lock();
            inner.connected = false;
            inner.authenticated = false;
            inner.ws_tx = None;
        }
        let _ = event_tx.send(Message::new("connection_lost", serde_json::json!({})));
    });

    Ok(tx)
}

/// State updates that must happen in the network task (the UI handler
/// only renders): auth bookkeeping, room list cache and the audio
/// capture auto-start once the server hands out the UDP token.
fn pre_handle_message(msg: &Message, state: &Arc<AppState>) {
    match msg.msg_type.as_str() {
        "auth_response" => {
            if let Ok(resp) =
                serde_json::from_value::<crate::protocol::AuthResponse>(msg.data.clone())
            {
                if resp.success {
                    let mut inner = state.inner.lock();
                    inner.authenticated = true;
                    inner.user_id = resp.user_id;
                    inner.server_name = resp.server_name.clone();
                    inner.self_role = resp.role.clone();
                    if let Some(ref rooms) = resp.rooms {
                        inner.rooms = rooms.clone();
                    }
                    inner.rebuild_token_map();
                }
            }
        }

        "room_list" => {
            if let Some(rooms) = msg.data.get("rooms") {
                if let Ok(room_list) =
                    serde_json::from_value::<Vec<crate::protocol::RoomInfo>>(rooms.clone())
                {
                    let mut inner = state.inner.lock();
                    inner.rooms = room_list;
                    inner.rebuild_token_map();
                    // Eigene Rolle aktuell halten (z. B. nach Admin-Rollenwechsel).
                    if let Some(role) = inner.role_in_rooms() {
                        inner.self_role = Some(role);
                    }
                }
            }
        }

        "audio_config_ack" => {
            if let Ok(ack) =
                serde_json::from_value::<crate::protocol::AudioConfigAck>(msg.data.clone())
            {
                tracing::info!(
                    "audio_config_ack: success={}, udp_token={:?}",
                    ack.success,
                    ack.udp_token
                );
                if ack.success {
                    let mut inner = state.inner.lock();
                    inner.session_token = ack.udp_token;
                    let already_capturing = inner.capturing;
                    let input_device = inner.input_device.clone();
                    drop(inner);

                    // Auto-start capture if not already running
                    if !already_capturing {
                        match crate::audio::capture::start_capture(state.clone(), input_device) {
                            Ok((stream, shutdown_tx)) => {
                                let mut inner = state.inner.lock();
                                inner.capturing = true;
                                inner.capture_shutdown = Some(shutdown_tx);
                                // Keep cpal stream alive — dropping stops capture
                                std::mem::forget(stream);
                                tracing::info!("Audio capture auto-started");
                            }
                            Err(e) => {
                                tracing::error!("Failed to auto-start capture: {}", e);
                            }
                        }
                    }
                }
            }
        }

        _ => {}
    }
}
