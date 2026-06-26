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
use crate::commit_log::CommitLeaf;
use crate::error::{BuilderError, Result};

/// One structural row of `mls_commit_log`. Carries `commit_sha256` (hex), never
/// the raw commit bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRow {
    pub seq: i64,
    pub conversation_id: String,
    pub epoch: i64,
    pub sender_id: String,
    pub commit_sha256: String,
}

impl CommitRow {
    /// Project this row into its canonical leaf. `epoch` is stored as a signed
    /// INTEGER in SQLite but is logically non-negative; we keep it as `u64` in
    /// the leaf and clamp a (never-expected) negative to 0 rather than panic.
    pub fn to_leaf(&self) -> CommitLeaf {
        CommitLeaf {
            conversation_id: self.conversation_id.clone(),
            epoch: self.epoch.max(0) as u64,
            sender_id: self.sender_id.clone(),
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

/// Read every `mls_commit_log` row in ascending `seq` order, hashing each
/// `commit_data` blob to hex and discarding the raw bytes.
pub async fn read_commit_log(conn: &libsql::Connection) -> Result<Vec<CommitRow>> {
    // Only the structural columns plus the blob (to hash it). `created_at`,
    // `added_user_id`, `added_device_ids` are intentionally not read — the leaf
    // commits to commit identity, not delivery metadata.
    let mut rows = conn
        .query(
            "SELECT seq, conversation_id, epoch, sender_id, commit_data \
             FROM mls_commit_log ORDER BY seq ASC",
            (),
        )
        .await?;

    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let seq: i64 = row.get(0)?;
        let conversation_id: String = row.get(1)?;
        let epoch: i64 = row.get(2)?;
        let sender_id: String = row.get(3)?;
        let commit_data: Vec<u8> = row.get(4)?;

        // Hash and drop the raw blob immediately — it is never retained.
        let commit_sha256 = sha256_hex(&commit_data);
        drop(commit_data);

        out.push(CommitRow {
            seq,
            conversation_id,
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

/// Guard against an empty source: building a bundle over zero commits is almost
/// always a misconfiguration (wrong DB / wrong table), so surface it.
pub fn ensure_non_empty(rows: &[CommitRow]) -> Result<()> {
    if rows.is_empty() {
        return Err(BuilderError::NoDbSource);
    }
    Ok(())
}

/// Guard against an empty account-key source. Separate from [`ensure_non_empty`]
/// so the two tenants can be checked independently.
pub fn ensure_account_non_empty(rows: &[AccountKeyRow]) -> Result<()> {
    if rows.is_empty() {
        return Err(BuilderError::NoDbSource);
    }
    Ok(())
}
