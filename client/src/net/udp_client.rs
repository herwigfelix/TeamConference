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

        // Sample-getakteter Mischer: jede Quelle hat eine eigene Frame-Schlange
        // (bereits im Wiedergabeformat, 20-ms-Frames). Eine selbstkorrigierende
        // 20-ms-Uhr (wie das Datei-Pacing) gibt pro 20 ms GENAU EIN gemischtes
        // Frame aus. Damit hängt die Ausgaberate nicht mehr an der Ankunfts-
        // reihenfolge der Quellen: Zwei gleichzeitige Quellen (z. B. lokales
        // Mithören per Loopback + ein entfernter Nutzer) liefern weiterhin
        // Echtzeit-Tempo, statt den Wiedergabepuffer doppelt schnell zu füllen
        // (das verursachte das Stottern). Jede Schlange puffert ein paar Frames
        // (PRIME) als Mini-Jitterpuffer; der nachgelagerte adaptive Puffer in
        // playback.rs glättet die Geräte-Drift zusätzlich.
        const PRIME_FRAMES: usize = 2; // ~40 ms anpuffern, bevor eine Quelle läuft
        const MAX_QUEUE: usize = 25; // ~500 ms Deckel gegen Drift/Bursts
        const IDLE_RESET_TICKS: u64 = 10; // ~200 ms Stille → Uhr anhalten
        const LOCAL_KEY: (u32, u8) = (u32::MAX, crate::protocol::SOURCE_FILE);
        let mut sources: HashMap<(u32, u8), Source> = HashMap::new();
        let mut clock: Option<tokio::time::Instant> = None;
        let mut emitted: u64 = 0;
        let mut idle_ticks: u64 = 0;

        loop {
            // Nächster Misch-Tick: wenn die Uhr läuft, exakt bei
            // start + emitted*20 ms (selbstkorrigierend); sonst nie.
            let tick = async {
                match clock {
                    Some(start) => {
                        tokio::time::sleep_until(
                            start + tokio::time::Duration::from_millis(emitted * 20),
                        )
                        .await
                    }
                    None => std::future::pending::<()>().await,
                }
            };

            tokio::select! {
                _ = recv_shutdown_rx.changed() => { break; }

                // Misch-Tick: ein 20-ms-Frame aus allen aktiven Quellen mischen.
                _ = tick => {
                    let gains: HashMap<u32, f32> = {
                        let inner = recv_state.inner.lock();
                        sources
                            .keys()
                            .map(|(token, _)| {
                                let g = inner
                                    .token_to_user
                                    .get(token)
                                    .map(|uid| inner.user_volume(*uid))
                                    .unwrap_or(1.0);
                                (*token, g)
                            })
                            .collect()
                    };

                    let mut mix: Vec<i32> = Vec::new();
                    let mut any = false;
                    for (key, src) in sources.iter_mut() {
                        // Quelle erst starten, wenn sie genug angepuffert hat.
                        if !src.started {
                            if src.queue.len() >= PRIME_FRAMES {
                                src.started = true;
                            } else {
                                continue;
                            }
                        }
                        // Ein Frame entnehmen; ist die Schlange (Jitter) leer,
                        // trägt die Quelle für diesen Tick nichts bei (Stille).
                        if let Some(frame) = src.queue.pop_front() {
                            let gain = if key.0 == u32::MAX {
                                1.0
                            } else {
                                gains.get(&key.0).copied().unwrap_or(1.0)
                            };
                            mix_add(&mut mix, &frame, gain);
                            any = true;
                        }
                    }

                    if any {
                        let out = mix_to_bytes(&mix);
                        let tx = recv_state.playback_tx.lock();
                        if let Some(ref tx) = *tx {
                            let _ = tx.try_send(out);
                        }
                        idle_ticks = 0;
                    } else {
                        idle_ticks += 1;
                    }
                    emitted += 1;

                    // Anhaltend keine Daten → Uhr stoppen, Quellen neu anpuffern.
                    if idle_ticks >= IDLE_RESET_TICKS
                        && sources.values().all(|s| s.queue.is_empty())
                    {
                        clock = None;
                        emitted = 0;
                        idle_ticks = 0;
                        sources.retain(|_, s| {
                            s.started = false;
                            !s.queue.is_empty()
                        });
                    }
                }

                // Lokal eingespeistes Audio (Loopback-Mithören oder gestreamte
                // Datei, bereits im Wiedergabeformat).
                Some(frame) = local_rx.recv() => {
                    enqueue_frame(&mut sources, LOCAL_KEY, frame, MAX_QUEUE);
                    if clock.is_none() {
                        clock = Some(tokio::time::Instant::now());
                        emitted = 0;
                        idle_ticks = 0;
                    }
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
                                        let frame: Vec<i16> = converted
                                            .chunks_exact(2)
                                            .map(|c| i16::from_le_bytes([c[0], c[1]]))
                                            .collect();
                                        let key = (header.token, header.source_id);
                                        enqueue_frame(&mut sources, key, frame, MAX_QUEUE);
                                        if clock.is_none() {
                                            clock = Some(tokio::time::Instant::now());
                                            emitted = 0;
                                            idle_ticks = 0;
                                        }
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

/// Eine Quelle (Nutzer-Token + Quellen-ID) im sample-getakteten Mischer: eine
/// Schlange fertiger 20-ms-Frames (Wiedergabeformat) plus ein „läuft schon"-
/// Flag, das erst nach dem Anpuffern (PRIME_FRAMES) gesetzt wird.
struct Source {
    queue: std::collections::VecDeque<Vec<i16>>,
    started: bool,
}

/// Ein 20-ms-Frame in die Schlange der Quelle legen (Schlange bei Bedarf neu
/// anlegen). Über `max_queue` hinaus wird das älteste Frame verworfen — so kann
/// eine schneller als die Mischuhr liefernde Quelle den Speicher nicht fluten.
fn enqueue_frame(
    sources: &mut HashMap<(u32, u8), Source>,
    key: (u32, u8),
    frame: Vec<i16>,
    max_queue: usize,
) {
    let src = sources.entry(key).or_insert_with(|| Source {
        queue: std::collections::VecDeque::new(),
        started: false,
    });
    src.queue.push_back(frame);
    while src.queue.len() > max_queue {
        src.queue.pop_front();
    }
}

/// Ein Frame (skaliert mit `gain`) in den Misch-Akkumulator addieren
/// (resize bei Bedarf). Begrenzung erfolgt erst in `mix_to_bytes`.
fn mix_add(mix: &mut Vec<i32>, frame: &[i16], gain: f32) {
    if mix.len() < frame.len() {
        mix.resize(frame.len(), 0);
    }
    if (gain - 1.0).abs() < f32::EPSILON {
        for (i, &s) in frame.iter().enumerate() {
            mix[i] += s as i32;
        }
    } else {
        for (i, &s) in frame.iter().enumerate() {
            mix[i] += (s as f32 * gain) as i32;
        }
    }
}

/// Misch-Akkumulator zu i16-LE-Bytes (mit Begrenzung) wandeln.
fn mix_to_bytes(mix: &[i32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(mix.len() * 2);
    for &v in mix {
        out.extend_from_slice(&(v.clamp(-32768, 32767) as i16).to_le_bytes());
    }
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
