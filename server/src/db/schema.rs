use tokio_rusqlite::Connection;

const MIGRATION: &str = include_str!("../../migrations/001_initial.sql");

/// `klango_mode`: im Klango-Modus gibt es KEINEN Standardraum. Die Raumliste
/// zeigt dort die Gruppen des Nutzers und die offenen Raeume; eine "Lobby",
/// in der jeder landet, waere ein Raum ohne Zweck — und ein Raum, den niemand
/// schliessen kann. Die Migration legt sie unvermeidlich an (INSERT OR IGNORE
/// mit fester id), deshalb wird sie hier direkt danach wieder entfernt.
pub async fn initialize(conn: &Connection, klango_mode: bool) -> anyhow::Result<()> {
    conn.call(move |conn| {
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
            // Klango-Modus (docs/klango.md 1.2): Gruppenraum, Eigentümer,
            // temporär (verschwindet leer), privat (Anrufraum).
            "ALTER TABLE rooms ADD COLUMN group_id TEXT NOT NULL DEFAULT ''",
            "ALTER TABLE rooms ADD COLUMN owner_id INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE rooms ADD COLUMN temporary INTEGER NOT NULL DEFAULT 0",
            "ALTER TABLE rooms ADD COLUMN private INTEGER NOT NULL DEFAULT 0",
        ] {
            let _ = conn.execute(stmt, []);
        }
        // Eindeutigkeit der zentralen Identität (mehrere NULLs erlaubt SQLite).
        let _ = conn.execute(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_users_central_uid ON users(central_uid)",
            [],
        );
        let _ = conn.execute("CREATE INDEX IF NOT EXISTS idx_rooms_tenant ON rooms(tenant)", []);
        // Temporäre Räume überleben keinen Neustart: was hier noch liegt, sind
        // Reste eines Absturzes (ihre Admin-/Sperrlisten lebten ohnehin nur im
        // Speicher).
        let _ = conn.execute("DELETE FROM rooms WHERE temporary = 1", []);
        if klango_mode {
            let _ = conn.execute("DELETE FROM rooms WHERE is_default = 1", []);
        }
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
