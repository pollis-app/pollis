//! Reads `mls_commit_log` and `account_key_log` in `seq` order from Turso or a
//! local SQLite file.
//!
//! Privacy: each `commit_data` blob is hashed as it is read and dropped; it and
//! the auth token are never returned, logged or persisted.

use sha2::{Digest, Sha256};

use crate::account_key::AccountKeyLeaf;
use crate::commit_log::{
    derive_conversation_pseudonym, derive_sender_pseudonym, window_for_seq, CommitLeaf,
};
use crate::error::{BuilderError, Result};

/// One structural row of `mls_commit_log`: the commit's hash, never its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitRow {
    pub seq: i64,
    pub conversation_id: String,
    /// Suite generation of the commit's lineage (#454); 0 if never migrated.
    pub generation: i64,
    pub epoch: i64,
    pub sender_id: String,
    pub commit_sha256: String,
}

impl CommitRow {
    /// The published leaf: ids replaced by windowed pseudonyms (#701) so the
    /// public log carries no stable identifier. Negative INTEGERs clamp to 0.
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

    /// A leaf keyed on the real ids. Never published: only for `build_bundle`'s
    /// cross-window source-integrity gate.
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

#[cfg(feature = "db-source")]
fn is_remote_url(db: &str) -> bool {
    let lower = db.to_ascii_lowercase();
    lower.starts_with("libsql://")
        || lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("ws://")
        || lower.starts_with("wss://")
}

/// Connect to a remote URL with `token` (the log DB and main DB have separate
/// tokens), or to a local SQLite file path, ignoring `token`.
#[cfg(feature = "db-source")]
pub async fn connect_with_token(db: &str, token: &str) -> Result<libsql::Connection> {
    let database = if is_remote_url(db) {
        // libsql is built without its own `tls` feature (#1303).
        libsql::Builder::new_remote(db.to_string(), token.to_string())
            .connector(pollis_libsql_connector::Connector::new())
            .build()
            .await?
    } else {
        libsql::Builder::new_local(db).build().await?
    };
    Ok(database.connect()?)
}

/// [`connect_with_token`] with `TURSO_AUTH_TOKEN` (empty if unset).
#[cfg(feature = "db-source")]
pub async fn connect(db: &str) -> Result<libsql::Connection> {
    let token = std::env::var("TURSO_AUTH_TOKEN").unwrap_or_default();
    connect_with_token(db, &token).await
}

/// The builder deploys independently of the DS that owns the log DB's
/// migrations, so it probes. Probing rather than catching "no such column" means
/// an unreachable DB fails instead of degrading to the narrow read.
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

/// Read every `mls_commit_log` row in `seq` order, hashing and discarding each
/// `commit_data` blob. A log DB without `generation` (#454) has only
/// generation-0 commits, so 0 is exact there.
#[cfg(feature = "db-source")]
pub async fn read_commit_log(conn: &libsql::Connection) -> Result<Vec<CommitRow>> {
    // The leaf commits to commit identity, not delivery metadata.
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

/// One structural row of `account_key_log`. The public key is published as hex.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountKeyRow {
    pub seq: i64,
    pub user_id: String,
    pub identity_version: i64,
    pub account_id_pub: String,
}

impl AccountKeyRow {
    /// A negative `identity_version` clamps to 0 rather than panic.
    pub fn to_leaf(&self) -> AccountKeyLeaf {
        AccountKeyLeaf {
            user_id: self.user_id.clone(),
            identity_version: self.identity_version.max(0) as u64,
            account_id_pub: self.account_id_pub.clone(),
            seq: self.seq,
        }
    }
}

/// Read every `account_key_log` row in `seq` order.
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

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    hex::encode(digest)
}

/// An empty table (connected, no commits yet) is distinct from a missing `--db`
/// ([`BuilderError::NoDbSource`]).
pub fn ensure_non_empty(rows: &[CommitRow]) -> Result<()> {
    if rows.is_empty() {
        return Err(BuilderError::EmptyCommitLog);
    }
    Ok(())
}

pub fn ensure_account_non_empty(rows: &[AccountKeyRow]) -> Result<()> {
    if rows.is_empty() {
        return Err(BuilderError::EmptyAccountKeyLog);
    }
    Ok(())
}
