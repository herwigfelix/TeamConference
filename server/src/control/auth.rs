use std::sync::Arc;
use tokio_rusqlite::Connection;
use tokio::sync::mpsc;
use crate::config::Config;
use crate::control::protocol::*;
use crate::db::queries;
use crate::user::manager::{OnlineUser, UserManager};
use crate::room::manager::RoomManager;

pub async fn handle_login(
    login: AuthLogin,
    peer_ip: String,
    config: &Config,
    db: &Arc<Connection>,
    users: &Arc<UserManager>,
    rooms: &Arc<RoomManager>,
    central: Option<&crate::control::central::CentralVerifier>,
    tx: mpsc::UnboundedSender<Message>,
) -> AuthResponse {
    // Check max users
    if users.user_count().await >= config.server.max_users as usize {
        return AuthResponse {
            success: false,
            user_id: None,
            token: None,
            server_name: None,
            rooms: None,
            role: None,
            error: Some("Server is full".to_string()),
        };
    }

    let reject = |msg: &str| AuthResponse {
        success: false,
        user_id: None,
        token: None,
        server_name: None,
        rooms: None,
        role: None,
        error: Some(msg.to_string()),
    };

    // ── Anmeldung: Klango-Token ODER zentrales Token ODER lokaler Benutzer/Passwort ──
    // Ergebnis: (Account, Tenant/Unterserver, Rolle für diese Sitzung,
    // Klango-Gruppen mit Adminrecht, Anzeigename aus dem Token).
    let (db_user, tenant, session_role, klango_groups, token_nick) = if let Some(token) = login.klango_token.as_deref() {
        // Klango-Modus (docs/klango.md 1.1): HMAC-Token des Klango-Servers.
        if !config.server.klango_mode() {
            return reject("Dieser Server unterstützt keine Klango-Anmeldung");
        }
        let claims = match crate::control::klango::verify_token(&config.server.klango_secret, token) {
            Ok(c) => c,
            Err(e) => {
                tracing::info!("Klango-Token abgelehnt: {}", e);
                return reject("Klango-Token ungültig — bitte neu anmelden");
            }
        };
        let sub = claims.sub.trim().to_lowercase();
        let central_uid = format!("klango:{}", sub);
        let db_user = match queries::find_user_by_central_uid(db, central_uid.clone()).await {
            Ok(Some(u)) => u,
            Ok(None) => {
                let uname = unique_username(db, &sub).await;
                match queries::create_central_user(db, uname.clone(), central_uid.clone(), "user".to_string()).await {
                    Ok(id) => {
                        tracing::info!("Klango-Konto lokal angelegt: {} ({})", uname, central_uid);
                        crate::db::queries::DbUser { id, username: uname, role: "user".to_string() }
                    }
                    Err(e) => {
                        tracing::error!("Anlegen des Klango-Kontos fehlgeschlagen: {}", e);
                        return reject("Konto konnte nicht angelegt werden");
                    }
                }
            }
            Err(e) => {
                tracing::error!("central_uid-Lookup fehlgeschlagen: {}", e);
                return reject("Internal server error");
            }
        };
        let role = if claims.sadm { "admin".to_string() } else { db_user.role.clone() };
        (db_user, String::new(), role, claims.adm, claims.nick)
    } else if let Some(token) = login.central_token.as_deref() {
        // Client will zentrales Login.
        if !config.server.central_login {
            return reject("Dieser Server unterstützt kein zentrales Login");
        }
        let Some(verifier) = central else {
            return reject("Zentrales Login auf diesem Server nicht verfügbar");
        };
        let claims = match verifier.verify(token) {
            Ok(c) => c,
            Err(e) => {
                tracing::info!("Zentrales Token abgelehnt: {}", e);
                return reject("Zentrales Token ungültig — bitte neu anmelden");
            }
        };
        if !claims.apr {
            return reject("Konto noch nicht freigegeben — bitte auf die Freigabe warten");
        }
        // Lokalen Account zur zentralen Identität finden oder anlegen.
        let db_user = match queries::find_user_by_central_uid(db, claims.sub.clone()).await {
            Ok(Some(u)) => u,
            Ok(None) => {
                let uname = unique_username(db, &claims.un).await;
                match queries::create_central_user(db, uname.clone(), claims.sub.clone(), "user".to_string()).await {
                    Ok(id) => {
                        tracing::info!("Zentraler Account lokal angelegt: {} ({})", uname, claims.sub);
                        crate::db::queries::DbUser { id, username: uname, role: "user".to_string() }
                    }
                    Err(e) => {
                        tracing::error!("Anlegen des zentralen Accounts fehlgeschlagen: {}", e);
                        return reject("Konto konnte nicht angelegt werden");
                    }
                }
            }
            Err(e) => {
                tracing::error!("central_uid-Lookup fehlgeschlagen: {}", e);
                return reject("Internal server error");
            }
        };

        // Tenant (Unterserver) bestimmen.
        if config.server.multi_tenant {
            let Some(server_id) = login.server_id.as_deref().filter(|s| !s.is_empty()) else {
                return reject("Bitte einen Server wählen (server_id fehlt)");
            };
            // Zugriff über das Hub-Verzeichnis prüfen (öffentlich oder Mitglied).
            match crate::control::hubdir::lookup(&config.server.central_login_url, token, server_id).await {
                Ok(dir) => {
                    // Tenant + Standard-Raum bei Erstkontakt anlegen.
                    if queries::get_tenant(db, server_id.to_string()).await.ok().flatten().is_none() {
                        let _ = queries::create_tenant(db, server_id.to_string(), dir.owner_uid.clone(), dir.name.clone()).await;
                        let _ = queries::create_default_room(db, server_id.to_string()).await;
                        tracing::info!("Unterserver provisioniert: {} ({})", dir.name, server_id);
                    }
                    // Eigentümer des Unterservers ist dort Admin.
                    let role = if dir.owner_uid == claims.sub { "admin".to_string() } else { db_user.role.clone() };
                    (db_user, server_id.to_string(), role, Vec::new(), None)
                }
                Err(e) => return reject(&format!("Kein Zugriff auf diesen Server: {}", e)),
            }
        } else {
            let role = db_user.role.clone();
            (db_user, String::new(), role, Vec::new(), None)
        }
    } else {
        // Klassischer Pfad (Benutzername/Passwort).
        if config.server.multi_tenant {
            return reject("Dieser Hub erfordert zentrales Login");
        }
        let username = login.username.clone();
        let password = login.password.clone();
        let db_user = match queries::authenticate_user(db, username.clone(), password.clone()).await {
            Ok(Some(user)) => user,
            Ok(None) => {
                let exists = queries::find_user_by_username(db, username.clone())
                    .await
                    .ok()
                    .flatten()
                    .is_some();
                if exists {
                    return reject("Invalid username or password");
                }
                // Selbstregistrierung ist bei zentralem Login und im
                // Klango-Modus deaktiviert.
                if config.server.central_login || config.server.klango_mode() || !queries::is_registration_open(db).await {
                    return reject("Invalid username or password");
                }
                match queries::create_user(db, username.clone(), password, "user".to_string()).await {
                    Ok(id) => {
                        tracing::info!("Neuer Account per Registrierung angelegt: {}", username);
                        crate::db::queries::DbUser {
                            id,
                            username: username.clone(),
                            role: "user".to_string(),
                        }
                    }
                    Err(e) => {
                        tracing::error!("Registrierung fehlgeschlagen: {}", e);
                        return reject("Konto konnte nicht angelegt werden");
                    }
                }
            }
            Err(e) => {
                tracing::error!("Auth error: {}", e);
                return reject("Internal server error");
            }
        };
        let role = db_user.role.clone();
        (db_user, String::new(), role, Vec::new(), None)
    };

    // Check ban
    // S9: Bans speichern die bloße IP (ohne Port); peer_ip ist aber "ip:port".
    // Daher den Host-Teil extrahieren, sonst matcht der IP-Ban nie.
    let ban_ip = peer_ip
        .parse::<std::net::SocketAddr>()
        .map(|s| s.ip().to_string())
        .unwrap_or_else(|_| peer_ip.clone());
    if let Ok(Some(ban)) = queries::is_user_banned(db, db_user.id, ban_ip).await {
        return AuthResponse {
            success: false,
            user_id: None,
            token: None,
            server_name: None,
            rooms: None,
            role: None,
            error: Some(format!("You are banned: {}", ban.reason)),
        };
    }

    let nickname = login
        .nickname
        .filter(|n| !n.trim().is_empty())
        .or(token_nick.filter(|n| !n.trim().is_empty()))
        .unwrap_or_else(|| db_user.username.clone());

    let online_user = OnlineUser {
        user_id: db_user.id,
        username: db_user.username,
        nickname,
        role: session_role.clone(),
        tenant: tenant.clone(),
        session_token: 0,
        audio_id: 0,
        room_id: None,
        muted: false,
        deafened: false,
        admin_muted: false,
        loopback: false,
        udp_addr: None,
        audio_enabled: false,
        sample_rate: config.audio.default_sample_rate,
        bit_depth: config.audio.default_bit_depth,
        channels: config.audio.default_channels,
        klango_groups,
        tx,
    };

    let token = users.add_user(online_user).await;

    // Raumliste des eigenen Unterservers (im Einzelserver-Modus: tenant = ""),
    // aus Sicht dieses Nutzers (private Anrufräume nur für Beteiligte).
    let room_list = rooms.get_room_list_for(db_user.id).await.unwrap_or_default();

    AuthResponse {
        success: true,
        user_id: Some(db_user.id),
        token: Some(token.to_string()),
        server_name: Some(config.server.name.clone()),
        rooms: Some(room_list),
        role: Some(session_role),
        error: None,
    }
}

/// Aus dem zentralen Anzeigenamen einen lokal eindeutigen Benutzernamen ableiten
/// (Sonderzeichen entfernen, bei Kollision Zahl anhängen).
async fn unique_username(db: &Arc<Connection>, base: &str) -> String {
    let clean: String = base
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    let clean = if clean.is_empty() { "user".to_string() } else { clean };
    if queries::find_user_by_username(db, clean.clone()).await.ok().flatten().is_none() {
        return clean;
    }
    for i in 2..1000 {
        let cand = format!("{}{}", clean, i);
        if queries::find_user_by_username(db, cand.clone()).await.ok().flatten().is_none() {
            return cand;
        }
    }
    format!("{}-{}", clean, uuid::Uuid::new_v4())
}
