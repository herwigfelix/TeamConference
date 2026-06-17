//! Zugriffsprüfung gegen das Hub-Verzeichnis (nur im Multi-Tenant-Modus).
//!
//! Beim Beitritt zu einem Unterserver fragt der Server `GET <hub>/servers/:id`
//! mit dem Access-Token des Nutzers ab. Liefert die Antwort `owner_uid`, hat der
//! Nutzer Zugriff (öffentlich oder Mitglied) — sonst nicht.

pub struct DirServer {
    pub owner_uid: String,
    pub name: String,
}

pub async fn lookup(hub_url: &str, token: &str, server_id: &str) -> Result<DirServer, String> {
    let endpoint = format!("{}/servers/{}", hub_url.trim_end_matches('/'), server_id);
    let resp = reqwest::Client::new()
        .get(&endpoint)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| format!("Hub nicht erreichbar: {}", e))?;
    if !resp.status().is_success() {
        return Err(format!("Server nicht verfügbar ({})", resp.status()));
    }
    let v: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("ungültige Hub-Antwort: {}", e))?;
    let s = v.get("server").ok_or_else(|| "ungültige Hub-Antwort".to_string())?;
    // owner_uid ist nur enthalten, wenn der Nutzer den Server sehen darf.
    match s.get("owner_uid").and_then(|x| x.as_str()) {
        Some(owner) => Ok(DirServer {
            owner_uid: owner.to_string(),
            name: s.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        }),
        None => Err("kein Zugriff (privat oder nicht eingeladen)".into()),
    }
}
