use serde::{Deserialize, Serialize};

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

    pub fn with_id(msg_type: &str, id: &str, data: serde_json::Value) -> Self {
        Self {
            msg_type: msg_type.to_string(),
            id: Some(id.to_string()),
            data,
        }
    }
}

// Auth
#[derive(Debug, Deserialize)]
pub struct AuthLogin {
    pub username: String,
    pub password: String,
    #[serde(default)]
    pub nickname: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rooms: Option<Vec<RoomInfo>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// Rooms
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
    #[serde(default)]
    pub sample_rate: i64,
    #[serde(default)]
    pub bit_depth: i64,
    #[serde(default)]
    pub channels: i64,
    /// Opus-Bitrate in Bit/s; 0 = automatisch aus Kanälen ableiten
    #[serde(default)]
    pub bitrate: i64,
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
    /// UDP-Sitzungstoken dieses Nutzers. Wird mitgeschickt, damit Clients
    /// eingehende Audiopakete (die nur das Token tragen) einem Nutzer zuordnen
    /// und so eine lokale Pro-Nutzer-Lautstärke anwenden können.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp_token: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct RoomJoin {
    pub room_id: i64,
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RoomLeave {
    pub room_id: i64,
}

#[derive(Debug, Deserialize)]
pub struct RoomCreate {
    pub name: String,
    #[serde(default)]
    pub parent_id: Option<i64>,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default)]
    pub max_users: Option<i64>,
    #[serde(default)]
    pub sample_rate: Option<i64>,
    #[serde(default)]
    pub bit_depth: Option<i64>,
    #[serde(default)]
    pub channels: Option<i64>,
    #[serde(default)]
    pub bitrate: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct RoomDelete {
    pub room_id: i64,
}

#[derive(Debug, Deserialize)]
pub struct RoomUpdate {
    pub room_id: i64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub password: Option<Option<String>>,
    #[serde(default)]
    pub max_users: Option<i64>,
    #[serde(default)]
    pub sample_rate: Option<i64>,
    #[serde(default)]
    pub bit_depth: Option<i64>,
    #[serde(default)]
    pub channels: Option<i64>,
    #[serde(default)]
    pub bitrate: Option<i64>,
}

// Chat
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRoom {
    pub room_id: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_user: Option<UserInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatPrivate {
    pub to_user_id: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_user: Option<UserInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatServer {
    pub message: String,
}

// Audio config
#[derive(Debug, Deserialize)]
pub struct AudioConfigRequest {
    pub sample_rate: u32,
    pub bit_depth: u8,
    pub channels: u8,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool { true }

#[derive(Debug, Serialize)]
pub struct AudioConfigAck {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub udp_token: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub struct AudioMute {
    pub muted: bool,
}

#[derive(Debug, Deserialize)]
pub struct AudioDeafen {
    pub deafened: bool,
}

#[derive(Debug, Serialize)]
pub struct AudioUserState {
    pub user_id: i64,
    pub muted: bool,
    pub deafened: bool,
}

#[derive(Debug, Deserialize)]
pub struct AudioLoopback {
    pub enabled: bool,
}

// Files
#[derive(Debug, Deserialize)]
pub struct FileUploadStart {
    pub room_id: i64,
    pub filename: String,
    pub size: i64,
}

#[derive(Debug, Serialize)]
pub struct FileUploadAck {
    pub upload_id: String,
    pub success: bool,
}

#[derive(Debug, Deserialize)]
pub struct FileUploadChunk {
    pub upload_id: String,
    pub data: String, // base64
    pub offset: i64,
}

#[derive(Debug, Deserialize)]
pub struct FileUploadComplete {
    pub upload_id: String,
}

#[derive(Debug, Deserialize)]
pub struct FileListRequest {
    pub room_id: i64,
}

#[derive(Debug, Serialize)]
pub struct FileInfo {
    pub id: i64,
    pub filename: String,
    pub size_bytes: i64,
    pub uploaded_by: Option<i64>,
    pub uploaded_at: String,
}

#[derive(Debug, Deserialize)]
pub struct FileDownloadRequest {
    pub file_id: i64,
}

#[derive(Debug, Serialize)]
pub struct FileDownloadData {
    pub file_id: i64,
    pub data: String, // base64
    pub offset: i64,
    pub total: i64,
}

// Admin
#[derive(Debug, Deserialize)]
pub struct AdminKick {
    pub user_id: i64,
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AdminBan {
    pub user_id: i64,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub duration_minutes: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct AdminMove {
    pub user_id: i64,
    pub room_id: i64,
}

#[derive(Debug, Deserialize)]
pub struct AdminMute {
    pub user_id: i64,
    pub muted: bool,
}

#[derive(Debug, Deserialize)]
pub struct AdminServerMessage {
    pub message: String,
}

// Audio file streaming
#[derive(Debug, Deserialize)]
pub struct StreamFileStart {
    pub filename: String,
    pub room_id: i64,
}

#[derive(Debug, Serialize)]
pub struct StreamFileStatus {
    pub user_id: i64,
    pub filename: String,
    pub playing: bool,
}
