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

/// The group's head epoch as a **pure** function of the log's current
/// `MAX(epoch)`: `None` (empty log) → `0`, `Some(m)` → `m + 1`. This is the
/// `COALESCE(MAX(epoch), -1) + 1` SQL lifted out of [`head_epoch`] so Kani can
/// prove (a) the empty-log case yields `0` and **never underflows** `u64` (the
/// SQL's transient `-1` never surfaces in the head), and (b) `Some(m)` gives
/// `m + 1` with no wrap for any `m < u64::MAX` — a head that strictly exceeds the
/// max epoch, keeping the head monotone and the log gapless by construction (I1).
pub fn head_epoch_of(max_epoch: Option<u64>) -> u64 {
    match max_epoch {
        // Empty/unknown group: the first commit is built from epoch 0.
        None => 0,
        // A commit built from epoch `m` advances the group `m -> m + 1`, so the
        // head — the next epoch a member may commit from — is `m + 1`.
        Some(m) => m + 1,
    }
}

/// The Delivery Service's accept decision as a **pure** predicate: a submitted
/// commit is accepted IFF its `based_on_epoch` equals the current `head`. This
/// models the atomic `WHERE ?2 = (SELECT COALESCE(MAX(epoch), -1) + 1 …)` guard
/// in [`submit_commit`]. It is NOT wired into `submit_commit` — the real decision
/// must stay inside the single conditional `INSERT` to be race-free (two racing
/// writers are serialized by SQLite; a Rust-side check would reintroduce a
/// read/write TOCTOU). It is proved here as the model of record: `accepts` is
/// total, deterministic, and for a fixed head admits exactly ONE epoch, so no two
/// distinct epochs are ever both accepted at one head — no fork.
pub fn accepts(based_on_epoch: u64, head: u64) -> bool {
    based_on_epoch == head
}

/// The group's head epoch = `MAX(epoch) + 1` (0 for an empty/unknown group).
/// Reads `MAX(epoch)` and applies the pure, Kani-proved [`head_epoch_of`].
pub async fn head_epoch(conn: &Connection, conversation_id: &str) -> Result<i64> {
    let mut rows = conn
        .query(
            "SELECT MAX(epoch) FROM mls_commit_log WHERE conversation_id = ?1",
            libsql::params![conversation_id],
        )
        .await?;
    let row = rows.next().await?.ok_or_else(|| anyhow!("no head row"))?;
    // `MAX(epoch)` over an empty set is SQL NULL → `None` → head 0. Epochs are
    // non-negative by construction, so the `i64 -> u64` mapping is exact.
    let max_epoch: Option<u64> = row.get::<Option<i64>>(0)?.map(|m| m as u64);
    Ok(head_epoch_of(max_epoch) as i64)
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

    // Decode the GroupInfo / Welcome blobs BEFORE opening the transaction so a
    // malformed payload is the same clean `Err` it is today — no half-open
    // transaction, and the caller's error/HTTP mapping is unchanged.
    let group_info = body.group_info.as_deref().map(b64_decode).transpose()?;
    let welcomes = body
        .welcomes
        .iter()
        .map(|w| Ok((w, b64_decode(&w.welcome)?)))
        .collect::<Result<Vec<_>>>()?;

    // The commit, GroupInfo, and Welcome(s) for one submit are ONE bundle: they
    // all-commit-or-all-rollback. A partial write (commit lands, Welcome fails)
    // used to be possible and was only recoverable via the client's
    // external-join fallback — the safety net was the exception path, not a
    // guarantee. IMMEDIATE takes the write lock at BEGIN, so concurrent
    // submitters serialize on it (busy_timeout) exactly as the bare conditional
    // INSERT did — one winner per epoch, no fork.
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;

    // The atomic decision: insert this commit at `based_on_epoch` ONLY IF that
    // equals the current head. One statement → no read/write race.
    let affected = tx
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
        // Not at the head: nothing was written. Read the current head + the
        // commits the client is missing within this same view, then roll back
        // (no-op — the bundle wrote nothing) and reject.
        let head = head_epoch(&tx, &body.conversation_id).await?;
        let missing = fetch_commits(&tx, &body.conversation_id, body.based_on_epoch).await?;
        tx.rollback().await?;
        return Ok(SubmitResponse::Rejected { head, missing });
    }

    // Won the epoch. Publish the resulting-epoch GroupInfo + any Welcomes so a
    // future joiner / newly-added device can come online. All part of the same
    // transaction as the commit above, so a failure here rolls the commit back
    // too — the recipient never sees a commit with no matching Welcome.
    if let Some(gi) = &group_info {
        // Epoch-monotone guard (matches the standalone /v1/group-info upsert in
        // `writes::upsert_group_info`): an older epoch can never clobber a newer
        // one, so both writers of `mls_group_info` obey one rule.
        tx.execute(
            "INSERT INTO mls_group_info (conversation_id, epoch, group_info, updated_by_device_id) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(conversation_id) DO UPDATE SET \
                 epoch = excluded.epoch, \
                 group_info = excluded.group_info, \
                 updated_by_device_id = excluded.updated_by_device_id, \
                 updated_at = datetime('now') \
             WHERE excluded.epoch > mls_group_info.epoch",
            libsql::params![
                body.conversation_id.clone(),
                body.based_on_epoch + 1,
                gi.clone(),
                body.sender_id.clone(),
            ],
        )
        .await?;
    }
    for (w, welcome) in &welcomes {
        // Idempotent on the UNIQUE (conversation_id, recipient_id,
        // recipient_device_id) tuple (migration 000002 (commit-log DB)): a re-sent Welcome for
        // the same recipient/device refreshes the blob and re-arms delivery
        // (`delivered = 0`) instead of erroring or stacking a duplicate row — so
        // a resubmit/retry of this commit bundle can never wedge on a dup.
        tx.execute(
            "INSERT INTO mls_welcome \
                 (id, conversation_id, recipient_id, welcome_data, recipient_device_id, delivered) \
             VALUES (?1, ?2, ?3, ?4, ?5, 0) \
             ON CONFLICT(conversation_id, recipient_id, recipient_device_id) DO UPDATE SET \
                 welcome_data = excluded.welcome_data, \
                 delivered = 0",
            libsql::params![
                ulid::Ulid::new().to_string(),
                body.conversation_id.clone(),
                w.recipient_id.clone(),
                welcome.clone(),
                w.recipient_device_id.clone(),
            ],
        )
        .await?;
    }

    tx.commit().await?;

    Ok(SubmitResponse::Accepted {
        epoch: body.based_on_epoch,
    })
}

// ─── Kani proof harnesses (I1 — DS head arithmetic + accept decision) ─────────
//
// Behind `#[cfg(kani)]` only. These prove the two pure functions above:
// `head_epoch_of` never wraps (empty-log → 0, `Some(m)` → `m + 1`), and `accepts`
// admits exactly one epoch per head (no fork). No `Vec`/`String`, no async, no DB
// — pure integer reasoning, so CBMC is instantaneous.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// I1: the head arithmetic never underflows/wraps. The empty-log case is `0`
    /// (the SQL's transient `-1` never surfaces); `Some(m)` is `m + 1 > m` for
    /// every representable `m` short of the single wrapping input.
    #[kani::proof]
    fn i1_head_epoch_no_wrap() {
        // Empty log → head 0, no underflow.
        assert!(head_epoch_of(None) == 0);

        let m: u64 = kani::any();
        // Real epochs sit far below u64::MAX; exclude only the lone wrapping input
        // so `m + 1` is exercised across the entire remaining range.
        kani::assume(m < u64::MAX);
        let head = head_epoch_of(Some(m));
        assert!(head == m + 1);
        // The head strictly exceeds the max epoch: monotone head, no wrap.
        assert!(head > m);
    }

    /// I1: the accept decision admits exactly one epoch per head — no two distinct
    /// epochs are ever both accepted (no fork), and any stale/forward submit is
    /// rejected.
    #[kani::proof]
    fn i1_accept_single_epoch() {
        let head: u64 = kani::any();
        let a: u64 = kani::any();
        let b: u64 = kani::any();
        kani::assume(head <= 3);
        kani::assume(a <= 3);
        kani::assume(b <= 3);

        // Two accepted submits at one head must be the same epoch.
        if accepts(a, head) && accepts(b, head) {
            assert!(a == b);
        }
        // A stale (`based_on < head`) or forward-gap (`based_on > head`) submit is
        // rejected — the only accepted epoch is the head itself.
        if a != head {
            assert!(!accepts(a, head));
        }
    }

    /// Negative harness: a broken head calc that translates the SQL literally as
    /// unsigned `(max.unwrap_or(u64::MAX)) + 1` — the empty-log `-1` underflow the
    /// real `head_epoch_of(None) == 0` avoids. `should_panic`: Kani must find the
    /// wrap (empty log → head 0 assertion fails, or arithmetic overflow).
    fn head_epoch_of_mutant(max_epoch: Option<u64>) -> u64 {
        // BUG: models `COALESCE(MAX, -1) + 1` by reusing u64::MAX as "-1" and
        // adding 1 → wraps to 0 only after an overflowing add. On an empty log
        // this overflows (`u64::MAX + 1`), which CBMC flags.
        max_epoch.unwrap_or(u64::MAX) + 1
    }

    #[kani::proof]
    #[kani::should_panic]
    fn i1_head_epoch_mutant_refuted() {
        // Empty log drives the mutant's `u64::MAX + 1` overflow.
        let head = head_epoch_of_mutant(None);
        assert!(head == 0);
    }
}
