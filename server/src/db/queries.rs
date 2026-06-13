use tokio_rusqlite::Connection;
use argon2::{Argon2, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use rand::rngs::OsRng;

#[derive(Debug, Clone)]
pub struct DbUser {
    pub id: i64,
    pub username: String,
    pub role: String,
}

#[derive(Debug, Clone)]
pub struct DbRoom {
    pub id: i64,
    pub name: String,
    pub parent_id: Option<i64>,
    pub password_hash: Option<String>,
    pub max_users: i64,
    pub description: String,
    pub is_default: bool,
    pub sort_order: i64,
    pub sample_rate: i64,
    pub bit_depth: i64,
    pub channels: i64,
}

#[derive(Debug, Clone)]
pub struct DbBan {
    pub id: i64,
    pub user_id: Option<i64>,
    pub ip_address: Option<String>,
    pub reason: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DbRoomFile {
    pub id: i64,
    pub room_id: i64,
    pub filename: String,
    pub storage_path: String,
    pub size_bytes: i64,
    pub uploaded_by: Option<i64>,
    pub uploaded_at: String,
}

#[derive(Debug, Clone)]
pub struct DbOfflineMessage {
    pub id: i64,
    pub from_user_id: i64,
    pub to_user_id: i64,
    pub content: String,
    pub created_at: String,
}

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("Password hashing failed: {}", e))?;
    Ok(hash.to_string())
}

pub fn verify_password(password: &str, hash: &str) -> bool {
    let parsed = match argon2::PasswordHash::new(hash) {
        Ok(h) => h,
        Err(_) => return false,
    };
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .is_ok()
}

pub async fn create_user(
    conn: &Connection,
    username: String,
    password: String,
    role: String,
) -> anyhow::Result<i64> {
    let password_hash = hash_password(&password)?;
    conn.call(move |conn| {
        conn.execute(
            "INSERT INTO users (username, password_hash, role) VALUES (?1, ?2, ?3)",
            rusqlite::params![username, password_hash, role],
        )?;
        Ok(conn.last_insert_rowid())
    })
    .await
    .map_err(|e| anyhow::anyhow!("Failed to create user: {}", e))
}

pub async fn authenticate_user(
    conn: &Connection,
    username: String,
    password: String,
) -> anyhow::Result<Option<DbUser>> {
    conn.call(move |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, username, password_hash, role FROM users WHERE username = ?1",
        )?;
        let result = stmt.query_row(rusqlite::params![username], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        });

        match result {
            Ok((id, uname, hash, role)) => {
                if verify_password(&password, &hash) {
                    Ok(Some(DbUser {
                        id,
                        username: uname,
                        role,
                    }))
                } else {
                    Ok(None)
                }
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    })
    .await
    .map_err(|e| anyhow::anyhow!("Authentication query failed: {}", e))
}

pub async fn get_user_by_id(conn: &Connection, user_id: i64) -> anyhow::Result<Option<DbUser>> {
    conn.call(move |conn| {
        let mut stmt = conn.prepare("SELECT id, username, role FROM users WHERE id = ?1")?;
        let result = stmt.query_row(rusqlite::params![user_id], |row| {
            Ok(DbUser {
                id: row.get(0)?,
                username: row.get(1)?,
                role: row.get(2)?,
            })
        });
        match result {
            Ok(user) => Ok(Some(user)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    })
    .await
    .map_err(|e| anyhow::anyhow!("Get user failed: {}", e))
}

pub async fn is_user_banned(conn: &Connection, user_id: i64, ip: String) -> anyhow::Result<Option<DbBan>> {
    conn.call(move |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, user_id, ip_address, reason, expires_at FROM bans
             WHERE (user_id = ?1 OR ip_address = ?2)
             AND (expires_at IS NULL OR expires_at > datetime('now'))"
        )?;
        let result = stmt.query_row(rusqlite::params![user_id, ip], |row| {
            Ok(DbBan {
                id: row.get(0)?,
                user_id: row.get(1)?,
                ip_address: row.get(2)?,
                reason: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                expires_at: row.get(4)?,
            })
        });
        match result {
            Ok(ban) => Ok(Some(ban)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    })
    .await
    .map_err(|e| anyhow::anyhow!("Ban check failed: {}", e))
}

pub async fn create_ban(
    conn: &Connection,
    user_id: Option<i64>,
    ip_address: Option<String>,
    reason: String,
    banned_by: i64,
    duration_minutes: Option<i64>,
) -> anyhow::Result<()> {
    conn.call(move |conn| {
        let expires = duration_minutes.map(|d| {
            format!("datetime('now', '+{} minutes')", d)
        });
        if let Some(exp) = expires {
            conn.execute(
                &format!(
                    "INSERT INTO bans (user_id, ip_address, reason, banned_by, expires_at) VALUES (?1, ?2, ?3, ?4, {})",
                    exp
                ),
                rusqlite::params![user_id, ip_address, reason, banned_by],
            )?;
        } else {
            conn.execute(
                "INSERT INTO bans (user_id, ip_address, reason, banned_by) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![user_id, ip_address, reason, banned_by],
            )?;
        }
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("Failed to create ban: {}", e))
}

pub async fn get_all_rooms(conn: &Connection) -> anyhow::Result<Vec<DbRoom>> {
    conn.call(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, name, parent_id, password_hash, max_users, description, is_default, sort_order, sample_rate, bit_depth, channels
             FROM rooms ORDER BY sort_order, name"
        )?;
        let rooms = stmt.query_map([], |row| {
            Ok(DbRoom {
                id: row.get(0)?,
                name: row.get(1)?,
                parent_id: row.get(2)?,
                password_hash: row.get(3)?,
                max_users: row.get(4)?,
                description: row.get::<_, Option<String>>(5)?.unwrap_or_default(),
                is_default: row.get(6)?,
                sort_order: row.get(7)?,
                sample_rate: row.get(8)?,
                bit_depth: row.get(9)?,
                channels: row.get(10)?,
            })
        })?.filter_map(|r| r.ok()).collect();
        Ok(rooms)
    })
    .await
    .map_err(|e| anyhow::anyhow!("Failed to get rooms: {}", e))
}

pub async fn create_room(
    conn: &Connection,
    name: String,
    parent_id: Option<i64>,
    password: Option<String>,
    max_users: i64,
    sample_rate: i64,
    bit_depth: i64,
    channels: i64,
) -> anyhow::Result<i64> {
    let password_hash = password.map(|p| hash_password(&p)).transpose()?;
    conn.call(move |conn| {
        conn.execute(
            "INSERT INTO rooms (name, parent_id, password_hash, max_users, sample_rate, bit_depth, channels)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![name, parent_id, password_hash, max_users, sample_rate, bit_depth, channels],
        )?;
        Ok(conn.last_insert_rowid())
    })
    .await
    .map_err(|e| anyhow::anyhow!("Failed to create room: {}", e))
}

pub async fn delete_room(conn: &Connection, room_id: i64) -> anyhow::Result<()> {
    conn.call(move |conn| {
        conn.execute("DELETE FROM rooms WHERE id = ?1", rusqlite::params![room_id])?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("Failed to delete room: {}", e))
}

#[allow(clippy::too_many_arguments)]
pub async fn update_room(
    conn: &Connection,
    room_id: i64,
    name: Option<String>,
    password: Option<Option<String>>,
    max_users: Option<i64>,
    sample_rate: Option<i64>,
    bit_depth: Option<i64>,
    channels: Option<i64>,
) -> anyhow::Result<()> {
    let password_hash = match password {
        Some(Some(p)) => Some(Some(hash_password(&p)?)),
        Some(None) => Some(None),
        None => None,
    };
    conn.call(move |conn| {
        if let Some(name) = name {
            conn.execute("UPDATE rooms SET name = ?1 WHERE id = ?2", rusqlite::params![name, room_id])?;
        }
        if let Some(ph) = password_hash {
            conn.execute("UPDATE rooms SET password_hash = ?1 WHERE id = ?2", rusqlite::params![ph, room_id])?;
        }
        if let Some(mu) = max_users {
            conn.execute("UPDATE rooms SET max_users = ?1 WHERE id = ?2", rusqlite::params![mu, room_id])?;
        }
        if let Some(sr) = sample_rate {
            conn.execute("UPDATE rooms SET sample_rate = ?1 WHERE id = ?2", rusqlite::params![sr, room_id])?;
        }
        if let Some(bd) = bit_depth {
            conn.execute("UPDATE rooms SET bit_depth = ?1 WHERE id = ?2", rusqlite::params![bd, room_id])?;
        }
        if let Some(ch) = channels {
            conn.execute("UPDATE rooms SET channels = ?1 WHERE id = ?2", rusqlite::params![ch, room_id])?;
        }
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("Failed to update room: {}", e))
}

pub async fn save_room_file(
    conn: &Connection,
    room_id: i64,
    filename: String,
    storage_path: String,
    size_bytes: i64,
    uploaded_by: i64,
) -> anyhow::Result<i64> {
    conn.call(move |conn| {
        conn.execute(
            "INSERT INTO room_files (room_id, filename, storage_path, size_bytes, uploaded_by) VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![room_id, filename, storage_path, size_bytes, uploaded_by],
        )?;
        Ok(conn.last_insert_rowid())
    })
    .await
    .map_err(|e| anyhow::anyhow!("Failed to save room file: {}", e))
}

pub async fn get_room_files(conn: &Connection, room_id: i64) -> anyhow::Result<Vec<DbRoomFile>> {
    conn.call(move |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, room_id, filename, storage_path, size_bytes, uploaded_by, uploaded_at
             FROM room_files WHERE room_id = ?1 ORDER BY uploaded_at DESC"
        )?;
        let files = stmt.query_map(rusqlite::params![room_id], |row| {
            Ok(DbRoomFile {
                id: row.get(0)?,
                room_id: row.get(1)?,
                filename: row.get(2)?,
                storage_path: row.get(3)?,
                size_bytes: row.get(4)?,
                uploaded_by: row.get(5)?,
                uploaded_at: row.get(6)?,
            })
        })?.filter_map(|r| r.ok()).collect();
        Ok(files)
    })
    .await
    .map_err(|e| anyhow::anyhow!("Failed to get room files: {}", e))
}

pub async fn get_room_file_by_id(conn: &Connection, file_id: i64) -> anyhow::Result<Option<DbRoomFile>> {
    conn.call(move |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, room_id, filename, storage_path, size_bytes, uploaded_by, uploaded_at
             FROM room_files WHERE id = ?1"
        )?;
        let result = stmt.query_row(rusqlite::params![file_id], |row| {
            Ok(DbRoomFile {
                id: row.get(0)?,
                room_id: row.get(1)?,
                filename: row.get(2)?,
                storage_path: row.get(3)?,
                size_bytes: row.get(4)?,
                uploaded_by: row.get(5)?,
                uploaded_at: row.get(6)?,
            })
        });
        match result {
            Ok(f) => Ok(Some(f)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    })
    .await
    .map_err(|e| anyhow::anyhow!("Failed to get file: {}", e))
}

pub async fn save_offline_message(
    conn: &Connection,
    from_user_id: i64,
    to_user_id: i64,
    content: String,
) -> anyhow::Result<()> {
    conn.call(move |conn| {
        conn.execute(
            "INSERT INTO offline_messages (from_user_id, to_user_id, content) VALUES (?1, ?2, ?3)",
            rusqlite::params![from_user_id, to_user_id, content],
        )?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("Failed to save offline message: {}", e))
}

pub async fn get_offline_messages(conn: &Connection, user_id: i64) -> anyhow::Result<Vec<DbOfflineMessage>> {
    conn.call(move |conn| {
        let mut stmt = conn.prepare(
            "SELECT id, from_user_id, to_user_id, content, created_at
             FROM offline_messages WHERE to_user_id = ?1 ORDER BY created_at"
        )?;
        let msgs = stmt.query_map(rusqlite::params![user_id], |row| {
            Ok(DbOfflineMessage {
                id: row.get(0)?,
                from_user_id: row.get(1)?,
                to_user_id: row.get(2)?,
                content: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?.filter_map(|r| r.ok()).collect();
        Ok(msgs)
    })
    .await
    .map_err(|e| anyhow::anyhow!("Failed to get offline messages: {}", e))
}

pub async fn delete_offline_messages(conn: &Connection, user_id: i64) -> anyhow::Result<()> {
    conn.call(move |conn| {
        conn.execute("DELETE FROM offline_messages WHERE to_user_id = ?1", rusqlite::params![user_id])?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("Failed to delete offline messages: {}", e))
}

pub async fn delete_room_file(conn: &Connection, file_id: i64) -> anyhow::Result<Option<String>> {
    conn.call(move |conn| {
        let mut stmt = conn.prepare("SELECT storage_path FROM room_files WHERE id = ?1")?;
        let path = stmt.query_row(rusqlite::params![file_id], |row| {
            row.get::<_, String>(0)
        });
        match path {
            Ok(p) => {
                conn.execute("DELETE FROM room_files WHERE id = ?1", rusqlite::params![file_id])?;
                Ok(Some(p))
            }
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    })
    .await
    .map_err(|e| anyhow::anyhow!("Failed to delete room file: {}", e))
}

// ── Account-Verwaltung ──

/// Find a user by name without verifying a password (existence check).
pub async fn find_user_by_username(
    conn: &Connection,
    username: String,
) -> anyhow::Result<Option<DbUser>> {
    conn.call(move |conn| {
        let mut stmt = conn.prepare("SELECT id, username, role FROM users WHERE username = ?1")?;
        let result = stmt.query_row(rusqlite::params![username], |row| {
            Ok(DbUser {
                id: row.get(0)?,
                username: row.get(1)?,
                role: row.get(2)?,
            })
        });
        match result {
            Ok(u) => Ok(Some(u)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    })
    .await
    .map_err(|e| anyhow::anyhow!("Lookup failed: {}", e))
}

/// List all accounts (id, username, role), sorted by name.
pub async fn list_users(conn: &Connection) -> anyhow::Result<Vec<DbUser>> {
    conn.call(|conn| {
        let mut stmt = conn.prepare("SELECT id, username, role FROM users ORDER BY username")?;
        let rows = stmt.query_map([], |row| {
            Ok(DbUser {
                id: row.get(0)?,
                username: row.get(1)?,
                role: row.get(2)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
    .await
    .map_err(|e| anyhow::anyhow!("Listing users failed: {}", e))
}

pub async fn delete_user(conn: &Connection, user_id: i64) -> anyhow::Result<()> {
    conn.call(move |conn| {
        conn.execute("DELETE FROM users WHERE id = ?1", rusqlite::params![user_id])?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("Deleting user failed: {}", e))
}

pub async fn update_password(conn: &Connection, user_id: i64, new_password: String) -> anyhow::Result<()> {
    let hash = hash_password(&new_password)?;
    conn.call(move |conn| {
        conn.execute(
            "UPDATE users SET password_hash = ?1 WHERE id = ?2",
            rusqlite::params![hash, user_id],
        )?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("Updating password failed: {}", e))
}

pub async fn update_role(conn: &Connection, user_id: i64, role: String) -> anyhow::Result<()> {
    conn.call(move |conn| {
        conn.execute(
            "UPDATE users SET role = ?1 WHERE id = ?2",
            rusqlite::params![role, user_id],
        )?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("Updating role failed: {}", e))
}

// ── Server-Einstellungen (Key-Value) ──

pub async fn get_setting(conn: &Connection, key: &str) -> anyhow::Result<Option<String>> {
    let key = key.to_string();
    conn.call(move |conn| {
        let mut stmt = conn.prepare("SELECT value FROM settings WHERE key = ?1")?;
        let result = stmt.query_row(rusqlite::params![key], |row| row.get::<_, String>(0));
        match result {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    })
    .await
    .map_err(|e| anyhow::anyhow!("Reading setting failed: {}", e))
}

pub async fn set_setting(conn: &Connection, key: &str, value: &str) -> anyhow::Result<()> {
    let key = key.to_string();
    let value = value.to_string();
    conn.call(move |conn| {
        conn.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = ?2",
            rusqlite::params![key, value],
        )?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("Writing setting failed: {}", e))
}

/// Whether self-registration on login is currently enabled.
pub async fn is_registration_open(conn: &Connection) -> bool {
    matches!(get_setting(conn, "registration_open").await, Ok(Some(ref v)) if v == "true")
}

pub async fn set_registration(conn: &Connection, open: bool) -> anyhow::Result<()> {
    set_setting(conn, "registration_open", if open { "true" } else { "false" }).await
}
