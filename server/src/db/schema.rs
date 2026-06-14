use tokio_rusqlite::Connection;

const MIGRATION: &str = include_str!("../../migrations/001_initial.sql");

pub async fn initialize(conn: &Connection) -> anyhow::Result<()> {
    conn.call(|conn| {
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(MIGRATION)?;
        // Audio-Spalten für bestehende DBs nachrüsten (Fehler ignorieren, falls
        // die Spalte schon existiert — SQLite kennt kein ADD COLUMN IF NOT EXISTS).
        for stmt in [
            "ALTER TABLE rooms ADD COLUMN sample_rate INTEGER NOT NULL DEFAULT 48000",
            "ALTER TABLE rooms ADD COLUMN bit_depth INTEGER NOT NULL DEFAULT 16",
            "ALTER TABLE rooms ADD COLUMN channels INTEGER NOT NULL DEFAULT 1",
            "ALTER TABLE rooms ADD COLUMN bitrate INTEGER NOT NULL DEFAULT 0",
        ] {
            let _ = conn.execute(stmt, []);
        }
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("Database initialization failed: {}", e))
}
