//! The DB source reader: connects to Turso/libSQL (remote) or a local SQLite
//! file and reads `mls_commit_log` in `seq` order.
//!
//! Privacy: the raw `commit_data` blob is hashed to `sha256` **as each row is
//! read** and immediately dropped — it is never returned, logged, or persisted.
//! The auth token is read from the environment and never logged either.

use sha2::{Digest, Sha256};

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

/// Connect to a commit-log database.
///
/// * If `db` looks like a URL → remote Turso; the auth token is read from
///   `TURSO_AUTH_TOKEN` (empty if unset, which works for unauthenticated dev
///   instances).
/// * Otherwise `db` is treated as a local SQLite file path (no network) — this
///   is what the tests use.
pub async fn connect(db: &str) -> Result<libsql::Connection> {
    let database = if is_remote_url(db) {
        let token = std::env::var("TURSO_AUTH_TOKEN").unwrap_or_default();
        libsql::Builder::new_remote(db.to_string(), token)
            .build()
            .await?
    } else {
        libsql::Builder::new_local(db).build().await?
    };
    Ok(database.connect()?)
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
