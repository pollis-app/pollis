use rusqlite::{Connection, OptionalExtension};
use crate::error::{Error, Result};

// Bump this string whenever local_schema.sql changes OR encryption is added.
// On mismatch the old DB file is deleted and recreated from scratch.
// Version 4: per-user DB files (pollis_{user_id}.db), preferences + ui_state tables.
// Version 5: mls_kv table for openmls StorageProvider.
// Version 6: attachment table rewritten with convergent-encryption schema.
// Version 7: attachment table removed — dedup lives on Turso, metadata in message payload.
// Version 8: message table gains edited_at and deleted_at columns.
const LOCAL_SCHEMA_VERSION: &str = "8";
const SCHEMA: &str = include_str!("local_schema.sql");

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

        // A DB is "fresh" if the file didn't exist or we wipe it below. Tracked
        // because `auto_vacuum=INCREMENTAL` can only be set before any table is
        // created on a fresh DB; an existing DB has to be converted via VACUUM.
        let mut is_fresh = !db_path.exists();

        // Check if the stored schema version matches. If not, wipe and recreate.
        //
        // Be narrow about what justifies nuking a user's encrypted DB: wrong
        // SQLCipher key, missing schema_version row, or an explicit version
        // mismatch. Any *other* rusqlite failure (lock contention mid-open,
        // transient I/O) is surfaced instead — we refuse to eat the local
        // database on an error we don't understand.
        if db_path.exists() {
            let conn = Connection::open(db_path)?;
            // Key must be applied before any SQL — required for SQLCipher.
            conn.execute_batch(&key_pragma)?;

            let version_res = conn.query_row(
                "SELECT value FROM kv WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            );

            let should_wipe = match version_res {
                Ok(v) => v != LOCAL_SCHEMA_VERSION,
                Err(rusqlite::Error::QueryReturnedNoRows) => true,
                Err(rusqlite::Error::SqliteFailure(ffi_err, _))
                    if ffi_err.code == rusqlite::ErrorCode::NotADatabase =>
                {
                    // Wrong SQLCipher key or genuinely not-a-database bytes.
                    true
                }
                Err(e) => return Err(e.into()),
            };

            if should_wipe {
                drop(conn);
                std::fs::remove_file(db_path).map_err(|e| {
                    crate::error::Error::Other(anyhow::anyhow!("remove stale db: {e}"))
                })?;
                is_fresh = true;
            }
        }

        let conn = Connection::open(db_path)?;
        // Key must be applied before any other SQL on an encrypted database.
        conn.execute_batch(&key_pragma)?;
        // Reclaimable storage: incremental auto_vacuum lets `reclaim()` shrink
        // the file after eviction deletes. On a fresh DB the pragma must run
        // before the file gains any pages (so before journal_mode/CREATE TABLE);
        // an existing NONE DB is converted in place via VACUUM below. Idempotent
        // across opens — a no-op once already INCREMENTAL.
        if is_fresh {
            conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;")?;
        }
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        if !is_fresh {
            ensure_incremental_auto_vacuum(&conn)?;
        }
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
        // Must precede table creation, mirroring the fresh-create path above.
        conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

// ── Message retention / local eviction ────────────────────────────────────────
//
// Device-local message lookback: old LOCAL messages are evicted so the encrypted
// local SQLite file does not grow forever. The retention window is a device-only
// setting in `ui_state` (never synced to remote, unlike the `preferences` table).
// This is bounded *local* history only — it never deletes anything remote and is
// orthogonal to MLS epoch visibility.

/// `ui_state` key holding the retention window in days (text integer).
const RETENTION_KEY: &str = "message_retention_days";

/// Retention windows offered to the user, in days. `0` means "Forever" (no
/// eviction). Any other value must appear in this set to be accepted.
pub const ALLOWED_RETENTION_DAYS: [i64; 4] = [0, 30, 90, 365];

/// Convert an existing `auto_vacuum=NONE` database to `INCREMENTAL` in place.
/// A no-op if already FULL/INCREMENTAL, so it is safe to call on every open.
/// `VACUUM` is required because `auto_vacuum` cannot otherwise change on a DB
/// that already holds tables; it rewrites the file and preserves all data.
fn ensure_incremental_auto_vacuum(conn: &Connection) -> Result<()> {
    // 0 = NONE, 1 = FULL, 2 = INCREMENTAL.
    let mode: i64 = conn.query_row("PRAGMA auto_vacuum;", [], |row| row.get(0))?;
    if mode == 0 {
        conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL; VACUUM;")?;
    }
    Ok(())
}

/// Reclaim free pages produced by deletes and truncate the WAL so the on-disk
/// file actually shrinks. A plain `DELETE` only marks pages free; with
/// `auto_vacuum=INCREMENTAL` set, `incremental_vacuum` returns them to the OS.
pub fn reclaim(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA incremental_vacuum;")?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}

/// Read the device-local retention window in days. Absent or `"0"` => `0`
/// (Forever — no eviction).
pub fn get_message_retention_days(conn: &Connection) -> Result<i64> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM ui_state WHERE key = ?1",
            rusqlite::params![RETENTION_KEY],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw.and_then(|v| v.trim().parse::<i64>().ok()).unwrap_or(0))
}

/// Set the device-local retention window. `days` must be one of
/// [`ALLOWED_RETENTION_DAYS`]. Runs an eviction sweep immediately so the new
/// window's effect is visible without waiting for the next lifecycle hook.
pub fn set_message_retention_days(conn: &Connection, days: i64) -> Result<()> {
    if !ALLOWED_RETENTION_DAYS.contains(&days) {
        return Err(Error::Other(anyhow::anyhow!(
            "invalid message_retention_days {days}: must be one of {ALLOWED_RETENTION_DAYS:?}"
        )));
    }
    conn.execute(
        "INSERT INTO ui_state (key, value, updated_at) VALUES (?1, ?2, datetime('now')) \
         ON CONFLICT(key) DO UPDATE SET value = ?2, updated_at = datetime('now')",
        rusqlite::params![RETENTION_KEY, days.to_string()],
    )?;
    evict_old_messages(conn)?;
    Ok(())
}

/// Delete local messages older than the configured retention window, then
/// reclaim the freed pages. Returns the number of rows deleted. A retention of
/// `0` (Forever) is a no-op. Only the `message` table is touched — `mls_kv`
/// (MLS decryption keys) is never affected.
pub fn evict_old_messages(conn: &Connection) -> Result<usize> {
    let days = get_message_retention_days(conn)?;
    if days <= 0 {
        return Ok(0);
    }
    // `received_at` is stored as "YYYY-MM-DD HH:MM:SS" (datetime('now') format),
    // which compares correctly against datetime('now', '-N days').
    let modifier = format!("-{days} days");
    let deleted = conn.execute(
        "DELETE FROM message WHERE received_at < datetime('now', ?1)",
        rusqlite::params![modifier],
    )?;
    reclaim(conn)?;
    Ok(deleted)
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

        // signal_session, signed_prekey, one_time_prekey, group_sender_key were
        // Signal Protocol tables removed in migration 000009 and are no longer
        // created for new local databases.
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

    // ── Retention / eviction ──────────────────────────────────────────────────

    /// Insert a message with an explicit `received_at` (a datetime() expression
    /// or literal). `received_at_sql` is spliced as SQL so callers can pass
    /// `datetime('now','-100 days')`.
    fn insert_message(conn: &Connection, id: &str, received_at_sql: &str) {
        conn.execute(
            &format!(
                "INSERT INTO message (id, conversation_id, sender_id, ciphertext, sent_at, received_at)
                 VALUES (?1, 'conv-a', 'sender1', X'00', '2024-01-01T00:00:00Z', {received_at_sql})"
            ),
            rusqlite::params![id],
        )
        .unwrap();
    }

    fn message_ids(conn: &Connection) -> Vec<String> {
        let mut stmt = conn.prepare("SELECT id FROM message ORDER BY id").unwrap();
        let ids = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        ids
    }

    #[test]
    fn evicts_old_messages_keeps_recent() {
        let db = db();
        let conn = db.conn();
        insert_message(conn, "old", "datetime('now','-100 days')");
        insert_message(conn, "recent", "datetime('now','-1 day')");

        set_message_retention_days(conn, 30).unwrap();

        // The set already swept; an explicit sweep confirms idempotence + count.
        let deleted = evict_old_messages(conn).unwrap();
        assert_eq!(deleted, 0, "second sweep finds nothing new to delete");
        assert_eq!(message_ids(conn), vec!["recent".to_string()]);
    }

    #[test]
    fn retention_zero_is_no_op() {
        let db = db();
        let conn = db.conn();
        insert_message(conn, "old", "datetime('now','-1000 days')");

        // Unset retention defaults to 0 (Forever).
        assert_eq!(get_message_retention_days(conn).unwrap(), 0);
        assert_eq!(evict_old_messages(conn).unwrap(), 0);
        assert_eq!(message_ids(conn), vec!["old".to_string()]);
    }

    #[test]
    fn set_retention_triggers_immediate_sweep() {
        let db = db();
        let conn = db.conn();
        insert_message(conn, "old", "datetime('now','-100 days')");
        insert_message(conn, "recent", "datetime('now','-1 day')");

        // Setting the window must evict immediately, not on the next lifecycle.
        set_message_retention_days(conn, 90).unwrap();
        assert_eq!(get_message_retention_days(conn).unwrap(), 90);
        assert_eq!(message_ids(conn), vec!["recent".to_string()]);
    }

    #[test]
    fn set_retention_rejects_invalid_values() {
        let db = db();
        let conn = db.conn();
        assert!(set_message_retention_days(conn, 45).is_err());
        assert!(set_message_retention_days(conn, -1).is_err());
        // Valid values are accepted.
        for days in ALLOWED_RETENTION_DAYS {
            set_message_retention_days(conn, days).unwrap();
        }
    }

    #[test]
    fn auto_vacuum_in_place_upgrade() {
        // A DB created with auto_vacuum=NONE, then converted in place.
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA auto_vacuum=NONE;").unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        let before: i64 = conn
            .query_row("PRAGMA auto_vacuum;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(before, 0, "starts as NONE");

        ensure_incremental_auto_vacuum(&conn).unwrap();

        let after: i64 = conn
            .query_row("PRAGMA auto_vacuum;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(after, 2, "converted to INCREMENTAL (2)");
    }

    #[test]
    fn reclaim_runs_after_delete() {
        let db = db();
        let conn = db.conn();
        insert_message(conn, "m1", "datetime('now','-100 days')");
        conn.execute("DELETE FROM message", []).unwrap();
        reclaim(conn).expect("reclaim should succeed after a delete");
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
    // Mobile passes POLLIS_DATA_DIR (app sandbox / Documents) once the bridge
    // is wired (issue #185); temp_dir is a compile-complete fallback so the
    // function is total on iOS/Android.
    #[cfg(any(target_os = "ios", target_os = "android"))]
    {
        std::env::temp_dir().join("pollis")
    }
}
