use rusqlite::{Connection, OptionalExtension};
use crate::error::{Error, Result};

// Windows takes rusqlite's LINKED `sqlcipher` feature, so `libsqlite3-sys`
// only emits `-l sqlcipher` and compiles nothing; `pollis-sqlcipher` is what
// supplies that library, along with the `#[no_mangle]` crypto provider it calls
// back into (#992, and the long note in Cargo.toml). Cargo would build the
// crate regardless, but a dependency no Rust code names does not get its rlib
// onto the link line — so naming it here is what actually puts SQLCipher in the
// binary. Nothing to import: the contact surface is C symbols.
#[cfg(target_os = "windows")]
use pollis_sqlcipher as _;

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
        crate::private_fs::create_dir_all(&data_dir)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("create data dir: {e}")))?;

        let db_path = data_dir.join(format!("pollis_{user_id}.db"));
        Self::open_at(&db_path, key)
    }

    fn open_at(db_path: &std::path::Path, key: &[u8]) -> Result<Self> {
        let key_pragma = format!("PRAGMA key = \"x'{}'\"", hex::encode(key));

        // #992: a database written by a build whose `PRAGMA key` did nothing.
        // Every Windows client before this change wrote one, and a developer or
        // test machine may still hold one anywhere. SQLCipher cannot open it —
        // it would surface as `NotADatabase` and fall into the wipe below — but
        // "cannot open" is not good enough on its own, because that path leaves
        // the plaintext pages sitting on the disk until the filesystem gets
        // round to reusing them. Detect it explicitly and destroy it.
        let destroyed = destroy_plaintext_database(db_path)?;

        // A DB is "fresh" if the file didn't exist, was just destroyed above, or
        // we wipe it below. Tracked because `auto_vacuum=INCREMENTAL` can only be
        // set before any table is created on a fresh DB; an existing DB has to be
        // converted via VACUUM. `destroyed` is carried separately from
        // `db_path.exists()` because a destroyed database can still leave a
        // zero-length file behind — see `dispose`.
        let mut is_fresh = destroyed || !db_path.exists();

        // Check if the stored schema version matches. If not, wipe and recreate.
        //
        // Be narrow about what justifies nuking a user's encrypted DB: wrong
        // SQLCipher key, missing schema_version row, or an explicit version
        // mismatch. Any *other* rusqlite failure (lock contention mid-open,
        // transient I/O) is surfaced instead — we refuse to eat the local
        // database on an error we don't understand.
        if !destroyed && db_path.exists() {
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
                // Through the shredder, sidecars included. This arm is also
                // where a PLAINTEXT database lands whenever
                // `sqlcipher_is_linked` declined to answer, and removing the
                // main file alone left a `-wal` full of message bodies next to
                // the fresh encrypted database that replaced it.
                destroy_unusable_database(db_path)?;
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

        // The database file and its WAL sidecars are created by the SQLite VFS,
        // which knows nothing about `private_fs` and opens at the process
        // umask — so they are tightened here, once the connection (and
        // therefore the `-wal`/`-shm` pair) exists. The WAL holds recently
        // written pages; it is exactly as private as the database.
        restrict_database_files(db_path);

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

// ── Plaintext-database disposal (#992) ───────────────────────────────────────

/// The 16 bytes an unencrypted SQLite file begins with. SQLCipher encrypts the
/// header along with everything else, so finding this at offset 0 means the
/// codec never engaged when the file was written.
///
/// There is exactly one way a genuinely encrypted database could still start
/// with it — `PRAGMA cipher_plaintext_header_size`, which deliberately leaves
/// the header readable so tools can identify the file. Pollis never sets it, and
/// if that ever changes this check has to change with it or it will start
/// destroying good databases.
const SQLITE_PLAINTEXT_HEADER: &[u8; 16] = b"SQLite format 3\0";

/// Sidecars SQLite keeps beside the main file. The `-wal` in particular holds
/// recently written pages in the clear on a plaintext database, so removing only
/// the main file would leave message bodies behind.
const DB_SIDECAR_SUFFIXES: [&str; 3] = ["-wal", "-shm", "-journal"];

/// Whether the `sqlite3` this binary actually linked is SQLCipher — i.e.
/// whether `PRAGMA key` does anything at all.
///
/// In every **shipped** build this is true, and it is not a runtime question
/// there: [`tests::sqlcipher_is_the_sqlite_we_actually_linked`] fails the build
/// if it ever stops being, on Linux, macOS and Windows alike, with no `cfg`
/// exception. Asking at runtime covers the one case a manifest cannot decide —
/// a binary that links a **second** sqlite3 beside ours, where the linker picks
/// which one answers `sqlite3_*`.
///
/// Exactly one such binary exists: the integration harness in
/// `src-tauri/tests/flows`, which runs the Delivery Service in-process and so
/// pulls in `libsql`'s own amalgamation. That one wins the symbols, the harness
/// client's local database is plaintext, and `flows::the_harness_links_a_second_
/// sqlite3` in that suite pins the fact rather than leaving it to be
/// rediscovered. No `pollis-core`, `pollis-tui` or desktop binary contains a
/// second sqlite3 — #987/#988 removed `libsql` from the client precisely so.
///
/// Asked on a caller-supplied connection, never on a scratch in-memory one:
/// `Connection::open_in_memory()` would run `sqlite3_initialize()` at a moment
/// of this function's choosing, and libsql calls `sqlite3_config` on first use
/// and panics if sqlite3 is already initialised (`.codesight/wiki/testing.md`,
/// "libsql local DBs cannot go in pollis-core's `--lib` tests"). The one caller
/// hands it a connection the open path was going to make anyway.
fn sqlcipher_is_linked(conn: &Connection) -> bool {
    match conn.query_row("PRAGMA cipher_version", [], |row| row.get::<_, String>(0)) {
        Ok(v) => !v.trim().is_empty(),
        // Stock SQLite does not know this pragma and answers with no rows at
        // all. That is the "no codec" ANSWER, not a failure.
        Err(rusqlite::Error::QueryReturnedNoRows) => false,
        // Anything else is a question that could not be asked. It still reads
        // as "no codec", because declining to destroy is the safe direction —
        // but it must not be silent, because the caller then leaves a plaintext
        // database on the disk on the strength of it, and the wipe that
        // eventually removes it has to be the one that takes the sidecars too.
        Err(e) => {
            eprintln!(
                "[db] could not ask whether SQLCipher is linked (`PRAGMA cipher_version`: {e}); \
                 assuming it is not, so a plaintext database at this path is left in place \
                 rather than destroyed."
            );
            false
        }
    }
}

/// Destroy `db_path` if — and only if — it is an unencrypted SQLite database.
///
/// **Recreate empty, not migrate.** `ATTACH ... KEY` + `sqlcipher_export` would
/// preserve the contents, but it is a multi-statement conversion with a window
/// where both a plaintext and an encrypted copy exist, and there is no
/// production Windows user base to preserve anything for. Starting empty is
/// already one of the three sanctioned history losses ("a new device starts
/// empty", CLAUDE.md), and it ends with no plaintext on disk — which a
/// migration only achieves if its cleanup also works.
///
/// Idempotent: a missing file, an encrypted file, or anything that is not a
/// SQLite database at all is left exactly as it was found. A failure to read is
/// propagated rather than swallowed — the one thing this must never do is
/// decide it cannot deal with a plaintext database and open it anyway.
///
/// Returns whether it destroyed anything, so the caller knows the file it is
/// about to open is a blank slate — including in the one case where the bytes
/// are gone but the (now zero-length) file is not, described in [`dispose`].
///
/// It is an **upgrade** step, and it is gated on [`sqlcipher_is_linked`] because
/// that is what makes it an upgrade rather than plain data loss. Destroying a
/// plaintext database is worth doing when the replacement will be encrypted. In
/// a binary with no codec the replacement is another plaintext file, so the
/// "upgrade" is an endless destroy-and-recreate that additionally pulls the file
/// out from under any connection another client in the same process already
/// holds open — which is exactly what it did to the flows harness's
/// multi-device tests, where two simulated devices share one
/// `pollis_{user_id}.db`. A build that can encrypt is a build-time guarantee
/// (see [`sqlcipher_is_linked`]), so on every shipped platform this gate is
/// always open.
fn destroy_plaintext_database(db_path: &std::path::Path) -> Result<bool> {
    // ONE resolution of the path, and every destructive step below runs on the
    // handle it produced (#1000). Resolving `db_path` separately for the header
    // probe, the `stat`, the write and the unlink — which is what this used to
    // do — is four chances for the file underneath to become a different one,
    // and `File::open`/`fs::metadata`/`OpenOptions::open` all follow a symlink
    // planted at the last component while `remove_file` does not. A link at
    // `pollis_<uid>.db` therefore aimed the zeroing at whatever SQLite database
    // it pointed to and then deleted only the link.
    let (mut file, meta) = match open_regular_no_follow(db_path)? {
        Target::Missing => return Ok(false),
        Target::NotRegular => {
            // Never write through it, and — unlike the disposal in `open_at` —
            // never unlink it either: nothing here has established that the
            // thing it points at is a plaintext database, and this function's
            // contract is to leave everything else exactly as found.
            eprintln!(
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

    // Whether `PRAGMA key` does anything is a property of THIS BINARY, not of
    // the file, so re-resolving the path for the probe cannot mislead the
    // answer — and see [`sqlcipher_is_linked`] for why the question is not
    // asked on a scratch in-memory database instead.
    let probe = Connection::open(db_path)?;
    let encrypts = sqlcipher_is_linked(&probe);
    drop(probe);

    if !encrypts {
        // Loud, because the only way to get here is a binary carrying a second
        // sqlite3 that won the symbols, and every database it writes is
        // plaintext regardless of what this function does about the old one.
        eprintln!(
            "[db] {db_path:?} is an UNENCRYPTED database and this binary has no SQLCipher \
             (`PRAGMA cipher_version` answers nothing), so the file is left alone — replacing \
             it would only produce another plaintext one. Something in this link is shipping a \
             second sqlite3 amalgamation; see db::local::sqlcipher_is_linked."
        );
        return Ok(false);
    }

    dispose(db_path, file, &meta, Zero::Yes)?;
    // Sidecars are disposed of by path — each is its own single open — and the
    // `-wal` in particular holds recently written pages in the clear.
    for suffix in DB_SIDECAR_SUFFIXES {
        dispose_path(&sidecar_path(db_path, suffix), Zero::Yes)?;
    }
    Ok(true)
}

/// Remove a database file and its sidecars because the open path has decided
/// the database is unusable — the wrong SQLCipher key, no `kv` row, an
/// incompatible [`LOCAL_SCHEMA_VERSION`].
///
/// Zeroes the bytes first when they are plaintext. That is not a hypothetical:
/// [`sqlcipher_is_linked`] answers "no" for any error at all, and a build where
/// it declines drops a plaintext database straight into this path via
/// `NotADatabase` — which used to `remove_file` the main file alone and leave a
/// `-wal` full of message bodies beside it.
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
        // A symlink here IS unlinked — the path has to stop being a database
        // for the open below to recreate one — but the unlink removes the link
        // itself and never touches what it points at, and nothing is ever
        // written through it.
        Target::NotRegular => {
            eprintln!(
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

/// Tighten the database file and its WAL sidecars to owner-only.
///
/// The SQLite VFS creates all three, so [`crate::private_fs`] never sees them
/// at creation time; this is the one place that catches them afterwards.
/// Best-effort and never fatal: a mode that could not be set is worth a line on
/// stderr, not a failed login on a database that is otherwise fine. On Windows
/// it is a documented no-op — see the module docs on `private_fs`.
fn restrict_database_files(db_path: &std::path::Path) {
    let mut paths = vec![db_path.to_path_buf()];
    paths.extend(DB_SIDECAR_SUFFIXES.iter().map(|s| sidecar_path(db_path, s)));
    for path in paths {
        if let Err(e) = crate::private_fs::restrict_existing(&path) {
            eprintln!("[db] could not restrict permissions on {path:?}: {e}");
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

/// What a path the disposal helpers looked at turned out to be. Produced by
/// exactly one `open`, so the file identity every later step acts on is the one
/// this call resolved.
enum Target {
    /// Nothing at that path.
    Missing,
    /// Something is there, but it is not a regular file — a symlink (which the
    /// no-follow open refused to traverse), a directory, a device node. Never
    /// written through.
    NotRegular,
    /// An open read/write handle on a regular file, plus the metadata read
    /// FROM that handle rather than from the path.
    Regular(std::fs::File, std::fs::Metadata),
}

/// Open `path` read/write without traversing a symlink at the final component,
/// and only hand back a handle when what opened is a regular file.
///
/// `O_NOFOLLOW` on unix and `FILE_FLAG_OPEN_REPARSE_POINT` on Windows;
/// `symlink_metadata` — which never traverses — is the portable floor that
/// classifies whatever the open refused, and the `is_file()` check on the
/// handle's own metadata catches the rest (directories, fifos, devices).
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
        // Open the reparse point itself instead of its target, so a Windows
        // symlink or junction at this path is classified below rather than
        // followed. (0x0020_0000 = FILE_FLAG_OPEN_REPARSE_POINT; naming the
        // constant here keeps a `windows-sys` dependency out of this crate.)
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        opts.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }

    let file = match opts.open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Target::Missing),
        Err(e) => {
            // `O_NOFOLLOW` reports a symlink as ELOOP (ENOTDIR on some
            // systems); a directory reports EISDIR. Ask the path what it is
            // without traversing it, and treat anything that is not a regular
            // file as untouchable rather than as an error.
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
/// the handle — never re-opening the path — and leaves the cursor back at
/// offset 0 for the overwrite that may follow. A file shorter than the header
/// is not one.
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

/// [`dispose`], for a path not already open — the sidecars. A path with
/// nothing at it is not an error: a `-wal` exists only while a connection is
/// open, and the caller should not have to know which sidecars are there.
fn dispose_path(path: &std::path::Path, zero: Zero) -> Result<()> {
    match open_regular_no_follow(path)? {
        Target::Missing => Ok(()),
        Target::NotRegular => {
            eprintln!("[db] {path:?} is not a regular file; leaving it alone.");
            Ok(())
        }
        Target::Regular(file, meta) => dispose(path, file, &meta, zero),
    }
}

/// Overwrite an open file's bytes (when `zero` says so), truncate it, then
/// unlink the path — but only while the path still names the same file the
/// handle does.
///
/// The overwrite is best-effort by nature: on a copy-on-write or log-structured
/// filesystem, and on any SSD doing wear levelling, the old blocks may survive
/// the rewrite. It still closes the easy case — the plaintext is gone from the
/// path the file occupied and from any later read of that path — and it costs
/// one pass over a file that is about to be deleted anyway.
///
/// **The unlink may fail after the bytes are already gone, and that is
/// tolerated.** SQLite's Windows VFS opens without `FILE_SHARE_DELETE`, so a
/// connection another process (or another client inside this one) holds makes
/// `DeleteFile` fail with a sharing violation even though every write
/// succeeded. Propagating that would abort `open_for_user` with the database
/// already destroyed and no way forward. Instead the file is left truncated to
/// zero length — the plaintext is gone either way — and the caller treats a
/// destroyed database as a fresh one, which is exactly what a zero-length file
/// then opens as. Without a preceding overwrite there is nothing to salvage
/// and the error is propagated as before.
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
        // Truncate through the same handle, so that if the unlink below cannot
        // run there is an EMPTY file left rather than a file-sized run of
        // zeros — which `open_at` opens as a fresh database and SQLite would
        // otherwise reject as `NotADatabase` forever.
        file.set_len(0)
            .map_err(|e| Error::Other(anyhow::anyhow!("truncate {path:?}: {e}")))?;
    }
    drop(file);

    match remove_if_unchanged(path, meta) {
        Ok(()) => Ok(()),
        Err(e) if zero == Zero::Yes => {
            eprintln!(
                "[db] {path:?} was overwritten and emptied but could not be unlinked ({e}); \
                 continuing — the bytes are gone and the empty file opens as a fresh database."
            );
            Ok(())
        }
        Err(e) => Err(Error::Other(anyhow::anyhow!("remove {path:?}: {e}"))),
    }
}

/// Unlink `path`, unless something moved a different file (or a symlink) into
/// it since the handle was opened.
///
/// `remove_file` never follows a symlink, so the worst a swap could do is
/// delete the attacker's link rather than their target — but deleting a file
/// this process never inspected is still not something to do quietly. On unix
/// the check is exact (same device, same inode); elsewhere it is the portable
/// floor — the path must still be a regular file, which `symlink_metadata`
/// answers without traversing.
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
    /// question whose answer distinguishes the two builds — and since #992 the
    /// answer must be the same on EVERY platform. There is no longer a `cfg`
    /// here and there must not be one again: Windows was the exception, Windows
    /// is where the plaintext databases came from, and an exception is exactly
    /// what let it stand for a year.
    ///
    /// This is necessary but not sufficient. It says a SQLCipher build is
    /// present; it says nothing about whether the pages that reach the disk are
    /// ciphertext. [`the_local_database_file_is_encrypted_at_rest`] settles that.
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

    /// `docs/security-whitepaper.md` §7.0 states, per platform, which crypto
    /// provider SQLCipher takes its AES and HMAC from. That table records a
    /// build outcome, not a source-level fact, and on macOS the outcome is
    /// decided by the environment: `libsqlite3-sys`'s build script selects
    /// CommonCrypto only when it finds no OpenSSL, so an `OPENSSL_DIR` (or
    /// `OPENSSL_LIB_DIR` + `OPENSSL_INCLUDE_DIR`) present on a build machine
    /// moves macOS onto OpenSSL instead — nothing in the source tree changes
    /// and every other test still passes.
    ///
    /// `PRAGMA cipher_provider` is the build answering that question about
    /// itself, so ask it. This does not decide whether the database is
    /// encrypted — [`the_local_database_file_is_encrypted_at_rest`] does that —
    /// it decides that the documented provider and the compiled one are the
    /// same provider.
    ///
    /// The pragma answers only once a codec is attached, so key the connection
    /// first.
    #[test]
    fn the_crypto_provider_is_the_one_this_platform_documents() {
        let expected = if cfg!(target_os = "windows") {
            // `pollis-sqlcipher`, compiled `-DSQLCIPHER_CRYPTO_LIBTOMCRYPT`
            // against the RustCrypto-backed provider in its `abi` module. It
            // reports upstream's provider name; `cipher_provider_version` is
            // the honest one and that crate's own suite pins it.
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

    /// The premise the plaintext-disposal gate rests on, in the configuration
    /// that ships. `sqlcipher_is_linked` exists to keep a binary carrying a
    /// SECOND sqlite3 from destroying databases it cannot replace with anything
    /// better — it must never be the thing that quietly switches encryption off
    /// here, where `pollis-core` is the only sqlite3 in the link.
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

    /// Every path below goes through [`LocalDb::open_for_user`] — the function
    /// the application itself calls — rather than a hand-built `Connection`, so
    /// a regression that keys a test connection while leaving the real one bare
    /// cannot hide here.
    ///
    /// `set_data_dir` is process-global, so all of these share one scratch
    /// directory (set exactly once) and separate themselves by user id instead.
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

    /// The claim `docs/security-whitepaper.md` §7 makes, checked against the
    /// bytes rather than against a pragma.
    ///
    /// `PRAGMA cipher_version` answering only proves the library is present. The
    /// question that matters is whether a message written through the ordinary
    /// application path is readable in the file afterwards — which is exactly
    /// what was NOT true on Windows before #992, with every pragma and every
    /// write reporting success the whole time.
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

        // Encrypted is only half of it: the key has to be what unlocks it.
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

        // ...and the right key still works, so this is encryption and not
        // corruption.
        let reopened = LocalDb::open_for_user(USER, TEST_KEY).expect("reopen");
        let stored: String = reopened
            .conn()
            .query_row("SELECT content FROM message WHERE id = 'm1'", [], |row| row.get(0))
            .expect("message survived the round trip");
        assert_eq!(stored, MARKER);
    }

    /// #992's upgrade path. A database left behind by a build that wrote
    /// plaintext must be destroyed on the next open — not opened, not silently
    /// fallen back to, and not left on the disk in the clear.
    #[test]
    fn a_plaintext_database_is_destroyed_not_opened() {
        const USER: &str = "plaintext-legacy";
        const MARKER: &str = "LEGACY-PLAINTEXT-MARKER-8b3c07";

        std::fs::create_dir_all(scratch_data_dir()).expect("data dir");
        let path = db_file(USER);

        // Exactly what a pre-#992 Windows client produced: SQLCipher is linked,
        // but nothing ever keys the connection, so it behaves as stock SQLite.
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

        // Idempotent: the second open finds an encrypted database and leaves it
        // alone rather than destroying it again.
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

    /// Path-based mirror of [`starts_with_plaintext_header`], for assertions
    /// only. The production path deliberately has no such function: it resolves
    /// a path exactly once and asks the resulting handle.
    fn is_plaintext_sqlite(path: &std::path::Path) -> Result<bool> {
        match open_regular_no_follow(path)? {
            Target::Regular(mut f, _) => starts_with_plaintext_header(&mut f, path),
            _ => Ok(false),
        }
    }

    /// A symlink planted at the database path must not aim the shredder at
    /// whatever it points to.
    ///
    /// `File::open`, `fs::metadata` and `OpenOptions::open` all follow a
    /// symlink; `remove_file` does not. The old disposal path used the first
    /// three to find, zero and truncate "the database" and the fourth to delete
    /// it — so a link at `pollis_<uid>.db` destroyed any SQLite file its owner
    /// could write and then tidied the link away, leaving nothing to show what
    /// had happened.
    #[test]
    #[cfg(unix)]
    fn a_symlink_at_the_database_path_never_reaches_its_target() {
        const USER: &str = "symlink-victim";
        let dir = scratch_data_dir();
        let victim = dir.join("someone-elses-notes.db");

        // A plaintext SQLite file — i.e. exactly what the disposal path is
        // built to destroy, so nothing but the symlink check stands between it
        // and the shredder.
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

        // And the whole open path, which is what a real user reaches: it may
        // remove the LINK so a real database can take that name, but the file
        // the link pointed at must come through byte-identical.
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

    /// The Windows half-state, exercised where it can be provoked.
    ///
    /// SQLite's Windows VFS opens without `FILE_SHARE_DELETE`, so an unlink can
    /// fail with the bytes already overwritten. The docstring used to claim the
    /// operation "cannot half-succeed"; it can, and propagating the error left
    /// `open_for_user` with a destroyed database and no way forward. A
    /// read-only parent directory reproduces the same shape on unix: the write
    /// succeeds, the unlink does not.
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

        // Deny writes to the DIRECTORY (which is what unlink needs) while
        // leaving the FILE writable (which is what the overwrite needs).
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500))
            .expect("chmod dir");
        // Running as root ignores the mode entirely, so there is nothing to
        // provoke — skip rather than assert something untrue.
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

    /// The `sqlcipher_is_linked`-declined fallback: `open_at` reaches
    /// `NotADatabase`, wipes, and must take the sidecars with it.
    ///
    /// It used to `remove_file` the main database alone, so a `-wal` full of
    /// recently written message bodies stayed on the disk next to the fresh
    /// encrypted database that replaced it — plaintext at rest, produced by the
    /// very step meant to remove it.
    #[test]
    fn wiping_an_unusable_database_takes_the_sidecars_with_it() {
        const USER: &str = "unusable-with-sidecars";
        let path = db_file(USER);
        let _ = std::fs::remove_file(&path);

        // A file SQLCipher cannot open under our key, so `open_at`'s version
        // probe answers `NotADatabase` — the same arm a plaintext database
        // reaches whenever the disposal step above declined to act.
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

    /// The mode on the files SQLite creates. `private_fs` never sees them — the
    /// VFS opens them — so `open_at` is the only place that can tighten them.
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
}
