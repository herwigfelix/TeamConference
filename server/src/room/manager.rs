use std::sync::Arc;
use tokio_rusqlite::Connection;
use crate::control::protocol::RoomInfo;
use crate::db::queries;
use crate::user::manager::UserManager;

pub struct RoomManager {
    db: Arc<Connection>,
    users: Arc<UserManager>,
}

impl RoomManager {
    pub fn new(db: Arc<Connection>, users: Arc<UserManager>) -> Arc<Self> {
        Arc::new(Self { db, users })
    }

    pub async fn get_room_list(&self) -> anyhow::Result<Vec<RoomInfo>> {
        let db_rooms = queries::get_all_rooms(&self.db).await?;
        let mut rooms = Vec::new();

        for r in db_rooms {
            let users_in_room = self.users.get_users_in_room(r.id).await;
            rooms.push(RoomInfo {
                id: r.id,
                name: r.name,
                parent_id: r.parent_id,
                users: users_in_room.iter().map(|u| u.to_info()).collect(),
                max_users: r.max_users,
                description: r.description,
                has_password: r.password_hash.is_some(),
            });
        }

        Ok(rooms)
    }

    pub async fn get_default_room_id(&self) -> anyhow::Result<i64> {
        let rooms = queries::get_all_rooms(&self.db).await?;
        rooms
            .iter()
            .find(|r| r.is_default)
            .map(|r| r.id)
            .ok_or_else(|| anyhow::anyhow!("No default room configured"))
    }

    pub async fn join_room(
        &self,
        user_id: i64,
        room_id: i64,
        password: Option<&str>,
    ) -> anyhow::Result<()> {
        let rooms = queries::get_all_rooms(&self.db).await?;
        let room = rooms
            .iter()
            .find(|r| r.id == room_id)
            .ok_or_else(|| anyhow::anyhow!("Room not found"))?;

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

    pub async fn create_room(
        &self,
        name: String,
        parent_id: Option<i64>,
        password: Option<String>,
        max_users: i64,
    ) -> anyhow::Result<i64> {
        queries::create_room(&self.db, name, parent_id, password, max_users).await
    }

    pub async fn delete_room(&self, room_id: i64) -> anyhow::Result<()> {
        // Move all users in this room to default
        let default_room = self.get_default_room_id().await?;
        let users = self.users.get_users_in_room(room_id).await;
        for user in &users {
            self.users.set_room(user.user_id, Some(default_room)).await;
        }
        queries::delete_room(&self.db, room_id).await
    }

    pub async fn update_room(
        &self,
        room_id: i64,
        name: Option<String>,
        password: Option<Option<String>>,
        max_users: Option<i64>,
    ) -> anyhow::Result<()> {
        queries::update_room(&self.db, room_id, name, password, max_users).await
    }

    pub async fn room_exists(&self, room_id: i64) -> anyhow::Result<bool> {
        let rooms = queries::get_all_rooms(&self.db).await?;
        Ok(rooms.iter().any(|r| r.id == room_id))
    }
}
