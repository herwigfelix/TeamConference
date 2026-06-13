use std::sync::Arc;
use tokio_rusqlite::Connection;
use crate::control::protocol::*;
use crate::db::queries;
use crate::user::manager::UserManager;
use crate::room::manager::RoomManager;

pub async fn handle_kick(
    admin_id: i64,
    kick: AdminKick,
    users: &Arc<UserManager>,
    rooms: &Arc<RoomManager>,
) -> anyhow::Result<()> {
    let admin = users.get_user(admin_id).await
        .ok_or_else(|| anyhow::anyhow!("Admin not found"))?;

    if !admin.is_moderator() {
        anyhow::bail!("Insufficient permissions");
    }

    let reason = kick.reason.unwrap_or_default();

    // Notify the kicked user
    let msg = Message::new("user_kicked", serde_json::json!({
        "reason": reason
    }));
    users.send_to_user(kick.user_id, msg).await;

    // Remove from room and disconnect
    rooms.leave_room(kick.user_id).await;

    if let Some(kicked) = users.remove_user(kick.user_id).await {
        // Notify room
        if let Some(room_id) = kicked.room_id {
            let leave_msg = Message::new("room_user_left", serde_json::json!({
                "room_id": room_id,
                "user_id": kick.user_id
            }));
            users.broadcast_to_room(room_id, leave_msg, None).await;
        }
    }

    tracing::info!("User {} kicked user {} (reason: {})", admin_id, kick.user_id, reason);
    Ok(())
}

pub async fn handle_ban(
    admin_id: i64,
    ban: AdminBan,
    users: &Arc<UserManager>,
    rooms: &Arc<RoomManager>,
    db: &Arc<Connection>,
) -> anyhow::Result<()> {
    let admin = users.get_user(admin_id).await
        .ok_or_else(|| anyhow::anyhow!("Admin not found"))?;

    if !admin.is_admin() {
        anyhow::bail!("Only admins can ban users");
    }

    let reason = ban.reason.clone().unwrap_or_default();
    let ip = users.get_user(ban.user_id).await
        .and_then(|u| u.udp_addr.map(|a| a.ip().to_string()));

    queries::create_ban(
        db,
        Some(ban.user_id),
        ip,
        reason.clone(),
        admin_id,
        ban.duration_minutes,
    ).await?;

    // Notify and disconnect
    let expires = ban.duration_minutes.map(|d| format!("{} minutes", d));
    let msg = Message::new("user_banned", serde_json::json!({
        "reason": reason,
        "expires_at": expires
    }));
    users.send_to_user(ban.user_id, msg).await;

    rooms.leave_room(ban.user_id).await;

    if let Some(banned_user) = users.remove_user(ban.user_id).await {
        if let Some(room_id) = banned_user.room_id {
            let leave_msg = Message::new("room_user_left", serde_json::json!({
                "room_id": room_id,
                "user_id": ban.user_id
            }));
            users.broadcast_to_room(room_id, leave_msg, None).await;
        }
    }

    tracing::info!("User {} banned user {} (reason: {})", admin_id, ban.user_id, reason);
    Ok(())
}

pub async fn handle_move(
    admin_id: i64,
    mv: AdminMove,
    users: &Arc<UserManager>,
    rooms: &Arc<RoomManager>,
) -> anyhow::Result<()> {
    let admin = users.get_user(admin_id).await
        .ok_or_else(|| anyhow::anyhow!("Admin not found"))?;

    if !admin.is_moderator() {
        anyhow::bail!("Insufficient permissions");
    }

    let target = users.get_user(mv.user_id).await
        .ok_or_else(|| anyhow::anyhow!("User not found"))?;

    let old_room_id = target.room_id;

    // Notify old room
    if let Some(old_id) = old_room_id {
        let leave_msg = Message::new("room_user_left", serde_json::json!({
            "room_id": old_id,
            "user_id": mv.user_id
        }));
        users.broadcast_to_room(old_id, leave_msg, Some(mv.user_id)).await;
    }

    // Move user
    rooms.join_room(mv.user_id, mv.room_id, None).await?;

    // Get room list for room name
    let room_list = rooms.get_room_list().await?;
    let room_name = room_list.iter()
        .find(|r| r.id == mv.room_id)
        .map(|r| r.name.clone())
        .unwrap_or_default();

    // Notify the moved user
    let msg = Message::new("user_moved", serde_json::json!({
        "room_id": mv.room_id,
        "room_name": room_name
    }));
    users.send_to_user(mv.user_id, msg).await;

    // Notify new room
    let updated_target = users.get_user(mv.user_id).await;
    if let Some(ut) = updated_target {
        let join_msg = Message::new("room_user_joined", serde_json::json!({
            "room_id": mv.room_id,
            "user": ut.to_info()
        }));
        users.broadcast_to_room(mv.room_id, join_msg, Some(mv.user_id)).await;
    }

    tracing::info!("Admin {} moved user {} to room {}", admin_id, mv.user_id, mv.room_id);
    Ok(())
}

pub async fn handle_admin_mute(
    admin_id: i64,
    mute: AdminMute,
    users: &Arc<UserManager>,
) -> anyhow::Result<()> {
    let admin = users.get_user(admin_id).await
        .ok_or_else(|| anyhow::anyhow!("Admin not found"))?;

    if !admin.is_moderator() {
        anyhow::bail!("Insufficient permissions");
    }

    users.set_admin_muted(mute.user_id, mute.muted).await;

    let target = users.get_user(mute.user_id).await
        .ok_or_else(|| anyhow::anyhow!("User not found"))?;

    // Broadcast state change
    if let Some(room_id) = target.room_id {
        let state_msg = Message::new("audio_user_state", serde_json::to_value(AudioUserState {
            user_id: mute.user_id,
            muted: target.muted || mute.muted,
            deafened: target.deafened,
        })?);
        users.broadcast_to_room(room_id, state_msg, None).await;
    }

    tracing::info!("Admin {} {} user {}", admin_id, if mute.muted { "muted" } else { "unmuted" }, mute.user_id);
    Ok(())
}
