use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio_rusqlite::Connection;
use crate::control::protocol::{Message, RoomInfo};
use crate::db::queries::{self, DbRoom, RoomExtra};
use crate::user::manager::{OnlineUser, UserManager};

/// Eintrag in der Sperrliste eines Raums (nur im Speicher, lebt mit dem Raum).
#[derive(Debug, Clone)]
pub struct RoomBanEntry {
    pub username: String,
    pub nickname: String,
    /// Unix-Sekunden; None = bis der Raum verschwindet.
    pub expires_at: Option<i64>,
}

/// Klango-Modus (docs/klango.md 1.2): Raum-Admins und Sperren eines Raums.
/// Bewusst NICHT in der Datenbank — sie gelten, bis der Raum gelöscht wird.
#[derive(Debug, Default)]
pub struct RoomMeta {
    pub admins: HashSet<i64>,
    pub bans: HashMap<i64, RoomBanEntry>,
    /// Anrufraum: wer ihn sehen und betreten darf (Anrufer und Angerufener).
    /// Bleibt auch nach dem Annehmen bestehen, denn der Angerufene tritt erst
    /// danach selbst bei (docs/klango.md 1.4).
    pub invited: HashSet<i64>,
}

/// Offener Anruf (docs/klango.md 1.4), Schlüssel ist der private Raum.
#[derive(Debug, Clone)]
pub struct PendingCall {
    pub caller: i64,
    pub callee: i64,
    /// Zur Erkennung veralteter Zeitablauf-Tasks.
    pub since: std::time::Instant,
}

pub fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

pub struct RoomManager {
    db: Arc<Connection>,
    users: Arc<UserManager>,
    meta: RwLock<HashMap<i64, RoomMeta>>,
    calls: RwLock<HashMap<i64, PendingCall>>,
}

impl RoomManager {
    pub fn new(db: Arc<Connection>, users: Arc<UserManager>) -> Arc<Self> {
        Arc::new(Self {
            db,
            users,
            meta: RwLock::new(HashMap::new()),
            calls: RwLock::new(HashMap::new()),
        })
    }

    async fn room_info(&self, r: DbRoom) -> RoomInfo {
        let users_in_room = self.users.get_users_in_room(r.id).await;
        let admins: Vec<i64> = self
            .meta
            .read()
            .await
            .get(&r.id)
            .map(|m| {
                let mut v: Vec<i64> = m.admins.iter().copied().collect();
                v.sort_unstable();
                v
            })
            .unwrap_or_default();
        RoomInfo {
            id: r.id,
            name: r.name,
            parent_id: r.parent_id,
            users: users_in_room.iter().map(|u| u.to_info()).collect(),
            max_users: r.max_users,
            description: r.description,
            has_password: r.password_hash.is_some(),
            sample_rate: r.sample_rate,
            bit_depth: r.bit_depth,
            channels: r.channels,
            bitrate: r.bitrate,
            group_id: r.group_id,
            owner_id: r.owner_id,
            admins,
            temporary: r.temporary,
            private: r.private,
        }
    }

    /// Alle Räume eines Tenants — OHNE Sichtbarkeitsfilter. Für Nachrichten an
    /// einen Nutzer `get_room_list_for` nehmen.
    pub async fn get_room_list(&self, tenant: &str) -> anyhow::Result<Vec<RoomInfo>> {
        let db_rooms = queries::get_all_rooms(&self.db, tenant.to_string()).await?;
        let mut rooms = Vec::new();
        for r in db_rooms {
            rooms.push(self.room_info(r).await);
        }
        Ok(rooms)
    }

    /// Darf `user` den Raum sehen? Private Räume (Anrufe) nur die Beteiligten.
    async fn visible_to(&self, r: &DbRoom, user: &OnlineUser) -> bool {
        if !r.private {
            return true;
        }
        if user.room_id == Some(r.id) {
            return true;
        }
        self.is_invited(r.id, user.user_id).await
    }

    pub async fn is_invited(&self, room_id: i64, user_id: i64) -> bool {
        self.meta
            .read()
            .await
            .get(&room_id)
            .map(|m| m.invited.contains(&user_id))
            .unwrap_or(false)
    }

    pub async fn set_invited(&self, room_id: i64, user_id: i64, on: bool) {
        let mut meta = self.meta.write().await;
        let m = meta.entry(room_id).or_default();
        if on {
            m.invited.insert(user_id);
        } else {
            m.invited.remove(&user_id);
        }
    }

    /// Raumliste aus Sicht EINES Nutzers (docs/klango.md 1.2, Sichtbarkeit).
    pub async fn get_room_list_for(&self, user_id: i64) -> anyhow::Result<Vec<RoomInfo>> {
        let Some(user) = self.users.get_user(user_id).await else {
            return Ok(Vec::new());
        };
        let db_rooms = queries::get_all_rooms(&self.db, user.tenant.clone()).await?;
        let mut rooms = Vec::new();
        for r in db_rooms {
            if self.visible_to(&r, &user).await {
                rooms.push(self.room_info(r).await);
            }
        }
        Ok(rooms)
    }

    /// `room_list` an jeden Nutzer des Tenants — je Empfänger gefiltert.
    pub async fn broadcast_room_list(&self, tenant: &str) {
        let all = self.users.get_all_users().await;
        for u in all.into_iter().filter(|u| u.tenant == tenant) {
            let list = self.get_room_list_for(u.user_id).await.unwrap_or_default();
            let _ = u.tx.send(Message::new("room_list", serde_json::json!({ "rooms": list })));
        }
    }

    /// `room_list` nur an einen Nutzer.
    pub async fn send_room_list(&self, user_id: i64) {
        let list = self.get_room_list_for(user_id).await.unwrap_or_default();
        self.users
            .send_to_user(user_id, Message::new("room_list", serde_json::json!({ "rooms": list })))
            .await;
    }

    pub async fn get_room(&self, room_id: i64, tenant: &str) -> anyhow::Result<Option<DbRoom>> {
        let rooms = queries::get_all_rooms(&self.db, tenant.to_string()).await?;
        Ok(rooms.into_iter().find(|r| r.id == room_id))
    }

    pub async fn get_default_room_id(&self, tenant: &str) -> anyhow::Result<i64> {
        let rooms = queries::get_all_rooms(&self.db, tenant.to_string()).await?;
        rooms
            .iter()
            .find(|r| r.is_default)
            .map(|r| r.id)
            .ok_or_else(|| anyhow::anyhow!("No default room configured"))
    }

    // ── Rechte (docs/klango.md 1.3) ──

    pub fn is_room_mod(user: &OnlineUser, room: &DbRoom, meta: Option<&RoomMeta>) -> bool {
        user.is_moderator()
            || (room.owner_id != 0 && room.owner_id == user.user_id)
            || meta.map(|m| m.admins.contains(&user.user_id)).unwrap_or(false)
            || (!room.group_id.is_empty() && user.klango_groups.iter().any(|g| *g == room.group_id))
    }

    pub async fn user_is_room_mod(&self, user: &OnlineUser, room: &DbRoom) -> bool {
        let meta = self.meta.read().await;
        Self::is_room_mod(user, room, meta.get(&room.id))
    }

    pub fn can_manage_admins(user: &OnlineUser, room: &DbRoom) -> bool {
        user.is_admin() || (room.group_id.is_empty() && room.owner_id != 0 && room.owner_id == user.user_id)
    }

    // ── Raum-Admins und Sperren ──

    pub async fn set_room_admin(&self, room_id: i64, user_id: i64, admin: bool) {
        let mut meta = self.meta.write().await;
        let m = meta.entry(room_id).or_default();
        if admin {
            m.admins.insert(user_id);
        } else {
            m.admins.remove(&user_id);
        }
    }

    pub async fn add_ban(&self, room_id: i64, user_id: i64, ban: RoomBanEntry) {
        let mut meta = self.meta.write().await;
        meta.entry(room_id).or_default().bans.insert(user_id, ban);
    }

    pub async fn remove_ban(&self, room_id: i64, user_id: i64) {
        let mut meta = self.meta.write().await;
        if let Some(m) = meta.get_mut(&room_id) {
            m.bans.remove(&user_id);
        }
    }

    /// Aktive Sperren eines Raums (abgelaufene werden dabei entfernt).
    pub async fn bans(&self, room_id: i64) -> Vec<(i64, RoomBanEntry)> {
        let now = now_unix();
        let mut meta = self.meta.write().await;
        let Some(m) = meta.get_mut(&room_id) else { return Vec::new() };
        m.bans.retain(|_, b| b.expires_at.map(|e| e > now).unwrap_or(true));
        let mut v: Vec<(i64, RoomBanEntry)> = m.bans.iter().map(|(k, b)| (*k, b.clone())).collect();
        v.sort_by_key(|(k, _)| *k);
        v
    }

    pub async fn is_banned(&self, room_id: i64, user_id: i64) -> bool {
        self.bans(room_id).await.iter().any(|(k, _)| *k == user_id)
    }

    // ── Beitreten / Verlassen ──

    pub async fn join_room(
        &self,
        user_id: i64,
        room_id: i64,
        password: Option<&str>,
        tenant: &str,
    ) -> anyhow::Result<()> {
        // Nur Räume des eigenen Unterservers (Tenant) sind sichtbar/beitretbar.
        let rooms = queries::get_all_rooms(&self.db, tenant.to_string()).await?;
        let room = rooms
            .iter()
            .find(|r| r.id == room_id)
            .ok_or_else(|| anyhow::anyhow!("Room not found"))?;

        // Klango-Modus: Sperre des Raums, private Räume nur für Beteiligte.
        if self.is_banned(room_id, user_id).await {
            anyhow::bail!("banned");
        }
        if room.private {
            let already_in = self.users.get_user(user_id).await.and_then(|u| u.room_id) == Some(room_id);
            if !already_in && !self.is_invited(room_id, user_id).await {
                anyhow::bail!("private");
            }
        }

        // Check password
        if let Some(ref hash) = room.password_hash {
            match password {
                Some(pw) => {
                    if !queries::verify_password(pw, hash) {
                        anyhow::bail!("Invalid room password");
                    }
                }
                None => anyhow::bail!("Room requires a password"),
            }
        }

        // Check max users
        if room.max_users > 0 {
            let current = self.users.get_users_in_room(room_id).await.len() as i64;
            if current >= room.max_users {
                anyhow::bail!("Room is full");
            }
        }

        self.users.set_room(user_id, Some(room_id)).await;
        Ok(())
    }

    pub async fn leave_room(&self, user_id: i64) {
        self.users.set_room(user_id, None).await;
    }

    /// Nach JEDEM Verlassen eines Raums (docs/klango.md 1.2, Aufräumen):
    /// Raum-Stummschaltung zurücksetzen, laufenden Anruf des Nutzers beenden
    /// und einen leeren temporären Raum löschen. `room_list` geht danach an
    /// alle im Tenant, wenn sich etwas geändert hat.
    pub async fn after_leave(self: &Arc<Self>, user_id: i64, old_room: Option<i64>, tenant: &str) {
        self.users.set_admin_muted(user_id, false).await;
        let Some(room_id) = old_room else { return };
        // Nur wenn der verlassene Raum der Anrufraum ist, endet der Anruf —
        // ein Angerufener, der aus einem anderen Raum herüberwechselt, darf
        // seinen eigenen Anruf nicht damit abbrechen.
        if let Some((call_room, _)) = self.call_of_user(user_id).await {
            if call_room == room_id {
                self.end_call_for(user_id, "cancelled").await;
            }
        }
        if self.cleanup_room(room_id, tenant).await {
            self.broadcast_room_list(tenant).await;
        }
    }

    /// Verbindungsende: offenen Anruf beenden, leeren temporären Raum löschen.
    /// Der Nutzer ist zu diesem Zeitpunkt schon aus dem UserManager entfernt.
    pub async fn on_disconnect(self: &Arc<Self>, user_id: i64, old_room: Option<i64>, tenant: &str) {
        self.end_call_for(user_id, "cancelled").await;
        if let Some(room_id) = old_room {
            if self.cleanup_room(room_id, tenant).await {
                self.broadcast_room_list(tenant).await;
            }
        }
    }

    /// Alle leeren temporären Räume eines Tenants löschen (nach Aktionen, die
    /// Nutzer ohne Umweg über `after_leave` entfernen, z. B. Serveradmin-Kick).
    pub async fn sweep_empty(&self, tenant: &str) {
        let Ok(rooms) = queries::get_all_rooms(&self.db, tenant.to_string()).await else { return };
        let mut changed = false;
        for r in rooms.iter().filter(|r| r.temporary) {
            if self.cleanup_room(r.id, tenant).await {
                changed = true;
            }
        }
        if changed {
            self.broadcast_room_list(tenant).await;
        }
    }

    /// Temporären, leeren Raum löschen. True, wenn gelöscht wurde.
    pub async fn cleanup_room(&self, room_id: i64, tenant: &str) -> bool {
        let Ok(Some(room)) = self.get_room(room_id, tenant).await else { return false };
        if !room.temporary || room.is_default {
            return false;
        }
        if !self.users.get_users_in_room(room_id).await.is_empty() {
            return false;
        }
        if queries::delete_room(&self.db, room_id).await.is_err() {
            return false;
        }
        self.meta.write().await.remove(&room_id);
        self.calls.write().await.remove(&room_id);
        tracing::info!("Temporärer Raum {} ({}) gelöscht", room_id, room.name);
        true
    }

    // ── Anlegen / Löschen / Ändern ──

    #[allow(clippy::too_many_arguments)]
    pub async fn create_room(
        &self,
        name: String,
        parent_id: Option<i64>,
        password: Option<String>,
        max_users: i64,
        sample_rate: i64,
        bit_depth: i64,
        channels: i64,
        bitrate: i64,
        tenant: &str,
    ) -> anyhow::Result<i64> {
        queries::create_room(&self.db, name, parent_id, password, max_users, sample_rate, bit_depth, channels, bitrate, tenant.to_string()).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_room_ext(
        &self,
        name: String,
        password: Option<String>,
        max_users: i64,
        sample_rate: i64,
        bit_depth: i64,
        channels: i64,
        bitrate: i64,
        tenant: &str,
        extra: RoomExtra,
    ) -> anyhow::Result<i64> {
        queries::create_room_ext(&self.db, name, None, password, max_users, sample_rate, bit_depth, channels, bitrate, tenant.to_string(), extra).await
    }

    /// Anzahl der Räume im Tenant und der temporären Räume dieses Eigentümers
    /// (für die Grenzen aus docs/klango.md 1.2).
    pub async fn room_counts(&self, tenant: &str, owner_id: i64) -> anyhow::Result<(usize, usize)> {
        let rooms = queries::get_all_rooms(&self.db, tenant.to_string()).await?;
        let mine = rooms.iter().filter(|r| r.temporary && r.owner_id == owner_id && !r.private).count();
        Ok((rooms.len(), mine))
    }

    /// Gruppenraum finden (höchstens einer je group_id im Tenant).
    pub async fn find_group_room(&self, group_id: &str, tenant: &str) -> anyhow::Result<Option<DbRoom>> {
        let rooms = queries::get_all_rooms(&self.db, tenant.to_string()).await?;
        Ok(rooms.into_iter().find(|r| r.group_id == group_id))
    }

    /// Klassisches Löschen: Nutzer in den Standardraum schieben.
    pub async fn delete_room(&self, room_id: i64, tenant: &str) -> anyhow::Result<()> {
        // Nutzer dieses Raums in den Standard-Raum DESSELBEN Unterservers schieben.
        let default_room = self.get_default_room_id(tenant).await?;
        let users = self.users.get_users_in_room(room_id).await;
        for user in &users {
            self.users.set_room(user.user_id, Some(default_room)).await;
        }
        self.meta.write().await.remove(&room_id);
        self.calls.write().await.remove(&room_id);
        queries::delete_room(&self.db, room_id).await
    }

    /// Klango-Modus: Löschen ohne Verschieben — die Nutzer stehen danach in
    /// keinem Raum und bekommen `room_closed`.
    pub async fn close_room(&self, room_id: i64, room_name: &str) -> anyhow::Result<()> {
        let users = self.users.get_users_in_room(room_id).await;
        for user in &users {
            self.users.set_room(user.user_id, None).await;
            self.users.set_admin_muted(user.user_id, false).await;
            let _ = user.tx.send(Message::new("room_closed", serde_json::json!({
                "room_id": room_id,
                "room_name": room_name,
            })));
        }
        self.meta.write().await.remove(&room_id);
        self.calls.write().await.remove(&room_id);
        queries::delete_room(&self.db, room_id).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_room(
        &self,
        room_id: i64,
        name: Option<String>,
        password: Option<Option<String>>,
        max_users: Option<i64>,
        sample_rate: Option<i64>,
        bit_depth: Option<i64>,
        channels: Option<i64>,
        bitrate: Option<i64>,
    ) -> anyhow::Result<()> {
        queries::update_room(&self.db, room_id, name, password, max_users, sample_rate, bit_depth, channels, bitrate).await
    }

    pub async fn room_exists(&self, room_id: i64, tenant: &str) -> anyhow::Result<bool> {
        let rooms = queries::get_all_rooms(&self.db, tenant.to_string()).await?;
        Ok(rooms.iter().any(|r| r.id == room_id))
    }

    // ── Anrufe (docs/klango.md 1.4) ──

    pub async fn pending_call(&self, room_id: i64) -> Option<PendingCall> {
        self.calls.read().await.get(&room_id).cloned()
    }

    /// Offener Anruf, an dem der Nutzer beteiligt ist (als Anrufer oder
    /// Angerufener).
    pub async fn call_of_user(&self, user_id: i64) -> Option<(i64, PendingCall)> {
        self.calls
            .read()
            .await
            .iter()
            .find(|(_, c)| c.caller == user_id || c.callee == user_id)
            .map(|(k, c)| (*k, c.clone()))
    }

    pub async fn register_call(&self, room_id: i64, call: PendingCall) {
        self.calls.write().await.insert(room_id, call);
    }

    /// Anruf aus der Liste nehmen (z. B. angenommen). Gibt ihn zurück.
    pub async fn take_call(&self, room_id: i64) -> Option<PendingCall> {
        self.calls.write().await.remove(&room_id)
    }

    /// Offenen Anruf eines Nutzers beenden, weil er weg ist (getrennt, Raum
    /// verlassen, gekickt). Die Gegenseite wird benachrichtigt; der private
    /// Raum wird über das normale Aufräumen entsorgt, sobald er leer ist.
    pub async fn end_call_for(self: &Arc<Self>, user_id: i64, reason: &str) {
        let Some((room_id, call)) = self.call_of_user(user_id).await else { return };
        self.take_call(room_id).await;
        if call.caller == user_id {
            self.users.send_to_user(call.callee, Message::new("call_cancelled", serde_json::json!({
                "room_id": room_id, "reason": reason,
            }))).await;
        } else {
            let callee = self.users.get_user(call.callee).await;
            self.users.send_to_user(call.caller, Message::new("call_answered", serde_json::json!({
                "room_id": room_id,
                "user_id": call.callee,
                "username": callee.as_ref().map(|u| u.username.clone()).unwrap_or_default(),
                "accept": false,
                "reason": "offline",
            }))).await;
        }
        // Der Angerufene war nie im Raum; der Anrufer steht evtl. noch drin —
        // dann räumt sein eigenes Verlassen auf.
        let tenant = self.users.user_tenant(call.caller).await;
        if self.cleanup_room(room_id, &tenant).await {
            self.broadcast_room_list(&tenant).await;
        }
    }

    /// Zeitablauf eines Anrufs (60 s nach `call_invite`): beide Seiten
    /// benachrichtigen, den Anrufer aus dem Raum nehmen, aufräumen.
    pub async fn expire_call(self: &Arc<Self>, room_id: i64, since: std::time::Instant) {
        let call = {
            let mut calls = self.calls.write().await;
            match calls.get(&room_id) {
                Some(c) if c.since == since => calls.remove(&room_id),
                _ => None,
            }
        };
        let Some(call) = call else { return };
        self.users.send_to_user(call.callee, Message::new("call_cancelled", serde_json::json!({
            "room_id": room_id, "reason": "timeout",
        }))).await;
        let callee = self.users.get_user(call.callee).await;
        self.users.send_to_user(call.caller, Message::new("call_answered", serde_json::json!({
            "room_id": room_id,
            "user_id": call.callee,
            "username": callee.map(|u| u.username).unwrap_or_default(),
            "accept": false,
            "reason": "timeout",
        }))).await;
        let tenant = self.users.user_tenant(call.caller).await;
        if let Some(u) = self.users.get_user(call.caller).await {
            if u.room_id == Some(room_id) {
                self.users.broadcast_to_room(room_id, Message::new("room_user_left", serde_json::json!({
                    "room_id": room_id, "user_id": call.caller,
                })), Some(call.caller)).await;
                self.users.set_room(call.caller, None).await;
            }
        }
        // Der Angerufene sah den Raum bisher; jetzt nicht mehr.
        self.send_room_list(call.callee).await;
        if self.cleanup_room(room_id, &tenant).await {
            self.broadcast_room_list(&tenant).await;
        }
    }
}
