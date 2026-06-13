use std::sync::Arc;
use tokio_rusqlite::Connection;
use crate::control::protocol::*;
use crate::db::queries;
use crate::user::manager::UserManager;

pub async fn handle_room_chat(
    user_id: i64,
    chat: ChatRoom,
    users: &Arc<UserManager>,
) -> anyhow::Result<()> {
    let sender = users.get_user(user_id).await
        .ok_or_else(|| anyhow::anyhow!("User not found"))?;

    if sender.room_id != Some(chat.room_id) {
        anyhow::bail!("You are not in this room");
    }

    let msg = Message::new("chat_room", serde_json::to_value(ChatRoom {
        room_id: chat.room_id,
        message: chat.message,
        from_user: Some(sender.to_info()),
    })?);

    users.broadcast_to_room(chat.room_id, msg, None).await;
    Ok(())
}

pub async fn handle_private_chat(
    user_id: i64,
    chat: ChatPrivate,
    users: &Arc<UserManager>,
    db: &Arc<Connection>,
) -> anyhow::Result<()> {
    let sender = users.get_user(user_id).await
        .ok_or_else(|| anyhow::anyhow!("User not found"))?;

    let msg = Message::new("chat_private", serde_json::to_value(ChatPrivate {
        to_user_id: chat.to_user_id,
        message: chat.message.clone(),
        from_user: Some(sender.to_info()),
    })?);

    if users.is_online(chat.to_user_id).await {
        users.send_to_user(chat.to_user_id, msg).await;
    } else {
        // Save as offline message
        queries::save_offline_message(db, user_id, chat.to_user_id, chat.message).await?;
    }

    Ok(())
}

pub async fn deliver_offline_messages(
    user_id: i64,
    users: &Arc<UserManager>,
    db: &Arc<Connection>,
) -> anyhow::Result<()> {
    let messages = queries::get_offline_messages(db, user_id).await?;

    for msg in &messages {
        let from_user = queries::get_user_by_id(db, msg.from_user_id).await?;
        let from_info = from_user.map(|u| UserInfo {
            id: u.id,
            nickname: u.username,
            role: u.role,
            muted: false,
            deafened: false,
        });

        let chat_msg = Message::new("chat_private", serde_json::to_value(ChatPrivate {
            to_user_id: user_id,
            message: msg.content.clone(),
            from_user: from_info,
        })?);

        users.send_to_user(user_id, chat_msg).await;
    }

    if !messages.is_empty() {
        queries::delete_offline_messages(db, user_id).await?;
    }

    Ok(())
}

pub async fn send_server_message(message: String, users: &Arc<UserManager>) {
    let msg = Message::new("chat_server", serde_json::json!({
        "message": message
    }));
    users.broadcast_all(msg).await;
}
