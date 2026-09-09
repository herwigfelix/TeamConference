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
    /// Unterserver-Zugehörigkeit ('' = Einzelserver-Modus).
    pub tenant: String,
    /// S1: geheimer Auth-Token — nur der Besitzer kennt ihn (via audio_config_ack),
    /// authentifiziert ausgehende UDP-Pakete. Wird NIE an andere gebroadcastet.
    pub session_token: u32,
    /// S1: öffentliche Audio-ID — dient nur der Zuordnung eingehender Audio zu
    /// Nutzern und wird via UserInfo.udp_token an alle Raum-Mitglieder verteilt.
    pub audio_id: u32,
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
    /// Klango-Modus: Gruppen (Klango-gid als String), in denen der Nutzer
    /// Admin oder Moderator ist — aus dem Anmelde-Token (docs/klango.md 1.1).
    pub klango_groups: Vec<String>,
    pub tx: mpsc::UnboundedSender<Message>,
}

impl OnlineUser {
    pub fn to_info(&self) -> UserInfo {
        UserInfo {
            id: self.user_id,
            nickname: self.nickname.clone(),
            username: self.username.clone(),
            role: self.role.clone(),
            muted: self.muted || self.admin_muted,
            deafened: self.deafened,
            // S1: öffentliche Audio-ID broadcasten, NICHT den geheimen Auth-Token.
            udp_token: Some(self.audio_id),
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
    /// Geheimer Auth-Token → user_id (für die UDP-Authentifizierung).
    token_map: RwLock<HashMap<u32, i64>>,
    /// Öffentliche Audio-ID → user_id (nur zur Eindeutigkeit; Relay nutzt die
    /// audio_id direkt vom Absender).
    audio_map: RwLock<HashMap<u32, i64>>,
    /// Klango-Modus: Klangoid (kleingeschrieben) → alle Verbindungen dieses
    /// Kontos. BEWUSST getrennt von `users`, denn beide zählen Verschiedenes:
    /// `users` ist die KONFERENZ-Sitzung, davon gibt es eine je Konto (in zwei
    /// Sprachräumen gleichzeitig zu stehen ergäbe keinen Sinn). Benachrichtigungen
    /// dagegen sollen ALLE Rechner erreichen, an denen jemand angemeldet ist —
    /// sonst bekäme der Rechner im Arbeitszimmer nichts mehr mit, sobald sich
    /// derselbe Nutzer am Laptop anmeldet.
    push_listeners: RwLock<HashMap<String, Vec<(u64, mpsc::UnboundedSender<Message>)>>>,
}

impl UserManager {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            users: RwLock::new(HashMap::new()),
            token_map: RwLock::new(HashMap::new()),
            audio_map: RwLock::new(HashMap::new()),
            push_listeners: RwLock::new(HashMap::new()),
        })
    }

    pub async fn add_user(&self, user: OnlineUser) -> u32 {
        let user_id = user.user_id;
        let mut users = self.users.write().await;
        let mut tokens = self.token_map.write().await;
        let mut audio_ids = self.audio_map.write().await;

        // Alte Session beim Reconnect entfernen (Token + Audio-ID freigeben).
        //
        // Der alten Verbindung wird das GESAGT. Vorher verstummte sie still:
        // sie hielt sich weiter für angemeldet, konnte aber keinem Raum mehr
        // beitreten, weil der Server sie nicht mehr kannte. Als Push-Zuhörer
        // bleibt sie bestehen — Benachrichtigungen erreichen sie weiterhin.
        if let Some(old) = users.remove(&user_id) {
            tokens.remove(&old.session_token);
            audio_ids.remove(&old.audio_id);
            let _ = old.tx.send(Message::new("session_replaced", serde_json::json!({})));
        }

        // S1: geheimer Auth-Token — zufällig (CSPRNG-frei über rand), ≠0,
        // kollisionsfrei. Ersetzt den früheren hochzählenden, erratbaren Zähler.
        let token = loop {
            let t: u32 = rand::random();
            if t != 0 && !tokens.contains_key(&t) {
                break t;
            }
        };
        // S1: öffentliche Audio-ID — ebenfalls zufällig und eindeutig.
        let audio_id = loop {
            let a: u32 = rand::random();
            if a != 0 && !audio_ids.contains_key(&a) {
                break a;
            }
        };

        tokens.insert(token, user_id);
        audio_ids.insert(audio_id, user_id);
        let mut user = user;
        user.session_token = token;
        user.audio_id = audio_id;
        users.insert(user_id, user);
        token
    }

    pub async fn remove_user(&self, user_id: i64) -> Option<OnlineUser> {
        let mut users = self.users.write().await;
        let mut tokens = self.token_map.write().await;
        let mut audio_ids = self.audio_map.write().await;
        if let Some(user) = users.remove(&user_id) {
            tokens.remove(&user.session_token);
            audio_ids.remove(&user.audio_id);
            Some(user)
        } else {
            None
        }
    }

    /// True, wenn die Sitzung dieses Kontos noch zu `tx` gehört.
    ///
    /// Nötig, seit sich ein Konto an mehreren Rechnern anmelden darf: die
    /// zweite Anmeldung übernimmt die Konferenz-Sitzung (`add_user` verdrängt
    /// die erste). Trennt sich danach die ERSTE Verbindung, darf ihr Aufräumen
    /// die Sitzung der zweiten nicht mitreißen — sonst wäre der Nutzer nach dem
    /// Schließen des alten Fensters plötzlich nirgends mehr angemeldet.
    pub async fn session_belongs_to(
        &self,
        user_id: i64,
        tx: &mpsc::UnboundedSender<Message>,
    ) -> bool {
        self.users
            .read()
            .await
            .get(&user_id)
            .is_some_and(|u| u.tx.same_channel(tx))
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

    /// Klango-Modus: Online-Nutzer über den Kontonamen (Klango-ID) finden,
    /// unabhängig von Groß-/Kleinschreibung, nur im selben Tenant.
    pub async fn get_user_by_username(&self, username: &str, tenant: &str) -> Option<OnlineUser> {
        let want = username.trim().to_lowercase();
        self.users
            .read()
            .await
            .values()
            .find(|u| u.tenant == tenant && u.username.to_lowercase() == want)
            .cloned()
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

    /// Tenant des Nutzers ('' wenn unbekannt / Einzelserver).
    pub async fn user_tenant(&self, user_id: i64) -> String {
        self.users
            .read()
            .await
            .get(&user_id)
            .map(|u| u.tenant.clone())
            .unwrap_or_default()
    }

    /// An alle Nutzer DESSELBEN Unterservers senden. Im Einzelserver-Modus
    /// (tenant == "") sind das alle Nutzer.
    pub async fn broadcast_tenant(&self, tenant: &str, msg: Message) {
        let users = self.users.read().await;
        for user in users.values() {
            if user.tenant == tenant {
                let _ = user.tx.send(msg.clone());
            }
        }
    }

    /// Wie `broadcast_tenant`, aber ohne `exclude_user`.
    pub async fn broadcast_tenant_except(&self, tenant: &str, msg: Message, exclude_user: i64) {
        let users = self.users.read().await;
        for user in users.values() {
            if user.tenant == tenant && user.user_id != exclude_user {
                let _ = user.tx.send(msg.clone());
            }
        }
    }

    // ── Push-Zuhörer (docs/klango.md 1.6) ──

    /// Diese Verbindung als Empfänger für Benachrichtigungen eintragen.
    /// `conn_id` ist die laufende Nummer der WebSocket-Verbindung; sie und
    /// nicht der Kontoname identifiziert den Eintrag, weil ein Konto mehrere
    /// Verbindungen haben darf.
    pub async fn add_push_listener(
        &self,
        klangoid: &str,
        conn_id: u64,
        tx: mpsc::UnboundedSender<Message>,
    ) {
        let key = klangoid.trim().to_lowercase();
        if key.is_empty() {
            return;
        }
        let mut map = self.push_listeners.write().await;
        let list = map.entry(key).or_default();
        // Eine erneute Anmeldung auf DERSELBEN Verbindung ersetzt den Eintrag,
        // statt ihn zu verdoppeln.
        list.retain(|(id, _)| *id != conn_id);
        list.push((conn_id, tx));
    }

    /// Eintrag dieser Verbindung entfernen (beim Trennen).
    pub async fn remove_push_listener(&self, klangoid: &str, conn_id: u64) {
        let key = klangoid.trim().to_lowercase();
        let mut map = self.push_listeners.write().await;
        if let Some(list) = map.get_mut(&key) {
            list.retain(|(id, _)| *id != conn_id);
            if list.is_empty() {
                map.remove(&key);
            }
        }
    }

    /// Nachricht an alle Verbindungen der genannten Konten. Rückgabe: wie viele
    /// Verbindungen erreicht wurden. Geschlossene Kanäle werden dabei
    /// aussortiert — ein Empfänger, der nicht mehr da ist, zählt nicht mit.
    pub async fn push_to(&self, klangoids: &[String], msg: Message) -> usize {
        let mut sent = 0usize;
        let mut map = self.push_listeners.write().await;
        for name in klangoids {
            let key = name.trim().to_lowercase();
            let Some(list) = map.get_mut(&key) else { continue };
            list.retain(|(_, tx)| {
                if tx.send(msg.clone()).is_ok() {
                    sent += 1;
                    true
                } else {
                    false
                }
            });
            if list.is_empty() {
                map.remove(&key);
            }
        }
        sent
    }

    /// Alle Konten, die gerade mindestens eine Verbindung halten
    /// (kleingeschrieben). Grundlage der Anwesenheitsmeldung an den
    /// Klango-Server.
    pub async fn push_listener_names(&self) -> Vec<String> {
        self.push_listeners.read().await.keys().cloned().collect()
    }
}
