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

use std::collections::HashMap;

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
///
/// Abstracted by the `Submit` action in `specs/tla/CommitLog.tla` (Spec A,
/// I1/I2): the conditional insert here is the spec's `SoundSubmit => b = Head`
/// guard, and TLC proves that under any interleaving of racing submitters the
/// log stays one-per-epoch / gapless / head-monotone. The teeth config
/// (`CommitLogBroken.cfg`) drops exactly this guard and TLC forks the log.
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

// ─── Commit-log retention floor (I4, issue #539) ─────────────────────────────
//
// `mls_commit_log` is append-only and, without a floor, grows unbounded with
// membership-churn × time per conversation. The Delivery Service (sole writer)
// prunes commits BELOW a retention floor so storage + a long-offline member's
// catch-up cost stay bounded, WITHOUT ever dropping a commit a current member
// still needs. Two tiers, faithful to `specs/tla/Delivery.tla` (Spec B, I3/I4):
//
//   * TIER 1 (free, zero loss). The floor is kept at/below `min_since` — the
//     MIN applied epoch across all CURRENT member DEVICES (the signal
//     `mls_commit_since` records on each catch-up). Everyone still needs commits
//     `>= min_since`, so nothing anyone is waiting on is deleted. This is exactly
//     the spec's SOUND `GCBound = Min(cursor over members)` and its
//     `NoLossForCurrentMember` invariant — the floor is guarded by the SLOWEST
//     member, NEVER the fastest (the spec's refuted `Max` variant).
//
//   * TIER 2 (hard cap). `head - PRUNE_MAX_BEHIND_HEAD` bounds the log length even
//     against a perpetually-offline device that pins `min_since` low forever. When
//     it exceeds the Tier-1 floor the straggler is pruned past; on its return it
//     reads an earliest-available epoch above its own, trips the client gap
//     detector (`pollis_core::commands::mls::invariants::classify` →
//     `GapRecover`), and external-joins at head — forfeiting only the pruned-gap
//     messages (accepted loss #1, "messages sent before you joined the tree").
//     `may_rejoin` (I5) still blocks a removed/revoked device from that rejoin.

/// Tier-1 slack: retain this many epochs BELOW the slowest current member's
/// applied epoch. Pure conservative buffer — a member at `min_since` needs
/// commits from `min_since` onward, so keeping `min_since - K` never drops
/// anything it is waiting on; the extra slack absorbs in-flight re-fetches.
pub const PRUNE_SLACK_EPOCHS: i64 = 8;

/// Tier-2 hard cap: the commit log is never retained deeper than this many
/// epochs behind the head, EVEN IF a current member is further behind (that
/// straggler recovers via external-join — accepted loss #1). Bounds storage +
/// catch-up against a never-returning device.
pub const PRUNE_MAX_BEHIND_HEAD: i64 = 512;

/// The retention floor (EXCLUSIVE): the prune deletes commits with `epoch < floor`.
///
/// Pure so it can be unit-/property-tested in isolation (see the tests below) and
/// audited against `specs/tla/Delivery.tla`. Inputs:
///   * `min_since` — MIN reported applied epoch over current member devices, or
///     `None` when no member has reported.
///   * `all_reported` — whether EVERY current member device has a reported
///     high-water. Tier 1 only prunes when the whole roster is accounted for; a
///     single unreported member means `min_since` is not a safe lower bound, so
///     Tier 1 contributes nothing (floor 0) and only Tier 2's cap applies.
///   * `head` — the group head epoch (`MAX(epoch)+1`).
///
/// `floor = max(tier1, tier2)`, clamped to `>= 0`:
///   * `tier1 = (min_since - PRUNE_SLACK_EPOCHS)` when `all_reported`, else 0.
///   * `tier2 = head - PRUNE_MAX_BEHIND_HEAD`.
///
/// The ONLY way the floor exceeds `min_since` is Tier 2 binding (a member more
/// than `PRUNE_MAX_BEHIND_HEAD` epochs behind head) — the deliberate,
/// documented accepted-loss path. Tier 1 alone is always `<= min_since`, i.e.
/// the spec's `NoLossForCurrentMember`.
pub fn prune_floor(min_since: Option<i64>, all_reported: bool, head: i64) -> i64 {
    let tier1 = match (all_reported, min_since) {
        (true, Some(m)) => (m - PRUNE_SLACK_EPOCHS).max(0),
        // A current member has not reported: `min_since` is not a safe lower
        // bound over the roster, so Tier 1 must not prune. Tier 2 still applies.
        _ => 0,
    };
    let tier2 = (head - PRUNE_MAX_BEHIND_HEAD).max(0);
    tier1.max(tier2)
}

/// Record device `device_id`'s commit-catch-up high-water for a conversation.
/// `since` is the epoch the client is caught up FROM — its current local MLS
/// epoch — so it still needs every commit `>= since`. Monotone: the upsert keeps
/// `MAX(existing, since)`, so a stale/reordered report can never LOWER a device's
/// recorded epoch (which would raise the floor and prune commits it still needs).
pub async fn record_commit_since(
    conn: &Connection,
    conversation_id: &str,
    user_id: &str,
    device_id: &str,
    since: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO mls_commit_since (conversation_id, user_id, device_id, since_epoch) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(conversation_id, user_id, device_id) DO UPDATE SET \
             since_epoch = MAX(since_epoch, excluded.since_epoch), \
             updated_at  = datetime('now')",
        libsql::params![
            conversation_id.to_string(),
            user_id.to_string(),
            device_id.to_string(),
            since,
        ],
    )
    .await?;
    Ok(())
}

/// The CURRENT member device ids for a conversation, read from the MAIN DB.
/// A `conversation_id` is a group id, a DM channel id, or a channel id (mirrors
/// [`crate::writes::is_member`]'s three membership shapes). Revoked devices are
/// excluded — a revoked device can never rejoin (I5), so it must not pin the
/// floor down. Cross-DB: membership lives on MAIN, the high-water on LOG, so the
/// floor is composed in Rust across the two connections (no single SQL join).
async fn current_member_devices(main: &Connection, conversation_id: &str) -> Result<Vec<String>> {
    let mut rows = main
        .query(
            "SELECT DISTINCT ud.device_id \
             FROM user_device ud \
             WHERE ud.revoked_at IS NULL AND ud.user_id IN ( \
                 SELECT user_id FROM dm_channel_member WHERE dm_channel_id = ?1 \
                 UNION \
                 SELECT user_id FROM group_member WHERE group_id = ?1 \
                 UNION \
                 SELECT gm.user_id FROM channels c \
                     JOIN group_member gm ON gm.group_id = c.group_id WHERE c.id = ?1 \
             )",
            libsql::params![conversation_id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().await? {
        out.push(r.get::<String>(0)?);
    }
    Ok(out)
}

/// Every reported `(device_id -> since_epoch)` high-water for a conversation,
/// read from the LOG DB.
async fn recorded_since(log: &Connection, conversation_id: &str) -> Result<HashMap<String, i64>> {
    let mut rows = log
        .query(
            "SELECT device_id, since_epoch FROM mls_commit_since WHERE conversation_id = ?1",
            libsql::params![conversation_id.to_string()],
        )
        .await?;
    let mut out = HashMap::new();
    while let Some(r) = rows.next().await? {
        out.insert(r.get::<String>(0)?, r.get::<i64>(1)?);
    }
    Ok(out)
}

/// Delete commits below `floor` (EXCLUSIVE) for a conversation. The single
/// retention write, kept small + public so it can be driven with an explicit
/// floor from tests. Never touches the UNIQUE(conversation_id, epoch) index
/// (fork-dedup, migration 000003 main DB) — a pure row DELETE leaves the
/// remaining epochs and their one-per-epoch guarantee intact. `floor <= 0` is a
/// no-op (nothing to prune below epoch 0).
pub async fn delete_commits_below(
    conn: &Connection,
    conversation_id: &str,
    floor: i64,
) -> Result<u64> {
    if floor <= 0 {
        return Ok(0);
    }
    let deleted = conn
        .execute(
            "DELETE FROM mls_commit_log WHERE conversation_id = ?1 AND epoch < ?2",
            libsql::params![conversation_id.to_string(), floor],
        )
        .await?;
    Ok(deleted)
}

/// Outcome of a prune pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PruneReport {
    /// The computed retention floor (exclusive).
    pub floor: i64,
    /// Commit rows deleted this pass.
    pub deleted: u64,
}

/// Compute the retention floor from live membership + reported high-waters and
/// prune the commit log below it. EVENT-DRIVEN — called on commit-append
/// (`submit`) and on a device's catch-up report (`commits` GET), never on a
/// timer (repo rule: no periodic polling). `main` supplies membership, `log`
/// supplies the high-waters + the commit log to prune.
pub async fn prune_commit_log(
    main: &Connection,
    log: &Connection,
    conversation_id: &str,
) -> Result<PruneReport> {
    let head = head_epoch(log, conversation_id).await?;
    let members = current_member_devices(main, conversation_id).await?;
    let recorded = recorded_since(log, conversation_id).await?;

    let (min_since, all_reported) = if members.is_empty() {
        // No current members (everyone left / the group was deleted): Tier 1 has
        // no lower bound to protect, so only Tier 2 bounds the orphaned log.
        (None, false)
    } else {
        let mut min: Option<i64> = None;
        let mut all = true;
        for d in &members {
            match recorded.get(d) {
                Some(&s) => min = Some(min.map_or(s, |m: i64| m.min(s))),
                None => all = false,
            }
        }
        (min, all)
    };

    let floor = prune_floor(min_since, all_reported, head);
    let deleted = delete_commits_below(log, conversation_id, floor).await?;
    Ok(PruneReport { floor, deleted })
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

// ─── Retention-floor unit tests (I4, issue #539) ─────────────────────────────
#[cfg(test)]
mod retention_tests {
    use super::*;

    /// TIER 1 (the spec's `NoLossForCurrentMember`): with the whole roster
    /// reported and Tier 2 not binding, the floor never exceeds the slowest
    /// member's applied epoch — so every current member keeps every commit it
    /// still needs.
    #[test]
    fn tier1_floor_never_above_slowest_member() {
        // head close to the slowest member ⇒ Tier 2 (`head - 512`) does not bind.
        let head = 20;
        for min_since in 0..=20i64 {
            let floor = prune_floor(Some(min_since), true, head);
            assert!(
                floor <= min_since,
                "floor {floor} must not exceed slowest member epoch {min_since}"
            );
            // And it is exactly the slack-buffered Tier-1 floor.
            assert_eq!(floor, (min_since - PRUNE_SLACK_EPOCHS).max(0));
        }
    }

    /// An unreported member (roster not fully accounted for) disables Tier 1: the
    /// floor collapses to Tier 2 only, so a member we cannot bound is never
    /// pruned past by Tier 1.
    #[test]
    fn tier1_disabled_until_whole_roster_reports() {
        // `all_reported = false` ⇒ Tier 1 contributes nothing.
        assert_eq!(prune_floor(Some(1_000), false, 100), 0);
        // Even with a known min, an incomplete roster keeps Tier 1 at 0.
        assert_eq!(prune_floor(Some(50), false, 40), 0);
        // No member has reported at all.
        assert_eq!(prune_floor(None, false, 40), 0);
    }

    /// TIER 2 (hard cap): a member stuck far below head is eventually pruned past
    /// so storage stays bounded — the floor rises to `head - PRUNE_MAX_BEHIND_HEAD`
    /// even though the straggler still "needs" those epochs (accepted loss #1).
    #[test]
    fn tier2_hard_cap_bounds_a_stuck_member() {
        let stuck = 1i64; // one member pinned at epoch 1 forever
        let head = 5_000i64; // group raced far ahead
        let floor = prune_floor(Some(stuck), true, head);
        assert_eq!(floor, head - PRUNE_MAX_BEHIND_HEAD);
        assert!(
            floor > stuck,
            "Tier 2 must prune past the stuck member to bound storage"
        );
    }

    /// Tier 2 clamps at 0 for a short log (no underflow), and the floor is never
    /// negative.
    #[test]
    fn floor_never_negative() {
        assert_eq!(prune_floor(None, false, 0), 0);
        assert_eq!(prune_floor(Some(0), true, 0), 0);
        assert_eq!(prune_floor(Some(3), true, 3), 0); // min(3)-8 → clamp 0; head 3 < cap
    }
}
