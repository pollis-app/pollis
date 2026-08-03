//! The DB source reader: connects to Turso/libSQL (remote) or a local SQLite
//! file and reads `mls_commit_log` (and `account_key_log`) in `seq` order.
//!
//! Privacy: the raw `commit_data` blob is hashed to `sha256` **as each row is
//! read** and immediately dropped — it is never returned, logged, or persisted.
//! The account public key, by contrast, is public by design and is read out
//! verbatim (hex). The auth token is read from the environment and never logged
//! either.

use sha2::{Digest, Sha256};

use crate::account_key::AccountKeyLeaf;
use crate::commit_log::{
    derive_conversation_pseudonym, derive_sender_pseudonym, window_for_seq, CommitLeaf,
};
use crate::error::{BuilderError, Result};

/// One structural row of `mls_commit_log`. Carries `commit_sha256` (hex), never
/// the raw commit bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRow {
    pub seq: i64,
    pub conversation_id: String,
    /// Suite generation of the lineage this commit belongs to (#454 P4). 0 for
    /// every conversation that has never migrated, and for any log DB predating
    /// migration `000004_commit_generation.sql`.
    pub generation: i64,
    pub epoch: i64,
    pub sender_id: String,
    pub commit_sha256: String,
}

impl CommitRow {
    /// Project this row into the **published** leaf: the real `conversation_id`
    /// and `sender_id` are replaced by windowed pseudonyms (#701), so the public
    /// log never carries a stable identifier. The window is `seq / WINDOW_SIZE`,
    /// derivable by a member from the leaf's own `seq`. `epoch` and `generation`
    /// are stored as signed INTEGERs in SQLite but are logically non-negative; we
    /// keep them as `u64` in the leaf and clamp a (never-expected) negative to 0
    /// rather than panic.
    pub fn to_leaf(&self) -> CommitLeaf {
        let window = window_for_seq(self.seq);
        CommitLeaf {
            conversation_pseudonym: derive_conversation_pseudonym(&self.conversation_id, window),
            generation: self.generation.max(0) as u64,
            epoch: self.epoch.max(0) as u64,
            sender_pseudonym: derive_sender_pseudonym(
                &self.conversation_id,
                &self.sender_id,
                window,
            ),
            seq: self.seq,
            commit_sha256: self.commit_sha256.clone(),
        }
    }

    /// Project this row into a leaf keyed on the **real** `conversation_id` /
    /// `sender_id` rather than pseudonyms. This is **never published** — it exists
    /// only so the builder can run [`crate::commit_log::CommitLogInvariant`] at
    /// full (cross-window) strength over the source rows before pseudonymising
    /// them (`build_bundle`'s source-integrity gate). The published tree carries
    /// pseudonyms, whose public invariant is only within-window; this is the one
    /// point the real ids are in hand to prove there is no boundary-straddling
    /// fork or regression at all.
    pub fn to_identity_leaf(&self) -> CommitLeaf {
        CommitLeaf {
            conversation_pseudonym: self.conversation_id.clone(),
            generation: self.generation.max(0) as u64,
            epoch: self.epoch.max(0) as u64,
            sender_pseudonym: self.sender_id.clone(),
            seq: self.seq,
            commit_sha256: self.commit_sha256.clone(),
        }
    }
}

/// Does this `--db` value look like a remote libSQL/Turso URL (vs. a local file
/// path)?
fn is_remote_url(db: &str) -> bool {
    let lower = db.to_ascii_lowercase();
    lower.starts_with("libsql://")
        || lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ws://")
        || lower.starts_with("wss://")
}

/// Connect to a database with an explicitly-supplied auth token.
///
/// * If `db` looks like a URL → remote Turso, authenticated with the PASSED
///   `token` (each database carries its own token: after Goal A's cutover the
///   commit log lives in a separate "log DB" with its own credentials, distinct
///   from the main DB that still owns `account_key_log`).
/// * Otherwise `db` is treated as a local SQLite file path (no network) and the
///   `token` is ignored — this is what the tests use.
pub async fn connect_with_token(db: &str, token: &str) -> Result<libsql::Connection> {
    let database = if is_remote_url(db) {
        libsql::Builder::new_remote(db.to_string(), token.to_string())
            .build()
            .await?
    } else {
        libsql::Builder::new_local(db).build().await?
    };
    Ok(database.connect()?)
}

/// Connect to a commit-log database, reading the auth token from the
/// environment.
///
/// * If `db` looks like a URL → remote Turso; the auth token is read from
///   `TURSO_AUTH_TOKEN` (empty if unset, which works for unauthenticated dev
///   instances).
/// * Otherwise `db` is treated as a local SQLite file path (no network) — this
///   is what the tests use.
pub async fn connect(db: &str) -> Result<libsql::Connection> {
    let token = std::env::var("TURSO_AUTH_TOKEN").unwrap_or_default();
    connect_with_token(db, &token).await
}

/// Does `table` have a column named `column`?
///
/// The builder ships and deploys independently of the Delivery Service that owns
/// the log DB's migrations, so it cannot assume the two are in lockstep. Probing
/// beats catching a "no such column" error: the query below fails cleanly on a
/// genuinely unreachable DB instead of silently degrading to the narrow read.
async fn has_column(conn: &libsql::Connection, table: &str, column: &str) -> Result<bool> {
    let mut rows = conn
        .query(&format!("PRAGMA table_info({table})"), ())
        .await?;
    while let Some(row) = rows.next().await? {
        // PRAGMA table_info columns: cid, name, type, notnull, dflt_value, pk.
        if row.get::<String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Read every `mls_commit_log` row in ascending `seq` order, hashing each
/// `commit_data` blob to hex and discarding the raw bytes.
///
/// `generation` (#454 P4) is read when the log DB has it and reported as 0 when
/// it does not — which is correct, not a fallback approximation: a log DB
/// predating migration `000004_commit_generation.sql` cannot contain a migrated
/// conversation, so every commit in it genuinely belongs to generation 0.
pub async fn read_commit_log(conn: &libsql::Connection) -> Result<Vec<CommitRow>> {
    // Only the structural columns plus the blob (to hash it). `created_at`,
    // `added_user_id`, `added_device_ids` are intentionally not read — the leaf
    // commits to commit identity, not delivery metadata.
    let has_generation = has_column(conn, "mls_commit_log", "generation").await?;
    let sql = if has_generation {
        "SELECT seq, conversation_id, epoch, sender_id, commit_data, generation \
         FROM mls_commit_log ORDER BY seq ASC"
    } else {
        "SELECT seq, conversation_id, epoch, sender_id, commit_data, 0 \
         FROM mls_commit_log ORDER BY seq ASC"
    };
    let mut rows = conn.query(sql, ()).await?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let seq: i64 = row.get(0)?;
        let conversation_id: String = row.get(1)?;
        let epoch: i64 = row.get(2)?;
        let sender_id: String = row.get(3)?;
        let commit_data: Vec<u8> = row.get(4)?;
        let generation: i64 = row.get(5)?;

        // Hash and drop the raw blob immediately — it is never retained.
        let commit_sha256 = sha256_hex(&commit_data);
        drop(commit_data);

        out.push(CommitRow {
            seq,
            conversation_id,
            generation,
            epoch,
            sender_id,
            commit_sha256,
        });
    }
    Ok(out)
}

/// One structural row of `account_key_log`. Carries the account public key as
/// lowercase hex — public by design, never hashed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountKeyRow {
    pub seq: i64,
    pub user_id: String,
    pub identity_version: i64,
    pub account_id_pub: String,
}

impl AccountKeyRow {
    /// Project this row into its canonical leaf. `identity_version` is stored as
    /// a signed INTEGER in SQLite but is logically positive; clamp a
    /// (never-expected) negative to 0 rather than panic.
    pub fn to_leaf(&self) -> AccountKeyLeaf {
        AccountKeyLeaf {
            user_id: self.user_id.clone(),
            identity_version: self.identity_version.max(0) as u64,
            account_id_pub: self.account_id_pub.clone(),
            seq: self.seq,
        }
    }
}

/// Read every `account_key_log` row in ascending `seq` order. `account_id_pub` is
/// a BLOB (the raw Ed25519 public key); it is hex-encoded — never hashed — since
/// the account-key directory exists precisely to publish it.
pub async fn read_account_key_log(conn: &libsql::Connection) -> Result<Vec<AccountKeyRow>> {
    let mut rows = conn
        .query(
            "SELECT seq, user_id, identity_version, account_id_pub \
             FROM account_key_log ORDER BY seq ASC",
            (),
        )
        .await?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let seq: i64 = row.get(0)?;
        let user_id: String = row.get(1)?;
        let identity_version: i64 = row.get(2)?;
        let account_id_pub: Vec<u8> = row.get(3)?;

        out.push(AccountKeyRow {
            seq,
            user_id,
            identity_version,
            account_id_pub: hex::encode(account_id_pub),
        });
    }
    Ok(out)
}

/// `sha256(bytes)` as lowercase hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}

/// Guard against an empty commit-log source. An empty table is a distinct
/// condition from a missing `--db` argument: the DB connected fine, it just has
/// no commits yet (e.g. immediately after Goal A's cutover). So it returns
/// [`BuilderError::EmptyCommitLog`], never [`BuilderError::NoDbSource`] — the
/// latter is reserved for the genuine missing-source case in `builder.rs`.
pub fn ensure_non_empty(rows: &[CommitRow]) -> Result<()> {
    if rows.is_empty() {
        return Err(BuilderError::EmptyCommitLog);
    }
    Ok(())
}

/// Guard against an empty account-key source. Separate from [`ensure_non_empty`]
/// so the two tenants can be checked independently — an empty account-key log
/// returns its own [`BuilderError::EmptyAccountKeyLog`].
pub fn ensure_account_non_empty(rows: &[AccountKeyRow]) -> Result<()> {
    if rows.is_empty() {
        return Err(BuilderError::EmptyAccountKeyLog);
    }
    Ok(())
}
