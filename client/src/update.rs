//! Einfacher Auto-Updater.
//!
//! Beim Start (und auf Wunsch über das Menü) wird das neueste GitHub-Release
//! abgefragt. Ist es neuer als die laufende Version, fragt der Client nach und
//! lädt auf Bestätigung das zur Plattform passende Paket (macOS: .dmg,
//! Windows: .zip, Linux: .tar.gz) in den Download-Ordner. Anschließend wird die
//! Datei mit dem Standardprogramm geöffnet (dmg mounten, Archiv öffnen), sodass
//! der Nutzer die neue Version installieren kann.
//!
//! Netzwerkzugriffe laufen über `ureq` (rustls/ring, blockierend) in einem
//! eigenen Thread; Ergebnisse werden als synthetische `Message` über den
//! UI-Kanal zurückgereicht und vom Handler auf dem UI-Thread verarbeitet.

use crate::app::Ctx;
use crate::protocol::Message;

const REPO: &str = "herwigfelix/TeamConference";

/// Laufende Client-Version (aus Cargo.toml).
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// Release-Übersichtsseite (Fallback, falls kein passendes Asset existiert).
pub fn releases_page() -> String {
    format!("https://github.com/{}/releases/latest", REPO)
}

/// "v1.2.3-beta" → [1, 2, 3]
fn parse_ver(s: &str) -> Vec<u32> {
    s.trim()
        .trim_start_matches('v')
        .split('-')
        .next()
        .unwrap_or("")
        .split('.')
        .map(|p| p.parse().unwrap_or(0))
        .collect()
}

/// Ob `latest` eine höhere Version als `current` ist.
fn is_newer(latest: &str, current: &str) -> bool {
    let (l, c) = (parse_ver(latest), parse_ver(current));
    for i in 0..l.len().max(c.len()) {
        let lv = l.get(i).copied().unwrap_or(0);
        let cv = c.get(i).copied().unwrap_or(0);
        if lv != cv {
            return lv > cv;
        }
    }
    false
}

/// Passt der Asset-Name zur aktuellen Plattform?
fn platform_matches(name: &str) -> bool {
    let n = name.to_lowercase();
    #[cfg(target_os = "macos")]
    {
        return n.ends_with(".dmg");
    }
    #[cfg(target_os = "windows")]
    {
        return n.ends_with(".zip");
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        return n.ends_with(".tar.gz") || n.ends_with(".tgz");
    }
    #[allow(unreachable_code)]
    {
        let _ = n;
        false
    }
}

/// Auf neue Version prüfen. Bei `manual = true` wird auch „bereits aktuell"
/// bzw. ein Fehler gemeldet; beim automatischen Start-Check bleibt es still.
pub fn check_for_update(ctx: &Ctx, manual: bool) {
    let ev_tx = ctx.ev_tx.clone();
    std::thread::spawn(move || match fetch_latest() {
        Ok((tag, asset)) => {
            if is_newer(&tag, current_version()) {
                let (url, filename) = asset.unwrap_or_default();
                let _ = ev_tx.send(Message::new(
                    "client_update",
                    serde_json::json!({ "version": tag, "url": url, "filename": filename }),
                ));
            } else if manual {
                let _ = ev_tx.send(Message::new(
                    "client_error",
                    serde_json::json!({
                        "message": format!("TeamConference ist aktuell (Version {}).", current_version())
                    }),
                ));
            }
        }
        Err(e) => {
            if manual {
                let _ = ev_tx.send(Message::new(
                    "client_error",
                    serde_json::json!({ "message": format!("Aktualisierungsprüfung fehlgeschlagen: {}", e) }),
                ));
            }
        }
    });
}

/// Neuestes Release abfragen: liefert (tag_name, Option<(download_url, dateiname)>).
fn fetch_latest() -> Result<(String, Option<(String, String)>), String> {
    let url = format!("https://api.github.com/repos/{}/releases/latest", REPO);
    let body = ureq::get(&url)
        .set("User-Agent", "TeamConference-Updater")
        .set("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?
        .into_string()
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
    let tag = json
        .get("tag_name")
        .and_then(|v| v.as_str())
        .ok_or("Antwort ohne tag_name")?
        .to_string();
    let mut asset = None;
    if let Some(arr) = json.get("assets").and_then(|v| v.as_array()) {
        for a in arr {
            let name = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if platform_matches(name) {
                if let Some(dl) = a.get("browser_download_url").and_then(|v| v.as_str()) {
                    asset = Some((dl.to_string(), name.to_string()));
                    break;
                }
            }
        }
    }
    Ok((tag, asset))
}

/// Asset herunterladen (eigener Thread) und den Zielpfad zurückmelden.
pub fn download_update(
    ev_tx: tokio::sync::mpsc::UnboundedSender<Message>,
    url: String,
    filename: String,
) {
    std::thread::spawn(move || match download(&url, &filename) {
        Ok(path) => {
            let _ = ev_tx.send(Message::new(
                "client_update_done",
                serde_json::json!({ "path": path.to_string_lossy() }),
            ));
        }
        Err(e) => {
            let _ = ev_tx.send(Message::new(
                "client_error",
                serde_json::json!({ "message": format!("Download fehlgeschlagen: {}", e) }),
            ));
        }
    });
}

fn download(url: &str, filename: &str) -> Result<std::path::PathBuf, String> {
    let dir = dirs::download_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    std::fs::create_dir_all(&dir).ok();
    let safe = if filename.is_empty() {
        "TeamConference-Update"
    } else {
        filename
    };
    let path = dir.join(safe);
    let resp = ureq::get(url)
        .set("User-Agent", "TeamConference-Updater")
        .call()
        .map_err(|e| e.to_string())?;
    let mut reader = resp.into_reader();
    let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    std::io::copy(&mut reader, &mut file).map_err(|e| e.to_string())?;
    Ok(path)
}

/// Datei oder URL mit dem Standardprogramm öffnen
/// (macOS: `open`, Windows: `explorer`/Browser, Linux: `xdg-open`).
pub fn open_path(path: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("explorer").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}
