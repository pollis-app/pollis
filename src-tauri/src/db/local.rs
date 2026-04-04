use rusqlite::Connection;
use crate::error::Result;

// Bump this string whenever local_schema.sql changes OR encryption is added.
// On mismatch the old DB file is deleted and recreated from scratch.
// Version 4: per-user DB files (pollis_{user_id}.db), preferences + ui_state tables.
// Version 5: mls_kv table for openmls StorageProvider.
// Version 6: attachment table rewritten with convergent-encryption schema.
// Version 7: attachment table removed — dedup lives on Turso, metadata in message payload.
const LOCAL_SCHEMA_VERSION: &str = "7";
const SCHEMA: &str = include_str!("migrations/local_schema.sql");

pub struct LocalDb {
    conn: Connection,
}

impl LocalDb {
    /// Open the per-user database at `pollis_{user_id}.db`.
    pub fn open_for_user(user_id: &str, key: &[u8]) -> Result<Self> {
        let data_dir = dirs_path();
        std::fs::create_dir_all(&data_dir)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("create data dir: {e}")))?;

        let db_path = data_dir.join(format!("pollis_{user_id}.db"));
        Self::open_at(&db_path, key)
    }

    fn open_at(db_path: &std::path::Path, key: &[u8]) -> Result<Self> {
        let key_pragma = format!("PRAGMA key = \"x'{}'\"", hex::encode(key));

        // Check if the stored schema version matches. If not, wipe and recreate.
        if db_path.exists() {
            match Connection::open(db_path) {
                Ok(conn) => {
                    // Apply key before any SQL — required for SQLCipher.
                    let _ = conn.execute_batch(&key_pragma);
                    let version: Option<String> = conn
                        .query_row(
                            "SELECT value FROM kv WHERE key = 'schema_version'",
                            [],
                            |row| row.get(0),
                        )
                        .ok();
                    if version.as_deref() != Some(LOCAL_SCHEMA_VERSION) {
                        drop(conn);
                        std::fs::remove_file(db_path).map_err(|e| {
                            crate::error::Error::Other(anyhow::anyhow!("remove stale db: {e}"))
                        })?;
                    }
                }
                Err(_) => {
                    // Unreadable (wrong key or corrupt) — delete and start fresh.
                    std::fs::remove_file(db_path).ok();
                }
            }
        }

        let conn = Connection::open(db_path)?;
        // Key must be applied before any other SQL on an encrypted database.
        conn.execute_batch(&key_pragma)?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        conn.execute(
            "INSERT OR REPLACE INTO kv (key, value) VALUES ('schema_version', ?1)",
            rusqlite::params![LOCAL_SCHEMA_VERSION],
        )?;

        Ok(Self { conn })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> LocalDb {
        LocalDb::open_in_memory().expect("in-memory db")
    }

    #[test]
    fn migration_creates_tables() {
        let db = db();
        let conn = db.conn();

        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, sent_at)
             VALUES ('m1', 'conv1', 'user1', X'deadbeef', '2024-01-01T00:00:00Z')",
            [],
        ).expect("message table exists");

        conn.execute(
            "INSERT INTO dm_conversation (id, peer_user_id) VALUES ('dm1', 'user2')",
            [],
        ).expect("dm_conversation table exists");

        conn.execute(
            "INSERT INTO signal_session (user_id, device_id, session_data)
             VALUES ('user1', 1, X'cafebabe')",
            [],
        ).expect("signal_session table exists");
    }

    #[test]
    fn message_insert_and_query_by_conversation() {
        let db = db();
        let conn = db.conn();

        for i in 1..=3u32 {
            conn.execute(
                "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at)
                 VALUES (?1, 'conv-a', 'sender1', X'00', ?2, ?3)",
                rusqlite::params![
                    format!("msg-{i}"),
                    format!("hello {i}"),
                    format!("2024-01-01T00:00:0{i}Z"),
                ],
            ).unwrap();
        }

        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, sent_at)
             VALUES ('other', 'conv-b', 'sender2', X'00', '2024-01-01T00:00:00Z')",
            [],
        ).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM message WHERE conversation_id = 'conv-a'",
            [],
            |row| row.get(0),
        ).unwrap();

        assert_eq!(count, 3);
    }

    #[test]
    fn message_content_roundtrip() {
        let db = db();
        let conn = db.conn();
        let content = "Hello, world!";

        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at)
             VALUES ('m1', 'conv1', 'user1', X'00', ?1, '2024-01-01T00:00:00Z')",
            rusqlite::params![content],
        ).unwrap();

        let stored: String = conn.query_row(
            "SELECT content FROM message WHERE id = 'm1'",
            [],
            |row| row.get(0),
        ).unwrap();

        assert_eq!(stored, content);
    }

    #[test]
    fn signed_prekey_primary_key_is_unique() {
        let db = db();
        let conn = db.conn();

        conn.execute(
            "INSERT INTO signed_prekey (id, public_key, signature) VALUES (1, X'aabb', X'ccdd')",
            [],
        ).unwrap();

        let result = conn.execute(
            "INSERT INTO signed_prekey (id, public_key, signature) VALUES (1, X'eeff', X'1122')",
            [],
        );

        assert!(result.is_err(), "duplicate key_id should fail");
    }

    #[test]
    fn one_time_prekey_used_flag_update() {
        let db = db();
        let conn = db.conn();

        conn.execute(
            "INSERT INTO one_time_prekey (id, public_key) VALUES (42, X'aabb')",
            [],
        ).unwrap();

        let used: i64 = conn.query_row(
            "SELECT used FROM one_time_prekey WHERE id = 42",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(used, 0);

        conn.execute("UPDATE one_time_prekey SET used = 1 WHERE id = 42", []).unwrap();

        let used: i64 = conn.query_row(
            "SELECT used FROM one_time_prekey WHERE id = 42",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(used, 1);
    }

    #[test]
    fn dm_conversation_peer_must_be_unique() {
        let db = db();
        let conn = db.conn();

        conn.execute(
            "INSERT INTO dm_conversation (id, peer_user_id) VALUES ('dm1', 'peer-a')",
            [],
        ).unwrap();

        let result = conn.execute(
            "INSERT INTO dm_conversation (id, peer_user_id) VALUES ('dm2', 'peer-a')",
            [],
        );

        assert!(result.is_err(), "duplicate peer_user_id should violate UNIQUE constraint");
    }

}

pub fn dirs_path() -> std::path::PathBuf {
    // POLLIS_DATA_DIR lets a second dev instance use a separate local DB
    // without having to override $HOME (which breaks rustup/cargo).
    if let Ok(dir) = std::env::var("POLLIS_DATA_DIR") {
        return std::path::PathBuf::from(dir);
    }

    #[cfg(target_os = "macos")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        std::path::PathBuf::from(home)
            .join("Library/Application Support/com.pollis.app")
    }
    #[cfg(target_os = "linux")]
    {
        let home = std::env::var("HOME").unwrap_or_default();
        std::path::PathBuf::from(home).join(".local/share/pollis")
    }
    #[cfg(target_os = "windows")]
    {
        let appdata = std::env::var("APPDATA").unwrap_or_default();
        std::path::PathBuf::from(appdata).join("pollis")
    }
}
