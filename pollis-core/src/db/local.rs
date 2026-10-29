use rusqlite::{Connection, OptionalExtension};
use crate::error::{Error, Result};

// Windows links rusqlite's `sqlcipher` feature, so `pollis-sqlcipher` supplies
// the library (#992); naming the crate is what puts its rlib on the link line.
#[cfg(target_os = "windows")]
use pollis_sqlcipher as _;

// Bump only when an existing table's shape or the encryption changes
// incompatibly: a mismatch DELETES the DB file, losing the device's MLS state
// and history. Additive `CREATE TABLE IF NOT EXISTS` needs no bump (`SCHEMA` is
// re-applied on every open); additive columns go in `ADDITIVE_COLUMNS`.
const LOCAL_SCHEMA_VERSION: &str = "8";
const SCHEMA: &str = include_str!("local_schema.sql");

/// The local schema, for unit tests. Prefer [`apply_local_schema`].
#[cfg(test)]
pub(crate) fn schema_sql() -> &'static str {
    SCHEMA
}

/// Make `conn` a usable local database: register the scalar functions the
/// schema's triggers call, then apply the schema.
///
/// The pairing is the point: the FTS5 triggers call `pollis_search_text`
/// (#850), so a connection with the schema but not the function fails every
/// INSERT into `message` with `no such function`.
pub fn apply_local_schema(conn: &Connection) -> Result<()> {
    crate::db::search_text::register(conn)?;
    conn.execute_batch(SCHEMA)?;
    Ok(())
}

/// `<data_dir>/pollis_<user_id>.db`, the only way a user id becomes a database
/// path. Re-checks the id shape at the join, mirroring the response chokepoint
/// (`commands::auth::accept_server_user_id`).
pub fn user_db_path(data_dir: &std::path::Path, user_id: &str) -> Result<std::path::PathBuf> {
    if !crate::util::is_safe_id(user_id) {
        return Err(Error::Other(anyhow::anyhow!(
            "refusing to build a local database path for a malformed user_id"
        )));
    }
    Ok(data_dir.join(format!("pollis_{user_id}.db")))
}

pub struct LocalDb {
    conn: Connection,
}

impl LocalDb {
    /// Open the per-user database at `pollis_{user_id}.db`.
    pub fn open_for_user(user_id: &str, key: &[u8]) -> Result<Self> {
        Self::open_for_user_in(&dirs_path(), user_id, key)
    }

    /// [`LocalDb::open_for_user`] under an explicit data directory
    /// ([`crate::state::AppState::local_data_dir`]).
    pub fn open_for_user_in(data_dir: &std::path::Path, user_id: &str, key: &[u8]) -> Result<Self> {
        crate::private_fs::create_dir_all(data_dir)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("create data dir: {e}")))?;

        let db_path = user_db_path(data_dir, user_id)?;
        Self::open_at(&db_path, key)
    }

    fn open_at(db_path: &std::path::Path, key: &[u8]) -> Result<Self> {
        let key_pragma = format!("PRAGMA key = \"x'{}'\"", hex::encode(key));

        // #992: destroy a database written by a build whose `PRAGMA key` did
        // nothing, rather than leave its plaintext pages on disk.
        let destroyed = destroy_plaintext_database(db_path)?;

        // Fresh means missing, just destroyed, or wiped below: `auto_vacuum`
        // can only be set before any table exists. `destroyed` is tracked
        // separately because disposal can leave a zero-length file (`dispose`).
        let mut is_fresh = destroyed || !db_path.exists();

        // Wipe only on the wrong key, a missing version row or a version
        // mismatch; any other failure (lock contention, I/O) is surfaced rather
        // than eat the user's database.
        if !destroyed && db_path.exists() {
            let conn = Connection::open(db_path)?;
            conn.execute_batch(&key_pragma)?;

            let version_res = conn.query_row(
                "SELECT value FROM kv WHERE key = 'schema_version'",
                [],
                |row| row.get::<_, String>(0),
            );

            // Logged (#1312): a wipe deletes the device's history and MLS state.
            let wipe_reason: Option<String> = match version_res {
                Ok(v) if v != LOCAL_SCHEMA_VERSION => Some(format!(
                    "schema version {v} is not {LOCAL_SCHEMA_VERSION}"
                )),
                Ok(_) => None,
                Err(rusqlite::Error::QueryReturnedNoRows) => {
                    Some("no schema version recorded".to_string())
                }
                Err(rusqlite::Error::SqliteFailure(ffi_err, _))
                    if ffi_err.code == rusqlite::ErrorCode::NotADatabase =>
                {
                    Some("it does not open with this device's key".to_string())
                }
                Err(e) => return Err(e.into()),
            };
            let should_wipe = wipe_reason.is_some();
            if let Some(reason) = &wipe_reason {
                crate::diag!(
                    "[db] DESTROYING the local database — {reason}. This device's message \
                     history and MLS state are gone; it rejoins its groups from scratch"
                );
            }

            if should_wipe {
                drop(conn);
                // Through the shredder, sidecars included: a plaintext DB lands
                // here when `sqlcipher_is_linked` declines to answer, and its
                // `-wal` holds message bodies.
                destroy_unusable_database(db_path)?;
                is_fresh = true;
            }
        }

        let conn = Connection::open(db_path)?;
        // Key must be applied before any other SQL.
        conn.execute_batch(&key_pragma)?;
        // On a fresh DB `auto_vacuum` must precede any pages; an existing
        // NONE DB is converted via VACUUM below.
        if is_fresh {
            conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;")?;
        }
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")?;
        if !is_fresh {
            ensure_incremental_auto_vacuum(&conn)?;
        }
        // Before the schema batch, which indexes one of these columns.
        add_missing_columns(&conn)?;
        apply_local_schema(&conn)?;
        // Orphaned outbox attachment bytes (#1335); nothing can be mid-send now.
        crate::commands::messages::sweep_orphan_attachments_on(&conn)?;
        conn.execute(
            "INSERT OR REPLACE INTO kv (key, value) VALUES ('schema_version', ?1)",
            rusqlite::params![LOCAL_SCHEMA_VERSION],
        )?;

        // The SQLite VFS creates the DB and its WAL sidecars at the umask.
        restrict_database_files(db_path);

        Ok(Self { conn })
    }

    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL;")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        apply_local_schema(&conn)?;
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

// Plaintext-database disposal (#992, #1000); see `docs/security-whitepaper.md` §7.

/// The 16 bytes an unencrypted SQLite file begins with; SQLCipher encrypts the
/// header too. Pollis never sets `cipher_plaintext_header_size`, which would
/// make this check destroy good databases.
const SQLITE_PLAINTEXT_HEADER: &[u8; 16] = b"SQLite format 3\0";

/// Sidecars SQLite keeps beside the main file; the `-wal` of a plaintext
/// database holds recent pages in the clear.
const DB_SIDECAR_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];

/// Whether the linked `sqlite3` is SQLCipher, i.e. whether `PRAGMA key` does
/// anything.
///
/// Always true in shipped builds, which
/// [`tests::sqlcipher_is_the_sqlite_we_actually_linked`] enforces. It can be
/// false only in a binary linking a second sqlite3 (the `src-tauri/tests/flows`
/// harness, via the in-process DS's `libsql`). Asked on the caller's
/// connection, never an in-memory one: libsql panics if sqlite3 is already
/// initialised (`.codesight/wiki/testing.md`).
fn sqlcipher_is_linked(conn: &Connection) -> bool {
    match conn.query_row("PRAGMA cipher_version", [], |row| row.get::<_, String>(0)) {
        Ok(v) => !v.trim().is_empty(),
        // Stock SQLite answers with no rows: the "no codec" answer.
        Err(rusqlite::Error::QueryReturnedNoRows) => false,
        // Couldn't ask. Treated as "no codec", since declining to destroy is
        // safe, but logged: the eventual wipe must take the sidecars too.
        Err(e) => {
            crate::diag!(
                "[db] could not ask whether SQLCipher is linked (`PRAGMA cipher_version`: {e}); \
                 assuming it is not, so a plaintext database at this path is left in place \
                 rather than destroyed."
            );
            false
        }
    }
}

/// Destroy `db_path` if, and only if, it is an unencrypted SQLite database.
///
/// Recreates empty rather than migrating: a "new device starts empty" loss,
/// with no window where plaintext and encrypted copies coexist. Leaves anything
/// else as found and propagates read failures, so it never opens a plaintext
/// database anyway. Returns whether it destroyed anything.
///
/// Gated on [`sqlcipher_is_linked`]: without a codec the replacement would be
/// plaintext too, an endless destroy-and-recreate under other clients' open
/// connections (the flows harness's shared multi-device DB).
fn destroy_plaintext_database(db_path: &std::path::Path) -> Result<bool> {
    // One resolution of the path, and every destructive step runs on that
    // handle (#1000), so a symlink planted at the path is never followed.
    let (mut file, meta) = match open_regular_no_follow(db_path)? {
        Target::Missing => return Ok(false),
        Target::NotRegular => {
            // Never written through or unlinked: nothing has shown that its
            // target is a plaintext database.
            crate::diag!(
                "[db] {db_path:?} is not a regular file (a symlink, or something stranger), so \
                 the plaintext-database check is skipped rather than aimed at whatever it \
                 points to."
            );
            return Ok(false);
        }
        Target::Regular(f, m) => (f, m),
    };

    if !starts_with_plaintext_header(&mut file, db_path)? {
        return Ok(false);
    }

    // A property of this binary, not the file, so re-resolving the path here
    // cannot mislead it.
    let probe = Connection::open(db_path)?;
    let encrypts = sqlcipher_is_linked(&probe);
    drop(probe);

    if !encrypts {
        // Only a binary whose second sqlite3 won the symbols gets here.
        crate::diag!(
            "[db] {db_path:?} is an UNENCRYPTED database and this binary has no SQLCipher \
             (`PRAGMA cipher_version` answers nothing), so the file is left alone — replacing \
             it would only produce another plaintext one. Something in this link is shipping a \
             second sqlite3 amalgamation; see db::local::sqlcipher_is_linked."
        );
        return Ok(false);
    }

    dispose(db_path, file, &meta, Zero::Yes)?;
    for suffix in DB_SIDECAR_SUFFIXES {
        dispose_path(&sidecar_path(db_path, suffix), Zero::Yes)?;
    }
    Ok(true)
}

/// Remove a database file and its sidecars because the open path found it
/// unusable (wrong key, no `kv` row, incompatible [`LOCAL_SCHEMA_VERSION`]).
/// Zeroes the bytes first when they are plaintext.
fn destroy_unusable_database(db_path: &std::path::Path) -> Result<()> {
    let zero = match open_regular_no_follow(db_path)? {
        Target::Regular(mut file, meta) => {
            let zero = if starts_with_plaintext_header(&mut file, db_path)? {
                Zero::Yes
            } else {
                Zero::No
            };
            dispose(db_path, file, &meta, zero)?;
            zero
        }
        // The path must stop being a database, so a symlink is unlinked (the
        // link itself, never its target) and never written through.
        Target::NotRegular => {
            crate::diag!(
                "[db] {db_path:?} is not a regular file; removing the entry at that path \
                 (never following it) so a fresh encrypted database can take its place."
            );
            std::fs::remove_file(db_path).map_err(|e| {
                Error::Other(anyhow::anyhow!("remove stale db {db_path:?}: {e}"))
            })?;
            Zero::No
        }
        Target::Missing => Zero::No,
    };

    for suffix in DB_SIDECAR_SUFFIXES {
        dispose_path(&sidecar_path(db_path, suffix), zero)?;
    }
    Ok(())
}

/// Tighten the database file and its WAL sidecars to owner-only; the SQLite VFS
/// creates them, so [`crate::private_fs`] never sees them. Best-effort; a no-op
/// on Windows.
fn restrict_database_files(db_path: &std::path::Path) {
    let mut paths = vec![db_path.to_path_buf()];
    paths.extend(DB_SIDECAR_SUFFIXES.iter().map(|s| sidecar_path(db_path, s)));
    for path in paths {
        if let Err(e) = crate::private_fs::restrict_existing(&path) {
            crate::diag!("[db] could not restrict permissions on {path:?}: {e}");
        }
    }
}

fn sidecar_path(db_path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
    let mut sidecar = db_path.as_os_str().to_owned();
    sidecar.push(suffix);
    std::path::PathBuf::from(sidecar)
}

/// Whether [`dispose`] overwrites a file's bytes before unlinking it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Zero {
    Yes,
    No,
}

/// What a path turned out to be, from exactly one `open`, so every later step
/// acts on the file this call resolved.
enum Target {
    /// Nothing at that path.
    Missing,
    /// Not a regular file (a refused symlink, a directory, a device node).
    /// Never written through.
    NotRegular,
    /// A read/write handle on a regular file, plus metadata read from it.
    Regular(std::fs::File, std::fs::Metadata),
}

/// Open `path` read/write without traversing a symlink at the final component
/// (`O_NOFOLLOW` / `FILE_FLAG_OPEN_REPARSE_POINT`); a handle only for a regular
/// file.
fn open_regular_no_follow(path: &std::path::Path) -> Result<Target> {
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open a reparse point itself rather than its target; the constant is
        // named here to keep `windows-sys` out of this crate.
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        opts.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }

    let file = match opts.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Target::Missing),
        Err(e) => {
            // A refused symlink (ELOOP / ENOTDIR) or a directory is classified,
            // never traversed, and treated as untouchable rather than an error.
            if matches!(std::fs::symlink_metadata(path), Ok(m) if !m.file_type().is_file()) {
                return Ok(Target::NotRegular);
            }
            return Err(Error::Other(anyhow::anyhow!("open local db {path:?}: {e}")));
        }
    };
    let meta = file
        .metadata()
        .map_err(|e| Error::Other(anyhow::anyhow!("stat local db {path:?}: {e}")))?;
    if !meta.file_type().is_file() {
        return Ok(Target::NotRegular);
    }
    Ok(Target::Regular(file, meta))
}

/// Whether the open file starts with the plaintext SQLite magic. Reads through
/// the handle and rewinds to offset 0.
fn starts_with_plaintext_header(
    file: &mut std::fs::File,
    path: &std::path::Path,
) -> Result<bool> {
    use std::io::{Read, Seek, SeekFrom};

    let mut header = [0u8; SQLITE_PLAINTEXT_HEADER.len()];
    let read = match file.read_exact(&mut header) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => false,
        Err(e) => {
            return Err(Error::Other(anyhow::anyhow!("probe local db {path:?}: {e}")));
        }
    };
    file.seek(SeekFrom::Start(0))
        .map_err(|e| Error::Other(anyhow::anyhow!("rewind local db {path:?}: {e}")))?;
    Ok(read && &header == SQLITE_PLAINTEXT_HEADER)
}

/// [`dispose`] for a path not already open (the sidecars). Nothing at the path
/// is not an error.
fn dispose_path(path: &std::path::Path, zero: Zero) -> Result<()> {
    match open_regular_no_follow(path)? {
        Target::Missing => Ok(()),
        Target::NotRegular => {
            crate::diag!("[db] {path:?} is not a regular file; leaving it alone.");
            Ok(())
        }
        Target::Regular(file, meta) => dispose(path, file, &meta, zero),
    }
}

/// Overwrite an open file's bytes (when `zero` says so), truncate it, then
/// unlink the path only while it still names the same file.
///
/// The overwrite is best-effort on CoW filesystems and SSDs. A failed unlink
/// after zeroing is tolerated: SQLite's Windows VFS opens without
/// `FILE_SHARE_DELETE`, and the truncated file opens as a fresh database.
fn dispose(
    path: &std::path::Path,
    mut file: std::fs::File,
    meta: &std::fs::Metadata,
    zero: Zero,
) -> Result<()> {
    use std::io::{Seek, SeekFrom, Write};

    if zero == Zero::Yes {
        let len = meta.len();
        file.seek(SeekFrom::Start(0))
            .map_err(|e| Error::Other(anyhow::anyhow!("rewind {path:?}: {e}")))?;
        let zeros = [0u8; 64 * 1024];
        let mut written = 0u64;
        while written < len {
            let chunk = std::cmp::min(zeros.len() as u64, len - written) as usize;
            file.write_all(&zeros[..chunk])
                .map_err(|e| Error::Other(anyhow::anyhow!("shred {path:?}: {e}")))?;
            written += chunk as u64;
        }
        file.sync_all()
            .map_err(|e| Error::Other(anyhow::anyhow!("sync {path:?}: {e}")))?;
        // Truncate so a failed unlink leaves an empty file (a fresh DB), not a
        // run of zeros SQLite would reject forever.
        file.set_len(0)
            .map_err(|e| Error::Other(anyhow::anyhow!("truncate {path:?}: {e}")))?;
    }
    drop(file);

    match remove_if_unchanged(path, meta) {
        Ok(()) => Ok(()),
        Err(e) if zero == Zero::Yes => {
            crate::diag!(
                "[db] {path:?} was overwritten and emptied but could not be unlinked ({e}); \
                 continuing — the bytes are gone and the empty file opens as a fresh database."
            );
            Ok(())
        }
        Err(e) => Err(Error::Other(anyhow::anyhow!("remove {path:?}: {e}"))),
    }
}

/// Unlink `path` unless a different file (or a symlink) has replaced it since
/// the handle was opened: same device and inode on unix, still a regular file
/// elsewhere.
fn remove_if_unchanged(path: &std::path::Path, meta: &std::fs::Metadata) -> std::io::Result<()> {
    let now = match std::fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let same = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            now.file_type().is_file() && now.dev() == meta.dev() && now.ino() == meta.ino()
        }
        #[cfg(not(unix))]
        {
            let _ = meta;
            now.file_type().is_file()
        }
    };
    if !same {
        return Err(std::io::Error::other(
            "the path no longer names the file that was opened; refusing to unlink it",
        ));
    }
    std::fs::remove_file(path)
}

/// `ui_state` key holding the device-local retention window in days (text
/// integer). Eviction keeps the local file bounded and never touches remote.
const RETENTION_KEY: &str = "message_retention_days";

/// Retention windows offered to the user, in days; `0` means Forever.
pub const ALLOWED_RETENTION_DAYS: [i64; 4] = [0, 30, 90, 365];

/// Columns added to existing tables since a database was first created, so
/// they arrive without a [`LOCAL_SCHEMA_VERSION`] bump (which deletes the DB).
/// Each must be nullable or carry a DEFAULT.
const ADDITIVE_COLUMNS: &[(&str, &str, &str)] = &[
    ("message", "thread_id", "ALTER TABLE message ADD COLUMN thread_id TEXT"),
    // Must stay identical to the column in `local_schema.sql`.
    (
        "message",
        "undecryptable",
        "ALTER TABLE message ADD COLUMN undecryptable TEXT CHECK (
            undecryptable IS NULL
            OR (undecryptable IN ('stranded', 'failed', 'lineage') AND content IS NULL)
        )",
    ),
    // The durable outbox (#1335). Must stay identical to the column in
    // `local_schema.sql`.
    (
        "message",
        "send_state",
        "ALTER TABLE message ADD COLUMN send_state TEXT CHECK (
            send_state IS NULL
            OR (send_state IN ('pending', 'failed', 'rejected') AND undecryptable IS NULL AND content IS NOT NULL)
        )",
    ),
];

/// Apply [`ADDITIVE_COLUMNS`] on every open. "duplicate column name" (already
/// there) and "no such table" (fresh DB; the schema batch creates it) are
/// expected; anything else is surfaced.
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

/// Convert an `auto_vacuum=NONE` database to `INCREMENTAL` in place (needs a
/// VACUUM once tables exist). A no-op otherwise.
fn ensure_incremental_auto_vacuum(conn: &Connection) -> Result<()> {
    // 0 = NONE, 1 = FULL, 2 = INCREMENTAL.
    let mode: i64 = conn.query_row("PRAGMA auto_vacuum;", [], |row| row.get(0))?;
    if mode == 0 {
        conn.execute_batch("PRAGMA auto_vacuum=INCREMENTAL; VACUUM;")?;
    }
    Ok(())
}

/// Reclaim free pages and truncate the WAL so the file actually shrinks.
pub fn reclaim(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA incremental_vacuum;")?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}

/// Read the device-local retention window in days; absent means `0` (Forever).
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

/// Set the device-local retention window (one of [`ALLOWED_RETENTION_DAYS`])
/// and sweep immediately.
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

/// Delete local messages older than the retention window and reclaim the pages.
/// Returns the rows deleted. Only `message` is touched, never `mls_kv`.
pub fn evict_old_messages(conn: &Connection) -> Result<usize> {
    let days = get_message_retention_days(conn)?;
    if days <= 0 {
        return Ok(0);
    }
    // `received_at` uses datetime('now') format, so it compares directly.
    let modifier = format!("-{days} days");
    // An unsent message (#1335) is the only copy of what the user wrote.
    let deleted = conn.execute(
        "DELETE FROM message WHERE received_at < datetime('now', ?1) AND send_state IS NULL",
        rusqlite::params![modifier],
    )?;
    if deleted > 0 {
        crate::diag!("[db] retention ({days} days) removed {deleted} message(s) from this device");
    }
    reclaim(conn)?;
    Ok(deleted)
}

/// `kv` flag recording that the one-time FTS backfill (#850) has finished, so
/// it is not re-scanned on every open.
const SEARCH_BACKFILL_KEY: &str = "search_index_backfilled";

/// Rows indexed per backfill step, so the local-DB mutex is never held long.
const BACKFILL_CHUNK: i64 = 2_000;

/// Whether the backfill still has work to do.
pub fn search_backfill_pending(conn: &Connection) -> Result<bool> {
    let done: Option<String> = conn
        .query_row(
            "SELECT value FROM kv WHERE key = ?1",
            rusqlite::params![SEARCH_BACKFILL_KEY],
            |row| row.get(0),
        )
        .optional()?;
    Ok(done.as_deref() != Some("1"))
}

/// Index one chunk of messages the FTS table does not hold yet. Returns the
/// rows indexed; `0` means done (run [`finish_search_backfill`]).
///
/// Resumable: the work set comes from FTS5's `message_fts_docsize` shadow
/// table, not a stored cursor.
pub fn backfill_search_index_chunk(conn: &Connection) -> Result<usize> {
    let indexed = conn.execute(
        "INSERT INTO message_fts (rowid, body)
         SELECT rowid, pollis_search_text(content)
         FROM message
         WHERE content IS NOT NULL
           AND deleted_at IS NULL
           AND rowid NOT IN (SELECT rowid FROM message_fts_docsize)
         LIMIT ?1",
        rusqlite::params![BACKFILL_CHUNK],
    )?;
    Ok(indexed)
}

/// Merge the b-tree the backfill just built and record that it is done.
pub fn finish_search_backfill(conn: &Connection) -> Result<()> {
    conn.execute_batch("INSERT INTO message_fts(message_fts) VALUES('optimize');")?;
    conn.execute(
        "INSERT OR REPLACE INTO kv (key, value) VALUES (?1, '1')",
        rusqlite::params![SEARCH_BACKFILL_KEY],
    )?;
    Ok(())
}

/// Is the index internally consistent? `integrity-check` cannot see message
/// text (none is stored), so tests pair it with a row count.
pub fn search_index_is_healthy(conn: &Connection) -> bool {
    conn.execute_batch("INSERT INTO message_fts(message_fts) VALUES('integrity-check');")
        .is_ok()
}

/// Throw the index away and rebuild it from `message`: the Settings button and
/// the open-path auto-repair. Touches nothing but the index.
pub fn rebuild_search_index(conn: &Connection) -> Result<()> {
    conn.execute_batch("INSERT INTO message_fts(message_fts) VALUES('delete-all');")?;
    conn.execute(
        "DELETE FROM kv WHERE key = ?1",
        rusqlite::params![SEARCH_BACKFILL_KEY],
    )?;
    while backfill_search_index_chunk(conn)? > 0 {}
    finish_search_backfill(conn)?;
    Ok(())
}

/// In-process data-dir override; `None` (production) resolves from
/// `POLLIS_DATA_DIR` / the platform default. A lock, because test rigs that
/// swapped dirs with `set_var` raced other threads' `getenv` (#923).
static DATA_DIR: std::sync::RwLock<Option<std::path::PathBuf>> = std::sync::RwLock::new(None);

/// Point every data-dir-derived path at `dir` for the rest of this process,
/// overriding `POLLIS_DATA_DIR`. For test rigs.
pub fn set_data_dir(dir: impl Into<std::path::PathBuf>) {
    *DATA_DIR.write().expect("data-dir lock") = Some(dir.into());
}

/// The explicitly chosen data dir (the override, else `POLLIS_DATA_DIR`), or
/// `None` for a shipped desktop build. Keystore namespacing keys off this.
pub(crate) fn explicit_data_dir() -> Option<std::path::PathBuf> {
    if let Some(dir) = DATA_DIR.read().expect("data-dir lock").clone() {
        return Some(dir);
    }
    // POLLIS_DATA_DIR: a second dev instance, or the mobile bridge's sandbox.
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
    // iOS: Application Support, never the purgeable tmp/ (#1306).
    #[cfg(target_os = "ios")]
    {
        crate::ios_storage::data_dir()
    }
    // Android: `init_pollis` exports POLLIS_DATA_DIR; this is pre-init only.
    #[cfg(target_os = "android")]
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

    /// At-rest encryption (`docs/security-whitepaper.md` §7) assumes the linked
    /// `sqlite3_*` symbols are SQLCipher's. If another sqlite3 wins them,
    /// `PRAGMA key` is silently ignored and the file is plaintext. Only
    /// SQLCipher knows `PRAGMA cipher_version`, and since #992 the answer must
    /// be the same on every platform, with no `cfg` exception.
    /// [`the_local_database_file_is_encrypted_at_rest`] checks the bytes.
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

        assert!(
            linked,
            "`PRAGMA cipher_version` answered nothing ({version:?}): the linked sqlite3 is NOT \
             SQLCipher. `PRAGMA key` is being silently ignored and pollis_<user>.db is PLAINTEXT \
             on this platform. Check what else in the dependency graph ships an sqlite3 \
             amalgamation and is winning the `sqlite3_*` symbols."
        );
    }

    /// `docs/security-whitepaper.md` §7.0's per-platform crypto provider is a
    /// build outcome: on macOS an `OPENSSL_DIR` on the build machine silently
    /// swaps CommonCrypto for OpenSSL. `PRAGMA cipher_provider` (which needs a
    /// keyed connection) asks the build itself.
    #[test]
    fn the_crypto_provider_is_the_one_this_platform_documents() {
        let expected = if cfg!(target_os = "windows") {
            // `pollis-sqlcipher` reports upstream's provider name;
            // `cipher_provider_version` is pinned by that crate's own suite.
            "libtomcrypt"
        } else if cfg!(target_os = "macos") {
            "commoncrypto"
        } else {
            // Linux links the system OpenSSL; iOS/Android vendor a static one.
            "openssl"
        };

        let conn = Connection::open_in_memory().expect("open");
        conn.execute_batch(&format!("PRAGMA key = \"x'{}'\"", hex::encode(TEST_KEY)))
            .expect("apply key");
        let provider: String = conn
            .query_row("PRAGMA cipher_provider", [], |row| row.get(0))
            .expect("query cipher_provider");
        let provider = provider.trim();

        assert_eq!(
            provider, expected,
            "SQLCipher was compiled against the `{provider}` crypto provider, but \
             docs/security-whitepaper.md §7.0 documents `{expected}` for this platform. Either \
             the build environment changed which provider libsqlite3-sys selected (an OPENSSL_* \
             variable on the build machine does exactly this on macOS), or §7.0 is now wrong."
        );
    }

    /// `sqlcipher_is_linked` must never be what switches encryption off in
    /// the shipping configuration, where `pollis-core` is the only sqlite3.
    #[test]
    fn the_plaintext_gate_is_open_in_this_build() {
        let conn = Connection::open_in_memory().expect("open");
        assert!(
            sqlcipher_is_linked(&conn),
            "`sqlcipher_is_linked()` is false in pollis-core's own test binary, so \
             `destroy_plaintext_database` would decline to act. Nothing here links a second \
             sqlite3; if that changed, the local database is no longer encrypted either."
        );
    }

    /// These go through [`LocalDb::open_for_user`], the path the app uses, so a
    /// test connection cannot be keyed while the real one is bare.
    /// `set_data_dir` is process-global: one scratch dir, separated by user id.
    fn scratch_data_dir() -> &'static std::path::Path {
        static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
        let dir = DIR.get_or_init(|| {
            let dir = tempfile::Builder::new().prefix("pollis-local-db").tempdir().expect("tempdir");
            set_data_dir(dir.path());
            dir
        });
        dir.path()
    }

    fn db_file(user_id: &str) -> std::path::PathBuf {
        scratch_data_dir().join(format!("pollis_{user_id}.db"))
    }

    /// Every byte the database and its sidecars have on disk right now.
    fn on_disk_bytes(user_id: &str) -> Vec<u8> {
        let base = db_file(user_id);
        let mut bytes = std::fs::read(&base).expect("read database file");
        for suffix in DB_SIDECAR_SUFFIXES {
            let mut sidecar = base.as_os_str().to_owned();
            sidecar.push(suffix);
            if let Ok(extra) = std::fs::read(std::path::Path::new(&sidecar)) {
                bytes.extend_from_slice(&extra);
            }
        }
        bytes
    }

    const TEST_KEY: &[u8; 32] = b"pollis-local-db-test-key-32bytes";
    const WRONG_KEY: &[u8; 32] = b"pollis-local-db-test-key-32byteS";

    /// Checked against the bytes: written through the ordinary path, a message
    /// must not be readable in the file (it was on Windows before #992).
    #[test]
    fn the_local_database_file_is_encrypted_at_rest() {
        const USER: &str = "encrypted-at-rest";
        const MARKER: &str = "TOP-SECRET-PLAINTEXT-MARKER-4f2a91";

        {
            let db = LocalDb::open_for_user(USER, TEST_KEY).expect("open");
            db.conn()
                .execute(
                    "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at)
                     VALUES ('m1', 'c1', 'u1', X'00', ?1, '2024-01-01T00:00:00Z')",
                    rusqlite::params![MARKER],
                )
                .expect("insert");
            // Push the WAL into the main file so "on disk" means what it says.
            reclaim(db.conn()).expect("checkpoint");
        }

        let bytes = on_disk_bytes(USER);
        assert!(!bytes.is_empty(), "the database file is empty");

        assert!(
            !bytes.windows(MARKER.len()).any(|w| w == MARKER.as_bytes()),
            "the message body appears verbatim in {} bytes of pollis_{USER}.db — the local \
             database is NOT encrypted at rest",
            bytes.len()
        );

        let header = std::fs::read(db_file(USER)).expect("read database file");
        assert_ne!(
            &header[..16],
            SQLITE_PLAINTEXT_HEADER,
            "pollis_{USER}.db begins with the plaintext SQLite magic — `PRAGMA key` did nothing"
        );

        // The key has to be what unlocks it.
        let unkeyed = Connection::open(db_file(USER)).expect("open unkeyed");
        assert!(
            unkeyed.query_row("SELECT count(*) FROM message", [], |r| r.get::<_, i64>(0)).is_err(),
            "an unkeyed connection read the message table"
        );
        drop(unkeyed);

        let wrong = Connection::open(db_file(USER)).expect("open");
        wrong
            .execute_batch(&format!("PRAGMA key = \"x'{}'\"", hex::encode(WRONG_KEY)))
            .expect("apply key");
        assert!(
            wrong.query_row("SELECT count(*) FROM message", [], |r| r.get::<_, i64>(0)).is_err(),
            "a connection with the wrong key read the message table"
        );
        drop(wrong);

        // The right key still works: encryption, not corruption.
        let reopened = LocalDb::open_for_user(USER, TEST_KEY).expect("reopen");
        let stored: String = reopened
            .conn()
            .query_row("SELECT content FROM message WHERE id = 'm1'", [], |row| row.get(0))
            .expect("message survived the round trip");
        assert_eq!(stored, MARKER);
    }

    /// #992: a database left by a build that wrote plaintext is destroyed on
    /// the next open, not opened.
    #[test]
    fn a_plaintext_database_is_destroyed_not_opened() {
        const USER: &str = "plaintext-legacy";
        const MARKER: &str = "LEGACY-PLAINTEXT-MARKER-8b3c07";

        std::fs::create_dir_all(scratch_data_dir()).expect("data dir");
        let path = db_file(USER);

        // What a pre-#992 Windows client produced: nothing keys the connection.
        {
            let plain = Connection::open(&path).expect("create plaintext db");
            plain.execute_batch("CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT)").unwrap();
            plain
                .execute("INSERT INTO kv (key, value) VALUES ('secret', ?1)", rusqlite::params![
                    MARKER
                ])
                .unwrap();
        }
        assert!(is_plaintext_sqlite(&path).unwrap(), "the fixture is not a plaintext database");

        let db = LocalDb::open_for_user(USER, TEST_KEY).expect("open over a plaintext database");
        db.conn()
            .execute(
                "INSERT INTO message (id, conversation_id, sender_id, ciphertext, sent_at)
                 VALUES ('m1', 'c1', 'u1', X'00', '2024-01-01T00:00:00Z')",
                [],
            )
            .expect("the recreated database is usable");
        reclaim(db.conn()).expect("checkpoint");
        drop(db);

        assert!(!is_plaintext_sqlite(&path).unwrap(), "the database is still plaintext");
        let bytes = on_disk_bytes(USER);
        assert!(
            !bytes.windows(MARKER.len()).any(|w| w == MARKER.as_bytes()),
            "the old plaintext row is still readable on disk"
        );

        // Idempotent: the second open leaves the encrypted database alone.
        let again = LocalDb::open_for_user(USER, TEST_KEY).expect("reopen");
        let count: i64 =
            again.conn().query_row("SELECT count(*) FROM message", [], |r| r.get(0)).unwrap();
        assert_eq!(count, 1, "the second open wiped a perfectly good encrypted database");
    }

    /// The disposal helper on its own, over the cases `open_at` hands it.
    #[test]
    fn destroying_a_plaintext_database_is_narrow_and_idempotent() {
        let dir = tempfile::Builder::new().prefix("plaintext-probe").tempdir().unwrap();

        // Absent: nothing to do, and not an error.
        let missing = dir.path().join("absent.db");
        destroy_plaintext_database(&missing).expect("absent file");
        assert!(!missing.exists());

        // Too short to carry the header: left alone.
        let stub = dir.path().join("stub.db");
        std::fs::write(&stub, b"SQLite").unwrap();
        destroy_plaintext_database(&stub).expect("short file");
        assert!(stub.exists(), "a file too short to be a database was deleted");

        // Not a database at all: left alone.
        let other = dir.path().join("notes.txt");
        std::fs::write(&other, b"this is not a database, it is a note about one").unwrap();
        destroy_plaintext_database(&other).expect("non-database");
        assert!(other.exists(), "an unrelated file was deleted");

        // Plaintext, with a sidecar: both go, and running it twice is fine.
        let plain = dir.path().join("plain.db");
        std::fs::write(&plain, {
            let mut bytes = SQLITE_PLAINTEXT_HEADER.to_vec();
            bytes.extend_from_slice(b"pages full of message bodies");
            bytes
        })
        .unwrap();
        let wal = dir.path().join("plain.db-wal");
        std::fs::write(&wal, b"recently written pages, also in the clear").unwrap();
        destroy_plaintext_database(&plain).expect("plaintext database");
        assert!(!plain.exists(), "the plaintext database survived");
        assert!(!wal.exists(), "the plaintext WAL survived");
        destroy_plaintext_database(&plain).expect("second run must be a no-op");
    }

    /// #825: an additive column must not cost the user their history (a
    /// `LOCAL_SCHEMA_VERSION` bump would delete the file and fail this).
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

        add_missing_columns(&conn).expect("additive columns");
        apply_local_schema(&conn).expect("schema batch");

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

        // #1335: the outbox column arrived the same way; old rows read as delivered.
        let send_state: Option<String> = conn
            .query_row("SELECT send_state FROM message WHERE id = 'old1'", [], |row| row.get(0))
            .expect("send_state is readable on an upgraded DB");
        assert_eq!(send_state, None, "pre-outbox history is delivered history");
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at, send_state)
             VALUES ('new2', 'conv1', 'user1', X'', 'queued', '2024-01-03T00:00:00Z', 'pending')",
            [],
        )
        .expect("send_state is writable on an upgraded DB");
    }

    /// #1335: an unsent row is always a real message of ours with content.
    #[test]
    fn send_state_admits_only_unsent_real_messages() {
        let db = db();
        let conn = db.conn();
        let insert = |id: &str, content: Option<&str>, undecryptable: Option<&str>, state: Option<&str>| {
            conn.execute(
                "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at, undecryptable, send_state)
                 VALUES (?1, 'c1', 'u1', X'', ?2, '2024-01-01T00:00:00Z', ?3, ?4)",
                rusqlite::params![id, content, undecryptable, state],
            )
        };
        insert("ok-pending", Some("hi"), None, Some("pending")).expect("pending is admitted");
        insert("ok-failed", Some("hi"), None, Some("failed")).expect("failed is admitted");
        insert("ok-sent", Some("hi"), None, None).expect("delivered is admitted");
        assert!(insert("bad-state", Some("hi"), None, Some("sending")).is_err(), "unknown state");
        assert!(insert("bad-empty", None, None, Some("pending")).is_err(), "no content to send");
        assert!(
            insert("bad-placeholder", None, Some("failed"), Some("pending")).is_err(),
            "a placeholder is never ours to send"
        );
    }

    /// #1335: retention never evicts a message that is still waiting to send.
    #[test]
    fn eviction_spares_unsent_messages() {
        let db = db();
        let conn = db.conn();
        for (id, state) in [("old-sent", None), ("old-unsent", Some("failed"))] {
            conn.execute(
                "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at, received_at, send_state)
                 VALUES (?1, 'c1', 'u1', X'', 'x', '2020-01-01T00:00:00Z', '2020-01-01 00:00:00', ?2)",
                rusqlite::params![id, state],
            )
            .unwrap();
        }
        set_message_retention_days(conn, 30).expect("set retention");
        let left: Vec<String> = conn
            .prepare("SELECT id FROM message ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert_eq!(left, vec!["old-unsent".to_string()]);
    }

    /// On a fresh database the `message` table does not exist yet when additive
    /// columns run; that must not be an error.
    #[test]
    fn additive_columns_tolerate_a_fresh_database() {
        let conn = Connection::open_in_memory().expect("open");
        add_missing_columns(&conn).expect("no such table must be tolerated");
        apply_local_schema(&conn).expect("schema batch");
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

    /// Insert a message with `received_at_sql` spliced in as SQL (e.g.
    /// `datetime('now','-100 days')`).
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
        for days in ALLOWED_RETENTION_DAYS {
            set_message_retention_days(conn, days).unwrap();
        }
    }

    /// Rows the index holds, from FTS5's per-document shadow table (a
    /// contentless table cannot be scanned for values).
    fn fts_row_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT count(*) FROM message_fts_docsize", [], |r| r.get(0))
            .expect("count message_fts_docsize")
    }

    /// Rows that SHOULD be indexed, by the definition the triggers encode.
    fn indexable_row_count(conn: &Connection) -> i64 {
        conn.query_row(
            "SELECT count(*) FROM message WHERE content IS NOT NULL AND deleted_at IS NULL",
            [],
            |r| r.get(0),
        )
        .expect("count indexable messages")
    }

    fn assert_index_matches(conn: &Connection, after: &str) {
        conn.execute_batch("INSERT INTO message_fts(message_fts) VALUES('integrity-check');")
            .unwrap_or_else(|e| panic!("integrity-check failed after {after}: {e}"));
        assert_eq!(
            fts_row_count(conn),
            indexable_row_count(conn),
            "index drifted after {after}"
        );
    }

    fn insert_indexable(conn: &Connection, id: &str, content: &str) {
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at, received_at)
             VALUES (?1, 'conv-fts', 'sender-1', X'00', ?2, '2026-01-01T00:00:00Z', datetime('now'))",
            rusqlite::params![id, content],
        )
        .expect("insert message");
    }

    /// The index cannot drift: after every shape of write `message` admits
    /// (insert, edit, soft delete, moderator delete, retention eviction), FTS5
    /// is consistent and holds exactly the rows it should. Triggers maintain it,
    /// so a future write path cannot skip it.
    #[test]
    fn the_search_index_cannot_drift() {
        let db = db();
        let conn = db.conn();
        assert_index_matches(conn, "an empty database");

        insert_indexable(conn, "m-1", "the quarterly budget review");
        insert_indexable(conn, "m-2", "lunch plans");
        insert_indexable(
            conn,
            "m-3",
            r#"{"_att":[{"key":"media/abc","url":"https://r2/abc","name":"deck.pdf"}],"_txt":"slides"}"#,
        );
        assert_index_matches(conn, "insert");

        // A row inserted already deleted never enters the index.
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at, received_at, deleted_at)
             VALUES ('m-dead', 'conv-fts', 'sender-1', X'00', 'gone', '2026-01-01T00:00:00Z', datetime('now'), '2026-01-02T00:00:00Z')",
            [],
        )
        .expect("insert pre-deleted message");
        assert_index_matches(conn, "insert of an already-deleted message");

        conn.execute(
            "UPDATE message SET content = ?1, edited_at = ?2 WHERE id = ?3 AND deleted_at IS NULL",
            rusqlite::params!["the annual budget review", "2026-01-03T00:00:00Z", "m-1"],
        )
        .expect("edit");
        assert_index_matches(conn, "edit");
        // The old term must be gone, not merely outnumbered.
        let stale: i64 = conn
            .query_row(
                "SELECT count(*) FROM message_fts WHERE message_fts MATCH 'quarterly'",
                [],
                |r| r.get(0),
            )
            .expect("match query");
        assert_eq!(stale, 0, "an edited-away term is still indexed");

        conn.execute(
            "UPDATE message SET content = NULL, deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
            rusqlite::params!["2026-01-04T00:00:00Z", "m-2"],
        )
        .expect("soft delete");
        assert_index_matches(conn, "soft delete");

        // Moderator delete, including a replayed redaction's double delete.
        conn.execute(
            "UPDATE message SET content = NULL, deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
            rusqlite::params!["2026-01-05T00:00:00Z", "m-3"],
        )
        .expect("moderator delete");
        conn.execute(
            "UPDATE message SET content = NULL, deleted_at = ?1 WHERE id = ?2 AND deleted_at IS NULL",
            rusqlite::params!["2026-01-06T00:00:00Z", "m-3"],
        )
        .expect("replayed moderator delete");
        assert_index_matches(conn, "moderator delete");

        // Retention eviction: the only hard DELETE, over indexed and unindexed rows.
        insert_indexable(conn, "m-old", "ancient history worth finding");
        conn.execute(
            "UPDATE message SET received_at = datetime('now', '-400 days')",
            [],
        )
        .expect("age every row");
        assert_index_matches(conn, "ageing rows");
        set_message_retention_days(conn, 30).expect("set retention");
        assert_eq!(indexable_row_count(conn), 0, "eviction should have emptied the table");
        assert_index_matches(conn, "retention eviction");
    }

    /// The backfill converges on the same invariant. Rows are inserted with the
    /// triggers dropped, the state of every pre-#850 database.
    #[test]
    fn the_backfill_reaches_the_same_invariant() {
        let db = db();
        let conn = db.conn();
        conn.execute_batch(
            "DROP TRIGGER message_fts_ai; DROP TRIGGER message_fts_au; DROP TRIGGER message_fts_ad;",
        )
        .expect("drop triggers");

        for i in 0..50 {
            insert_indexable(conn, &format!("pre-{i}"), &format!("historic message {i}"));
        }
        assert_eq!(fts_row_count(conn), 0, "premise: nothing indexed yet");
        assert!(search_backfill_pending(conn).expect("pending"));

        conn.execute_batch(super::schema_sql())
            .expect("re-apply schema, restoring the triggers");
        while backfill_search_index_chunk(conn).expect("backfill chunk") > 0 {}
        finish_search_backfill(conn).expect("finish");

        assert!(!search_backfill_pending(conn).expect("pending"));
        assert_index_matches(conn, "backfill");
        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM message_fts WHERE message_fts MATCH 'historic'",
                [],
                |r| r.get(0),
            )
            .expect("match");
        assert_eq!(hits, 50);
    }

    /// Before the backfill reaches a row, a contentless 'delete' for it is a
    /// hard SQLITE_CORRUPT, which the docsize guard in the delete triggers
    /// prevents. The first post-upgrade open's retention sweep hits this
    /// deterministically; edits and redactions can race it.
    #[test]
    fn pre_backfill_writes_do_not_corrupt_the_index() {
        let db = db();
        let conn = db.conn();
        conn.execute_batch(
            "DROP TRIGGER message_fts_ai; DROP TRIGGER message_fts_au; DROP TRIGGER message_fts_ad;",
        )
        .expect("drop triggers");
        for i in 0..10 {
            insert_indexable(conn, &format!("pre-{i}"), &format!("historic message {i}"));
        }
        conn.execute_batch(super::schema_sql())
            .expect("re-apply schema, restoring the triggers");
        assert_eq!(fts_row_count(conn), 0, "premise: index empty, rows unindexed");

        // Retention eviction against a fully unindexed table.
        conn.execute(
            "UPDATE message SET received_at = datetime('now', '-400 days') WHERE id = 'pre-0'",
            [],
        )
        .expect("age one row");
        set_message_retention_days(conn, 30).expect("eviction sweep over unindexed rows");
        // Only consistency holds until the backfill finishes.
        conn.execute_batch("INSERT INTO message_fts(message_fts) VALUES('integrity-check');")
            .expect("index consistent after pre-backfill eviction");

        conn.execute(
            "UPDATE message SET content = 'edited before backfill', edited_at = '2026-01-03T00:00:00Z'
             WHERE id = 'pre-1' AND deleted_at IS NULL",
            [],
        )
        .expect("pre-backfill edit");

        for _ in 0..2 {
            conn.execute(
                "UPDATE message SET content = NULL, deleted_at = '2026-01-04T00:00:00Z'
                 WHERE id = 'pre-2' AND deleted_at IS NULL",
                [],
            )
            .expect("pre-backfill soft delete");
        }
        conn.execute("DELETE FROM message WHERE id = 'pre-3'", [])
            .expect("pre-backfill hard delete");
        conn.execute_batch("INSERT INTO message_fts(message_fts) VALUES('integrity-check');")
            .expect("index consistent after pre-backfill writes");

        while backfill_search_index_chunk(conn).expect("backfill chunk") > 0 {}
        finish_search_backfill(conn).expect("finish");
        assert_index_matches(conn, "backfill after pre-backfill writes");
        let edited: i64 = conn
            .query_row(
                "SELECT count(*) FROM message_fts WHERE message_fts MATCH 'edited'",
                [],
                |r| r.get(0),
            )
            .expect("match");
        assert_eq!(edited, 1, "the pre-backfill edit must be indexed exactly once");
    }

    /// Rebuilding is safe at any time, including on a healthy index.
    #[test]
    fn rebuilding_the_index_is_idempotent() {
        let db = db();
        let conn = db.conn();
        insert_indexable(conn, "m-1", "findable");
        assert!(search_index_is_healthy(conn));

        super::rebuild_search_index(conn).expect("rebuild");
        assert_index_matches(conn, "rebuild");

        super::rebuild_search_index(conn).expect("second rebuild");
        assert_index_matches(conn, "a second rebuild");

        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM message_fts WHERE message_fts MATCH 'findable'",
                [],
                |r| r.get(0),
            )
            .expect("match");
        assert_eq!(hits, 1);
    }

    /// End to end: proves the scalar function is registered on the connection
    /// the triggers run on.
    #[test]
    fn the_indexed_body_is_the_transformed_one() {
        let db = db();
        let conn = db.conn();
        insert_indexable(
            conn,
            "m-att",
            r#"{"_att":[{"key":"media/9f3c1a2b","url":"https://r2/9f3c1a2b","name":"Q3-budget.xlsx","hash":"deadbeef"}],"_txt":"here you go"}"#,
        );

        let matches = |expr: &str| -> i64 {
            conn.query_row(
                "SELECT count(*) FROM message_fts WHERE message_fts MATCH ?1",
                rusqlite::params![expr],
                |r| r.get(0),
            )
            .expect("match query")
        };
        assert_eq!(matches("\"Q3-budget.xlsx\""), 1, "filename should be findable");
        assert_eq!(matches("\"9f3c1a2b\""), 0, "R2 key must not be findable");
        assert_eq!(matches("\"deadbeef\""), 0, "content hash must not be findable");
        assert_eq!(matches("\"zzpollisatt\""), 1, "has:attachment sentinel");
    }

    #[test]
    fn auto_vacuum_in_place_upgrade() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA auto_vacuum=NONE;").unwrap();
        apply_local_schema(&conn).unwrap();
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

    /// Path-based mirror of [`starts_with_plaintext_header`], for assertions only.
    fn is_plaintext_sqlite(path: &std::path::Path) -> Result<bool> {
        match open_regular_no_follow(path)? {
            Target::Regular(mut f, _) => starts_with_plaintext_header(&mut f, path),
            _ => Ok(false),
        }
    }

    /// A symlink planted at the database path must not aim the shredder at its
    /// target (#1000).
    #[test]
    #[cfg(unix)]
    fn a_symlink_at_the_database_path_never_reaches_its_target() {
        const USER: &str = "symlink-victim";
        let dir = scratch_data_dir();
        let victim = dir.join("someone-elses-notes.db");

        // A plaintext SQLite file: only the symlink check protects it.
        let mut bytes = SQLITE_PLAINTEXT_HEADER.to_vec();
        bytes.extend_from_slice(b"pages the user cares about");
        std::fs::write(&victim, &bytes).expect("write victim");

        let link = db_file(USER);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&victim, &link).expect("plant symlink");

        // The narrow helper first: it must decline, not act.
        assert!(
            !destroy_plaintext_database(&link).expect("disposal must not fail"),
            "the disposal path acted on a symlink"
        );
        assert_eq!(
            std::fs::read(&victim).expect("victim still readable"),
            bytes,
            "the shredder followed a symlink and destroyed its target"
        );

        // The open path may remove the link, but the target must come through
        // byte-identical.
        let db = LocalDb::open_for_user(USER, TEST_KEY).expect("open over a planted symlink");
        drop(db);
        assert_eq!(
            std::fs::read(&victim).expect("victim still readable"),
            bytes,
            "opening the local database destroyed the symlink's target"
        );
        assert!(
            !std::fs::symlink_metadata(&link).expect("db path exists").is_symlink(),
            "the database path is still a symlink, so the next open writes through it"
        );
    }

    /// A directory (or anything else that is not a regular file) at the
    /// database path is left alone by the narrow disposal helper too.
    #[test]
    fn a_directory_at_the_database_path_is_left_alone() {
        let dir = scratch_data_dir().join("not-a-file.db");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create directory");
        assert!(!destroy_plaintext_database(&dir).expect("must not fail"));
        assert!(dir.is_dir(), "a directory at the database path was removed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An unlink failing after the overwrite (Windows without
    /// `FILE_SHARE_DELETE`) is tolerated; a read-only parent dir reproduces it
    /// on unix.
    #[test]
    #[cfg(unix)]
    fn an_unlink_that_fails_after_the_shred_is_tolerated() {
        use std::os::unix::fs::PermissionsExt;

        let dir = scratch_data_dir().join("readonly-parent");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create dir");
        let path = dir.join("pollis_locked.db");
        let mut bytes = SQLITE_PLAINTEXT_HEADER.to_vec();
        bytes.extend_from_slice(b"message bodies, in the clear");
        std::fs::write(&path, &bytes).expect("write plaintext db");

        // Deny writes to the directory (unlink) but not the file (overwrite).
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500))
            .expect("chmod dir");
        // Root ignores the mode, so there is nothing to provoke.
        let root_ignores_the_mode = std::fs::write(dir.join("probe"), b"x").is_ok();
        if root_ignores_the_mode {
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }

        let destroyed = destroy_plaintext_database(&path)
            .expect("a failed unlink after a successful shred must not be an error");
        assert!(destroyed, "the caller must be told the database is gone");
        assert_eq!(
            std::fs::metadata(&path).expect("file still there").len(),
            0,
            "the plaintext must be gone even when the file could not be unlinked"
        );

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("restore");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A wipe on `NotADatabase` (where a plaintext DB lands when
    /// `sqlcipher_is_linked` declines) must take the sidecars too.
    #[test]
    fn wiping_an_unusable_database_takes_the_sidecars_with_it() {
        const USER: &str = "unusable-with-sidecars";
        let path = db_file(USER);
        let _ = std::fs::remove_file(&path);

        // A file SQLCipher cannot open under our key: `NotADatabase`.
        std::fs::write(&path, b"not a database under any key at all").expect("write");
        let wal = std::path::PathBuf::from({
            let mut s = path.as_os_str().to_owned();
            s.push("-wal");
            s
        });
        const MARKER: &str = "WAL-PLAINTEXT-MARKER-6c1d40";
        std::fs::write(&wal, MARKER).expect("write wal");

        let db = LocalDb::open_for_user(USER, TEST_KEY).expect("open replaces the unusable file");
        drop(db);

        let leftover = std::fs::read(&wal).unwrap_or_default();
        assert!(
            !leftover.windows(MARKER.len()).any(|w| w == MARKER.as_bytes()),
            "the stale plaintext WAL survived the wipe"
        );
    }

    /// The VFS creates these files, so `open_at` is the only place that can
    /// tighten them.
    #[test]
    #[cfg(unix)]
    fn the_database_and_its_sidecars_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        const USER: &str = "owner-only-db";
        let db = LocalDb::open_for_user(USER, TEST_KEY).expect("open");
        db.conn()
            .execute(
                "INSERT INTO message (id, conversation_id, sender_id, ciphertext, sent_at)
                 VALUES ('m1', 'c1', 'u1', X'00', '2024-01-01T00:00:00Z')",
                [],
            )
            .expect("insert so the WAL exists");

        let base = db_file(USER);
        let mut checked = 0;
        let mut paths = vec![base.clone()];
        paths.extend(DB_SIDECAR_SUFFIXES.iter().map(|s| sidecar_path(&base, s)));
        for path in paths {
            let Ok(meta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            assert_eq!(
                meta.permissions().mode() & 0o777,
                0o600,
                "{path:?} is readable by other local users"
            );
            checked += 1;
        }
        assert!(
            checked >= 2,
            "expected the database and at least one sidecar to exist, checked {checked}"
        );

        assert_eq!(
            std::fs::symlink_metadata(scratch_data_dir())
                .expect("data dir")
                .permissions()
                .mode()
                & 0o777,
            0o700,
            "the data directory itself is traversable by other local users"
        );
    }

    /// The passphrase `openssl-passphrase.db` is keyed with; duplicated in
    /// `pollis-sqlcipher/tests/sqlcipher.rs`, which reads the fixtures.
    const FIXTURE_PASSPHRASE: &str = "pollis-interop-fixture-passphrase";
    /// The raw key `openssl-rawkey.db` is keyed with — the shape
    /// [`LocalDb::open_for_user`] itself uses.
    const FIXTURE_RAW_KEY: &[u8; 32] = b"pollis-interop-fixture-key-32byt";
    /// Rows per fixture: enough to span pages beyond the salt-bearing first one.
    const FIXTURE_ROWS: i64 = 64;

    /// The body stored at `id`, deterministic so the reader asserts exact values.
    fn fixture_body(id: i64) -> String {
        format!("interop-row-{id:03}-{}", "ab".repeat(96))
    }

    /// Regenerates the cross-provider fixtures `pollis-sqlcipher/tests/sqlcipher.rs`
    /// reads. They must come from a different implementation (here, SQLCipher
    /// over system OpenSSL / CommonCrypto), since a provider reading its own
    /// output would agree with itself even when wrong. Hence not on Windows,
    /// where `pollis-core` links `pollis-sqlcipher` itself.
    ///
    /// ```text
    /// cargo test -p pollis-core --lib --no-default-features \
    ///     -- --ignored db::local::tests::regenerate_the_cross_provider_fixtures --nocapture
    /// ```
    #[test]
    #[cfg(not(target_os = "windows"))]
    #[ignore = "writes fixtures into the source tree; run deliberately"]
    fn regenerate_the_cross_provider_fixtures() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../pollis-sqlcipher/tests/fixtures");
        std::fs::create_dir_all(&dir).expect("create fixtures dir");

        for (name, key_pragma) in [
            ("openssl-passphrase.db", format!("PRAGMA key = '{FIXTURE_PASSPHRASE}'")),
            (
                "openssl-rawkey.db",
                format!("PRAGMA key = \"x'{}'\"", hex::encode(FIXTURE_RAW_KEY)),
            ),
        ] {
            let path = dir.join(name);
            let _ = std::fs::remove_file(&path);

            let conn = Connection::open(&path).expect("open fixture");
            conn.execute_batch(&key_pragma).expect("apply key");
            // A rollback journal, so the fixture is one file with no sidecar.
            conn.execute_batch("PRAGMA journal_mode = DELETE").expect("journal mode");
            conn.execute_batch(
                "CREATE TABLE interop (id INTEGER PRIMARY KEY, body TEXT NOT NULL)",
            )
            .expect("create table");
            for id in 1..=FIXTURE_ROWS {
                conn.execute(
                    "INSERT INTO interop (id, body) VALUES (?1, ?2)",
                    rusqlite::params![id, fixture_body(id)],
                )
                .expect("insert");
            }
            drop(conn);

            let bytes = std::fs::read(&path).expect("read fixture");
            assert_ne!(
                &bytes[..16],
                SQLITE_PLAINTEXT_HEADER,
                "{name} was written in plaintext — this build's SQLCipher is not encrypting"
            );
            println!("wrote {} ({} bytes)", path.display(), bytes.len());
        }
    }
}
