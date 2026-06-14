use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::protocol::{FileInfo, Message, RoomInfo};

/// Audio configuration for the local client.
#[derive(Debug, Clone)]
pub struct AudioConfig {
    pub sample_rate: u32,
    pub bit_depth: u8,
    pub channels: u8,
    /// Opus-Bitrate in Bit/s; 0 = automatisch aus Kanälen ableiten
    pub bitrate: u32,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            bit_depth: 16,
            channels: 1,
            bitrate: 0,
        }
    }
}

/// Effektive Opus-Bitrate (Bit/s) bestimmen: 0 = automatisch nach Kanalzahl.
pub fn effective_bitrate(configured: u32, channels: u8) -> i32 {
    if configured > 0 {
        configured as i32
    } else if channels <= 1 {
        128_000
    } else {
        256_000
    }
}

/// A file upload that waits for the server's `file_upload_ack`
/// (the server assigns the upload_id).
#[derive(Debug)]
pub struct PendingUpload {
    pub filename: String,
    pub data: Vec<u8>,
}

/// Mutable inner state protected by parking_lot::Mutex.
/// Shared between the UI thread and the tokio network/audio tasks.
#[derive(Debug, Default)]
pub struct InnerState {
    // Connection
    pub connected: bool,
    pub authenticated: bool,
    pub user_id: Option<i64>,
    pub session_token: Option<u32>,
    pub server_name: Option<String>,
    pub nickname: String,

    // Rooms
    pub rooms: Vec<RoomInfo>,
    pub current_room_id: Option<i64>,

    // Audio
    pub muted: bool,
    pub deafened: bool,
    pub loopback: bool,
    pub audio_config: AudioConfig,
    pub capturing: bool,
    pub input_device: Option<String>,
    pub output_device: Option<String>,

    // WS sender (for sending messages to server)
    pub ws_tx: Option<mpsc::UnboundedSender<Message>>,

    // UDP
    pub udp_socket: Option<Arc<UdpSocket>>,
    pub server_udp_addr: Option<String>,

    // Playback device info (set by playback.rs on start)
    pub playback_device_channels: u16,

    // Audio pipeline shutdown signals
    pub capture_shutdown: Option<tokio::sync::watch::Sender<bool>>,
    pub playback_shutdown: Option<tokio::sync::watch::Sender<bool>>,

    // File streaming
    pub stream_shutdown: Option<tokio::sync::watch::Sender<bool>>,
    pub streaming_file: bool,

    // Files in the current room (updated on file_list, read by download action)
    pub current_files: Vec<FileInfo>,

    // Upload that waits for the server-assigned upload_id
    pub pending_upload: Option<PendingUpload>,

    // Download accumulator: file_id -> (target path, received bytes)
    pub download_targets: HashMap<i64, (PathBuf, Vec<u8>)>,
}

impl InnerState {
    pub fn room_name(&self, room_id: i64) -> String {
        self.rooms
            .iter()
            .find(|r| r.id == room_id)
            .map(|r| r.name.clone())
            .unwrap_or_else(|| format!("Raum {}", room_id))
    }

    pub fn nickname_of(&self, user_id: i64) -> String {
        for room in &self.rooms {
            if let Some(u) = room.users.iter().find(|u| u.id == user_id) {
                return u.nickname.clone();
            }
        }
        format!("Nutzer {}", user_id)
    }

    /// Ob der angemeldete Nutzer Administrator ist (Rolle aus der Raumliste).
    pub fn is_self_admin(&self) -> bool {
        let Some(uid) = self.user_id else { return false };
        for room in &self.rooms {
            if let Some(u) = room.users.iter().find(|u| u.id == uid) {
                return u.role == "admin";
            }
        }
        false
    }
}

/// Thread-safe application state shared across UI and network tasks.
pub struct AppState {
    pub inner: Mutex<InnerState>,
    /// crossbeam sender for UDP recv task → audio playback
    pub playback_tx: Mutex<Option<crossbeam_channel::Sender<Vec<u8>>>>,
    /// crossbeam receiver for audio playback callback
    pub playback_rx: Mutex<Option<crossbeam_channel::Receiver<Vec<u8>>>>,
    /// Atomic flag: file streaming is active, playback should mix audio
    pub file_streaming: AtomicBool,
    /// Atomic flag: file streaming is paused (no audio is produced while true)
    pub stream_paused: AtomicBool,
    /// Playback volume as f32 bits (0.0 – 1.0), read lock-free in the audio callback
    pub volume_bits: Arc<AtomicU32>,
    /// Datei-Stream → Empfangs-Mischer: 20-ms-Blöcke (i16, Wiedergabeformat) zum
    /// lokalen Mithören der gestreamten Datei (der Server schickt sie nicht
    /// zurück). Wird im UDP-Empfangs-/Mischtakt mit eingehendem Audio gemischt.
    pub local_audio_tx: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<i16>>>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(InnerState {
                playback_device_channels: 2,
                ..Default::default()
            }),
            playback_tx: Mutex::new(None),
            playback_rx: Mutex::new(None),
            file_streaming: AtomicBool::new(false),
            stream_paused: AtomicBool::new(false),
            volume_bits: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            local_audio_tx: Mutex::new(None),
        }
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume_bits.load(Ordering::Relaxed))
    }

    pub fn set_volume(&self, v: f32) {
        // bis 2.0 (200 %) — Verstärkung über 1.0 wird in der Wiedergabe geclamped
        self.volume_bits
            .store(v.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);
    }

    /// Send a protocol message to the server via WebSocket.
    pub fn send_ws(&self, msg: Message) -> Result<(), String> {
        let state = self.inner.lock();
        if let Some(ref tx) = state.ws_tx {
            tx.send(msg)
                .map_err(|e| format!("WebSocket send failed: {}", e))
        } else {
            Err("Nicht verbunden".into())
        }
    }
}
