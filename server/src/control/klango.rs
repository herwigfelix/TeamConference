//! Klango-Modus (docs/klango.md): Anmelde-Token des Klango-Servers prüfen und
//! die Nachrichten bedienen, die es nur in diesem Modus gibt — Räume für
//! jedermann, Raum-Admins, Raum-Sperren, Anrufe.
//!
//! Aktiv, sobald `[server] klango_secret` gesetzt ist. Außerhalb des Modus
//! lehnen alle Handler hier mit "Insufficient permissions" ab, damit sich der
//! klassische Server nicht anders verhält als bisher.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::control::handler::SharedState;
use crate::control::protocol::*;
use crate::db::queries::{self, DbRoom, RoomExtra};
use crate::room::manager::{now_unix, PendingCall, RoomBanEntry, RoomManager};
use crate::user::manager::OnlineUser;

/// Wie lange ein Anruf klingelt, bevor der Server ihn abbricht.
const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
/// Grenzen für von Nutzern angelegte Räume (docs/klango.md 1.2).
const MAX_ROOMS_TOTAL: usize = 500;
const MAX_TEMP_ROOMS_PER_OWNER: usize = 3;

// ── Token ──

#[derive(Debug, Clone, Deserialize)]
pub struct KlangoClaims {
    /// Klango-ID (kleingeschrieben) — die Identität.
    pub sub: String,
    /// Anzeigename.
    #[serde(default)]
    pub nick: Option<String>,
    #[serde(default)]
    pub iat: i64,
    pub exp: i64,
    /// Gruppen (gid als String), in denen der Nutzer Admin oder Moderator ist.
    #[serde(default)]
    pub adm: Vec<String>,
    /// Klango-Serveradmin → Sitzungsrolle "admin".
    #[serde(default)]
    pub sadm: bool,
}

/// Token = b64url(claims) "." b64url(HMAC_SHA256(secret, b64url(claims))),
/// beides ohne Padding; die Signatur läuft über den Base64-STRING des
/// Claims-Teils. Vergleich in konstanter Zeit (`verify_slice`).
pub fn verify_token(secret: &str, token: &str) -> Result<KlangoClaims, String> {
    let (claims_b64, sig_b64) = token.split_once('.').ok_or("malformed token")?;
    if claims_b64.is_empty() || sig_b64.contains('.') {
        return Err("malformed token".into());
    }
    let sig = URL_SAFE_NO_PAD.decode(sig_b64).map_err(|_| "bad sig b64")?;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).map_err(|_| "bad secret")?;
    mac.update(claims_b64.as_bytes());
    mac.verify_slice(&sig).map_err(|_| "bad signature".to_string())?;

    let claims_json = URL_SAFE_NO_PAD.decode(claims_b64).map_err(|_| "bad claims b64")?;
    let claims: KlangoClaims = serde_json::from_slice(&claims_json).map_err(|_| "bad claims json")?;
    if claims.sub.trim().is_empty() {
        return Err("empty sub".into());
    }
    if claims.exp <= now_unix() {
        return Err("expired".into());
    }
    Ok(claims)
}

// ── Hilfen ──

fn error(tx: &mpsc::UnboundedSender<Message>, message: &str) {
    let _ = tx.send(Message::new("error", serde_json::json!({ "message": message })));
}

fn deny(tx: &mpsc::UnboundedSender<Message>) {
    error(tx, "Insufficient permissions");
}

/// Angemeldeter Nutzer plus Raum seines Tenants — oder eine Fehlermeldung an
/// den Absender.
async fn user_and_room(
    state: &SharedState,
    uid: i64,
    room_id: i64,
    tx: &mpsc::UnboundedSender<Message>,
) -> Option<(OnlineUser, DbRoom)> {
    let Some(user) = state.users.get_user(uid).await else {
        error(tx, "User not found");
        return None;
    };
    match state.rooms.get_room(room_id, &user.tenant).await {
        Ok(Some(room)) => Some((user, room)),
        _ => {
            error(tx, "Room not found");
            None
        }
    }
}

fn parse<T: serde::de::DeserializeOwned>(data: serde_json::Value, tx: &mpsc::UnboundedSender<Message>) -> Option<T> {
    match serde_json::from_value::<T>(data) {
        Ok(v) => Some(v),
        Err(_) => {
            error(tx, "Invalid message");
            None
        }
    }
}

/// Raum betreten — gemeinsamer Ablauf für `room_join`, `room_join_group` und
/// den Anrufer beim `call_invite`. Verlässt vorher den alten Raum (mit
/// Aufräumen), meldet `room_joined`, `room_user_joined` und die Raumliste.
/// True bei Erfolg.
pub async fn join_room_flow(
    state: &SharedState,
    uid: i64,
    room_id: i64,
    password: Option<&str>,
    tx: &mpsc::UnboundedSender<Message>,
) -> bool {
    let Some(user) = state.users.get_user(uid).await else { return false };
    let tenant = user.tenant.clone();
    let old_room = user.room_id;

    if let Err(e) = state.rooms.join_room(uid, room_id, password, &tenant).await {
        error(tx, &e.to_string());
        return false;
    }

    // Erst den alten Raum informieren und aufräumen (der Nutzer steht schon
    // im neuen — ein leerer temporärer Raum wird so sofort erkannt).
    if let Some(old) = old_room {
        if old != room_id {
            state.users.broadcast_to_room(old, Message::new("room_user_left", serde_json::json!({
                "room_id": old, "user_id": uid,
            })), Some(uid)).await;
            state.rooms.after_leave(uid, Some(old), &tenant).await;
        }
    }

    let _ = tx.send(Message::new("room_joined", serde_json::json!({ "room_id": room_id })));
    if old_room != Some(room_id) {
        if let Some(u) = state.users.get_user(uid).await {
            state.users.broadcast_to_room(room_id, Message::new("room_user_joined", serde_json::json!({
                "room_id": room_id, "user": u.to_info(),
            })), Some(uid)).await;
        }
    }
    state.rooms.send_room_list(uid).await;
    true
}

/// Nutzer aus seinem Raum nehmen (Kick/Sperre): Raum informieren, Zustand
/// zurücksetzen, aufräumen.
async fn remove_from_room(state: &SharedState, target: &OnlineUser, room_id: i64) {
    state.users.set_room(target.user_id, None).await;
    state.users.broadcast_to_room(room_id, Message::new("room_user_left", serde_json::json!({
        "room_id": room_id, "user_id": target.user_id,
    })), None).await;
    state.rooms.after_leave(target.user_id, Some(room_id), &target.tenant).await;
    state.rooms.send_room_list(target.user_id).await;
}

async fn send_bans(state: &SharedState, room_id: i64, tx: &mpsc::UnboundedSender<Message>) {
    let bans: Vec<serde_json::Value> = state
        .rooms
        .bans(room_id)
        .await
        .into_iter()
        .map(|(uid, b)| {
            serde_json::json!({
                "user_id": uid,
                "username": b.username,
                "nickname": b.nickname,
                "expires_at": b.expires_at,
            })
        })
        .collect();
    let _ = tx.send(Message::new("room_bans_result", serde_json::json!({
        "room_id": room_id, "bans": bans,
    })));
}

// ── Räume ──

/// `room_create` im Klango-Modus: jeder darf, der Raum ist temporär und gehört
/// dem Ersteller. Serveradmins bekommen mit `persistent: true` weiterhin
/// dauerhafte Räume.
pub async fn handle_room_create(
    state: &SharedState,
    uid: i64,
    req: RoomCreate,
    tx: &mpsc::UnboundedSender<Message>,
) {
    let Some(user) = state.users.get_user(uid).await else { return };
    let name = req.name.trim().to_string();
    if name.is_empty() || name.chars().count() > 80 {
        error(tx, "Ungültiger Raumname");
        return;
    }
    let persistent = user.is_admin() && req.persistent.unwrap_or(false);
    match state.rooms.room_counts(&user.tenant, uid).await {
        Ok((total, mine)) => {
            if total >= MAX_ROOMS_TOTAL {
                error(tx, "Zu viele Räume auf dem Server");
                return;
            }
            if !persistent && mine >= MAX_TEMP_ROOMS_PER_OWNER {
                error(tx, "Du hast schon zu viele eigene Räume");
                return;
            }
        }
        Err(e) => {
            error(tx, &e.to_string());
            return;
        }
    }
    let extra = RoomExtra {
        group_id: String::new(),
        owner_id: uid,
        temporary: !persistent,
        private: false,
    };
    match state.rooms.create_room_ext(
        name,
        req.password.filter(|p| !p.is_empty()),
        req.max_users.unwrap_or(0),
        req.sample_rate.unwrap_or(state.config.audio.default_sample_rate as i64),
        req.bit_depth.unwrap_or(state.config.audio.default_bit_depth as i64),
        req.channels.unwrap_or(state.config.audio.default_channels as i64),
        req.bitrate.unwrap_or(0),
        &user.tenant,
        extra,
    ).await {
        Ok(room_id) => {
            let _ = tx.send(Message::new("room_created", serde_json::json!({ "room_id": room_id })));
            state.rooms.broadcast_room_list(&user.tenant).await;
        }
        Err(e) => error(tx, &e.to_string()),
    }
}

pub async fn handle_room_join_group(
    state: &SharedState,
    uid: i64,
    data: serde_json::Value,
    tx: &mpsc::UnboundedSender<Message>,
) {
    let Some(req) = parse::<RoomJoinGroup>(data, tx) else { return };
    let Some(user) = state.users.get_user(uid).await else { return };
    let gid = req.group_id.trim().to_string();
    if gid.is_empty() || gid.chars().count() > 40 {
        error(tx, "Ungültige Gruppe");
        return;
    }
    let room_id = match state.rooms.find_group_room(&gid, &user.tenant).await {
        Ok(Some(r)) => r.id,
        Ok(None) => {
            let name = {
                let n = req.name.trim();
                if n.is_empty() { format!("Gruppe {}", gid) } else { n.chars().take(80).collect() }
            };
            match state.rooms.room_counts(&user.tenant, uid).await {
                Ok((total, _)) if total >= MAX_ROOMS_TOTAL => {
                    error(tx, "Zu viele Räume auf dem Server");
                    return;
                }
                Err(e) => {
                    error(tx, &e.to_string());
                    return;
                }
                _ => {}
            }
            let extra = RoomExtra { group_id: gid.clone(), owner_id: 0, temporary: true, private: false };
            match state.rooms.create_room_ext(
                name, None, 0,
                state.config.audio.default_sample_rate as i64,
                state.config.audio.default_bit_depth as i64,
                state.config.audio.default_channels as i64,
                0, &user.tenant, extra,
            ).await {
                Ok(id) => {
                    // Der neue Raum soll für alle sichtbar werden.
                    state.rooms.broadcast_room_list(&user.tenant).await;
                    id
                }
                Err(e) => {
                    error(tx, &e.to_string());
                    return;
                }
            }
        }
        Err(e) => {
            error(tx, &e.to_string());
            return;
        }
    };
    join_room_flow(state, uid, room_id, None, tx).await;
}

pub async fn handle_room_admin_set(
    state: &SharedState,
    uid: i64,
    data: serde_json::Value,
    tx: &mpsc::UnboundedSender<Message>,
) {
    let Some(req) = parse::<RoomAdminSet>(data, tx) else { return };
    let Some((user, room)) = user_and_room(state, uid, req.room_id, tx).await else { return };
    if !RoomManager::can_manage_admins(&user, &room) {
        deny(tx);
        return;
    }
    // Kontoname des Ziels: online bevorzugt, sonst aus der Datenbank.
    let username = match state.users.get_user(req.user_id).await {
        Some(t) => t.username,
        None => match queries::get_user_by_id(&state.db, req.user_id).await {
            Ok(Some(u)) => u.username,
            _ => {
                error(tx, "User not found");
                return;
            }
        },
    };
    state.rooms.set_room_admin(room.id, req.user_id, req.admin).await;
    state.users.broadcast_to_room(room.id, Message::new("room_admin_changed", serde_json::json!({
        "room_id": room.id, "user_id": req.user_id, "username": username, "admin": req.admin,
    })), None).await;
    state.rooms.broadcast_room_list(&user.tenant).await;
}

/// Gemeinsame Prüfung für Kick und Sperre: Moderator, Ziel im Raum, Ziel kein
/// Moderator. Liefert das Ziel.
async fn kick_target(
    state: &SharedState,
    user: &OnlineUser,
    room: &DbRoom,
    target_id: i64,
    tx: &mpsc::UnboundedSender<Message>,
) -> Option<OnlineUser> {
    if !state.rooms.user_is_room_mod(user, room).await {
        deny(tx);
        return None;
    }
    let Some(target) = state.users.get_user(target_id).await else {
        error(tx, "User not found");
        return None;
    };
    if target.room_id != Some(room.id) || target.tenant != user.tenant {
        error(tx, "User is not in this room");
        return None;
    }
    if target.user_id == user.user_id || state.rooms.user_is_room_mod(&target, room).await {
        deny(tx);
        return None;
    }
    Some(target)
}

pub async fn handle_room_kick(
    state: &SharedState,
    uid: i64,
    data: serde_json::Value,
    tx: &mpsc::UnboundedSender<Message>,
) {
    let Some(req) = parse::<RoomKick>(data, tx) else { return };
    let Some((user, room)) = user_and_room(state, uid, req.room_id, tx).await else { return };
    let Some(target) = kick_target(state, &user, &room, req.user_id, tx).await else { return };
    let reason = req.reason.unwrap_or_default();
    state.users.send_to_user(target.user_id, Message::new("room_kicked", serde_json::json!({
        "room_id": room.id, "room_name": room.name, "reason": reason,
    }))).await;
    remove_from_room(state, &target, room.id).await;
    tracing::info!("Raum {}: {} hat {} hinausgeworfen", room.id, user.username, target.username);
}

pub async fn handle_room_ban(
    state: &SharedState,
    uid: i64,
    data: serde_json::Value,
    tx: &mpsc::UnboundedSender<Message>,
) {
    let Some(req) = parse::<RoomBan>(data, tx) else { return };
    let Some((user, room)) = user_and_room(state, uid, req.room_id, tx).await else { return };
    let Some(target) = kick_target(state, &user, &room, req.user_id, tx).await else { return };
    let reason = req.reason.unwrap_or_default();
    let expires_at = req.duration_minutes.filter(|m| *m > 0).map(|m| now_unix() + m * 60);
    state.rooms.add_ban(room.id, target.user_id, RoomBanEntry {
        username: target.username.clone(),
        nickname: target.nickname.clone(),
        expires_at,
    }).await;
    state.users.send_to_user(target.user_id, Message::new("room_banned", serde_json::json!({
        "room_id": room.id, "room_name": room.name, "reason": reason, "expires_at": expires_at,
    }))).await;
    remove_from_room(state, &target, room.id).await;
    tracing::info!("Raum {}: {} hat {} gesperrt", room.id, user.username, target.username);
}

pub async fn handle_room_unban(
    state: &SharedState,
    uid: i64,
    data: serde_json::Value,
    tx: &mpsc::UnboundedSender<Message>,
) {
    let Some(req) = parse::<RoomUnban>(data, tx) else { return };
    let Some((user, room)) = user_and_room(state, uid, req.room_id, tx).await else { return };
    if !state.rooms.user_is_room_mod(&user, &room).await {
        deny(tx);
        return;
    }
    state.rooms.remove_ban(room.id, req.user_id).await;
    send_bans(state, room.id, tx).await;
}

pub async fn handle_room_bans(
    state: &SharedState,
    uid: i64,
    data: serde_json::Value,
    tx: &mpsc::UnboundedSender<Message>,
) {
    let Some(req) = parse::<RoomBans>(data, tx) else { return };
    let Some((user, room)) = user_and_room(state, uid, req.room_id, tx).await else { return };
    if !state.rooms.user_is_room_mod(&user, &room).await {
        deny(tx);
        return;
    }
    send_bans(state, room.id, tx).await;
}

pub async fn handle_room_mute(
    state: &SharedState,
    uid: i64,
    data: serde_json::Value,
    tx: &mpsc::UnboundedSender<Message>,
) {
    let Some(req) = parse::<RoomMute>(data, tx) else { return };
    let Some((user, room)) = user_and_room(state, uid, req.room_id, tx).await else { return };
    if !state.rooms.user_is_room_mod(&user, &room).await {
        deny(tx);
        return;
    }
    let Some(target) = state.users.get_user(req.user_id).await else {
        error(tx, "User not found");
        return;
    };
    if target.room_id != Some(room.id) {
        error(tx, "User is not in this room");
        return;
    }
    state.users.set_admin_muted(target.user_id, req.muted).await;
    state.users.broadcast_to_room(room.id, Message::new("audio_user_state", serde_json::json!({
        "user_id": target.user_id,
        "muted": target.muted || req.muted,
        "deafened": target.deafened,
    })), None).await;
}

pub async fn handle_user_lookup(
    state: &SharedState,
    uid: i64,
    data: serde_json::Value,
    tx: &mpsc::UnboundedSender<Message>,
) {
    let Some(req) = parse::<UserLookup>(data, tx) else { return };
    let tenant = state.users.user_tenant(uid).await;
    let found = state.users.get_user_by_username(&req.username, &tenant).await;
    let _ = tx.send(Message::new("user_lookup_result", match found {
        Some(u) => serde_json::json!({
            "username": u.username, "online": true, "user_id": u.user_id,
            "nickname": u.nickname, "room_id": u.room_id,
        }),
        None => serde_json::json!({ "username": req.username.trim(), "online": false }),
    }));
}

// ── Anrufe (docs/klango.md 1.4) ──

fn call_failed(tx: &mpsc::UnboundedSender<Message>, to: &str, reason: &str) {
    let _ = tx.send(Message::new("call_failed", serde_json::json!({
        "to_username": to, "reason": reason,
    })));
}

pub async fn handle_call_invite(
    state: &Arc<SharedState>,
    uid: i64,
    data: serde_json::Value,
    tx: &mpsc::UnboundedSender<Message>,
) {
    let Some(req) = parse::<CallInvite>(data, tx) else { return };
    let Some(caller) = state.users.get_user(uid).await else { return };
    let to = req.to_username.trim().to_string();
    let Some(callee) = state.users.get_user_by_username(&to, &caller.tenant).await else {
        call_failed(tx, &to, "offline");
        return;
    };
    if callee.user_id == uid {
        call_failed(tx, &to, "self");
        return;
    }
    if state.rooms.call_of_user(uid).await.is_some() {
        call_failed(tx, &to, "busy");
        return;
    }
    let callee_busy = state.rooms.call_of_user(callee.user_id).await.is_some()
        || match callee.room_id {
            Some(r) => matches!(state.rooms.get_room(r, &callee.tenant).await, Ok(Some(room)) if room.private),
            None => false,
        };
    if callee_busy {
        call_failed(tx, &to, "busy");
        return;
    }

    let name = format!("{} & {}", caller.nickname, callee.nickname);
    let extra = RoomExtra { group_id: String::new(), owner_id: uid, temporary: true, private: true };
    let room_id = match state.rooms.create_room_ext(
        name, None, 2,
        state.config.audio.default_sample_rate as i64,
        state.config.audio.default_bit_depth as i64,
        state.config.audio.default_channels as i64,
        0, &caller.tenant, extra,
    ).await {
        Ok(id) => id,
        Err(e) => {
            call_failed(tx, &to, &e.to_string());
            return;
        }
    };
    let since = std::time::Instant::now();
    state.rooms.set_invited(room_id, uid, true).await;
    state.rooms.set_invited(room_id, callee.user_id, true).await;
    state.rooms.register_call(room_id, PendingCall { caller: uid, callee: callee.user_id, since }).await;

    if !join_room_flow(state, uid, room_id, None, tx).await {
        state.rooms.take_call(room_id).await;
        let _ = state.rooms.cleanup_room(room_id, &caller.tenant).await;
        call_failed(tx, &to, "join");
        return;
    }

    state.rooms.send_room_list(callee.user_id).await;
    state.users.send_to_user(callee.user_id, Message::new("call_incoming", serde_json::json!({
        "room_id": room_id,
        "from_user_id": uid,
        "from_username": caller.username,
        "from_nickname": caller.nickname,
    }))).await;
    let _ = tx.send(Message::new("call_ringing", serde_json::json!({
        "room_id": room_id, "to_user_id": callee.user_id, "to_username": callee.username,
    })));

    // Zeitablauf: klingelt es zu lange, bricht der Server ab.
    let rooms = state.rooms.clone();
    tokio::spawn(async move {
        tokio::time::sleep(CALL_TIMEOUT).await;
        rooms.expire_call(room_id, since).await;
    });
    tracing::info!("Anruf {} -> {} (Raum {})", caller.username, callee.username, room_id);
}

pub async fn handle_call_answer(
    state: &SharedState,
    uid: i64,
    data: serde_json::Value,
    tx: &mpsc::UnboundedSender<Message>,
) {
    let Some(req) = parse::<CallAnswer>(data, tx) else { return };
    let Some(call) = state.rooms.pending_call(req.room_id).await else {
        error(tx, "No such call");
        return;
    };
    if call.callee != uid {
        deny(tx);
        return;
    }
    let Some(callee) = state.users.get_user(uid).await else { return };
    state.rooms.take_call(req.room_id).await;
    if req.accept {
        state.users.send_to_user(call.caller, Message::new("call_answered", serde_json::json!({
            "room_id": req.room_id, "user_id": uid, "username": callee.username, "accept": true,
        }))).await;
        // Der Angerufene tritt danach selbst mit room_join bei (er bleibt
        // eingeladen, s. RoomMeta::invited).
    } else {
        state.rooms.set_invited(req.room_id, uid, false).await;
        state.users.send_to_user(call.caller, Message::new("call_answered", serde_json::json!({
            "room_id": req.room_id, "user_id": uid, "username": callee.username,
            "accept": false, "reason": "declined",
        }))).await;
        state.rooms.send_room_list(uid).await;
    }
}

pub async fn handle_call_cancel(
    state: &SharedState,
    uid: i64,
    data: serde_json::Value,
    tx: &mpsc::UnboundedSender<Message>,
) {
    let Some(req) = parse::<CallCancel>(data, tx) else { return };
    let Some(call) = state.rooms.pending_call(req.room_id).await else {
        error(tx, "No such call");
        return;
    };
    if call.caller != uid {
        deny(tx);
        return;
    }
    let Some(caller) = state.users.get_user(uid).await else { return };
    state.rooms.take_call(req.room_id).await;
    state.users.send_to_user(call.callee, Message::new("call_cancelled", serde_json::json!({
        "room_id": req.room_id, "reason": "cancelled",
    }))).await;
    state.rooms.set_invited(req.room_id, call.callee, false).await;
    if caller.room_id == Some(req.room_id) {
        state.users.set_room(uid, None).await;
        state.users.broadcast_to_room(req.room_id, Message::new("room_user_left", serde_json::json!({
            "room_id": req.room_id, "user_id": uid,
        })), None).await;
    }
    state.rooms.after_leave(uid, Some(req.room_id), &caller.tenant).await;
    state.rooms.send_room_list(call.callee).await;
    state.rooms.send_room_list(uid).await;
}

/// Einstieg aus dem Nachrichten-Dispatcher. True, wenn der Typ hier bedient
/// wurde (auch bei Ablehnung außerhalb des Klango-Modus).
pub async fn handle_message(
    state: &Arc<SharedState>,
    uid: i64,
    msg_type: &str,
    data: serde_json::Value,
    tx: &mpsc::UnboundedSender<Message>,
) -> bool {
    const TYPES: [&str; 11] = [
        "room_join_group", "room_admin_set", "room_kick", "room_ban", "room_unban", "room_bans",
        "room_mute", "user_lookup", "call_invite", "call_answer", "call_cancel",
    ];
    if !TYPES.contains(&msg_type) {
        return false;
    }
    if !state.config.server.klango_mode() {
        deny(tx);
        return true;
    }
    match msg_type {
        "room_join_group" => handle_room_join_group(state, uid, data, tx).await,
        "room_admin_set" => handle_room_admin_set(state, uid, data, tx).await,
        "room_kick" => handle_room_kick(state, uid, data, tx).await,
        "room_ban" => handle_room_ban(state, uid, data, tx).await,
        "room_unban" => handle_room_unban(state, uid, data, tx).await,
        "room_bans" => handle_room_bans(state, uid, data, tx).await,
        "room_mute" => handle_room_mute(state, uid, data, tx).await,
        "user_lookup" => handle_user_lookup(state, uid, data, tx).await,
        "call_invite" => handle_call_invite(state, uid, data, tx).await,
        "call_answer" => handle_call_answer(state, uid, data, tx).await,
        "call_cancel" => handle_call_cancel(state, uid, data, tx).await,
        _ => {}
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make(secret: &str, claims: &str) -> String {
        let c = URL_SAFE_NO_PAD.encode(claims.as_bytes());
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(c.as_bytes());
        let sig = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        format!("{}.{}", c, sig)
    }

    #[test]
    fn token_roundtrip() {
        let exp = now_unix() + 60;
        let t = make("geheim", &format!(r#"{{"sub":"felix","nick":"Felix","iat":1,"exp":{},"adm":["12"],"sadm":true}}"#, exp));
        let c = verify_token("geheim", &t).unwrap();
        assert_eq!(c.sub, "felix");
        assert_eq!(c.adm, vec!["12".to_string()]);
        assert!(c.sadm);
        assert!(verify_token("falsch", &t).is_err());
    }

    #[test]
    fn token_expired_and_malformed() {
        let t = make("geheim", r#"{"sub":"felix","exp":1}"#);
        assert_eq!(verify_token("geheim", &t).unwrap_err(), "expired");
        assert!(verify_token("geheim", "abc").is_err());
        assert!(verify_token("geheim", "a.b.c").is_err());
    }
}
