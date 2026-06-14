use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use cpal::traits::{DeviceTrait, StreamTrait};

use crate::state::AppState;

/// Adaptive jitter buffer constants
const INITIAL_PRE_BUFFER_MS: usize = 60;
const MIN_PRE_BUFFER_MS: usize = 60;
const MAX_PRE_BUFFER_MS: usize = 500;
/// Während eine Datei gestreamt wird, halten wir einen größeren Mindestpuffer,
/// damit Decodier-/Pacing-Schwankungen nicht zu Stottern führen. Latenz ist
/// beim Datei-Streaming (anders als bei Live-Sprache) unkritisch.
pub(crate) const STREAMING_MIN_PRE_BUFFER_MS: usize = 200;
const BUFFER_INCREASE_MS: usize = 40;
const BUFFER_DECREASE_MS: usize = 20;
/// How many smooth callbacks before we try to decrease the buffer (~5 seconds)
const STABLE_CALLBACKS_BEFORE_DECREASE: u64 = 500;
/// Module-level adaptive buffer state — accessible from file_stream.rs to proactively bump.
pub(crate) static ADAPTIVE_PRE_BUFFER_MS: AtomicUsize = AtomicUsize::new(INITIAL_PRE_BUFFER_MS);
pub(crate) static CALLBACKS_SINCE_UNDERRUN: AtomicU64 = AtomicU64::new(0);
pub(crate) static PRE_BUFFERED: AtomicBool = AtomicBool::new(false);

/// Start audio playback to the speaker/headphones.
/// Lock-free design: the cpal callback reads directly from a crossbeam channel
/// into a local buffer — no mutexes on the audio thread. The user volume is
/// read from an atomic and applied as a gain per callback.
/// Uses an adaptive jitter buffer that grows on underruns and shrinks when stable.
pub fn start_playback(
    state: Arc<AppState>,
    output_device_name: Option<String>,
) -> Result<cpal::Stream, String> {
    let device = crate::audio::device::get_output_device(output_device_name.as_deref())?;

    let supported_config = device
        .default_output_config()
        .map_err(|e| format!("No default output config: {}", e))?;

    let sample_rate = supported_config.sample_rate().0;
    let channels = supported_config.channels();

    tracing::info!(
        "Playback: using device default config: {}Hz, {} channels, {:?}",
        sample_rate,
        channels,
        supported_config.sample_format()
    );

    let config = cpal::StreamConfig {
        channels,
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    // Create crossbeam channel — larger buffer for smoother playback
    let (playback_tx, playback_rx) = crossbeam_channel::bounded::<Vec<u8>>(128);

    // Store in state
    {
        let mut tx_lock = state.playback_tx.lock();
        *tx_lock = Some(playback_tx);
        let mut rx_lock = state.playback_rx.lock();
        *rx_lock = Some(playback_rx.clone());
    }

    // Store playback device channel count so udp_client can convert incoming audio
    {
        let mut inner = state.inner.lock();
        inner.playback_device_channels = channels;
    }

    let bytes_per_ms = (sample_rate as usize * channels as usize * 2) / 1000;

    let err_fn = |err: cpal::StreamError| {
        tracing::error!("Audio playback stream error: {}", err);
    };

    // Diagnostics
    static CALLBACK_COUNT: AtomicU64 = AtomicU64::new(0);
    static NONSILENT_COUNT: AtomicU64 = AtomicU64::new(0);
    CALLBACK_COUNT.store(0, Ordering::Relaxed);
    NONSILENT_COUNT.store(0, Ordering::Relaxed);

    // Reset adaptive jitter buffer for new playback session
    ADAPTIVE_PRE_BUFFER_MS.store(INITIAL_PRE_BUFFER_MS, Ordering::Relaxed);
    CALLBACKS_SINCE_UNDERRUN.store(0, Ordering::Relaxed);
    PRE_BUFFERED.store(false, Ordering::Relaxed);

    // Volume gain shared with the UI (lock-free)
    let volume_bits = state.volume_bits.clone();
    // State-Klon nur für das lock-freie Lesen des Streaming-Flags im Callback.
    let cb_state = state.clone();

    // The cpal callback owns its own local buffer.
    // No mutexes — crossbeam try_recv is lock-free.
    // Mixing is handled upstream in the UDP recv task, so callback just appends.
    let max_buf_cap = MAX_PRE_BUFFER_MS * 2 * bytes_per_ms;
    let mut local_buf: Vec<u8> = Vec::with_capacity(max_buf_cap);

    let stream = device
        .build_output_stream(
            &config,
            move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                let current_pre_buffer_ms = ADAPTIVE_PRE_BUFFER_MS.load(Ordering::Relaxed);
                let pre_buffer_bytes = current_pre_buffer_ms * bytes_per_ms;
                let max_buffer_bytes = current_pre_buffer_ms * 2 * bytes_per_ms;
                let volume = f32::from_bits(volume_bits.load(Ordering::Relaxed));

                // Drain all available chunks from channel into local buffer (lock-free)
                while let Ok(chunk) = playback_rx.try_recv() {
                    local_buf.extend_from_slice(&chunk);
                }

                // Cap buffer to prevent unbounded growth
                if local_buf.len() > max_buffer_bytes {
                    let drain = local_buf.len() - max_buffer_bytes;
                    local_buf.drain(..drain);
                }

                let cb_count = CALLBACK_COUNT.fetch_add(1, Ordering::Relaxed);
                let needed_bytes = data.len() * 2; // each f32 sample ← one i16 (2 bytes)

                // Pre-buffering: wait until we have enough data for smooth start
                if !PRE_BUFFERED.load(Ordering::Relaxed) {
                    if local_buf.len() >= pre_buffer_bytes {
                        PRE_BUFFERED.store(true, Ordering::Relaxed);
                        tracing::info!(
                            "Playback: pre-buffer filled ({} bytes, {}ms), starting output",
                            local_buf.len(),
                            local_buf.len() / bytes_per_ms
                        );
                    } else {
                        for sample in data.iter_mut() {
                            *sample = 0.0;
                        }
                        if cb_count == 500 {
                            tracing::warn!(
                                "Playback callback: 500 callbacks, still pre-buffering (buffer={})",
                                local_buf.len()
                            );
                        }
                        return;
                    }
                }

                if local_buf.len() >= needed_bytes {
                    // Fast path: convert i16 LE → f32 directly from buffer
                    for (i, sample) in data.iter_mut().enumerate() {
                        let offset = i * 2;
                        let s16 = i16::from_le_bytes([local_buf[offset], local_buf[offset + 1]]);
                        *sample = (s16 as f32 / 32768.0 * volume).clamp(-1.0, 1.0);
                    }
                    local_buf.drain(..needed_bytes);

                    let ns = NONSILENT_COUNT.fetch_add(1, Ordering::Relaxed);
                    if ns == 0 {
                        tracing::info!(
                            "Playback callback: first non-silent output, {} samples, buffer remaining={}",
                            data.len(),
                            local_buf.len()
                        );
                    }

                    // Stable playback — track for adaptive decrease
                    let stable = CALLBACKS_SINCE_UNDERRUN.fetch_add(1, Ordering::Relaxed);
                    if stable > 0 && stable % STABLE_CALLBACKS_BEFORE_DECREASE == 0 {
                        // Während des Datei-Streamings nicht unter den größeren
                        // Streaming-Mindestpuffer schrumpfen.
                        let floor = if cb_state.file_streaming.load(Ordering::Relaxed) {
                            STREAMING_MIN_PRE_BUFFER_MS
                        } else {
                            MIN_PRE_BUFFER_MS
                        };
                        let current = ADAPTIVE_PRE_BUFFER_MS.load(Ordering::Relaxed);
                        if current > floor {
                            let new_val = current.saturating_sub(BUFFER_DECREASE_MS).max(floor);
                            ADAPTIVE_PRE_BUFFER_MS.store(new_val, Ordering::Relaxed);
                            tracing::info!(
                                "Playback: stable for ~5s, buffer {}ms → {}ms",
                                current,
                                new_val
                            );
                        }
                    }
                } else {
                    // Buffer underrun — output what we have, pad rest with silence
                    let available = local_buf.len() / 2;
                    for (i, sample) in data.iter_mut().enumerate() {
                        if i < available {
                            let offset = i * 2;
                            let s16 =
                                i16::from_le_bytes([local_buf[offset], local_buf[offset + 1]]);
                            *sample = (s16 as f32 / 32768.0 * volume).clamp(-1.0, 1.0);
                        } else {
                            *sample = 0.0;
                        }
                    }
                    local_buf.clear();

                    // Underrun detected — increase adaptive buffer
                    if PRE_BUFFERED.load(Ordering::Relaxed) {
                        let current = ADAPTIVE_PRE_BUFFER_MS.load(Ordering::Relaxed);
                        let new_val = (current + BUFFER_INCREASE_MS).min(MAX_PRE_BUFFER_MS);
                        if new_val != current {
                            ADAPTIVE_PRE_BUFFER_MS.store(new_val, Ordering::Relaxed);
                            tracing::info!(
                                "Playback: underrun, buffer {}ms → {}ms",
                                current,
                                new_val
                            );
                        }
                        CALLBACKS_SINCE_UNDERRUN.store(0, Ordering::Relaxed);

                        // Re-enter pre-buffering to let the buffer recover
                        PRE_BUFFERED.store(false, Ordering::Relaxed);
                    }
                }
            },
            err_fn,
            None,
        )
        .map_err(|e| format!("Failed to build output stream: {}", e))?;

    stream
        .play()
        .map_err(|e| format!("Failed to start playback: {}", e))?;

    tracing::info!("Playback stream started successfully");
    Ok(stream)
}
