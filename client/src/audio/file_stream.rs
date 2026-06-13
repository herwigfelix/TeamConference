use std::path::Path;
use std::sync::Arc;

use tokio::sync::watch;

use crate::protocol::build_audio_packet;
use crate::state::AppState;

/// Decode an audio file and stream it as Opus-encoded audio via UDP.
/// Uses clock-based pacing for correct playback speed.
pub async fn stream_audio_file(
    file_path: &Path,
    state: Arc<AppState>,
    shutdown_rx: watch::Receiver<bool>,
) -> Result<(), String> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(file_path)
        .map_err(|e| format!("Failed to open file: {}", e))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = file_path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("Failed to probe audio format: {}", e))?;

    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| {
            t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL
                && t.codec_params.sample_rate.is_some()
        })
        .ok_or_else(|| "No audio track found".to_string())?
        .clone();

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("Failed to create decoder: {}", e))?;

    let file_sample_rate = track.codec_params.sample_rate.unwrap_or(48000);
    let file_channels = track.codec_params.channels.map(|c| c.count() as u8).unwrap_or(2);

    let send_sample_rate = file_sample_rate as u16;
    let send_channels = file_channels;

    // Create Opus encoder for the file's format
    let opus_channels = if send_channels == 1 {
        opus::Channels::Mono
    } else {
        opus::Channels::Stereo
    };
    let mut opus_encoder = opus::Encoder::new(48000, opus_channels, opus::Application::Audio)
        .map_err(|e| format!("Failed to create Opus encoder: {}", e))?;
    let bitrate = if send_channels == 1 { 128_000 } else { 256_000 };
    let _ = opus_encoder.set_bitrate(opus::Bitrate::Bits(bitrate));
    let mut opus_buf = vec![0u8; 4000];

    tracing::info!(
        "Streaming: file_sr={}, file_ch={}, opus_bitrate={}kbps",
        file_sample_rate, file_channels, bitrate / 1000
    );

    // Signal playback to mix concurrent audio streams
    state.file_streaming.store(true, std::sync::atomic::Ordering::Relaxed);

    // Proactively bump the adaptive jitter buffer so playback has headroom
    // during the transition to mixing mode (prevents stutter at stream start).
    {
        use crate::audio::playback::{ADAPTIVE_PRE_BUFFER_MS, CALLBACKS_SINCE_UNDERRUN, PRE_BUFFERED};
        use std::sync::atomic::Ordering;
        let current = ADAPTIVE_PRE_BUFFER_MS.load(Ordering::Relaxed);
        if current < 150 {
            ADAPTIVE_PRE_BUFFER_MS.store(150, Ordering::Relaxed);
            tracing::info!("FileStream: bumped playback buffer {}ms → 150ms for streaming", current);
        }
        // Reset stability counter so the buffer doesn't immediately shrink back
        CALLBACKS_SINCE_UNDERRUN.store(0, Ordering::Relaxed);
        // Re-enter pre-buffering to let the buffer fill to the new target
        PRE_BUFFERED.store(false, Ordering::Relaxed);
    }

    let mut seq: u32 = 0;
    let mut timestamp_ms: u32 = 0;

    // 20ms frame: 960 frames at 48kHz
    let frames_per_packet = file_sample_rate / 50;
    let chunk_samples = (frames_per_packet as usize) * (send_channels as usize);
    let chunk_bytes = chunk_samples * 2; // i16 = 2 bytes
    if chunk_bytes == 0 {
        return Err("Invalid audio format: zero chunk size".into());
    }

    let mut accumulator: Vec<u8> = Vec::with_capacity(chunk_bytes * 4);

    // Clock-based pacing
    let stream_start = tokio::time::Instant::now();
    let mut chunks_sent: u64 = 0;
    // Aufsummierte Pausendauer, damit das Pacing nach dem Fortsetzen stimmt
    let mut paused_offset = tokio::time::Duration::ZERO;

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        // Decode more audio if needed
        if accumulator.len() < chunk_bytes {
            let packet = match format.next_packet() {
                Ok(p) => p,
                Err(symphonia::core::errors::Error::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(e) => {
                    tracing::warn!("Audio file stream: packet read error: {}", e);
                    break;
                }
            };

            if packet.track_id() != track.id {
                continue;
            }

            let decoded = match decoder.decode(&packet) {
                Ok(d) => d,
                Err(e) => {
                    tracing::trace!("Decode error (skipping packet): {}", e);
                    continue;
                }
            };

            let spec = *decoded.spec();
            let num_frames = decoded.frames();
            if num_frames == 0 {
                continue;
            }

            let mut sample_buf = SampleBuffer::<i16>::new(num_frames as u64, spec);
            sample_buf.copy_interleaved_ref(decoded);

            for &sample in sample_buf.samples() {
                accumulator.extend_from_slice(&sample.to_le_bytes());
            }
        }

        // Send all full 20ms chunks
        while accumulator.len() >= chunk_bytes {
            if *shutdown_rx.borrow() {
                return Ok(());
            }

            // Pause: warten, solange pausiert; verstrichene Zeit als Offset merken,
            // damit nach dem Fortsetzen nicht im Schwall nachgeholt wird.
            if state.stream_paused.load(std::sync::atomic::Ordering::Relaxed) {
                let pause_start = tokio::time::Instant::now();
                while state.stream_paused.load(std::sync::atomic::Ordering::Relaxed) {
                    if *shutdown_rx.borrow() {
                        return Ok(());
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                }
                paused_offset += pause_start.elapsed();
            }

            // Clock-based pacing
            let target_time =
                stream_start + tokio::time::Duration::from_millis(chunks_sent * 20) + paused_offset;
            tokio::time::sleep_until(target_time).await;

            let chunk: Vec<u8> = accumulator.drain(..chunk_bytes).collect();

            // Convert i16 LE bytes to i16 slice for Opus encoder
            let pcm_samples: Vec<i16> = chunk
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]))
                .collect();

            // Opus encode
            let (payload, bit_depth) = match opus_encoder.encode(&pcm_samples, &mut opus_buf) {
                Ok(len) => {
                    if chunks_sent == 0 {
                        tracing::info!("FileStream: Opus encoded {} PCM bytes → {} Opus bytes", chunk.len(), len);
                    }
                    (opus_buf[..len].to_vec(), 0u8) // bit_depth=0 = Opus
                }
                Err(e) => {
                    if chunks_sent < 3 {
                        tracing::warn!("FileStream: Opus encode failed: {}, sending raw PCM", e);
                    }
                    (chunk, 16u8)
                }
            };

            let (socket, server_addr, token) = {
                let inner = state.inner.lock();
                (
                    inner.udp_socket.clone(),
                    inner.server_udp_addr.clone(),
                    inner.session_token.unwrap_or(0),
                )
            };

            if let (Some(ref sock), Some(ref addr)) = (socket, server_addr) {
                let packet_data = build_audio_packet(
                    token, seq, timestamp_ms,
                    send_sample_rate, bit_depth, send_channels, &payload,
                );
                if let Err(e) = sock.send_to(&packet_data, addr.as_str()).await {
                    tracing::warn!("UDP send error during stream: {}", e);
                }
            }

            seq = seq.wrapping_add(1);
            timestamp_ms += 20;
            chunks_sent += 1;
        }
    }

    // Flush remaining (pad to full frame for Opus)
    if !accumulator.is_empty() {
        accumulator.resize(chunk_bytes, 0);

        let target_time =
            stream_start + tokio::time::Duration::from_millis(chunks_sent * 20) + paused_offset;
        tokio::time::sleep_until(target_time).await;

        let pcm_samples: Vec<i16> = accumulator
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect();

        let (payload, bit_depth) = match opus_encoder.encode(&pcm_samples, &mut opus_buf) {
            Ok(len) => (opus_buf[..len].to_vec(), 0u8),
            Err(_) => (accumulator, 16u8),
        };

        let (socket, server_addr, token) = {
            let inner = state.inner.lock();
            (
                inner.udp_socket.clone(),
                inner.server_udp_addr.clone(),
                inner.session_token.unwrap_or(0),
            )
        };

        if let (Some(ref sock), Some(ref addr)) = (socket, server_addr) {
            let packet_data = build_audio_packet(
                token, seq, timestamp_ms,
                send_sample_rate, bit_depth, send_channels, &payload,
            );
            let _ = sock.send_to(&packet_data, addr.as_str()).await;
        }
    }

    state.file_streaming.store(false, std::sync::atomic::Ordering::Relaxed);

    Ok(())
}
