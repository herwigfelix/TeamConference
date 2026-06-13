use std::sync::Arc;
use std::path::Path;
use tokio::sync::watch;

use crate::audio::udp_server::{UdpAudioServer, build_audio_packet};
use crate::user::manager::UserManager;

pub struct AudioFileStreamer {
    stop_tx: watch::Sender<bool>,
}

impl AudioFileStreamer {
    pub fn new() -> Self {
        let (stop_tx, _) = watch::channel(false);
        Self { stop_tx }
    }

    pub async fn stream_file(
        &self,
        file_path: &Path,
        room_id: i64,
        user_id: i64,
        udp_server: Arc<UdpAudioServer>,
        users: Arc<UserManager>,
    ) -> anyhow::Result<()> {
        use symphonia::core::io::MediaSourceStream;
        use symphonia::core::probe::Hint;
        use symphonia::core::formats::FormatOptions;
        use symphonia::core::meta::MetadataOptions;
        use symphonia::core::codecs::DecoderOptions;

        let file = std::fs::File::open(file_path)?;
        let mss = MediaSourceStream::new(Box::new(file), Default::default());

        let mut hint = Hint::new();
        if let Some(ext) = file_path.extension().and_then(|e| e.to_str()) {
            hint.with_extension(ext);
        }

        let probed = symphonia::default::get_probe().format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )?;

        let mut format = probed.format;
        let track = format.tracks()
            .iter()
            .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
            .ok_or_else(|| anyhow::anyhow!("No audio track found"))?
            .clone();

        let mut decoder = symphonia::default::get_codecs()
            .make(&track.codec_params, &DecoderOptions::default())?;

        let sample_rate = track.codec_params.sample_rate.unwrap_or(48000) as u16;
        let channels = track.codec_params.channels
            .map(|c| c.count() as u8)
            .unwrap_or(1);

        let stop_rx = self.stop_tx.subscribe();
        let mut seq: u32 = 0;
        let mut timestamp_ms: u32 = 0;
        let samples_per_packet = sample_rate as u32 / 50; // 20ms packets

        // Notify room that streaming started
        let status_msg = crate::control::protocol::Message::new(
            "stream_file_status",
            serde_json::json!({
                "user_id": user_id,
                "filename": file_path.file_name().unwrap_or_default().to_string_lossy(),
                "playing": true
            }),
        );
        users.broadcast_to_room(room_id, status_msg, None).await;

        loop {
            // Check for stop signal
            if *stop_rx.borrow() {
                break;
            }

            let packet = match format.next_packet() {
                Ok(p) => p,
                Err(symphonia::core::errors::Error::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(_) => break,
            };

            if packet.track_id() != track.id {
                continue;
            }

            let decoded = match decoder.decode(&packet) {
                Ok(d) => d,
                Err(_) => continue,
            };

            // Convert to i16 PCM
            let mut pcm_buf = Vec::new();
            let spec = *decoded.spec();
            let num_frames = decoded.frames();

            // Create an i16 sample buffer
            let mut sample_buf = symphonia::core::audio::SampleBuffer::<i16>::new(
                num_frames as u64,
                spec,
            );
            sample_buf.copy_interleaved_ref(decoded);

            for &sample in sample_buf.samples() {
                pcm_buf.extend_from_slice(&sample.to_le_bytes());
            }

            // Send in chunks
            let bytes_per_sample = 2u16; // i16
            let chunk_size = (samples_per_packet as usize) * (channels as usize) * (bytes_per_sample as usize);

            for chunk in pcm_buf.chunks(chunk_size) {
                let audio_packet = build_audio_packet(
                    0, // token 0 for server-generated audio
                    seq,
                    timestamp_ms,
                    sample_rate,
                    16, // i16
                    channels,
                    chunk,
                );

                udp_server.send_audio_to_room(room_id, &audio_packet).await;

                seq = seq.wrapping_add(1);
                timestamp_ms += 20; // 20ms per packet

                // Pace the sending
                tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

                if *stop_rx.borrow() {
                    break;
                }
            }
        }

        // Notify room that streaming stopped
        let status_msg = crate::control::protocol::Message::new(
            "stream_file_status",
            serde_json::json!({
                "user_id": user_id,
                "filename": file_path.file_name().unwrap_or_default().to_string_lossy(),
                "playing": false
            }),
        );
        users.broadcast_to_room(room_id, status_msg, None).await;

        Ok(())
    }

    pub fn stop(&self) {
        let _ = self.stop_tx.send(true);
    }
}
