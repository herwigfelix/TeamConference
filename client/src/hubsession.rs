//! Hub-Sitzung: frisches Access-Token mit Refresh-Rotation.
//!
//! Der Hub rotiert das Refresh-Token bei jeder Erneuerung und sperrt die ganze
//! Sitzung („kompromittiert"), sobald ein bereits eingetauschtes Token erneut
//! auftaucht. Zwei gleichzeitige Erneuerungen mit demselben Token — parallele
//! Hub-Aufrufe in einem Client oder zwei Clients mit derselben client.json —
//! reichen dafür aus. Deshalb läuft die Erneuerung hier serialisiert:
//! prozessweit per Mutex und über alle Instanzen per Dateisperre neben der
//! client.json. Innerhalb der Sperre wird das jeweils aktuelle Token frisch
//! aus der Datei gelesen. Access-Tokens werden bis kurz vor Ablauf
//! wiederverwendet, damit nicht jeder Aufruf eine Rotation auslöst.
//!
//! Lehnt der Hub die Sitzung ab, wird sie lokal gelöscht und `take_lost()`
//! meldet das dem UI-Thread (Abmelden + Hinweis, siehe main.rs).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use rust_i18n::t;

use crate::config;
use crate::hub::{RefreshError, TokenBundle};

/// Zwischengespeichertes Access-Token (Konto, Token, Ablauf in Unix-Sekunden).
struct Cached {
    central_uid: String,
    access_token: String,
    expires_at: u64,
}

/// Prozessweite Sperre um Erneuerung + Cache.
static STATE: Mutex<Option<Cached>> = Mutex::new(None);
/// Sitzung wurde vom Hub abgelehnt und lokal gelöscht (für den UI-Thread).
static LOST: AtomicBool = AtomicBool::new(false);
static LOST_REASON: Mutex<String> = Mutex::new(String::new());

/// Sicherheitsabstand vor Ablauf eines Access-Tokens.
const EXPIRY_MARGIN_SECS: u64 = 30;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Ablaufzeit (`exp`) aus einem JWT lesen, ohne die Signatur zu prüfen.
fn jwt_expiry(token: &str) -> Option<u64> {
    let payload = token.split('.').nth(1)?;
    let bytes = URL_SAFE_NO_PAD.decode(payload.trim_end_matches('=')).ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("exp")?.as_u64()
}

/// Gültiges Access-Token aus dem Cache, falls es zum gespeicherten Konto gehört.
fn cached_token(cache: &Option<Cached>, central_uid: &str) -> Option<String> {
    cache
        .as_ref()
        .filter(|c| c.central_uid == central_uid && c.expires_at > now() + EXPIRY_MARGIN_SECS)
        .map(|c| c.access_token.clone())
}

fn remember(cache: &mut Option<Cached>, b: &TokenBundle) {
    *cache = jwt_expiry(&b.access_token).map(|exp| Cached {
        central_uid: b.central_uid.clone(),
        access_token: b.access_token.clone(),
        expires_at: exp,
    });
}

/// Dateisperre für alle Client-Instanzen mit derselben client.json. Wird beim
/// Drop freigegeben. Kann sie nicht angelegt werden, läuft es ohne (nur der
/// prozessweite Mutex schützt dann).
fn lock_file() -> Option<std::fs::File> {
    let path = config::config_file_path().with_extension("lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .ok()?;
    file.lock().ok()?;
    Some(file)
}

/// Token-Bündel als lokale Hub-Sitzung speichern (Login, Verifizierung,
/// Erneuerung) und das Access-Token merken.
pub fn store(b: &TokenBundle) {
    let mut cache = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let _file = lock_file();
    save_bundle(b);
    remember(&mut cache, b);
}

fn save_bundle(b: &TokenBundle) {
    let mut cfg = config::load_config();
    cfg.hub = Some(config::HubSession {
        central_uid: b.central_uid.clone(),
        username: b.username.clone(),
        display_name: b.display_name.clone(),
        role: b.role.clone(),
        refresh_token: b.refresh_token.clone(),
        status: b.status.clone(),
    });
    let _ = config::save_config(&cfg);
}

/// Zwischengespeichertes Access-Token verwerfen (beim Abmelden).
pub fn forget() {
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Gültiges Access-Token liefern; erneuert die Sitzung bei Bedarf. Blockiert
/// (Netzwerk) — nicht auf dem UI-Thread aufrufen, außer für kurze Dialog-Aktionen
/// wie bisher.
pub fn access_token() -> Result<String, String> {
    let mut cache = STATE.lock().unwrap_or_else(|e| e.into_inner());
    let _file = lock_file();
    // Erst nach dem Sperren lesen: eine andere Instanz kann das Token gerade
    // rotiert haben.
    let session = config::load_config()
        .hub
        .filter(|h| !h.refresh_token.is_empty())
        .ok_or_else(|| t!("hub.not_logged_in_hub").to_string())?;
    if let Some(token) = cached_token(&cache, &session.central_uid) {
        return Ok(token);
    }
    match crate::hub::refresh(&session.refresh_token) {
        Ok(bundle) => {
            save_bundle(&bundle);
            remember(&mut cache, &bundle);
            Ok(bundle.access_token)
        }
        Err(RefreshError::Rejected(reason)) => {
            *cache = None;
            // Nur löschen, wenn inzwischen keine neue Anmeldung gespeichert wurde.
            let mut cfg = config::load_config();
            if cfg.hub.as_ref().map(|h| &h.refresh_token) == Some(&session.refresh_token) {
                cfg.hub = None;
                let _ = config::save_config(&cfg);
            }
            *LOST_REASON.lock().unwrap_or_else(|e| e.into_inner()) = reason.clone();
            LOST.store(true, Ordering::SeqCst);
            Err(t!("hub.session_lost", reason = reason).to_string())
        }
        Err(RefreshError::Failed(e)) => Err(e),
    }
}

/// Wurde die Sitzung seit dem letzten Aufruf vom Hub abgelehnt? Liefert dann
/// den Grund (einmalig).
pub fn take_lost() -> Option<String> {
    if LOST.swap(false, Ordering::SeqCst) {
        Some(std::mem::take(&mut *LOST_REASON.lock().unwrap_or_else(|e| e.into_inner())))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn jwt(payload: serde_json::Value) -> String {
        format!("x.{}.sig", URL_SAFE_NO_PAD.encode(payload.to_string()))
    }

    #[test]
    fn reads_jwt_expiry() {
        assert_eq!(jwt_expiry(&jwt(serde_json::json!({ "exp": 1234 }))), Some(1234));
        assert_eq!(jwt_expiry(&jwt(serde_json::json!({ "sub": "u" }))), None);
        assert_eq!(jwt_expiry("kein-jwt"), None);
    }

    #[test]
    fn cache_only_serves_valid_tokens_of_same_account() {
        let fresh = Some(Cached { central_uid: "u1".into(), access_token: "a".into(), expires_at: now() + 600 });
        assert_eq!(cached_token(&fresh, "u1").as_deref(), Some("a"));
        assert_eq!(cached_token(&fresh, "u2"), None);
        let stale = Some(Cached { central_uid: "u1".into(), access_token: "a".into(), expires_at: now() + 5 });
        assert_eq!(cached_token(&stale, "u1"), None);
    }
}
