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
            error: Some("Server is full".to_string()),
        };
    }

    let reject = |msg: &str| AuthResponse {
        success: false,
        user_id: None,
        token: None,
        server_name: None,
        rooms: None,
        error: Some(msg.to_string()),
    };

    // Authenticate. Bei unbekanntem Benutzer und aktivierter Registrierung wird
    // der Account mit dem angegebenen Passwort angelegt und der Login fortgesetzt.
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
                // Benutzer existiert → falsches Passwort
                return reject("Invalid username or password");
            }
            if !queries::is_registration_open(db).await {
                return reject("Invalid username or password");
            }
            // Registrierung: neuen Account anlegen und einloggen
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

    // Check ban
    if let Ok(Some(ban)) = queries::is_user_banned(db, db_user.id, peer_ip).await {
        return AuthResponse {
            success: false,
            user_id: None,
            token: None,
            server_name: None,
            rooms: None,
            error: Some(format!("You are banned: {}", ban.reason)),
        };
    }

    let nickname = login.nickname.unwrap_or_else(|| db_user.username.clone());

    let online_user = OnlineUser {
        user_id: db_user.id,
        username: db_user.username,
        nickname,
        role: db_user.role,
        session_token: 0,
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
        tx,
    };

    let token = users.add_user(online_user).await;

    // Get room list
    let room_list = rooms.get_room_list().await.unwrap_or_default();

    AuthResponse {
        success: true,
        user_id: Some(db_user.id),
        token: Some(token.to_string()),
        server_name: Some(config.server.name.clone()),
        rooms: Some(room_list),
        error: None,
    }
}
