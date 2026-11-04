//! Übersetzungen der Oberfläche (rust-i18n).
//!
//! Alle Texte liegen in `client/locales/<code>.yml` und werden beim Kompilieren
//! eingebettet (`rust_i18n::i18n!` in main.rs). Im Code steht nur der Schlüssel:
//! `t!("connect.caption")` bzw. mit Platzhaltern `t!("hub.logged_in_as", name = n)`
//! zu `%{name}` in der YAML-Datei.
//!
//! Neue Sprache: `locales/de.yml` kopieren (z. B. als `fr.yml`), übersetzen und
//! unten in `LANGUAGES` eintragen. Fehlende Schlüssel fallen auf Englisch und
//! dann Deutsch zurück.
//!
//! Server-Meldungen kommen als sprachneutraler Code (`code` + `params`) und
//! werden hier unter `server.<code>` übersetzt; `message` bleibt Fallback für
//! ältere Server.

use rust_i18n::t;

/// Wählbare Sprachen: (Code, Name in der jeweiligen Sprache).
pub const LANGUAGES: &[(&str, &str)] = &[("de", "Deutsch"), ("en", "English")];

/// Sprache für die Sitzung aktivieren. `configured` ist der Code aus der
/// client.json; leer = Systemsprache.
pub fn init(configured: &str) {
    rust_i18n::set_locale(resolve(configured));
}

/// Gewünschte Sprache auf eine verfügbare abbilden. Unbekannte Systemsprachen
/// bekommen Englisch.
pub fn resolve(configured: &str) -> &'static str {
    let wanted = if configured.trim().is_empty() {
        sys_locale::get_locale().unwrap_or_default()
    } else {
        configured.to_string()
    };
    // "de-DE", "de_AT", "en-US" → "de" / "en"
    let lang = wanted
        .split(['-', '_', '.'])
        .next()
        .unwrap_or("")
        .to_lowercase();
    LANGUAGES
        .iter()
        .find(|(code, _)| *code == lang)
        .map(|(code, _)| *code)
        .unwrap_or("en")
}

/// Schlüssel übersetzen, `None` wenn er in keiner Sprache existiert
/// (rust-i18n liefert dann den Schlüssel selbst zurück).
fn lookup(key: &str) -> Option<String> {
    let text = t!(key).to_string();
    if text.is_empty() || text.ends_with(key) {
        None
    } else {
        Some(text)
    }
}

/// `%{name}`-Platzhalter mit Werten aus einem JSON-Objekt füllen.
fn fill(mut text: String, params: Option<&serde_json::Map<String, serde_json::Value>>) -> String {
    if let Some(params) = params {
        for (k, v) in params {
            let value = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            text = text.replace(&format!("%{{{}}}", k), &value);
        }
    }
    text
}

/// Server-Code übersetzen; ohne Übersetzung den Klartext-Fallback zeigen.
pub fn server_error(
    code: &Option<String>,
    params: &Option<serde_json::Map<String, serde_json::Value>>,
    fallback: Option<&str>,
) -> String {
    if let Some(text) = code.as_deref().and_then(|c| lookup(&format!("server.{}", c))) {
        return fill(text, params.as_ref());
    }
    match fallback {
        Some(f) if !f.is_empty() => f.to_string(),
        _ => t!("common.unknown_error").to_string(),
    }
}

/// Text einer Server-Nachricht mit `code`/`params`/`message` (z. B.
/// `account_ack`, `error`) in der eingestellten Sprache.
pub fn server_text(data: &serde_json::Value) -> String {
    let code = data.get("code").and_then(|v| v.as_str()).map(|s| s.to_string());
    let params = data.get("params").and_then(|v| v.as_object()).cloned();
    let message = data.get("message").and_then(|v| v.as_str()).unwrap_or("");
    if code.is_none() {
        return message.to_string();
    }
    server_error(&code, &params, Some(message))
}

/// Fehlercode des Server-Hubs (`{"error": "<code>"}`) übersetzen, falls unter
/// `hub_errors.<code>` bekannt; sonst unverändert durchreichen.
pub fn hub_error(code: &str) -> String {
    lookup(&format!("hub_errors.{}", code)).unwrap_or_else(|| code.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_maps_system_locales() {
        assert_eq!(resolve("de"), "de");
        assert_eq!(resolve("en"), "en");
        assert_eq!(resolve("de-AT"), "de");
        assert_eq!(resolve("en_US.UTF-8"), "en");
        assert_eq!(resolve("fr-FR"), "en");
    }

    #[test]
    fn server_codes_are_translated_with_params() {
        rust_i18n::set_locale("en");
        let data = serde_json::json!({
            "code": "account.deleted",
            "params": { "username": "bob" },
            "message": "Konto 'bob' gelöscht"
        });
        assert_eq!(server_text(&data), "Account 'bob' deleted");
        // Unbekannter Code → Klartext des Servers
        let data = serde_json::json!({ "code": "does.not.exist", "message": "x" });
        assert_eq!(server_text(&data), "x");
        // Ohne Code → message
        assert_eq!(server_text(&serde_json::json!({ "message": "hi" })), "hi");
    }

    /// Jede Sprachdatei muss dieselben Schlüssel haben wie de.yml.
    #[test]
    fn all_locales_have_the_same_keys() {
        fn keys(v: &serde_yaml::Value, prefix: &str, out: &mut Vec<String>) {
            if let serde_yaml::Value::Mapping(m) = v {
                for (k, v) in m {
                    let k = k.as_str().unwrap_or_default();
                    if k.starts_with('_') {
                        continue;
                    }
                    let full = if prefix.is_empty() { k.to_string() } else { format!("{}.{}", prefix, k) };
                    if v.is_mapping() {
                        keys(v, &full, out);
                    } else {
                        out.push(full);
                    }
                }
            }
        }
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("locales");
        let load = |code: &str| {
            let text = std::fs::read_to_string(dir.join(format!("{}.yml", code))).unwrap();
            let v: serde_yaml::Value = serde_yaml::from_str(&text).unwrap();
            let mut out = Vec::new();
            keys(&v, "", &mut out);
            out.sort();
            out
        };
        let base = load("de");
        for (code, _) in LANGUAGES {
            assert_eq!(load(code), base, "Schlüssel in {}.yml weichen von de.yml ab", code);
        }
    }
}
