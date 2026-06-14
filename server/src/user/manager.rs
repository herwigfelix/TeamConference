use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use crate::control::protocol::{Message, UserInfo};

#[derive(Debug, Clone)]
pub struct OnlineUser {
    pub user_id: i64,
    pub username: String,
    pub nickname: String,
    pub role: String,
    pub session_token: u32,
    pub room_id: Option<i64>,
    pub muted: bool,
    pub deafened: bool,
    pub admin_muted: bool,
    pub loopback: bool,
    pub udp_addr: Option<SocketAddr>,
    pub audio_enabled: bool,
    pub sample_rate: u32,
    pub bit_depth: u8,
    pub channels: u8,
    pub tx: mpsc::UnboundedSender<Message>,
}

impl OnlineUser {
    pub fn to_info(&self) -> UserInfo {
        UserInfo {
            id: self.user_id,
            nickname: self.nickname.clone(),
            role: self.role.clone(),
            muted: self.muted || self.admin_muted,
            deafened: self.deafened,
            udp_token: Some(self.session_token),
        }
    }

    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }

    pub fn is_moderator(&self) -> bool {
        self.role == "admin" || self.role == "moderator"
    }
}

pub struct UserManager {
    users: RwLock<HashMap<i64, OnlineUser>>,
    token_map: RwLock<HashMap<u32, i64>>,
    next_token: RwLock<u32>,
}

impl UserManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            users: RwLock::new(HashMap::new()),
            token_map: RwLock::new(HashMap::new()),
            next_token: RwLock::new(1),
        })
    }

    pub async fn add_user(&self, user: OnlineUser) -> u32 {
        let mut next = self.next_token.write().await;
        let token = *next;
        *next = next.wrapping_add(1);
        if *next == 0 { *next = 1; }
        drop(next);

        let user_id = user.user_id;
        let mut users = self.users.write().await;
        let mut tokens = self.token_map.write().await;

        // Remove old session if reconnecting
        if let Some(old) = users.remove(&user_id) {
            tokens.remove(&old.session_token);
        }

        tokens.insert(token, user_id);
        let mut user = user;
        user.session_token = token;
        users.insert(user_id, user);
        token
    }

    pub async fn remove_user(&self, user_id: i64) -> Option<OnlineUser> {
        let mut users = self.users.write().await;
        let mut tokens = self.token_map.write().await;
        if let Some(user) = users.remove(&user_id) {
            tokens.remove(&user.session_token);
            Some(user)
        } else {
            None
        }
    }

    pub async fn get_user(&self, user_id: i64) -> Option<OnlineUser> {
        self.users.read().await.get(&user_id).cloned()
    }

    pub async fn get_user_by_token(&self, token: u32) -> Option<OnlineUser> {
        let tokens = self.token_map.read().await;
        if let Some(&uid) = tokens.get(&token) {
            self.users.read().await.get(&uid).cloned()
        } else {
            None
        }
    }

    pub async fn is_online(&self, user_id: i64) -> bool {
        self.users.read().await.contains_key(&user_id)
    }

    pub async fn set_room(&self, user_id: i64, room_id: Option<i64>) {
        if let Some(user) = self.users.write().await.get_mut(&user_id) {
            user.room_id = room_id;
        }
    }

    pub async fn set_muted(&self, user_id: i64, muted: bool) {
        if let Some(user) = self.users.write().await.get_mut(&user_id) {
            user.muted = muted;
        }
    }

    pub async fn set_deafened(&self, user_id: i64, deafened: bool) {
        if let Some(user) = self.users.write().await.get_mut(&user_id) {
            user.deafened = deafened;
        }
    }

    pub async fn set_admin_muted(&self, user_id: i64, muted: bool) {
        if let Some(user) = self.users.write().await.get_mut(&user_id) {
            user.admin_muted = muted;
        }
    }

    pub async fn set_loopback(&self, user_id: i64, enabled: bool) {
        if let Some(user) = self.users.write().await.get_mut(&user_id) {
            user.loopback = enabled;
        }
    }

    pub async fn set_udp_addr(&self, user_id: i64, addr: SocketAddr) {
        if let Some(user) = self.users.write().await.get_mut(&user_id) {
            user.udp_addr = Some(addr);
        }
    }

    pub async fn set_audio_config(&self, user_id: i64, sample_rate: u32, bit_depth: u8, channels: u8, enabled: bool) {
        if let Some(user) = self.users.write().await.get_mut(&user_id) {
            user.sample_rate = sample_rate;
            user.bit_depth = bit_depth;
            user.channels = channels;
            user.audio_enabled = enabled;
        }
    }

    pub async fn get_users_in_room(&self, room_id: i64) -> Vec<OnlineUser> {
        self.users
            .read()
            .await
            .values()
            .filter(|u| u.room_id == Some(room_id))
            .cloned()
            .collect()
    }

    pub async fn get_all_users(&self) -> Vec<OnlineUser> {
        self.users.read().await.values().cloned().collect()
    }

    pub async fn user_count(&self) -> usize {
        self.users.read().await.len()
    }

    pub async fn send_to_user(&self, user_id: i64, msg: Message) {
        if let Some(user) = self.users.read().await.get(&user_id) {
            let _ = user.tx.send(msg);
        }
    }

    pub async fn broadcast_to_room(&self, room_id: i64, msg: Message, exclude_user: Option<i64>) {
        let users = self.users.read().await;
        for user in users.values() {
            if user.room_id == Some(room_id) {
                if let Some(exclude) = exclude_user {
                    if user.user_id == exclude { continue; }
                }
                let _ = user.tx.send(msg.clone());
            }
        }
    }

    pub async fn broadcast_all(&self, msg: Message) {
        let users = self.users.read().await;
        for user in users.values() {
            let _ = user.tx.send(msg.clone());
        }
    }

    /// An alle senden außer an `exclude_user` (z. B. Presence-Ereignisse, die
    /// der Auslöser nicht über sich selbst erhalten soll).
    pub async fn broadcast_all_except(&self, msg: Message, exclude_user: i64) {
        let users = self.users.read().await;
        for user in users.values() {
            if user.user_id == exclude_user {
                continue;
            }
            let _ = user.tx.send(msg.clone());
        }
    }
}
