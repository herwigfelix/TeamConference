use std::path::Path;
use std::sync::Arc;

use tokio::sync::watch;

use crate::net::udp_client::convert_channels;
use crate::protocol::{build_audio_packet, SOURCE_FILE};
use crate::state::AppState;

/// Ziel-Samplerate der Audio-Pipeline (Opus und Geräte laufen auf 48 kHz).
const TARGET_RATE: u32 = 48_000;

/// Einfacher linearer Resampler (interleaved i16) mit Kontinuität über Aufrufe
/// hinweg, damit an Paketgrenzen keine Knackser entstehen.
struct LinearResampler {
    in_rate: u32,
    channels: usize,
    t: f64,
    prev: Vec<f32>,
    primed: bool,
}

impl LinearResampler {
    fn new(in_rate: u32, channels: usize) -> Self {
        Self {
            in_rate,
            channels: channels.max(1),
            t: 0.0,
            prev: vec![0.0; channels.max(1)],
            primed: false,
        }
    }

    /// Resampelt `input` (interleaved i16 @ in_rate) nach TARGET_RATE und hängt
    /// das Ergebnis (interleaved i16) an `out` an.
    fn process(&mut self, input: &[i16], out: &mut Vec<i16>) {
        let ch = self.channels;
        if self.in_rate == TARGET_RATE {
            out.extend_from_slice(input);
            return;
        }
        let step = self.in_rate as f64 / TARGET_RATE as f64;
        let frames = input.len() / ch;
        for f in 0..frames {
            let cur: Vec<f32> = (0..ch).map(|c| input[f * ch + c] as f32).collect();
            if !self.primed {
                self.prev = cur.clone();
                self.primed = true;
            }
            while self.t < 1.0 {
                let t = self.t as f32;
                for c in 0..ch {
                    let v = self.prev[c] * (1.0 - t) + cur[c] * t;
                    out.push(v.clamp(-32768.0, 32767.0) as i16);
                }
                self.t += step;
            }
            self.t -= 1.0;
            self.prev = cur;
        }
    }
}

fn samples_to_bytes(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        out.extend_from_slice(&s.to_le_bytes());
    }
    out
}

fn bytes_to_samples(bytes: &[u8]) -> Vec<i16> {
    bytes
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect()
}

/// Eine Audiodatei decodieren und als **eigener** Audiostrom (Quelle 1) neben
/// dem Mikrofon (Quelle 0) senden.
///
/// Ablauf je 20-ms-Block:
///   1. decodieren, auf 48 kHz resampeln,
///   2. als eigenen Opus-Strom mit `source_id = 1` per UDP senden — andere
///      Clients dekodieren/mischen ihn getrennt vom Mikrofon (Reinreden möglich),
///      und das funktioniert auch ohne eigenes Mikrofon,
///   3. lokal in den Empfangs-Mischer einspeisen, damit der Streamer die Datei
///      mithört (der Server schickt den eigenen Strom nicht zurück).
/// Die Ausgabe wird per Wanduhr getaktet (korrekte Geschwindigkeit).
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

    let file = std::fs::File::open(file_path).map_err(|e| format!("Failed to open file: {}", e))?;
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
    let file_channels = track.codec_params.channels.map(|c| c.count()).unwrap_or(2).max(1);

    // Sende-Kanalzahl: Opus kann nur Mono/Stereo.
    let send_channels: u16 = if file_channels >= 2 { 2 } else { 1 };
    let playback_ch = {
        let inner = state.inner.lock();
        if inner.playback_device_channels > 0 { inner.playback_device_channels } else { 2 }
    };

    // Opus-Encoder für den Datei-Strom (Bitrate vom Raum; 0 = automatisch).
    let opus_channels = if send_channels == 1 {
        opus::Channels::Mono
    } else {
        opus::Channels::Stereo
    };
    let mut opus_encoder = opus::Encoder::new(TARGET_RATE, opus_channels, opus::Application::Audio)
        .map_err(|e| format!("Failed to create Opus encoder: {}", e))?;
    let configured_bitrate = state.inner.lock().audio_config.bitrate;
    let bitrate = crate::state::effective_bitrate(configured_bitrate, send_channels as u8);
    let _ = opus_encoder.set_bitrate(opus::Bitrate::Bits(bitrate));
    let mut opus_buf = vec![0u8; 4000];

    tracing::info!(
        "FileStream: file_sr={}, file_ch={} → 48k, send_ch={}, monitor_ch={}, bitrate={}kbps",
        file_sample_rate, file_channels, send_channels, playback_ch, bitrate / 1000
    );

    // Mischmodus für den Wiedergabe-Jitterpuffer (größerer Puffer).
    state.file_streaming.store(true, std::sync::atomic::Ordering::Relaxed);
    {
        use crate::audio::playback::{
            ADAPTIVE_PRE_BUFFER_MS, CALLBACKS_SINCE_UNDERRUN, PRE_BUFFERED,
            STREAMING_MIN_PRE_BUFFER_MS,
        };
        use std::sync::atomic::Ordering;
        let current = ADAPTIVE_PRE_BUFFER_MS.load(Ordering::Relaxed);
        if current < STREAMING_MIN_PRE_BUFFER_MS {
            ADAPTIVE_PRE_BUFFER_MS.store(STREAMING_MIN_PRE_BUFFER_MS, Ordering::Relaxed);
        }
        CALLBACKS_SINCE_UNDERRUN.store(0, Ordering::Relaxed);
        PRE_BUFFERED.store(false, Ordering::Relaxed);
    }

    let mut resampler = LinearResampler::new(file_sample_rate, file_channels);

    let block_frames = (TARGET_RATE / 50) as usize; // 960 Frames = 20 ms
    let block_samples = block_frames * file_channels; // interleaved @ file_channels
    if block_samples == 0 {
        state.file_streaming.store(false, std::sync::atomic::Ordering::Relaxed);
        return Err("Invalid audio format: zero block size".into());
    }

    let mut acc: Vec<i16> = Vec::with_capacity(block_samples * 4);

    let stream_start = tokio::time::Instant::now();
    let mut blocks_sent: u64 = 0;
    let mut seq: u32 = 0;
    let mut timestamp_ms: u32 = 0;
    let mut paused_offset = tokio::time::Duration::ZERO;

    let finish = |state: &AppState| {
        state.file_streaming.store(false, std::sync::atomic::Ordering::Relaxed);
    };

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        if acc.len() < block_samples {
            let packet = match format.next_packet() {
                Ok(p) => p,
                Err(symphonia::core::errors::Error::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    break;
                }
                Err(e) => {
                    tracing::warn!("FileStream: packet read error: {}", e);
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
            resampler.process(sample_buf.samples(), &mut acc);
        }

        while acc.len() >= block_samples {
            if *shutdown_rx.borrow() {
                finish(&state);
                return Ok(());
            }

            // Pause.
            if state.stream_paused.load(std::sync::atomic::Ordering::Relaxed) {
                let pause_start = tokio::time::Instant::now();
                while state.stream_paused.load(std::sync::atomic::Ordering::Relaxed) {
                    if *shutdown_rx.borrow() {
                        finish(&state);
                        return Ok(());
                    }
                    tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
                }
                paused_offset += pause_start.elapsed();
            }

            // Wanduhr-Pacing.
            let target_time =
                stream_start + tokio::time::Duration::from_millis(blocks_sent * 20) + paused_offset;
            tokio::time::sleep_until(target_time).await;

            let block: Vec<i16> = acc.drain(..block_samples).collect();
            let block_bytes = samples_to_bytes(&block);

            // 1) Als eigenen Opus-Strom (Quelle 1) senden.
            let send_bytes = convert_channels(&block_bytes, file_channels as u16, send_channels);
            let send_samples = bytes_to_samples(&send_bytes);
            let (payload, bit_depth) = match opus_encoder.encode(&send_samples, &mut opus_buf) {
                Ok(len) => (opus_buf[..len].to_vec(), 0u8),
                Err(_) => (send_bytes.clone(), 16u8),
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
                let packet = build_audio_packet(
                    token,
                    seq,
                    timestamp_ms,
                    TARGET_RATE as u16,
                    bit_depth,
                    send_channels as u8,
                    SOURCE_FILE,
                    &payload,
                );
                if let Err(e) = sock.send_to(&packet, addr.as_str()).await {
                    tracing::warn!("FileStream: UDP send error: {}", e);
                }
            }

            // 2) Lokal mithören (in den Empfangs-Mischer einspeisen).
            let mon_bytes = convert_channels(&block_bytes, file_channels as u16, playback_ch);
            {
                let tx = state.local_audio_tx.lock();
                if let Some(ref tx) = *tx {
                    let _ = tx.send(bytes_to_samples(&mon_bytes));
                }
            }

            seq = seq.wrapping_add(1);
            timestamp_ms = timestamp_ms.wrapping_add(20);
            blocks_sent += 1;
        }
    }

    finish(&state);
    Ok(())
}
