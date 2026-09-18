use serde::Deserialize;
use std::path::Path;

// Jeder Wert hat einen Default und kann über eine TC_*-Umgebungsvariable
// überschrieben werden. Reihenfolge: Default → config.toml (falls vorhanden,
// auch unvollständig) → Umgebungsvariablen. Damit ist für Docker keine
// config.toml nötig.

#[derive(Debug, Deserialize, Clone, Default)]
#[serde(default)]
pub struct Config {
    pub server: ServerConfig,
    pub network: NetworkConfig,
    pub tls: TlsConfig,
    pub audio: AudioConfig,
    pub storage: StorageConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct ServerConfig {
    pub name: String,
    pub welcome_message: String,
    pub max_users: u32,
    /// Anfangswert für die Selbstregistrierung beim ersten Start. Zur Laufzeit
    /// kann ein Admin sie umschalten (in der DB-Tabelle `settings` gespeichert).
    pub allow_registration: bool,
    /// Zentrales Login (Identity Provider srvhub.accessy.org) nutzen. Ist es an,
    /// melden sich normale Nutzer mit einem zentralen Access-Token an (statt
    /// Passwort); nur der lokale Admin-Account bleibt passwortbasiert.
    pub central_login: bool,
    /// Basis-URL des zentralen Logins (für die Public-Key-Abfrage /v2/keys).
    pub central_login_url: String,
    /// Optionaler Public Key (Hex) — überspringt die /v2/keys-Abfrage beim Start.
    pub central_login_pubkey: String,
    /// Multi-Tenant-Modus (Hub): EIN Prozess beherbergt viele „Unterserver" als
    /// abgeschottete Bereiche (server_id). Aus = klassischer Einzelserver
    /// (selbst hostbar, unverändertes Verhalten). Erfordert zentrales Login.
    pub multi_tenant: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            name: "TeamConference Server".into(),
            welcome_message: "Willkommen auf dem TeamConference Server!".into(),
            max_users: 100,
            allow_registration: false,
            central_login: false,
            central_login_url: "https://srvapi.accessy.org".into(),
            central_login_pubkey: String::new(),
            multi_tenant: false,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct NetworkConfig {
    pub control_host: String,
    pub control_port: u16,
    pub audio_host: String,
    pub audio_port: u16,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            control_host: "0.0.0.0".into(),
            control_port: 9500,
            audio_host: "0.0.0.0".into(),
            audio_port: 9501,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct TlsConfig {
    pub enabled: bool,
    pub cert_file: String,
    pub key_file: String,
    pub auto_generate: bool,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            cert_file: "certs/server.crt".into(),
            key_file: "certs/server.key".into(),
            auto_generate: true,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct AudioConfig {
    pub default_sample_rate: u32,
    pub default_bit_depth: u8,
    pub default_channels: u8,
    pub max_sample_rate: u32,
    pub max_bit_depth: u8,
}

impl Default for AudioConfig {
    fn default() -> Self {
        Self {
            default_sample_rate: 48000,
            default_bit_depth: 16,
            default_channels: 1,
            max_sample_rate: 192000,
            max_bit_depth: 32,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct StorageConfig {
    pub database_path: String,
    pub upload_dir: String,
    pub max_upload_size_mb: u64,
    /// Gesamt-Speicherlimit dieses Servers in Byte. **0 = unbegrenzt** (Default
    /// für selbst gehostete Server, die nur das zentrale Login nutzen). Nur
    /// Hub-gehostete Unterserver bekommen vom Hub ein Limit gesetzt (z. B. 2 GiB,
    /// von Hub-Admins erweiterbar) über TC_FILE_LIMIT_BYTES.
    pub file_limit_bytes: i64,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            database_path: "data/teamconference.db".into(),
            upload_dir: "data/uploads".into(),
            max_upload_size_mb: 100,
            file_limit_bytes: 0,
        }
    }
}

#[derive(Debug, Deserialize, Clone)]
#[serde(default)]
pub struct LoggingConfig {
    pub level: String,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self { level: "info".into() }
    }
}

/// Überschreibt `target` mit dem geparsten Wert der Umgebungsvariablen `var`,
/// falls gesetzt. (eprintln statt tracing — Logging ist hier noch nicht initialisiert.)
fn env_override<T: std::str::FromStr>(target: &mut T, var: &str)
where
    T::Err: std::fmt::Display,
{
    if let Ok(value) = std::env::var(var) {
        match value.parse::<T>() {
            Ok(parsed) => *target = parsed,
            Err(e) => eprintln!("WARNUNG: Ungültiger Wert für {}: {} ({})", var, value, e),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let mut config = if path.exists() {
            let content = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("Failed to read config file {:?}: {}", path, e))?;
            toml::from_str(&content)
                .map_err(|e| anyhow::anyhow!("Failed to parse config file: {}", e))?
        } else {
            eprintln!(
                "Konfigurationsdatei {:?} nicht gefunden — verwende Defaults und Umgebungsvariablen.",
                path
            );
            Config::default()
        };
        config.apply_env_overrides();
        Ok(config)
    }

    /// Alle Werte sind per TC_*-Umgebungsvariable überschreibbar (siehe README).
    fn apply_env_overrides(&mut self) {
        env_override(&mut self.server.name, "TC_SERVER_NAME");
        env_override(&mut self.server.welcome_message, "TC_WELCOME_MESSAGE");
        env_override(&mut self.server.max_users, "TC_MAX_USERS");
        env_override(&mut self.server.allow_registration, "TC_ALLOW_REGISTRATION");
        env_override(&mut self.server.central_login, "TC_CENTRAL_LOGIN");
        env_override(&mut self.server.central_login_url, "TC_CENTRAL_LOGIN_URL");
        env_override(&mut self.server.central_login_pubkey, "TC_CENTRAL_LOGIN_PUBKEY");
        env_override(&mut self.server.multi_tenant, "TC_MULTI_TENANT");

        env_override(&mut self.network.control_host, "TC_CONTROL_HOST");
        env_override(&mut self.network.control_port, "TC_CONTROL_PORT");
        env_override(&mut self.network.audio_host, "TC_AUDIO_HOST");
        env_override(&mut self.network.audio_port, "TC_AUDIO_PORT");

        env_override(&mut self.tls.enabled, "TC_TLS_ENABLED");
        env_override(&mut self.tls.cert_file, "TC_TLS_CERT_FILE");
        env_override(&mut self.tls.key_file, "TC_TLS_KEY_FILE");
        env_override(&mut self.tls.auto_generate, "TC_TLS_AUTO_GENERATE");

        env_override(&mut self.audio.default_sample_rate, "TC_AUDIO_DEFAULT_SAMPLE_RATE");
        env_override(&mut self.audio.default_bit_depth, "TC_AUDIO_DEFAULT_BIT_DEPTH");
        env_override(&mut self.audio.default_channels, "TC_AUDIO_DEFAULT_CHANNELS");
        env_override(&mut self.audio.max_sample_rate, "TC_AUDIO_MAX_SAMPLE_RATE");
        env_override(&mut self.audio.max_bit_depth, "TC_AUDIO_MAX_BIT_DEPTH");

        env_override(&mut self.storage.database_path, "TC_DATABASE_PATH");
        env_override(&mut self.storage.upload_dir, "TC_UPLOAD_DIR");
        env_override(&mut self.storage.max_upload_size_mb, "TC_MAX_UPLOAD_SIZE_MB");
        env_override(&mut self.storage.file_limit_bytes, "TC_FILE_LIMIT_BYTES");

        env_override(&mut self.logging.level, "TC_LOG_LEVEL");
    }
}
