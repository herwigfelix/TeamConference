//! HTTP-Client zum zentralen Login (Identity Provider, srvhub.accessy.org).
//!
//! Blockierende Aufrufe (ureq) — IMMER von einem Hintergrund-Thread aus nutzen,
//! nie auf dem UI-Thread. Die Basis-URL kommt aus `SRVHUB_BASE_URL` (für lokales
//! Debuggen, siehe start.sh --hub), sonst die Produktiv-URL.

use serde::Deserialize;
use serde_json::json;

const DEFAULT_BASE: &str = "https://srvhub.accessy.org";
const DEVICE_LABEL: &str = "TeamConference Desktop";

pub fn base_url() -> String {
    std::env::var("SRVHUB_BASE_URL")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.to_string())
}

/// Vom Hub ausgestelltes Token-Paar samt Kontostatus.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TokenBundle {
    pub access_token: String,
    pub refresh_token: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub approved: bool,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub central_uid: String,
    #[serde(default)]
    pub team_contact: String,
}

fn post(path: &str, body: serde_json::Value) -> Result<serde_json::Value, String> {
    let url = format!("{}{}", base_url(), path);
    match ureq::post(&url)
        .set("Content-Type", "application/json")
        .send_json(body)
    {
        Ok(resp) => resp
            .into_json::<serde_json::Value>()
            .map_err(|e| format!("Antwort unlesbar: {}", e)),
        Err(ureq::Error::Status(_code, resp)) => {
            let v = resp
                .into_json::<serde_json::Value>()
                .unwrap_or_else(|_| json!({}));
            Err(v
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("Serverfehler")
                .to_string())
        }
        Err(e) => Err(format!("Netzwerkfehler: {}", e)),
    }
}

fn bundle(v: serde_json::Value) -> Result<TokenBundle, String> {
    serde_json::from_value(v).map_err(|e| format!("Antwort unlesbar: {}", e))
}

/// Registrierung anstoßen → der Hub schickt einen SMS-/WhatsApp-Code.
pub fn register(
    phone: &str,
    username: &str,
    password: &str,
    display_name: &str,
) -> Result<(), String> {
    post(
        "/auth/register",
        json!({
            "phone": phone,
            "username": username,
            "password": password,
            "display_name": display_name,
        }),
    )
    .map(|_| ())
}

/// Code bestätigen → Token-Paar.
pub fn verify(phone: &str, code: &str) -> Result<TokenBundle, String> {
    post(
        "/auth/verify",
        json!({ "phone": phone, "code": code, "device_label": DEVICE_LABEL }),
    )
    .and_then(bundle)
}

/// Anmeldung mit Benutzername/Telefon + Passwort → Token-Paar.
pub fn login(identifier: &str, password: &str) -> Result<TokenBundle, String> {
    post(
        "/auth/login",
        json!({ "identifier": identifier, "password": password, "device_label": DEVICE_LABEL }),
    )
    .and_then(bundle)
}

/// Refresh-Token gegen ein frisches Paar tauschen (Rotation).
pub fn refresh(refresh_token: &str) -> Result<TokenBundle, String> {
    post(
        "/auth/refresh",
        json!({ "refresh_token": refresh_token, "device_label": DEVICE_LABEL }),
    )
    .and_then(bundle)
}

pub fn logout(refresh_token: &str) -> Result<(), String> {
    post("/auth/logout", json!({ "refresh_token": refresh_token })).map(|_| ())
}

pub fn reset_start(phone: &str) -> Result<(), String> {
    post("/auth/reset/start", json!({ "phone": phone })).map(|_| ())
}

pub fn reset_confirm(phone: &str, code: &str, new_password: &str) -> Result<(), String> {
    post(
        "/auth/reset/confirm",
        json!({ "phone": phone, "code": code, "new_password": new_password }),
    )
    .map(|_| ())
}
