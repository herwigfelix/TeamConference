use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub server: ServerConfig,
    pub network: NetworkConfig,
    pub tls: TlsConfig,
    pub audio: AudioConfig,
    pub storage: StorageConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Deserialize, Clone)]
pub struct ServerConfig {
    pub name: String,
    pub welcome_message: String,
    pub max_users: u32,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NetworkConfig {
    pub control_host: String,
    pub control_port: u16,
    pub audio_host: String,
    pub audio_port: u16,
}

#[derive(Debug, Deserialize, Clone)]
pub struct TlsConfig {
    pub enabled: bool,
    pub cert_file: String,
    pub key_file: String,
    pub auto_generate: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct AudioConfig {
    pub default_sample_rate: u32,
    pub default_bit_depth: u8,
    pub default_channels: u8,
    pub max_sample_rate: u32,
    pub max_bit_depth: u8,
}

#[derive(Debug, Deserialize, Clone)]
pub struct StorageConfig {
    pub database_path: String,
    pub upload_dir: String,
    pub max_upload_size_mb: u64,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LoggingConfig {
    pub level: String,
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("Failed to read config file {:?}: {}", path, e))?;
        let config: Config = toml::from_str(&content)
            .map_err(|e| anyhow::anyhow!("Failed to parse config file: {}", e))?;
        Ok(config)
    }
}
