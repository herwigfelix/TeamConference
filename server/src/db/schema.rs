use tokio_rusqlite::Connection;

const MIGRATION: &str = include_str!("../../migrations/001_initial.sql");

pub async fn initialize(conn: &Connection) -> anyhow::Result<()> {
    conn.call(|conn| {
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(MIGRATION)?;
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("Database initialization failed: {}", e))
}
