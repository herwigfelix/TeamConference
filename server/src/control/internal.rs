//! Interner Draht zum Klango-Server (docs/klango.md 1.6) — nur im Klango-Modus.
//!
//! Zwei Richtungen, beide mit demselben gemeinsamen Geheimnis
//! (`TC_KLANGO_SECRET`) in der Kopfzeile `X-Klango-Secret`:
//!
//!   Klango → hier   `POST /push`               Benachrichtigung an Nutzer
//!   hier → Klango   `POST /internal/presence`  wer gerade verbunden ist
//!
//! **Warum überhaupt.** Klango fragte bisher alle zwei Minuten nach, ob es
//! etwas Neues gibt — je angemeldetem Client. Der Konferenzserver hält aber
//! ohnehin zu jedem Client eine ständige, authentifizierte Verbindung. Über
//! die kann der Klango-Server sagen „für dich liegt etwas an", und der Client
//! holt es sich sofort. Das spart den Takt und erspart einen zweiten Daemon,
//! einen zweiten Port und einen WebSocket in Flask.
//!
//! **Warum die Anwesenheit hier entsteht.** Der Klango-Server erkannte
//! „online" bisher daran, dass in den letzten zehn Minuten eine Anfrage kam —
//! eine Nebenwirkung genau des Polls, der wegfällt. Wer wirklich verbunden
//! ist, weiß nur diese Stelle hier. Gemeldet wird deshalb die VOLLE Liste,
//! nicht ein Unterschied: dann erholt sich ein neu gestarteter Flask-Server
//! von selbst, statt auf Ewigkeit ein falsches Bild zu behalten.
//!
//! Beide Richtungen sind unkritisch: schlägt etwas fehl, wird es protokolliert
//! und sonst nichts. Konferenzen dürfen nicht darunter leiden, dass der
//! Klango-Server gerade neu startet.

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::sync::Notify;

use crate::control::protocol::Message;
use crate::user::manager::UserManager;

/// Größte zulässige Rumpflänge einer Push-Anfrage.
const MAX_BODY: usize = 64 * 1024;
/// Höchstzahl Empfänger je Anfrage.
const MAX_RECIPIENTS: usize = 500;
/// Mindestabstand zwischen zwei Anwesenheitsmeldungen. Eine Welle von
/// Anmeldungen ergibt so EINE Anfrage statt einer je Anmeldung.
const PRESENCE_DEBOUNCE: Duration = Duration::from_secs(2);
/// Fester Abgleich, auch wenn sich nichts geändert hat — damit ein Neustart
/// des Klango-Servers das Bild von selbst wieder richtig bekommt.
const PRESENCE_FULL_SYNC: Duration = Duration::from_secs(120);
/// Zeitlimit für beide Richtungen. Kurz: es hängt nichts daran.
const HTTP_TIMEOUT: Duration = Duration::from_secs(3);

/// Vergleich in konstanter Zeit. Die Länge verrät sich dabei — das ist bei
/// einem gemeinsamen Geheimnis fester Länge ohne Belang.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn plain(status: StatusCode, body: &str) -> Response<Full<Bytes>> {
    Response::builder()
        .status(status)
        .header("content-type", "application/json")
        .body(Full::new(Bytes::from(body.to_owned())))
        .expect("statische Antwort")
}

#[derive(serde::Deserialize)]
struct PushRequest {
    #[serde(default)]
    to: Vec<String>,
    #[serde(default)]
    kind: String,
}

async fn handle_push(
    req: Request<Incoming>,
    users: Arc<UserManager>,
    secret: Arc<String>,
) -> Result<Response<Full<Bytes>>, Infallible> {
    if req.method() != Method::POST || req.uri().path() != "/push" {
        return Ok(plain(StatusCode::NOT_FOUND, r#"{"ok":false}"#));
    }

    // Ohne Hinweis darauf, WAS falsch war — ein fehlender und ein falscher
    // Schlüssel sollen von außen gleich aussehen.
    let given = req
        .headers()
        .get("x-klango-secret")
        .map(|v| v.as_bytes().to_vec())
        .unwrap_or_default();
    if !ct_eq(&given, secret.as_bytes()) {
        return Ok(plain(StatusCode::FORBIDDEN, r#"{"ok":false}"#));
    }

    let body = match Limited::new(req.into_body(), MAX_BODY).collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return Ok(plain(StatusCode::BAD_REQUEST, r#"{"ok":false}"#)),
    };
    let parsed: PushRequest = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(_) => return Ok(plain(StatusCode::BAD_REQUEST, r#"{"ok":false}"#)),
    };
    if parsed.to.len() > MAX_RECIPIENTS {
        return Ok(plain(StatusCode::BAD_REQUEST, r#"{"ok":false}"#));
    }

    let kind = if parsed.kind.is_empty() {
        "whatsnew".to_string()
    } else {
        parsed.kind
    };
    let msg = Message::new("klango_push", serde_json::json!({ "kind": kind }));
    let delivered = users.push_to(&parsed.to, msg).await;

    Ok(plain(
        StatusCode::OK,
        &format!(r#"{{"ok":true,"delivered":{}}}"#, delivered),
    ))
}

/// Den internen Endpunkt starten. Bindet ausschließlich auf 127.0.0.1;
/// `port == 0` schaltet ihn ab. Schlägt das Binden fehl, läuft der Server
/// ohne ihn weiter (dann bleibt es beim bisherigen Abfragen im Client).
pub async fn start_push_endpoint(users: Arc<UserManager>, secret: String, port: u16) {
    if port == 0 {
        tracing::info!("Interner Push-Endpunkt aus (internal_port = 0)");
        return;
    }
    let addr = format!("127.0.0.1:{}", port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("Interner Push-Endpunkt kann {} nicht belegen: {}", addr, e);
            return;
        }
    };
    tracing::info!("Interner Push-Endpunkt auf http://{}/push", addr);

    let secret = Arc::new(secret);
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("Interner Endpunkt: accept fehlgeschlagen: {}", e);
                    continue;
                }
            };
            let users = users.clone();
            let secret = secret.clone();
            tokio::spawn(async move {
                let io = TokioIo::new(stream);
                let service = service_fn(move |req| handle_push(req, users.clone(), secret.clone()));
                if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                    tracing::debug!("Interner Endpunkt: Verbindung beendet: {}", e);
                }
            });
        }
    });
}

/// Anstoß für eine Anwesenheitsmeldung. `poke()` sagt nur „es hat sich etwas
/// geändert"; wann tatsächlich gemeldet wird, entscheidet der Hintergrundlauf.
pub struct Presence {
    notify: Arc<Notify>,
}

impl Presence {
    /// Nichts tun — für den Fall, dass keine Meldung eingerichtet ist.
    pub fn disabled() -> Arc<Self> {
        Arc::new(Self { notify: Arc::new(Notify::new()) })
    }

    pub fn poke(&self) {
        self.notify.notify_one();
    }
}

/// Anwesenheitsmeldung einrichten. Leere `klango_url` = aus (dann poked es ins
/// Leere, was nichts kostet).
pub fn start_presence(users: Arc<UserManager>, klango_url: String, secret: String) -> Arc<Presence> {
    let presence = Presence::disabled();
    let url = klango_url.trim().trim_end_matches('/').to_string();
    if url.is_empty() || secret.is_empty() {
        tracing::info!("Anwesenheitsmeldung an den Klango-Server aus (klango_url leer)");
        return presence;
    }
    let endpoint = format!("{}/internal/presence", url);
    tracing::info!("Anwesenheitsmeldung an {}", endpoint);

    let notify = presence.notify.clone();
    tokio::spawn(async move {
        let client = match reqwest::Client::builder().timeout(HTTP_TIMEOUT).build() {
            Ok(c) => c,
            Err(e) => {
                tracing::error!("Anwesenheitsmeldung: kein HTTP-Client: {}", e);
                return;
            }
        };
        // Damit ein Fehlschlag nicht bei jedem Anstoß eine Zeile schreibt.
        let mut klagte = false;
        loop {
            tokio::select! {
                _ = notify.notified() => {}
                _ = tokio::time::sleep(PRESENCE_FULL_SYNC) => {}
            }
            // Sammeln: weitere Anstöße in diesem Fenster fallen mit hinein.
            tokio::time::sleep(PRESENCE_DEBOUNCE).await;

            let online = users.push_listener_names().await;
            let mut body = HashMap::new();
            body.insert("online", online);
            match client
                .post(&endpoint)
                .header("X-Klango-Secret", &secret)
                .json(&body)
                .send()
                .await
            {
                Ok(r) if r.status().is_success() => {
                    klagte = false;
                }
                Ok(r) => {
                    if !klagte {
                        tracing::warn!("Anwesenheitsmeldung abgelehnt: {}", r.status());
                        klagte = true;
                    }
                }
                Err(e) => {
                    if !klagte {
                        tracing::warn!("Anwesenheitsmeldung nicht zustellbar: {}", e);
                        klagte = true;
                    }
                }
            }
        }
    });
    presence
}

#[cfg(test)]
mod tests {
    use super::ct_eq;

    #[test]
    fn ct_eq_vergleicht_inhalt_und_laenge() {
        assert!(ct_eq(b"geheim", b"geheim"));
        assert!(!ct_eq(b"geheim", b"geheiM"));
        assert!(!ct_eq(b"geheim", b"geheim2"));
        assert!(!ct_eq(b"", b"x"));
        assert!(ct_eq(b"", b""));
    }
}
