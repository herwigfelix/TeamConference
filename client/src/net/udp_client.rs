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

    // -- Recv task: UDP -> Opus decode -> playback --
    let (recv_shutdown_tx, recv_shutdown_rx) = watch::channel(false);
    let recv_socket = socket.clone();
    let recv_state = state.clone();

    tokio::spawn(async move {
        let mut buf = [0u8; 65536];
        let mut packets_received: u64 = 0;
        let mut parse_failures: u64 = 0;
        let mut no_playback_tx: u64 = 0;

        // Opus decoders — one for mono, one for stereo (created on demand)
        let mut decoder_mono: Option<opus::Decoder> = None;
        let mut decoder_stereo: Option<opus::Decoder> = None;
        // PCM output buffer for Opus decoding (max 960 frames * 2 channels)
        let mut pcm_out = vec![0i16; 960 * 2];

        // Inline pair-mixing: hold one chunk, mix with the next arrival, send immediately.
        // This avoids timer jitter — behaves like the direct mic path but mixes pairs.
        let mut pending_mix_chunk: Option<Vec<u8>> = None;

        loop {
            if *recv_shutdown_rx.borrow() {
                break;
            }

            let is_mixing = recv_state.file_streaming.load(std::sync::atomic::Ordering::Relaxed);

            tokio::select! {
                result = recv_socket.recv_from(&mut buf) => {
                    match result {
                        Ok((len, addr)) => {
                            if let Some(header) = AudioPacketHeader::parse(&buf[..len]) {
                                let payload = header.payload(&buf[..len]);

                                let deafened = {
                                    let inner = recv_state.inner.lock();
                                    inner.deafened
                                };

                                if packets_received == 0 {
                                    tracing::info!(
                                        "UDP recv: first packet from {}, len={}, sr={}, ch={}, bd={}, payload={}, deafened={}, opus={}",
                                        addr, len, header.sample_rate, header.channels, header.bit_depth, payload.len(), deafened,
                                        header.bit_depth == 0
                                    );
                                }

                                if !deafened && !payload.is_empty() {
                                    // Decode: either Opus (bit_depth=0) or raw PCM
                                    let pcm = if header.bit_depth == 0 {
                                        // Opus decode
                                        let decoder = if header.channels <= 1 {
                                            decoder_mono.get_or_insert_with(|| {
                                                match opus::Decoder::new(48000, opus::Channels::Mono) {
                                                    Ok(d) => {
                                                        tracing::info!("UDP recv: created Opus mono decoder");
                                                        d
                                                    }
                                                    Err(e) => {
                                                        tracing::error!("Failed to create Opus mono decoder: {}", e);
                                                        panic!("Opus decoder creation failed");
                                                    }
                                                }
                                            })
                                        } else {
                                            decoder_stereo.get_or_insert_with(|| {
                                                match opus::Decoder::new(48000, opus::Channels::Stereo) {
                                                    Ok(d) => {
                                                        tracing::info!("UDP recv: created Opus stereo decoder");
                                                        d
                                                    }
                                                    Err(e) => {
                                                        tracing::error!("Failed to create Opus stereo decoder: {}", e);
                                                        panic!("Opus decoder creation failed");
                                                    }
                                                }
                                            })
                                        };

                                        let out_samples = 960 * (header.channels.max(1) as usize);
                                        if pcm_out.len() < out_samples {
                                            pcm_out.resize(out_samples, 0);
                                        }

                                        match decoder.decode(payload, &mut pcm_out[..out_samples], false) {
                                            Ok(decoded_frames) => {
                                                let total_samples = decoded_frames * (header.channels.max(1) as usize);
                                                if packets_received == 0 {
                                                    tracing::info!(
                                                        "UDP recv: Opus decoded {} bytes → {} frames ({} samples)",
                                                        payload.len(), decoded_frames, total_samples
                                                    );
                                                }
                                                let mut bytes = Vec::with_capacity(total_samples * 2);
                                                for &s in &pcm_out[..total_samples] {
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
                                        let dev_ch = {
                                            let inner = recv_state.inner.lock();
                                            inner.playback_device_channels
                                        };
                                        let converted = convert_channels(&pcm_data, header.channels as u16, dev_ch);

                                        // Determine what to send: either mix pair or send directly
                                        let to_send = if is_mixing {
                                            if let Some(pending) = pending_mix_chunk.take() {
                                                // Mix pending + new and send immediately
                                                Some(mix_chunks(&[pending, converted]))
                                            } else {
                                                // Hold as pending, wait for pair
                                                pending_mix_chunk = Some(converted);
                                                None
                                            }
                                        } else {
                                            // Flush any leftover pending chunk from when mixing stopped
                                            if let Some(pending) = pending_mix_chunk.take() {
                                                let playback_tx = recv_state.playback_tx.lock();
                                                if let Some(ref tx) = *playback_tx {
                                                    let _ = tx.try_send(pending);
                                                }
                                            }
                                            Some(converted)
                                        };

                                        if let Some(chunk) = to_send {
                                            let playback_tx = recv_state.playback_tx.lock();
                                            if let Some(ref tx) = *playback_tx {
                                                match tx.try_send(chunk) {
                                                    Ok(()) => {}
                                                    Err(crossbeam_channel::TrySendError::Full(_)) => {
                                                        if packets_received % 500 == 0 {
                                                            tracing::warn!("UDP recv: playback channel full, dropping audio");
                                                        }
                                                    }
                                                    Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                                                        tracing::error!("UDP recv: playback channel disconnected");
                                                    }
                                                }
                                            } else {
                                                no_playback_tx += 1;
                                                if no_playback_tx == 1 || no_playback_tx == 50 {
                                                    tracing::warn!("UDP recv: no playback_tx available (count={})", no_playback_tx);
                                                }
                                            }
                                        }
                                    }
                                }

                                packets_received += 1;
                                if packets_received == 50 {
                                    tracing::info!("UDP recv: 50 packets received (1 second of audio)");
                                }
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
                // Flush pending chunk if no pair arrives within 10ms
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(10)), if pending_mix_chunk.is_some() => {
                    if let Some(chunk) = pending_mix_chunk.take() {
                        let playback_tx = recv_state.playback_tx.lock();
                        if let Some(ref tx) = *playback_tx {
                            let _ = tx.try_send(chunk);
                        }
                    }
                }
            }
        }
        tracing::info!("UDP recv task ended, total received: {}, parse failures: {}", packets_received, parse_failures);
    });

    Ok((send_shutdown_tx, recv_shutdown_tx))
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

/// Mix multiple i16 LE PCM byte chunks into one by adding samples with clamping.
fn mix_chunks(chunks: &[Vec<u8>]) -> Vec<u8> {
    let max_len = chunks.iter().map(|c| c.len()).max().unwrap_or(0);
    let num_samples = max_len / 2;
    let mut mixed = vec![0i32; num_samples];

    for chunk in chunks {
        for (i, bytes) in chunk.chunks_exact(2).enumerate() {
            mixed[i] += i16::from_le_bytes([bytes[0], bytes[1]]) as i32;
        }
    }

    let mut out = Vec::with_capacity(max_len);
    for &s in &mixed {
        out.extend_from_slice(&(s.clamp(-32768, 32767) as i16).to_le_bytes());
    }
    out
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
pub async fn send_audio_packet(
    socket: &UdpSocket,
    server_addr: &str,
    token: u32,
    seq: u32,
    timestamp_ms: u32,
    sample_rate: u16,
    bit_depth: u8,
    channels: u8,
    data: &[u8],
) -> Result<(), String> {
    let packet = build_audio_packet(token, seq, timestamp_ms, sample_rate, bit_depth, channels, data);
    socket
        .send_to(&packet, server_addr)
        .await
        .map_err(|e| format!("UDP send failed: {}", e))?;
    Ok(())
}
