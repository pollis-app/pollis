//! The DB source reader: connects to Turso/libSQL (remote) or a local SQLite
//! file and reads `mls_commit_log` (and `account_key_log`) in `seq` order.
//!
//! Privacy: `commit_data` is hashed as each row is read and never kept; the token is never logged.

use sha2::{Digest, Sha256};

use crate::account_key::AccountKeyLeaf;
use crate::commit_log::{
    derive_conversation_pseudonym, derive_sender_pseudonym, window_for_seq, CommitLeaf,
};
use crate::error::{BuilderError, Result};

/// One structural row of `mls_commit_log`, carrying `commit_sha256`, never the raw bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRow {
    pub seq: i64,
    pub conversation_id: String,
    /// Suite generation of the lineage this commit belongs to (#454 P4). 0 for
    /// every conversation that has never migrated.
    pub generation: i64,
    pub epoch: i64,
    pub sender_id: String,
    pub commit_sha256: String,
}

impl CommitRow {
    /// Project this row into the **published** leaf: the real `conversation_id`
    /// and `sender_id` are replaced by windowed pseudonyms (#701). Negatives clamp to 0.
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
    /// `sender_id`. **Never published**: only for the full-strength source-integrity gate.
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

/// Does this `--db` value look like a remote libSQL/Turso URL?
#[cfg(feature = "db-source")]
fn is_remote_url(db: &str) -> bool {
    let lower = db.to_ascii_lowercase();
    lower.starts_with("libsql://")
        || lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ws://")
        || lower.starts_with("wss://")
}

/// Connect to a database with an explicitly-supplied auth token.
/// A URL is remote Turso with the passed `token` (each DB has its own); anything else is a
/// local file.
#[cfg(feature = "db-source")]
pub async fn connect_with_token(db: &str, token: &str) -> Result<libsql::Connection> {
    let database = if is_remote_url(db) {
        // libsql is built without its own `tls` feature (#1303); this is
        // the connector it dials Turso through instead.
        libsql::Builder::new_remote(db.to_string(), token.to_string())
            .connector(pollis_libsql_connector::Connector::new())
            .build()
            .await?
    } else {
        libsql::Builder::new_local(db).build().await?
    };
    Ok(database.connect()?)
}

/// Connect to a commit-log database, with the token from `TURSO_AUTH_TOKEN` for remote URLs.
#[cfg(feature = "db-source")]
pub async fn connect(db: &str) -> Result<libsql::Connection> {
    let token = std::env::var("TURSO_AUTH_TOKEN").unwrap_or_default();
    connect_with_token(db, &token).await
}

/// Does `table` have a column named `column`?
/// Probed because the builder deploys independently of the DS that owns the migrations.
#[cfg(feature = "db-source")]
async fn has_column(conn: &libsql::Connection, table: &str, column: &str) -> Result<bool> {
    let mut rows = conn
        .query(&format!("PRAGMA table_info({table})"), ())
        .await?;
    while let Some(row) = rows.next().await? {
        if row.get::<String>(1)? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Read every `mls_commit_log` row in ascending `seq` order, hashing each
/// `commit_data` blob to hex and discarding the raw bytes.
/// `generation` (#454 P4) reads as 0 on a log DB predating it, which is exact.
#[cfg(feature = "db-source")]
pub async fn read_commit_log(conn: &libsql::Connection) -> Result<Vec<CommitRow>> {
    // Structural columns plus the blob to hash; delivery metadata is not read.
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
/// lowercase hex, never hashed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountKeyRow {
    pub seq: i64,
    pub user_id: String,
    pub identity_version: i64,
    pub account_id_pub: String,
}

impl AccountKeyRow {
    /// Project this row into its canonical leaf. `identity_version` is stored as
    /// a signed INTEGER; a negative clamps to 0 rather than panicking.
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
/// a BLOB, hex-encoded (never hashed) since the directory exists to publish it.
#[cfg(feature = "db-source")]
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
/// condition from a missing `--db`, so it returns [`BuilderError::EmptyCommitLog`].
pub fn ensure_non_empty(rows: &[CommitRow]) -> Result<()> {
    if rows.is_empty() {
        return Err(BuilderError::EmptyCommitLog);
    }
    Ok(())
}

/// Guard against an empty account-key source. Separate from [`ensure_non_empty`]
/// so the two tenants can be checked independently.
pub fn ensure_account_non_empty(rows: &[AccountKeyRow]) -> Result<()> {
    if rows.is_empty() {
        return Err(BuilderError::EmptyAccountKeyLog);
    }
    Ok(())
}
