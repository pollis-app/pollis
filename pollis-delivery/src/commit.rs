//! Commit serialization — the heart of the Delivery Service.
//!
//! `mls_commit_log.epoch` is the *source* epoch a commit was built from: a
//! commit with `epoch = N` advances the group `N -> N+1`. So the group's head
//! epoch is `MAX(epoch) + 1` — the next epoch a member may commit from.
//!
//! A submitted commit is **accepted iff its `based_on_epoch` equals the current
//! head**, and it claims that epoch atomically. The whole decision is a single
//! conditional `INSERT ... SELECT ... WHERE based_on = head ... ON CONFLICT DO
//! NOTHING`, so:
//!   - two clients racing at the same head → SQLite serializes the writers; one
//!     INSERT lands (head advances), the other's `WHERE` now sees the new head
//!     and inserts nothing. Exactly one winner. **No fork.**
//!   - a stale client (`based_on < head`) or a forward gap (`based_on > head`)
//!     → `WHERE` is false → nothing inserted → rejected. **No gap.**
//!   - the log is only ever appended to, never deleted/rewritten, because this
//!     service is the only writer and never issues such statements. **Append-only.**
//!
//! These invariants are properties of *this code*, not of DB triggers.

use anyhow::{anyhow, Result};
use libsql::Connection;
use serde::{Deserialize, Serialize};

// ── Wire types (blobs are base64 over the wire) ─────────────────────────────

#[derive(Debug, Deserialize)]
pub struct SubmitBody {
    pub conversation_id: String,
    /// The epoch this commit was built from = the head the client believes it's at.
    pub based_on_epoch: i64,
    pub sender_id: String,
    /// TLS-serialized MLS Commit, base64.
    pub commit: String,
    #[serde(default)]
    pub added_user_id: Option<String>,
    /// CSV of device ids added by this commit, if any.
    #[serde(default)]
    pub added_device_ids: Option<String>,
    /// New published GroupInfo at the *resulting* epoch (`based_on_epoch + 1`),
    /// base64. Lets a future joiner external-join. Optional.
    #[serde(default)]
    pub group_info: Option<String>,
    /// Welcomes for devices added by this commit. Optional.
    #[serde(default)]
    pub welcomes: Vec<WelcomeBody>,
}

#[derive(Debug, Deserialize)]
pub struct WelcomeBody {
    pub recipient_id: String,
    pub recipient_device_id: String,
    /// TLS-serialized MLS Welcome, base64.
    pub welcome: String,
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum SubmitResponse {
    /// The commit won its epoch. The group head is now `epoch + 1`.
    Accepted { epoch: i64 },
    /// The client wasn't at the head. Here's the current head and the commits
    /// it's missing so it can re-base and resubmit — no fork possible.
    Rejected { head: i64, missing: Vec<CommitWire> },
}

#[derive(Debug, Serialize)]
pub struct CommitWire {
    pub epoch: i64,
    pub seq: i64,
    pub sender_id: String,
    /// base64
    pub commit: String,
    pub added_user_id: Option<String>,
    pub added_device_ids: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize)]
pub struct CommitsResponse {
    pub head: i64,
    pub commits: Vec<CommitWire>,
}

// ── Core logic ──────────────────────────────────────────────────────────────

fn b64_decode(s: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    Ok(base64::engine::general_purpose::STANDARD.decode(s)?)
}

fn b64_encode(b: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(b)
}

/// The group's head epoch = `MAX(epoch) + 1` (0 for an empty/unknown group).
pub async fn head_epoch(conn: &Connection, conversation_id: &str) -> Result<i64> {
    let mut rows = conn
        .query(
            "SELECT COALESCE(MAX(epoch), -1) + 1 FROM mls_commit_log WHERE conversation_id = ?1",
            libsql::params![conversation_id],
        )
        .await?;
    let row = rows.next().await?.ok_or_else(|| anyhow!("no head row"))?;
    Ok(row.get::<i64>(0)?)
}

/// Commits with `epoch >= since`, contiguous, in apply order.
pub async fn fetch_commits(conn: &Connection, conversation_id: &str, since: i64) -> Result<Vec<CommitWire>> {
    let mut rows = conn
        .query(
            "SELECT epoch, seq, sender_id, commit_data, added_user_id, added_device_ids, created_at \
             FROM mls_commit_log WHERE conversation_id = ?1 AND epoch >= ?2 ORDER BY epoch ASC, seq ASC",
            libsql::params![conversation_id, since],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().await? {
        let commit: Vec<u8> = r.get(3)?;
        out.push(CommitWire {
            epoch: r.get(0)?,
            seq: r.get(1)?,
            sender_id: r.get(2)?,
            commit: b64_encode(&commit),
            added_user_id: r.get::<Option<String>>(4).ok().flatten(),
            added_device_ids: r.get::<Option<String>>(5).ok().flatten(),
            created_at: r.get(6)?,
        });
    }
    Ok(out)
}

/// Submit a commit. Accepts iff `based_on_epoch` is the current head; otherwise
/// rejects with the head + the missing commits. See the module docs for why
/// this is race-free / fork-free / gap-free / append-only.
pub async fn submit_commit(conn: &Connection, body: &SubmitBody) -> Result<SubmitResponse> {
    let commit = b64_decode(&body.commit)?;

    // The atomic decision: insert this commit at `based_on_epoch` ONLY IF that
    // equals the current head. One statement → no read/write race.
    let affected = conn
        .execute(
            "INSERT INTO mls_commit_log \
                 (conversation_id, epoch, sender_id, commit_data, added_user_id, added_device_ids) \
             SELECT ?1, ?2, ?3, ?4, ?5, ?6 \
             WHERE ?2 = (SELECT COALESCE(MAX(epoch), -1) + 1 FROM mls_commit_log WHERE conversation_id = ?1) \
             ON CONFLICT(conversation_id, epoch) DO NOTHING",
            libsql::params![
                body.conversation_id.clone(),
                body.based_on_epoch,
                body.sender_id.clone(),
                commit,
                body.added_user_id.clone(),
                body.added_device_ids.clone(),
            ],
        )
        .await?;

    if affected == 0 {
        let head = head_epoch(conn, &body.conversation_id).await?;
        let missing = fetch_commits(conn, &body.conversation_id, body.based_on_epoch).await?;
        return Ok(SubmitResponse::Rejected { head, missing });
    }

    // Won the epoch. Publish the resulting-epoch GroupInfo + any Welcomes so a
    // future joiner / newly-added device can come online. (P1: best-effort
    // after the commit lands; P2 makes the whole submit one transaction.)
    if let Some(gi_b64) = &body.group_info {
        let gi = b64_decode(gi_b64)?;
        conn.execute(
            "INSERT INTO mls_group_info (conversation_id, epoch, group_info, updated_by_device_id) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(conversation_id) DO UPDATE SET \
                 epoch = excluded.epoch, \
                 group_info = excluded.group_info, \
                 updated_by_device_id = excluded.updated_by_device_id, \
                 updated_at = datetime('now')",
            libsql::params![
                body.conversation_id.clone(),
                body.based_on_epoch + 1,
                gi,
                body.sender_id.clone(),
            ],
        )
        .await?;
    }
    for w in &body.welcomes {
        let welcome = b64_decode(&w.welcome)?;
        conn.execute(
            "INSERT INTO mls_welcome \
                 (id, conversation_id, recipient_id, welcome_data, recipient_device_id) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            libsql::params![
                ulid::Ulid::new().to_string(),
                body.conversation_id.clone(),
                w.recipient_id.clone(),
                welcome,
                w.recipient_device_id.clone(),
            ],
        )
        .await?;
    }

    Ok(SubmitResponse::Accepted {
        epoch: body.based_on_epoch,
    })
}
