use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::OnceLock;

/// Optionaler, per Kommandozeile (`--cfg-path`) gesetzter Pfad zur Konfigdatei.
/// Wird nur einmal beim Start gesetzt; ist er leer, gilt der plattformübliche
/// Standardpfad (siehe `config_path`).
static CONFIG_PATH_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// Den Konfigpfad überschreiben (vom CLI-Flag `--cfg-path`). Nur einmal wirksam.
/// Übergebene relative Pfade werden so verwendet, wie sie sind.
pub fn set_config_path<P: Into<PathBuf>>(path: P) {
    let _ = CONFIG_PATH_OVERRIDE.set(path.into());
}

/// Ein gespeicherter Server (Lesezeichen) in der Serverliste.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerEntry {
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_true")]
    pub ssl: bool,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub nickname: String,
    /// Passwort im Klartext in der lokalen client.json (Lesezeichen-Komfort).
    #[serde(default)]
    pub password: String,
    /// Für diesen Server das zentrale Login (Identity Provider) statt Passwort
    /// verwenden. Erfordert eine Anmeldung im Tab „Server-Hub".
    #[serde(default)]
    pub use_central: bool,
    /// Im Hub-Modus (Multi-Tenant): ID des Unterservers, dem beigetreten wird.
    /// Leer = normaler Einzelserver.
    #[serde(default)]
    pub server_id: String,
    /// Audio-Port (Hub kann ≠ Steuerport+1 sein). 0 = Konvention Steuerport+1.
    #[serde(default)]
    pub audio_port: u16,
}

impl ServerEntry {
    /// Anzeigename für die Serverliste.
    pub fn label(&self) -> String {
        if self.name.trim().is_empty() {
            format!("{}:{}", self.host, self.port)
        } else {
            format!("{} ({}:{})", self.name, self.host, self.port)
        }
    }
}

/// Im Hub angemeldete Sitzung. Das Refresh-Token wird hier (lokal) gehalten;
/// das kurzlebige Access-Token wird bei Bedarf frisch geholt (Rotation).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct HubSession {
    pub central_uid: String,
    pub username: String,
    pub display_name: String,
    pub role: String,
    pub refresh_token: String,
    /// "active" (freigegeben) | "pending" (wartet auf Freigabe)
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClientConfig {
    /// Gespeicherte Server (Serverliste)
    #[serde(default)]
    pub servers: Vec<ServerEntry>,
    /// Aktuelle Hub-Anmeldung (zentrales Login), falls vorhanden.
    #[serde(default)]
    pub hub: Option<HubSession>,
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
    /// Server-Ereignisse per Sprachausgabe ansagen (Standard: an).
    #[serde(default = "default_true")]
    pub announce_events: bool,
}

fn default_port() -> u16 {
    9500
}
fn default_true() -> bool {
    true
}
fn default_sample_rate() -> u32 {
    48000
}
fn default_bit_depth() -> u8 {
    16
}
fn default_channels() -> u8 {
    1
}
fn default_volume() -> f32 {
    1.0
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            servers: Vec::new(),
            hub: None,
            sample_rate: default_sample_rate(),
            bit_depth: default_bit_depth(),
            channels: default_channels(),
            input_device: None,
            output_device: None,
            volume: default_volume(),
            announce_events: true,
        }
    }
}

fn config_path() -> PathBuf {
    // Per --cfg-path gesetzter Pfad hat Vorrang (sonst Standardpfad unten).
    if let Some(path) = CONFIG_PATH_OVERRIDE.get() {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).ok();
            }
        }
        return path.clone();
    }
    // Einstellungen unter accessyApplications/teamconference im plattformüblichen
    // Konfigverzeichnis (Windows: %APPDATA%\accessyApplications\teamconference,
    // macOS: ~/Library/Application Support/…, Linux: ~/.config/…).
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("accessyApplications")
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
    let json = serde_json::to_string_pretty(config).map_err(|e| format!("Serialize error: {}", e))?;
    std::fs::write(&path, json).map_err(|e| format!("Write error: {}", e))?;
    Ok(())
}
