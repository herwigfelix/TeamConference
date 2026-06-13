use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_true")]
    pub ssl: bool,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub nickname: String,
    #[serde(default = "default_sample_rate")]
    pub sample_rate: u32,
    #[serde(default = "default_bit_depth")]
    pub bit_depth: u8,
    #[serde(default = "default_channels")]
    pub channels: u8,
    #[serde(default)]
    pub input_device: Option<String>,
    #[serde(default)]
    pub output_device: Option<String>,
    #[serde(default = "default_volume")]
    pub volume: f32,
}

fn default_host() -> String { "localhost".into() }
fn default_port() -> u16 { 9500 }
fn default_true() -> bool { true }
fn default_sample_rate() -> u32 { 48000 }
fn default_bit_depth() -> u8 { 16 }
fn default_channels() -> u8 { 1 }
fn default_volume() -> f32 { 1.0 }

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            ssl: true,
            username: String::new(),
            nickname: String::new(),
            sample_rate: default_sample_rate(),
            bit_depth: default_bit_depth(),
            channels: default_channels(),
            input_device: None,
            output_device: None,
            volume: default_volume(),
        }
    }
}

fn config_path() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("teamconference");
    std::fs::create_dir_all(&dir).ok();
    dir.join("client.json")
}

pub fn load_config() -> ClientConfig {
    let path = config_path();
    if path.exists() {
        match std::fs::read_to_string(&path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => ClientConfig::default(),
        }
    } else {
        ClientConfig::default()
    }
}

pub fn save_config(config: &ClientConfig) -> Result<(), String> {
    let path = config_path();
    let json = serde_json::to_string_pretty(config)
        .map_err(|e| format!("Serialize error: {}", e))?;
    std::fs::write(&path, json)
        .map_err(|e| format!("Write error: {}", e))?;
    Ok(())
}
