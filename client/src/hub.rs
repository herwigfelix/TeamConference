//! HTTP-Client zum zentralen Login (Identity Provider, srvhub.accessy.org).
//!
//! Blockierende Aufrufe (ureq) — IMMER von einem Hintergrund-Thread aus nutzen,
//! nie auf dem UI-Thread. Die Basis-URL kommt aus `SRVHUB_BASE_URL` (für lokales
//! Debuggen, siehe start.sh --hub), sonst die Produktiv-URL.

use serde::Deserialize;
use serde_json::json;

const DEFAULT_BASE: &str = "https://srvapi.accessy.org";
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

fn get_auth(path: &str, token: &str) -> Result<serde_json::Value, String> {
    let url = format!("{}{}", base_url(), path);
    match ureq::get(&url)
        .set("Authorization", &format!("Bearer {}", token))
        .call()
    {
        Ok(resp) => resp
            .into_json::<serde_json::Value>()
            .map_err(|e| format!("Antwort unlesbar: {}", e)),
        Err(ureq::Error::Status(_c, resp)) => Err(resp
            .into_json::<serde_json::Value>()
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| "Serverfehler".into())),
        Err(e) => Err(format!("Netzwerkfehler: {}", e)),
    }
}

fn post_auth(path: &str, token: &str, body: serde_json::Value) -> Result<serde_json::Value, String> {
    let url = format!("{}{}", base_url(), path);
    match ureq::post(&url)
        .set("Authorization", &format!("Bearer {}", token))
        .set("Content-Type", "application/json")
        .send_json(body)
    {
        Ok(resp) => resp
            .into_json::<serde_json::Value>()
            .map_err(|e| format!("Antwort unlesbar: {}", e)),
        Err(ureq::Error::Status(_c, resp)) => Err(resp
            .into_json::<serde_json::Value>()
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| "Serverfehler".into())),
        Err(e) => Err(format!("Netzwerkfehler: {}", e)),
    }
}

/// Eintrag aus dem Server-Verzeichnis.
#[derive(Debug, Clone, Deserialize, serde::Serialize, Default)]
pub struct ServerInfo {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub is_public: bool,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub control_port: i64,
    #[serde(default)]
    pub audio_port: i64,
}

/// Server bearbeiten (Name/Beschreibung/öffentlich). Host/Ports unverändert
/// übernehmen, damit der Hub-gehostete Eintrag erreichbar bleibt.
#[allow(clippy::too_many_arguments)]
pub fn update_server(
    access_token: &str,
    server_id: &str,
    name: &str,
    description: &str,
    is_public: bool,
    host: &str,
    control_port: i64,
    audio_port: i64,
) -> Result<(), String> {
    post_auth(
        "/servers/update",
        access_token,
        json!({
            "server_id": server_id, "name": name, "description": description,
            "is_public": is_public, "host": host, "control_port": control_port, "audio_port": audio_port,
        }),
    )
    .map(|_| ())
}

pub fn delete_server(access_token: &str, server_id: &str) -> Result<(), String> {
    post_auth("/servers/delete", access_token, json!({ "server_id": server_id })).map(|_| ())
}

/// Eigenes Profil (Anzeigename + Bio) lesen — für vorausgefüllte Bearbeitung.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct Profile {
    #[serde(default)]
    #[allow(dead_code)]
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub bio: String,
}

pub fn get_my_profile(access_token: &str) -> Result<Profile, String> {
    let v = get_auth("/profile", access_token)?;
    serde_json::from_value(v.get("profile").cloned().unwrap_or_default())
        .map_err(|e| format!("Antwort unlesbar: {}", e))
}

/// Knapper Nutzereintrag (Admin-Listen / Suche).
#[derive(Debug, Clone, Deserialize, Default)]
pub struct UserSummary {
    pub central_uid: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub status: String,
}

pub fn admin_pending(access_token: &str) -> Result<Vec<UserSummary>, String> {
    let v = get_auth("/admin/pending", access_token)?;
    serde_json::from_value(v.get("pending").cloned().unwrap_or_default())
        .map_err(|e| format!("Antwort unlesbar: {}", e))
}

pub fn search_users(access_token: &str, q: &str) -> Result<Vec<UserSummary>, String> {
    let v = get_auth(&format!("/users/search?q={}", urlencode(q)), access_token)?;
    serde_json::from_value(v.get("results").cloned().unwrap_or_default())
        .map_err(|e| format!("Antwort unlesbar: {}", e))
}

pub fn admin_approve(access_token: &str, uid: &str) -> Result<(), String> {
    post_auth("/admin/approve", access_token, json!({ "central_uid": uid })).map(|_| ())
}
pub fn admin_ban(access_token: &str, uid: &str, reason: &str) -> Result<(), String> {
    post_auth("/admin/ban", access_token, json!({ "central_uid": uid, "reason": reason })).map(|_| ())
}
pub fn admin_unban(access_token: &str, uid: &str) -> Result<(), String> {
    post_auth("/admin/unban", access_token, json!({ "central_uid": uid })).map(|_| ())
}
pub fn admin_reset_password(access_token: &str, uid: &str, new_password: &str) -> Result<(), String> {
    post_auth("/admin/reset-password", access_token, json!({ "central_uid": uid, "new_password": new_password })).map(|_| ())
}
pub fn admin_promote(access_token: &str, uid: &str) -> Result<(), String> {
    post_auth("/admin/promote", access_token, json!({ "central_uid": uid })).map(|_| ())
}

/// Öffentliches Verzeichnis laden/durchsuchen.
pub fn list_servers(access_token: &str, q: &str) -> Result<Vec<ServerInfo>, String> {
    let path = if q.is_empty() {
        "/servers".to_string()
    } else {
        format!("/servers?q={}", urlencode(q))
    };
    let v = get_auth(&path, access_token)?;
    let arr = v.get("servers").cloned().unwrap_or_default();
    serde_json::from_value(arr).map_err(|e| format!("Antwort unlesbar: {}", e))
}

/// Neuen Server im Hub anlegen → server_id.
pub fn create_server(
    access_token: &str,
    name: &str,
    description: &str,
    is_public: bool,
    host: &str,
    control_port: i64,
    audio_port: i64,
) -> Result<String, String> {
    let v = post_auth(
        "/servers",
        access_token,
        json!({
            "name": name, "description": description, "is_public": is_public,
            "host": host, "control_port": control_port, "audio_port": audio_port,
        }),
    )?;
    Ok(v.get("server_id").and_then(|s| s.as_str()).unwrap_or("").to_string())
}

fn put_auth(path: &str, token: &str, body: serde_json::Value) -> Result<serde_json::Value, String> {
    let url = format!("{}{}", base_url(), path);
    match ureq::put(&url)
        .set("Authorization", &format!("Bearer {}", token))
        .set("Content-Type", "application/json")
        .send_json(body)
    {
        Ok(resp) => resp
            .into_json::<serde_json::Value>()
            .map_err(|e| format!("Antwort unlesbar: {}", e)),
        Err(ureq::Error::Status(_c, resp)) => Err(resp
            .into_json::<serde_json::Value>()
            .ok()
            .and_then(|v| v.get("error").and_then(|e| e.as_str()).map(|s| s.to_string()))
            .unwrap_or_else(|| "Serverfehler".into())),
        Err(e) => Err(format!("Netzwerkfehler: {}", e)),
    }
}

/// Offene Einladung.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct InviteInfo {
    pub id: String,
    #[serde(default)]
    pub server_name: String,
    #[serde(default)]
    #[allow(dead_code)]
    pub server_id: String,
}

pub fn list_invites(access_token: &str) -> Result<Vec<InviteInfo>, String> {
    let v = get_auth("/invites/mine", access_token)?;
    let arr = v.get("invites").cloned().unwrap_or_default();
    serde_json::from_value(arr).map_err(|e| format!("Antwort unlesbar: {}", e))
}

pub fn respond_invite(access_token: &str, invite_id: &str, accept: bool) -> Result<(), String> {
    post_auth(
        "/invites/respond",
        access_token,
        json!({ "invite_id": invite_id, "accept": accept }),
    )
    .map(|_| ())
}

/// Eigenes Profil (Anzeigename/Bio) setzen.
pub fn update_profile(access_token: &str, display_name: &str, bio: &str) -> Result<(), String> {
    put_auth(
        "/profile",
        access_token,
        json!({ "display_name": display_name, "bio": bio }),
    )
    .map(|_| ())
}

fn urlencode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
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
