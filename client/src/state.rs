use std::collections::{HashMap, HashSet};
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
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            sample_rate: 48000,
            bit_depth: 16,
            channels: 1,
        }
    }
}

/// A file upload that waits for the server's `file_upload_ack`
/// (the server assigns the upload_id).
#[derive(Debug)]
pub struct PendingUpload {
    pub filename: String,
    pub data: Vec<u8>,
}

/// Art eines Knotens in der Windows-Baumansicht.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeKind {
    Room,
    User,
}

/// Ein sichtbarer (aufgeklappter) Knoten der Windows-Baumansicht.
/// Das flache `ui_tree` bildet die `StandardListView`-Zeilen 1:1 ab.
#[derive(Debug, Clone)]
pub struct TreeNode {
    pub kind: TreeKind,
    /// Raum-ID (bei Room) bzw. Nutzer-ID (bei User)
    pub id: i64,
    /// Zugehöriger Raum: bei Room == id, bei User der Raum, in dem er ist
    pub room_id: i64,
}

/// Mutable inner state protected by parking_lot::Mutex.
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

    // UI bookkeeping: maps list indices to ids (rebuilt with the models)
    pub ui_room_ids: Vec<i64>,
    pub ui_user_ids: Vec<i64>,
    pub ui_files: Vec<FileInfo>,
    pub chat_log: String,

    // Windows-Baumansicht: flache Knotenliste + aufgeklappte Räume
    pub ui_tree: Vec<TreeNode>,
    pub expanded_rooms: HashSet<i64>,

    // Join that waits for a password dialog
    pub pending_join_room: Option<i64>,

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
    /// Playback volume as f32 bits (0.0 – 1.0), read lock-free in the audio callback
    pub volume_bits: Arc<AtomicU32>,
    /// Baumansicht statt zweier Listen (nur Windows). Steuert UI und Auswahl-Logik.
    pub tree_mode: bool,
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
            volume_bits: Arc::new(AtomicU32::new(1.0f32.to_bits())),
            tree_mode: cfg!(target_os = "windows"),
        }
    }

    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume_bits.load(Ordering::Relaxed))
    }

    pub fn set_volume(&self, v: f32) {
        self.volume_bits
            .store(v.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
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
