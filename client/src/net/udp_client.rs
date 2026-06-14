use std::collections::HashMap;
use std::net::ToSocketAddrs;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::watch;

use crate::protocol::{AudioPacketHeader, build_audio_packet};
use crate::state::AppState;

/// Start the UDP audio pipeline:
///   1. Bind a local UDP socket
///   2. Spawn a recv task: UDP recv -> Opus decode -> channel convert -> playback_tx
///
/// Audio sending is handled by the capture module (capture.rs).
pub async fn start_udp_audio(
    server_host: &str,
    server_port: u16,
    state: Arc<AppState>,
) -> Result<(watch::Sender<bool>, watch::Sender<bool>), String> {
    let server_addr = format!("{}:{}", server_host, server_port);
    let resolved_addr = resolve_ipv4(&server_addr)?;

    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .map_err(|e| format!("UDP bind failed: {}", e))?;

    let local_addr = socket.local_addr().map(|a| a.to_string()).unwrap_or_default();
    let socket = Arc::new(socket);

    tracing::info!("UDP audio: local={}, server={} (resolved={})", local_addr, server_addr, resolved_addr);

    {
        let mut inner = state.inner.lock();
        inner.udp_socket = Some(socket.clone());
        inner.server_udp_addr = Some(resolved_addr.clone());
    }

    let (send_shutdown_tx, _) = watch::channel(false);

    // Kanal für lokal eingespeistes Audio (gestreamte Datei zum Mithören).
    let (local_tx, mut local_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<i16>>();
    *state.local_audio_tx.lock() = Some(local_tx);

    // -- Recv task: UDP -> Opus decode (pro Quelle) -> mischen -> playback --
    let (recv_shutdown_tx, mut recv_shutdown_rx) = watch::channel(false);
    let recv_socket = socket.clone();
    let recv_state = state.clone();

    tokio::spawn(async move {
        let mut buf = [0u8; 65536];
        let mut packets_received: u64 = 0;
        let mut parse_failures: u64 = 0;

        // Ein Opus-Decoder je logischer Quelle: (token, source_id, channels).
        // So verfälschen sich gleichzeitige Ströme desselben Nutzers (Mikro +
        // Datei) nicht gegenseitig den Decoder-Zustand.
        let mut decoders: HashMap<(u32, u8, u8), opus::Decoder> = HashMap::new();
        let mut pcm_out = vec![0i16; 960 * 2];

        // Misch-Akkumulator: überlappende Quellen werden je Sample summiert.
        // Geflusht wird, sobald dieselbe Quelle erneut sendet (= ein neues
        // 20-ms-Fenster beginnt). Bei nur einer Quelle also Frame für Frame wie
        // der direkte Pfad (kein Wanduhr-Takt → kein Schweben/Stottern); bei
        // mehreren Quellen werden gleichzeitige Frames gemischt. Eine kurze
        // Inaktivitäts-Frist gibt ein letztes hängendes Frame aus, wenn Quellen
        // verstummen.
        let mut acc: Vec<i32> = Vec::new();
        let mut acc_sources: Vec<(u32, u8)> = Vec::new();
        let mut acc_started: Option<tokio::time::Instant> = None;
        let mut safety = tokio::time::interval(tokio::time::Duration::from_millis(8));

        loop {
            tokio::select! {
                _ = recv_shutdown_rx.changed() => { break; }

                // Sicherheits-Flush: hängendes Frame ausgeben, wenn ~24 ms lang
                // keine weitere (wiederholende) Quelle kam.
                _ = safety.tick() => {
                    if let Some(t0) = acc_started {
                        if t0.elapsed() >= tokio::time::Duration::from_millis(24) {
                            flush_acc(&mut acc, &mut acc_sources, &mut acc_started, &recv_state);
                        }
                    }
                }

                // Lokal eingespeistes Audio (gestreamte Datei, Wiedergabeformat).
                Some(frame) = local_rx.recv() => {
                    let key = (u32::MAX, crate::protocol::SOURCE_FILE);
                    if acc_sources.contains(&key) {
                        flush_acc(&mut acc, &mut acc_sources, &mut acc_started, &recv_state);
                    }
                    acc_add_i16(&mut acc, &frame);
                    acc_sources.push(key);
                    if acc_started.is_none() { acc_started = Some(tokio::time::Instant::now()); }
                }

                result = recv_socket.recv_from(&mut buf) => {
                    match result {
                        Ok((len, addr)) => {
                            if let Some(header) = AudioPacketHeader::parse(&buf[..len]) {
                                let payload = header.payload(&buf[..len]);

                                let (deafened, own_token) = {
                                    let inner = recv_state.inner.lock();
                                    (inner.deafened, inner.session_token.unwrap_or(0))
                                };

                                // Eigenen, vom Server zurückgespiegelten Datei-Strom
                                // (Loopback) verwerfen – er wird bereits lokal
                                // mitgehört, sonst doppelt.
                                let own_file = header.token == own_token
                                    && header.source_id == crate::protocol::SOURCE_FILE;

                                if packets_received == 0 {
                                    tracing::info!(
                                        "UDP recv: first packet from {}, len={}, sr={}, ch={}, src={}, bd={}, payload={}, deafened={}",
                                        addr, len, header.sample_rate, header.channels, header.source_id,
                                        header.bit_depth, payload.len(), deafened
                                    );
                                }

                                if !deafened && !own_file && !payload.is_empty() {
                                    let ch = header.channels.max(1);
                                    let pcm: Option<Vec<u8>> = if header.bit_depth == 0 {
                                        let key = (header.token, header.source_id, ch);
                                        let decoder = decoders.entry(key).or_insert_with(|| {
                                            let oc = if ch <= 1 { opus::Channels::Mono } else { opus::Channels::Stereo };
                                            opus::Decoder::new(48000, oc).expect("Opus decoder creation failed")
                                        });
                                        let out_samples = 960 * (ch as usize);
                                        if pcm_out.len() < out_samples {
                                            pcm_out.resize(out_samples, 0);
                                        }
                                        match decoder.decode(payload, &mut pcm_out[..out_samples], false) {
                                            Ok(frames) => {
                                                let total = frames * (ch as usize);
                                                let mut bytes = Vec::with_capacity(total * 2);
                                                for &s in &pcm_out[..total] {
                                                    bytes.extend_from_slice(&s.to_le_bytes());
                                                }
                                                Some(bytes)
                                            }
                                            Err(e) => {
                                                if packets_received < 5 {
                                                    tracing::warn!("UDP recv: Opus decode failed: {}", e);
                                                }
                                                None
                                            }
                                        }
                                    } else {
                                        Some(payload.to_vec())
                                    };

                                    if let Some(pcm_data) = pcm {
                                        let dev_ch = recv_state.inner.lock().playback_device_channels;
                                        let converted = convert_channels(&pcm_data, ch as u16, dev_ch);
                                        let key = (header.token, header.source_id);
                                        // Wiederholt sich die Quelle, beginnt ein neues
                                        // Fenster → vorheriges Gemisch ausgeben.
                                        if acc_sources.contains(&key) {
                                            flush_acc(&mut acc, &mut acc_sources, &mut acc_started, &recv_state);
                                        }
                                        acc_add_bytes(&mut acc, &converted);
                                        acc_sources.push(key);
                                        if acc_started.is_none() { acc_started = Some(tokio::time::Instant::now()); }
                                    }
                                }

                                packets_received += 1;
                            } else {
                                parse_failures += 1;
                                if parse_failures <= 3 {
                                    tracing::warn!("UDP recv: failed to parse audio header, len={}, first 4 bytes={:?}",
                                        len, &buf[..len.min(4)]);
                                }
                            }
                        }
                        Err(e) => {
                            tracing::warn!("UDP recv error: {}", e);
                        }
                    }
                }
            }
        }
        tracing::info!("UDP recv task ended, total received: {}, parse failures: {}", packets_received, parse_failures);
    });

    Ok((send_shutdown_tx, recv_shutdown_tx))
}

/// i16-LE-Bytes in den Misch-Akkumulator addieren (resize bei Bedarf).
fn acc_add_bytes(acc: &mut Vec<i32>, bytes: &[u8]) {
    let n = bytes.len() / 2;
    if acc.len() < n {
        acc.resize(n, 0);
    }
    for i in 0..n {
        acc[i] += i16::from_le_bytes([bytes[i * 2], bytes[i * 2 + 1]]) as i32;
    }
}

/// i16-Samples in den Misch-Akkumulator addieren (resize bei Bedarf).
fn acc_add_i16(acc: &mut Vec<i32>, samples: &[i16]) {
    if acc.len() < samples.len() {
        acc.resize(samples.len(), 0);
    }
    for (i, &v) in samples.iter().enumerate() {
        acc[i] += v as i32;
    }
}

/// Aktuelles Gemisch an die Wiedergabe geben und den Akkumulator zurücksetzen.
fn flush_acc(
    acc: &mut Vec<i32>,
    acc_sources: &mut Vec<(u32, u8)>,
    acc_started: &mut Option<tokio::time::Instant>,
    state: &Arc<AppState>,
) {
    acc_sources.clear();
    *acc_started = None;
    if acc.is_empty() {
        return;
    }
    let out = acc_flush(acc); // leert acc
    let tx = state.playback_tx.lock();
    if let Some(ref tx) = *tx {
        let _ = tx.try_send(out);
    }
}

/// Akkumulator zu i16-LE-Bytes (mit Begrenzung) leeren.
fn acc_flush(acc: &mut Vec<i32>) -> Vec<u8> {
    let mut out = Vec::with_capacity(acc.len() * 2);
    for &v in acc.iter() {
        out.extend_from_slice(&(v.clamp(-32768, 32767) as i16).to_le_bytes());
    }
    acc.clear();
    out
}

/// Resolve a host:port string to a concrete IPv4 SocketAddr string.
fn resolve_ipv4(addr: &str) -> Result<String, String> {
    let addrs: Vec<_> = addr
        .to_socket_addrs()
        .map_err(|e| format!("Failed to resolve {}: {}", addr, e))?
        .collect();

    for a in &addrs {
        if a.is_ipv4() {
            return Ok(a.to_string());
        }
    }

    addrs
        .first()
        .map(|a| a.to_string())
        .ok_or_else(|| format!("No addresses resolved for {}", addr))
}

/// Convert i16 LE PCM bytes from one channel count to another.
pub fn convert_channels(data: &[u8], from_ch: u16, to_ch: u16) -> Vec<u8> {
    if from_ch == to_ch || from_ch == 0 || to_ch == 0 {
        return data.to_vec();
    }

    let from = from_ch as usize;
    let to = to_ch as usize;

    let sample_count = data.len() / 2;
    let frame_count = sample_count / from;
    let mut output = Vec::with_capacity(frame_count * to * 2);

    for frame in 0..frame_count {
        let base = frame * from * 2;

        if from == 1 && to >= 2 {
            if base + 1 < data.len() {
                let sample_bytes = [data[base], data[base + 1]];
                for _ in 0..to {
                    output.extend_from_slice(&sample_bytes);
                }
            }
        } else if from >= 2 && to == 1 {
            let mut sum: i32 = 0;
            for ch in 0..from {
                let offset = base + ch * 2;
                if offset + 1 < data.len() {
                    sum += i16::from_le_bytes([data[offset], data[offset + 1]]) as i32;
                }
            }
            let avg = (sum / from as i32).clamp(-32768, 32767) as i16;
            output.extend_from_slice(&avg.to_le_bytes());
        } else {
            for ch in 0..to {
                if ch < from {
                    let offset = base + ch * 2;
                    if offset + 1 < data.len() {
                        output.extend_from_slice(&[data[offset], data[offset + 1]]);
                    } else {
                        output.extend_from_slice(&[0, 0]);
                    }
                } else {
                    output.extend_from_slice(&[0, 0]);
                }
            }
        }
    }

    output
}

/// Send audio data to the server via UDP.
#[allow(clippy::too_many_arguments)]
pub async fn send_audio_packet(
    socket: &UdpSocket,
    server_addr: &str,
    token: u32,
    seq: u32,
    timestamp_ms: u32,
    sample_rate: u16,
    bit_depth: u8,
    channels: u8,
    source_id: u8,
    data: &[u8],
) -> Result<(), String> {
    let packet = build_audio_packet(token, seq, timestamp_ms, sample_rate, bit_depth, channels, source_id, data);
    socket
        .send_to(&packet, server_addr)
        .await
        .map_err(|e| format!("UDP send failed: {}", e))?;
    Ok(())
}
