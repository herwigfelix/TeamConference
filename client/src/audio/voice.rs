//! Sprachverarbeitung fürs Mikrofon: Echo- und Rauschunterdrückung über den
//! WebRTC-Audioprozessor (`sonora`, reiner Rust-Port).
//!
//! Mikrofon („Capture", 48 kHz, Sendekanäle) und Lautsprechersignal („Render",
//! 48 kHz Mono) laufen durch DIESELBE Instanz: Die Echounterdrückung vergleicht
//! beide und entfernt den Lautsprecheranteil aus dem Mikrofonsignal — sonst hören
//! die anderen sich selbst zurück, wenn jemand ohne Kopfhörer spricht. Der
//! Prozessor arbeitet in 10-ms-Blöcken; die Pipeline liefert 20-ms-Frames, die
//! hier in zwei Blöcke geteilt werden.

use sonora::config::{
    EchoCanceller, HighPassFilter, MaxProcessingRate, NoiseSuppression, NoiseSuppressionLevel,
};
use sonora::{AudioProcessing, Config, StreamConfig};

/// Verarbeitungsrate (= Pipeline-Rate).
pub const RATE: u32 = 48_000;
/// Frames pro 10-ms-Block.
const CHUNK: usize = (RATE / 100) as usize;

/// Anzeigenamen der Stufen der Rauschunterdrückung (Index = gespeicherter Wert).
pub const NOISE_LEVELS: [&str; 5] = ["Aus", "Leicht", "Mittel", "Stark", "Sehr stark"];

/// Vom Nutzer gewählte Verarbeitung.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VoiceSettings {
    /// Rauschunterdrückung: 0 = aus, 1–4 = leicht bis sehr stark.
    pub noise: u8,
    /// Echounterdrückung an/aus.
    pub echo: bool,
}

impl VoiceSettings {
    /// Ob überhaupt etwas verarbeitet werden muss.
    pub fn active(&self) -> bool {
        self.noise > 0 || self.echo
    }

    fn config(&self) -> Config {
        let level = match self.noise {
            0 => None,
            1 => Some(NoiseSuppressionLevel::Low),
            2 => Some(NoiseSuppressionLevel::Moderate),
            3 => Some(NoiseSuppressionLevel::High),
            _ => Some(NoiseSuppressionLevel::VeryHigh),
        };
        let mut cfg = Config {
            high_pass_filter: Some(HighPassFilter::default()),
            echo_canceller: self.echo.then(EchoCanceller::default),
            noise_suppression: level.map(|level| NoiseSuppression { level, ..Default::default() }),
            ..Default::default()
        };
        // Volle Bandbreite behalten (Standard wäre 32 kHz → Höhen über 16 kHz weg).
        cfg.pipeline.maximum_internal_processing_rate = MaxProcessingRate::Rate48kHz;
        cfg
    }
}

/// Ein Prozessor je Aufnahme-Sitzung (Kanalzahl ist fest).
pub struct VoiceProcessor {
    apm: AudioProcessing,
    settings: VoiceSettings,
    channels: usize,
    out: Vec<i16>,
    mono: Vec<i16>,
    render_out: Vec<i16>,
}

impl VoiceProcessor {
    /// `channels` = Kanalzahl des Mikrofonsignals (1 oder 2).
    pub fn new(settings: VoiceSettings, channels: usize) -> Self {
        let channels = channels.clamp(1, 2);
        let apm = AudioProcessing::builder()
            .config(settings.config())
            .capture_config(StreamConfig::new(RATE, channels as u16))
            .render_config(StreamConfig::new(RATE, 1))
            .build();
        Self {
            apm,
            settings,
            channels,
            out: vec![0; CHUNK * channels],
            mono: vec![0; CHUNK],
            render_out: vec![0; CHUNK],
        }
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// Geänderte Einstellungen übernehmen (ohne den Lernzustand zu verwerfen,
    /// wenn sich nichts geändert hat).
    pub fn set_settings(&mut self, settings: VoiceSettings) {
        if settings != self.settings {
            self.apm.apply_config(settings.config());
            self.settings = settings;
        }
    }

    /// Mikrofon-Frame (interleaved i16, `channels` Kanäle, Vielfaches von 10 ms)
    /// an Ort und Stelle verarbeiten. `delay_ms` = geschätzte Zeit zwischen
    /// Render-Aufruf und Aufnahme des Echos (Wiedergabepuffer + Geräte).
    pub fn process_capture(&mut self, frame: &mut [i16], delay_ms: i32) {
        let n = CHUNK * self.channels;
        for block in frame.chunks_exact_mut(n) {
            if self.settings.echo {
                let _ = self.apm.set_stream_delay_ms(delay_ms);
            }
            if self.apm.process_capture_i16(block, &mut self.out).is_ok() {
                block.copy_from_slice(&self.out);
            }
        }
    }

    /// Lautsprechersignal (interleaved i16 mit `channels` Kanälen, Vielfaches
    /// von 10 ms) als Referenz für die Echounterdrückung einspeisen.
    pub fn process_render(&mut self, frame: &[i16], channels: usize) {
        if !self.settings.echo {
            return;
        }
        let ch = channels.max(1);
        for block in frame.chunks_exact(CHUNK * ch) {
            for (i, m) in self.mono.iter_mut().enumerate() {
                let sum: i32 = block[i * ch..(i + 1) * ch].iter().map(|&s| s as i32).sum();
                *m = (sum / ch as i32) as i16;
            }
            let _ = self.apm.process_render_i16(&self.mono, &mut self.render_out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stille bleibt Stille, und die Länge bleibt erhalten (lautlos, kein Gerät).
    #[test]
    fn verarbeitet_frames_in_10ms_bloecken() {
        let mut vp = VoiceProcessor::new(VoiceSettings { noise: 2, echo: true }, 1);
        let mut frame = vec![0i16; 960];
        for _ in 0..20 {
            vp.process_render(&[0i16; 960 * 2], 2);
            vp.process_capture(&mut frame, 60);
        }
        assert_eq!(frame.len(), 960);
        assert!(frame.iter().all(|&s| s.abs() < 50));
    }

    /// Ein reines Lautsprecher-Echo im Mikrofon wird deutlich gedämpft.
    #[test]
    fn echo_wird_gedaempft() {
        let mut vp = VoiceProcessor::new(VoiceSettings { noise: 0, echo: true }, 1);
        let mut seed: u32 = 1;
        let mut energy_in = 0f64;
        let mut energy_out = 0f64;
        // 4 s weißes Rauschen als Fernsignal, das 1:1 (halbe Lautstärke) im Mikro landet.
        for f in 0..200 {
            let far: Vec<i16> = (0..960)
                .map(|_| {
                    seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                    ((seed >> 16) as i16) / 4
                })
                .collect();
            vp.process_render(&far, 1);
            let mut mic: Vec<i16> = far.iter().map(|&s| s / 2).collect();
            let before: f64 = mic.iter().map(|&s| (s as f64).powi(2)).sum();
            vp.process_capture(&mut mic, 0);
            if f >= 150 {
                energy_in += before;
                energy_out += mic.iter().map(|&s| (s as f64).powi(2)).sum::<f64>();
            }
        }
        assert!(energy_out < energy_in * 0.25, "in={energy_in} out={energy_out}");
    }
}
