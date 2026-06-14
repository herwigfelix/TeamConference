use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use cpal::traits::{DeviceTrait, StreamTrait};
use tokio::sync::watch;

use crate::net::udp_client::send_audio_packet;
use crate::state::AppState;

/// Start audio capture from the microphone.
/// Uses the device's default config to avoid unsupported configuration errors.
/// Audio is encoded with Opus before sending for efficient, low-latency transmission.
pub fn start_capture(
    state: Arc<AppState>,
    input_device_name: Option<String>,
) -> Result<(cpal::Stream, watch::Sender<bool>), String> {
    let device = crate::audio::device::get_input_device(input_device_name.as_deref())?;

    // Use device's default config instead of hardcoded values
    let supported_config = device
        .default_input_config()
        .map_err(|e| format!("No default input config: {}", e))?;

    let sample_rate = supported_config.sample_rate().0;
    let channels = supported_config.channels();

    tracing::info!(
        "Capture: using device default config: {}Hz, {} channels, {:?}",
        sample_rate, channels, supported_config.sample_format()
    );

    let config = cpal::StreamConfig {
        channels,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    // Update state with actual audio config
    {
        let mut inner = state.inner.lock();
        inner.audio_config.sample_rate = sample_rate;
        inner.audio_config.channels = channels as u8;
    }

    // crossbeam channel: cpal callback -> tokio send task
    let (cb_tx, cb_rx) = crossbeam_channel::bounded::<Vec<u8>>(64);

    let (shutdown_tx, shutdown_rx) = watch::channel(false);

    // Track callback invocations for diagnostics
    static CALLBACK_COUNT: AtomicU64 = AtomicU64::new(0);
    CALLBACK_COUNT.store(0, Ordering::Relaxed);

    let err_fn = |err: cpal::StreamError| {
        tracing::error!("Audio capture stream error: {}", err);
    };

    // Build stream with f32 samples (most universally supported on macOS)
    let stream = device
        .build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // Convert f32 samples to i16 LE bytes
                let mut bytes = Vec::with_capacity(data.len() * 2);
                for &sample in data {
                    let s16 = (sample * 32767.0).clamp(-32768.0, 32767.0) as i16;
                    bytes.extend_from_slice(&s16.to_le_bytes());
                }

                let count = CALLBACK_COUNT.fetch_add(1, Ordering::Relaxed);
                if count == 0 {
                    tracing::info!("Capture: first audio callback, {} samples, {} bytes", data.len(), bytes.len());
                }

                let _ = cb_tx.try_send(bytes);
            },
            err_fn,
            None,
        )
        .map_err(|e| format!("Failed to build input stream: {}", e))?;

    stream.play().map_err(|e| format!("Failed to start capture: {}", e))?;

    // Tokio task: read from crossbeam, encode Opus, send UDP packets
    let send_state = state.clone();
    let send_shutdown = shutdown_rx.clone();
    tokio::spawn(async move {
        let mut seq: u32 = 0;
        let mut timestamp_ms: u32 = 0;
        let mut packets_sent: u64 = 0;
        // Zuletzt am Encoder gesetzte Bitrate, um redundante set_bitrate-Aufrufe
        // zu vermeiden. Die Soll-Bitrate kommt vom beigetretenen Raum.
        let mut last_bitrate: i32 = -1;

        // 20ms frame size at 48kHz
        let frame_samples = (sample_rate / 50) as usize; // 960 frames
        let frame_bytes = frame_samples * (channels as usize) * 2; // i16 = 2 bytes
        let mut accumulator: Vec<u8> = Vec::with_capacity(frame_bytes * 2);

        // Create Opus encoder
        let opus_channels = if channels == 1 {
            opus::Channels::Mono
        } else {
            opus::Channels::Stereo
        };
        let mut encoder = match opus::Encoder::new(48000, opus_channels, opus::Application::Audio) {
            Ok(mut enc) => {
                // High bitrate for transparent quality
                let bitrate = if channels == 1 { 128_000 } else { 256_000 };
                let _ = enc.set_bitrate(opus::Bitrate::Bits(bitrate));
                tracing::info!("Capture: Opus encoder created, ch={}, bitrate={}kbps", channels, bitrate / 1000);
                Some(enc)
            }
            Err(e) => {
                tracing::error!("Capture: failed to create Opus encoder: {}, falling back to raw PCM", e);
                None
            }
        };
        let mut opus_buf = vec![0u8; 4000]; // max Opus frame

        loop {
            if *send_shutdown.borrow() {
                break;
            }

            match cb_rx.try_recv() {
                Ok(data) => {
                    accumulator.extend_from_slice(&data);

                    while accumulator.len() >= frame_bytes {
                        let frame: Vec<u8> = accumulator.drain(..frame_bytes).collect();

                        let (socket, server_addr, token, muted, bitrate_cfg) = {
                            let inner = send_state.inner.lock();
                            (
                                inner.udp_socket.clone(),
                                inner.server_udp_addr.clone(),
                                inner.session_token.unwrap_or(0),
                                inner.muted,
                                inner.audio_config.bitrate,
                            )
                        };
                        // Soll-Bitrate des Raums anwenden (0 = automatisch).
                        let want_bitrate =
                            crate::state::effective_bitrate(bitrate_cfg, channels as u8);
                        if want_bitrate != last_bitrate {
                            if let Some(ref mut enc) = encoder {
                                let _ = enc.set_bitrate(opus::Bitrate::Bits(want_bitrate));
                            }
                            last_bitrate = want_bitrate;
                        }

                        // Bei Stummschaltung nichts senden (das Mikrofon ist die
                        // Quelle 0; der Datei-Stream ist eine eigene Quelle und
                        // läuft unabhängig in file_stream.rs).
                        if !muted {
                            if let (Some(ref sock), Some(ref addr)) = (socket, server_addr) {
                                let pcm_samples: Vec<i16> = frame
                                    .chunks_exact(2)
                                    .map(|c| i16::from_le_bytes([c[0], c[1]]))
                                    .collect();

                                // Opus-kodieren (Fallback: rohes PCM).
                                let (payload, bit_depth) = if let Some(ref mut enc) = encoder {
                                    match enc.encode(&pcm_samples, &mut opus_buf) {
                                        Ok(len) => (opus_buf[..len].to_vec(), 0u8),
                                        Err(e) => {
                                            if packets_sent < 3 {
                                                tracing::warn!("Capture: Opus encode failed: {}, sending raw PCM", e);
                                            }
                                            (frame.clone(), 16u8)
                                        }
                                    }
                                } else {
                                    (frame.clone(), 16u8)
                                };

                                match send_audio_packet(
                                    sock,
                                    addr,
                                    token,
                                    seq,
                                    timestamp_ms,
                                    sample_rate as u16,
                                    bit_depth,
                                    channels as u8,
                                    crate::protocol::SOURCE_MIC,
                                    &payload,
                                )
                                .await
                                {
                                    Ok(()) => {
                                        packets_sent += 1;
                                        if packets_sent == 1 {
                                            tracing::info!("Capture: first UDP packet sent successfully to {}", addr);
                                        }
                                    }
                                    Err(e) => {
                                        if packets_sent == 0 {
                                            tracing::error!("Capture: UDP send FAILED: {}, addr={}", e, addr);
                                        }
                                    }
                                }
                            } else if packets_sent == 0 {
                                tracing::warn!("Capture: cannot send - no UDP socket or server address");
                            }
                        }

                        seq = seq.wrapping_add(1);
                        timestamp_ms = timestamp_ms.wrapping_add(20);
                    }
                }
                Err(crossbeam_channel::TryRecvError::Empty) => {
                    tokio::time::sleep(tokio::time::Duration::from_millis(2)).await;
                }
                Err(crossbeam_channel::TryRecvError::Disconnected) => break,
            }
        }
        tracing::info!("Capture send task ended, total packets sent: {}", packets_sent);
    });

    Ok((stream, shutdown_tx))
}
