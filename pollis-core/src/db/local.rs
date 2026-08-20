use rusqlite::{Connection, OptionalExtension};
use crate::error::{Error, Result};

// Bump this string when an existing table's shape changes incompatibly OR
// encryption changes. On mismatch the old DB file is DELETED and recreated from
// scratch — which throws away the device's MLS state and message history, so it
// is a last resort, not routine hygiene.
//
// A purely additive `CREATE TABLE IF NOT EXISTS` needs no bump: `SCHEMA` is
// re-applied on every open (see `open_at`), so the new table appears on existing
// databases with nothing lost.
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
        // Additive columns run BEFORE the schema batch, because that batch now
        // contains an index over one of them and `CREATE INDEX` on a column an
        // existing DB has not gained yet would fail the whole batch.
        add_missing_columns(&conn)?;
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
/// Columns added to EXISTING tables since a database was first created.
///
/// `local_schema.sql` is re-applied on every open, but `CREATE TABLE IF NOT
/// EXISTS` cannot widen a table that already exists — and the alternative,
/// bumping [`LOCAL_SCHEMA_VERSION`], DELETES the database file along with the
/// device's MLS state and the user's entire message history. Adding a nullable
/// column is not remotely worth that, so additive columns land here instead.
///
/// Each entry must be nullable or carry a DEFAULT, so existing rows stay valid
/// without a backfill.
const ADDITIVE_COLUMNS: &[(&str, &str, &str)] = &[
    // (table, column, full ALTER fragment)
    ("message", "thread_id", "ALTER TABLE message ADD COLUMN thread_id TEXT"),
];

/// Apply [`ADDITIVE_COLUMNS`] to a database that predates them.
///
/// Runs on every open and is idempotent. Two failures are expected and
/// tolerated rather than propagated:
///
/// * **"duplicate column name"** — the column is already there, i.e. every
///   open after the first.
/// * **"no such table"** — a fresh database, where the schema batch that
///   follows creates the table already carrying the column.
///
/// Anything else is a real error and is surfaced, on the same principle as the
/// wipe guard in `open_at`: we do not quietly paper over failures we do not
/// understand on a database holding the user's history.
fn add_missing_columns(conn: &Connection) -> Result<()> {
    for (table, column, stmt) in ADDITIVE_COLUMNS {
        match conn.execute(stmt, []) {
            Ok(_) => {}
            Err(rusqlite::Error::SqliteFailure(_, Some(ref msg)))
                if msg.contains("duplicate column name")
                    || msg.starts_with("no such table") => {}
            Err(e) => {
                return Err(Error::Other(anyhow::anyhow!(
                    "add column {table}.{column}: {e}"
                )))
            }
        }
    }
    Ok(())
}

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

/// In-process data-dir override. `None` (the production state) means "resolve
/// from `POLLIS_DATA_DIR` / the platform default", so nothing changes for a
/// shipped build, a dev instance, or the mobile bridge.
///
/// Exists because the alternative — having test rigs point the process at a
/// scratch directory with `std::env::set_var` — is a **data race**: `setenv`
/// mutates a process-global array that any other thread's `getenv` may be
/// reading at the same moment, which is why Rust 2024 made `set_var` `unsafe`.
/// A rig that flips it between simulated devices while background tasks are live
/// (the pollis-tui smokes + UI driver do exactly that) can therefore fail under
/// load with nothing broken — see #923. This cell is the same knob with a lock
/// around it: a swap is atomic and a reader always sees one whole path.
static DATA_DIR: std::sync::RwLock<Option<std::path::PathBuf>> = std::sync::RwLock::new(None);

/// Point every data-dir-derived path — the local SQLCipher DB, `accounts.json`,
/// the file keystore, the overlay guard book — at `dir` for the rest of this
/// process. Overrides `POLLIS_DATA_DIR`.
///
/// Intended for test rigs that need a scratch directory (and, for a multi-device
/// rig, need to swap between per-device ones). Production leaves it unset.
pub fn set_data_dir(dir: impl Into<std::path::PathBuf>) {
    *DATA_DIR.write().expect("data-dir lock") = Some(dir.into());
}

/// The explicitly-chosen data dir, if there is one: the in-process override, else
/// `POLLIS_DATA_DIR`. `None` means "nobody asked for a specific directory", which
/// is what a shipped desktop build sees — and is why the keystore's keyring
/// namespacing keys off THIS rather than off [`dirs_path`], which always has a
/// value.
pub(crate) fn explicit_data_dir() -> Option<std::path::PathBuf> {
    if let Some(dir) = DATA_DIR.read().expect("data-dir lock").clone() {
        return Some(dir);
    }
    // POLLIS_DATA_DIR lets a second dev instance use a separate local DB
    // without having to override $HOME (which breaks rustup/cargo). It is also
    // how the mobile bridge scopes state into the app sandbox.
    std::env::var("POLLIS_DATA_DIR").ok().map(Into::into)
}

pub fn dirs_path() -> std::path::PathBuf {
    if let Some(dir) = explicit_data_dir() {
        return dir;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> LocalDb {
        LocalDb::open_in_memory().expect("in-memory db")
    }

    /// The entire at-rest story (`docs/security-whitepaper.md` §7) rests on one
    /// unchecked assumption: that the `sqlite3_*` symbols this crate calls at
    /// runtime are the ones `libsqlite3-sys`'s `bundled-sqlcipher` build
    /// compiled. Nothing enforced it. If ANOTHER SQLite lands in the same link
    /// and wins those symbols — a second `sqlite3` amalgamation from any
    /// dependency does exactly that, since the linker takes the first
    /// definition and never pulls the second — then `PRAGMA key` becomes an
    /// UNKNOWN pragma, and SQLite ignores unknown pragmas SILENTLY. Every write
    /// still succeeds; the file is just plaintext. There is no error, no
    /// warning, and no behavioural difference until someone reads the file.
    ///
    /// `PRAGMA cipher_version` exists only in SQLCipher, so it is the one
    /// question whose answer distinguishes the two builds. It is asserted in
    /// BOTH directions, per platform, because both answers are wrong somewhere:
    /// Linux/macOS/mobile must have SQLCipher, and Windows deliberately must
    /// NOT (it cannot link one — see the long note in `Cargo.toml`, #991). A
    /// one-way assert would let the Windows gap re-open silently on the other
    /// platforms, or let Windows quietly acquire encryption while the docs and
    /// the upgrade path still say it has none.
    #[test]
    fn sqlcipher_is_the_sqlite_we_actually_linked() {
        let conn = Connection::open_in_memory().expect("open");
        let version = conn
            .query_row("PRAGMA cipher_version", [], |row| row.get::<_, String>(0))
            .optional()
            .expect("query cipher_version");

        let linked = version
            .as_deref()
            .map(str::trim)
            .is_some_and(|v| !v.is_empty());

        #[cfg(not(target_os = "windows"))]
        assert!(
            linked,
            "`PRAGMA cipher_version` answered nothing ({version:?}): the linked sqlite3 is NOT \
             SQLCipher. `PRAGMA key` is being silently ignored and pollis_<user>.db is PLAINTEXT \
             on this platform. Check what else in the dependency graph ships an sqlite3 \
             amalgamation and is winning the `sqlite3_*` symbols."
        );

        #[cfg(target_os = "windows")]
        assert!(
            !linked,
            "SQLCipher is linked on Windows ({version:?}) — which this build is not supposed to \
             be able to do. If that is now genuinely possible, this is good news and three things \
             must change together: the rusqlite feature note in Cargo.toml, \
             docs/security-whitepaper.md §7.0, and the upgrade path — every existing Windows \
             pollis_<user>.db is PLAINTEXT and SQLCipher will refuse to open it, which wipes the \
             device's history and MLS state unless it is migrated first."
        );
    }

    /// #825: gaining `message.thread_id` must NOT cost the user their history.
    ///
    /// Builds a database with the pre-thread `message` shape, puts a message in
    /// it, then runs the same open-path steps `open_at` runs — additive columns
    /// first, then the schema batch. The row, and the MLS state alongside it,
    /// must still be there afterwards. Bumping `LOCAL_SCHEMA_VERSION` instead
    /// would delete the file and fail this test, which is the point.
    #[test]
    fn additive_column_preserves_existing_history() {
        let conn = Connection::open_in_memory().expect("open");

        // The `message` table exactly as it was before threads: no thread_id.
        conn.execute_batch(
            "CREATE TABLE message (
                 id TEXT PRIMARY KEY,
                 conversation_id TEXT NOT NULL,
                 sender_id TEXT NOT NULL,
                 ciphertext BLOB NOT NULL,
                 content TEXT,
                 reply_to_id TEXT,
                 sent_at TEXT NOT NULL,
                 received_at TEXT NOT NULL DEFAULT (datetime('now')),
                 delivered INTEGER NOT NULL DEFAULT 0,
                 edited_at TEXT,
                 deleted_at TEXT
             );
             CREATE TABLE mls_kv (k TEXT PRIMARY KEY, v BLOB NOT NULL);",
        )
        .expect("legacy schema");

        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at)
             VALUES ('old1', 'conv1', 'user1', X'deadbeef', 'from before threads', '2024-01-01T00:00:00Z')",
            [],
        )
        .expect("seed history");
        conn.execute(
            "INSERT INTO mls_kv (k, v) VALUES ('group_state', X'c0ffee')",
            [],
        )
        .expect("seed mls state");

        // The upgrade, in the order `open_at` performs it.
        add_missing_columns(&conn).expect("additive columns");
        conn.execute_batch(SCHEMA).expect("schema batch");

        // History survived.
        let (content, thread_id): (String, Option<String>) = conn
            .query_row(
                "SELECT content, thread_id FROM message WHERE id = 'old1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("pre-existing message still present");
        assert_eq!(content, "from before threads");
        assert_eq!(thread_id, None, "pre-thread messages are not in a thread");

        // MLS state survived — the expensive half of a wipe.
        let mls: Vec<u8> = conn
            .query_row("SELECT v FROM mls_kv WHERE k = 'group_state'", [], |row| {
                row.get(0)
            })
            .expect("mls state still present");
        assert_eq!(mls, vec![0xc0, 0xff, 0xee]);

        // The new column is usable, and re-running the upgrade is a no-op.
        add_missing_columns(&conn).expect("second run must be idempotent");
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, thread_id, sent_at)
             VALUES ('new1', 'conv1', 'user1', X'01', 'reply', 'old1', '2024-01-02T00:00:00Z')",
            [],
        )
        .expect("thread_id is writable");
    }

    /// A fresh database has no `message` table when additive columns run, and
    /// that must not be an error — the schema batch right after creates the
    /// table already carrying the column.
    #[test]
    fn additive_columns_tolerate_a_fresh_database() {
        let conn = Connection::open_in_memory().expect("open");
        add_missing_columns(&conn).expect("no such table must be tolerated");
        conn.execute_batch(SCHEMA).expect("schema batch");
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, thread_id, sent_at)
             VALUES ('m1', 'c1', 'u1', X'00', 't1', '2024-01-01T00:00:00Z')",
            [],
        )
        .expect("fresh db has thread_id");
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
