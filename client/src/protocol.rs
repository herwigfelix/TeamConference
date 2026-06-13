use serde::{Deserialize, Serialize};

/// Wire-format message matching server protocol exactly.
/// The server uses `{"type": "...", "id": "...", "data": {...}}`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    #[serde(rename = "type")]
    pub msg_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default)]
    pub data: serde_json::Value,
}

impl Message {
    pub fn new(msg_type: &str, data: serde_json::Value) -> Self {
        Self {
            msg_type: msg_type.to_string(),
            id: None,
            data,
        }
    }
}

// ── Auth ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResponse {
    pub success: bool,
    #[serde(default)]
    pub user_id: Option<i64>,
    #[serde(default)]
    pub token: Option<String>,
    #[serde(default)]
    pub server_name: Option<String>,
    #[serde(default)]
    pub rooms: Option<Vec<RoomInfo>>,
    #[serde(default)]
    pub error: Option<String>,
}

// ── Rooms ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomInfo {
    pub id: i64,
    pub name: String,
    pub parent_id: Option<i64>,
    #[serde(default)]
    pub users: Vec<UserInfo>,
    #[serde(default)]
    pub max_users: i64,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub has_password: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserInfo {
    pub id: i64,
    pub nickname: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub deafened: bool,
}

// ── Audio ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioConfigAck {
    pub success: bool,
    #[serde(default)]
    pub udp_token: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioUserState {
    pub user_id: i64,
    pub muted: bool,
    pub deafened: bool,
}

// ── Files ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileInfo {
    pub id: i64,
    pub filename: String,
    pub size_bytes: i64,
    pub uploaded_by: Option<i64>,
    pub uploaded_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileUploadAck {
    pub upload_id: String,
    pub success: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileDownloadData {
    pub file_id: i64,
    pub data: String,
    pub offset: i64,
    pub total: i64,
}

// ── Stream ──

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamFileStatus {
    pub user_id: i64,
    pub filename: String,
    pub playing: bool,
}

// ── Audio Packet (UDP) ──

pub const AUDIO_MAGIC: [u8; 4] = [0x54, 0x43, 0x4F, 0x4E]; // "TCON"
pub const AUDIO_HEADER_SIZE: usize = 22;

#[derive(Debug, Clone)]
#[allow(dead_code)] // einige Felder dienen nur der Diagnose/Vollständigkeit
pub struct AudioPacketHeader {
    pub token: u32,
    pub sequence: u32,
    pub timestamp_ms: u32,
    pub sample_rate: u16,
    pub bit_depth: u8,
    pub channels: u8,
    pub payload_length: u16,
}

impl AudioPacketHeader {
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < AUDIO_HEADER_SIZE {
            return None;
        }
        if data[0..4] != AUDIO_MAGIC {
            return None;
        }
        Some(Self {
            token: u32::from_le_bytes([data[4], data[5], data[6], data[7]]),
            sequence: u32::from_le_bytes([data[8], data[9], data[10], data[11]]),
            timestamp_ms: u32::from_le_bytes([data[12], data[13], data[14], data[15]]),
            sample_rate: u16::from_le_bytes([data[16], data[17]]),
            bit_depth: data[18],
            channels: data[19],
            payload_length: u16::from_le_bytes([data[20], data[21]]),
        })
    }

    pub fn payload<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        let end = (AUDIO_HEADER_SIZE + self.payload_length as usize).min(data.len());
        &data[AUDIO_HEADER_SIZE..end]
    }
}

pub fn build_audio_packet(
    token: u32,
    seq: u32,
    timestamp_ms: u32,
    sample_rate: u16,
    bit_depth: u8,
    channels: u8,
    pcm_data: &[u8],
) -> Vec<u8> {
    let payload_len = pcm_data.len() as u16;
    let mut packet = Vec::with_capacity(AUDIO_HEADER_SIZE + pcm_data.len());
    packet.extend_from_slice(&AUDIO_MAGIC);
    packet.extend_from_slice(&token.to_le_bytes());
    packet.extend_from_slice(&seq.to_le_bytes());
    packet.extend_from_slice(&timestamp_ms.to_le_bytes());
    packet.extend_from_slice(&sample_rate.to_le_bytes());
    packet.push(bit_depth);
    packet.push(channels);
    packet.extend_from_slice(&payload_len.to_le_bytes());
    packet.extend_from_slice(pcm_data);
    packet
}
