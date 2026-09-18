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
            // Zentrale Identität (Identity Provider). NULL für lokale Accounts.
            "ALTER TABLE users ADD COLUMN central_uid TEXT",
            // Multi-Tenant: Zugehörigkeit eines Raums zu einem Unterserver.
            // '' = Einzelserver-Modus (Default, unverändertes Verhalten).
            "ALTER TABLE rooms ADD COLUMN tenant TEXT NOT NULL DEFAULT ''",
        ] {
            let _ = conn.execute(stmt, []);
        }
        // Eindeutigkeit der zentralen Identität (mehrere NULLs erlaubt SQLite).
        let _ = conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_users_central_uid ON users(central_uid)",
            [],
        );
        let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_rooms_tenant ON rooms(tenant)", []);
        // Unterserver (Tenants) im Multi-Tenant-Modus.
        let _ = conn.execute(
            "CREATE TABLE IF NOT EXISTS tenants (
                id          TEXT PRIMARY KEY,
                owner_uid   TEXT NOT NULL DEFAULT '',
                name        TEXT NOT NULL DEFAULT '',
                created_at  DATETIME DEFAULT CURRENT_TIMESTAMP
             )",
            [],
        );
        Ok(())
    })
    .await
    .map_err(|e| anyhow::anyhow!("Database initialization failed: {}", e))
}
