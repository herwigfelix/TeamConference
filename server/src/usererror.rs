//! Meldungen an den Client mit sprachneutralem Code.
//!
//! Der Server übersetzt nicht selbst: Er schickt `code` (+ `params`) und
//! zusätzlich `message` als Klartext-Fallback für ältere Clients. Die Texte zu
//! den Codes stehen im Client unter `server.<code>` in `client/locales/*.yml`.
//! Neue Codes dort in jeder Sprachdatei ergänzen.

use std::fmt;

use crate::control::protocol::Message;

/// Fehler mit Code für die Übersetzung im Client.
#[derive(Debug)]
pub struct CodedError {
    pub code: &'static str,
    pub params: serde_json::Value,
    pub message: String,
}

impl fmt::Display for CodedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for CodedError {}

/// Fehler ohne Platzhalter.
pub fn coded(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    coded_with(code, serde_json::Value::Null, message)
}

/// Fehler mit Platzhalterwerten (`params` ist ein JSON-Objekt).
pub fn coded_with(code: &'static str, params: serde_json::Value, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(CodedError { code, params, message: message.into() })
}

/// JSON-Felder `message`/`code`/`params` für eine Meldung.
pub fn fields(code: &str, params: &serde_json::Value, message: &str) -> serde_json::Value {
    let mut v = serde_json::json!({ "message": message, "code": code });
    if params.is_object() {
        v["params"] = params.clone();
    }
    v
}

/// Felder eines beliebigen Fehlers: mit Code, falls es ein `CodedError` ist,
/// sonst (z. B. Datenbankfehler) nur `message`.
pub fn error_fields(e: &anyhow::Error) -> serde_json::Value {
    match e.downcast_ref::<CodedError>() {
        Some(c) => fields(c.code, &c.params, &c.message),
        None => serde_json::json!({ "message": e.to_string() }),
    }
}

/// `error`-Nachricht zu einem Fehler.
pub fn error_msg(e: &anyhow::Error) -> Message {
    Message::new("error", error_fields(e))
}

/// `error`-Nachricht direkt aus Code und Fallback-Text.
pub fn error_code(code: &str, message: &str) -> Message {
    Message::new("error", fields(code, &serde_json::Value::Null, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coded_errors_carry_code_and_params() {
        let e = coded_with("room.full", serde_json::json!({ "room": "Lobby" }), "Room is full");
        assert_eq!(e.to_string(), "Room is full");
        let f = error_fields(&e);
        assert_eq!(f["code"], "room.full");
        assert_eq!(f["params"]["room"], "Lobby");
        assert_eq!(f["message"], "Room is full");
    }

    #[test]
    fn plain_errors_have_only_a_message() {
        let f = error_fields(&anyhow::anyhow!("db down"));
        assert_eq!(f, serde_json::json!({ "message": "db down" }));
    }
}
