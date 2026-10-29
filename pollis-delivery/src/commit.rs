//! Commit serialization.
//!
//! `mls_commit_log.epoch` is the *source* epoch: a commit with `epoch = N`
//! advances the group `N -> N+1`, so the head is `MAX(epoch) + 1`. A commit is
//! accepted iff `based_on_epoch` equals the head, decided by one conditional
//! `INSERT ... SELECT ... WHERE` so racing writers get exactly one winner (no
//! fork), stale or future submits insert nothing (no gap), and the log is only
//! appended to by this service. These are properties of this code, not of DB
//! triggers.
//!
//! Suite generations (#454 P4) widen the key to `(generation, epoch)`, ordered
//! lexicographically: epoch 0 is accepted only as the opener of generation
//! `N + 1`, and only when `closes_epoch` names generation `N`'s current head.
//! Old rows and pre-hybrid clients are generation 0. See
//! `.codesight/wiki/mls.md` → "Suite generations".

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use libsql::Connection;
use crate::util::{b64, b64_decode};

// Request bodies live in `pollis-api` so client and server share one
// declaration.
pub use pollis_api::commit::*;

/// The head epoch as a pure function of `MAX(epoch)`: `None` → `0`, `Some(m)` →
/// `m + 1`. Lifted out of [`head_epoch`] so Kani can prove it never wraps (I1).
pub fn head_epoch_of(max_epoch: Option<u64>) -> u64 {
    match max_epoch {
        None => 0,
        Some(m) => m + 1,
    }
}

/// The accept decision as a pure predicate over `(generation, epoch)`, the
/// model of the `WHERE` guard in [`submit_commit`]. Not called by it: the real
/// decision must stay inside the single conditional `INSERT` to be race-free.
///
/// Accepts only: continuing the head lineage at its head, or opening
/// `head_generation + 1` at epoch 0 with `closes_epoch == Some(head_epoch)`.
/// Both can be satisfiable at one head; the atomic INSERT picks one.
pub fn accepts(
    generation: u64,
    based_on_epoch: u64,
    closes_epoch: Option<u64>,
    head_generation: u64,
    head_epoch: u64,
) -> bool {
    if generation == head_generation {
        based_on_epoch == head_epoch
    } else if generation == head_generation + 1 {
        based_on_epoch == 0 && closes_epoch == Some(head_epoch)
    } else {
        false
    }
}

/// The conversation's newest lineage, `COALESCE(MAX(generation), 0)`.
pub async fn head_generation(conn: &Connection, conversation_id: &str) -> Result<i64> {
    let mut rows = conn
        .query(
            "SELECT MAX(generation) FROM mls_commit_log WHERE conversation_id = ?1",
            libsql::params![conversation_id],
        )
        .await?;
    let row = rows.next().await?.ok_or_else(|| anyhow!("no head row"))?;
    Ok(row.get::<Option<i64>>(0)?.unwrap_or(0))
}

/// The head epoch within one lineage (0 for an empty one).
pub async fn head_epoch_in(
    conn: &Connection,
    conversation_id: &str,
    generation: i64,
) -> Result<i64> {
    let mut rows = conn
        .query(
            "SELECT MAX(epoch) FROM mls_commit_log WHERE conversation_id = ?1 AND generation = ?2",
            libsql::params![conversation_id, generation],
        )
        .await?;
    let row = rows.next().await?.ok_or_else(|| anyhow!("no head row"))?;
    let max_epoch: Option<u64> = row.get::<Option<i64>>(0)?.map(|m| m as u64);
    Ok(head_epoch_of(max_epoch) as i64)
}

/// The group's head epoch in its current lineage.
pub async fn head_epoch(conn: &Connection, conversation_id: &str) -> Result<i64> {
    let gen = head_generation(conn, conversation_id).await?;
    head_epoch_in(conn, conversation_id, gen).await
}

/// Commits in `generation` with `epoch >= since`, in apply order. Scoped to one
/// lineage: generations are different groups and are never interleaved.
pub async fn fetch_commits(
    conn: &Connection,
    conversation_id: &str,
    generation: i64,
    since: i64,
) -> Result<Vec<CommitWire>> {
    let mut rows = conn
        .query(
            "SELECT epoch, seq, sender_id, commit_data, added_user_id, added_device_ids, created_at \
             FROM mls_commit_log WHERE conversation_id = ?1 AND generation = ?2 AND epoch >= ?3 \
             ORDER BY epoch ASC, seq ASC",
            libsql::params![conversation_id, generation, since],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(r) = rows.next().await? {
        let commit: Vec<u8> = r.get(3)?;
        out.push(CommitWire {
            generation,
            epoch: r.get(0)?,
            seq: r.get(1)?,
            sender_id: r.get(2)?,
            commit: b64(&commit),
            added_user_id: r.get::<Option<String>>(4).ok().flatten(),
            added_device_ids: r.get::<Option<String>>(5).ok().flatten(),
            created_at: r.get(6)?,
        });
    }
    Ok(out)
}

/// The verdict of `POST /v1/commits`. Wider than the wire [`SubmitResponse`]
/// because a bundle can be refused outright, and `SubmitResponse` is a shipped
/// wire type that must not grow variants a deployed client cannot decode.
#[derive(Debug)]
pub enum SubmitVerdict {
    Response(SubmitResponse),
    /// A Welcome in the bundle is not one this submitter may write (C1); nothing
    /// was written.
    Forbidden,
    /// The bundle is malformed. `&'static str` so no caller input is echoed.
    Invalid(&'static str),
}

/// Submit a commit. Accepts iff `based_on_epoch` is the current head; otherwise
/// rejects with the head and the missing commits. Modelled by `Submit` in
/// `specs/tla/CommitLog.tla`.
///
/// Takes the main connection too because the Welcome gates (C1) check roster
/// rows that live there. The gates, shared with
/// [`crate::writes::apply_welcomes_resubmit`] and applied on the no-auth path
/// too:
///
///   1. A Welcome may not exceed [`crate::writes::WELCOME_MAX_BYTES`].
///   2. The recipient must be in the desired roster
///      ([`crate::writes::in_desired_roster`]), not merely a member: invitees
///      are Welcomed at invite time.
///   3. An undelivered Welcome published by another member at this generation
///      or later may only be overwritten by a group admin; a Welcome for a
///      newer generation always wins so a suite migration cannot be stranded.
pub async fn submit_commit(
    main_conn: &Connection,
    log_conn: &Connection,
    body: &SubmitBody,
) -> Result<SubmitVerdict> {
    let conn = log_conn;
    let commit = b64_decode(&body.commit)?;

    // Decode before opening the transaction so a malformed payload is a clean
    // `Err` with no half-open transaction.
    let group_info = body.group_info.as_deref().map(b64_decode).transpose()?;
    let welcomes = body
        .welcomes
        .iter()
        .map(|w| Ok((w, b64_decode(&w.welcome)?)))
        .collect::<Result<Vec<_>>>()?;

    // Gates 1 + 2 run before anything is written. Membership is cached per
    // recipient account, since a bundle carries one Welcome per device.
    let mut membership: HashMap<&str, bool> = HashMap::new();
    for (w, welcome) in &welcomes {
        if welcome.is_empty() || welcome.len() > crate::writes::WELCOME_MAX_BYTES {
            return Ok(SubmitVerdict::Invalid("welcome size out of range"));
        }
        let admissible = match membership.get(w.recipient_id.as_str()) {
            Some(known) => *known,
            None => {
                let answer = crate::writes::in_desired_roster(
                    main_conn,
                    &body.conversation_id,
                    &w.recipient_id,
                )
                .await?;
                membership.insert(w.recipient_id.as_str(), answer);
                answer
            }
        };
        if !admissible {
            return Ok(SubmitVerdict::Forbidden);
        }
    }

    // Gate 3's admin override, read on the main DB before the transaction: the
    // log-DB transaction cannot see that table when the DBs are separate.
    let may_override = if welcomes.is_empty() {
        true
    } else {
        crate::writes::is_conversation_admin(main_conn, &body.conversation_id, &body.sender_id)
            .await?
    };

    // The commit, GroupInfo and Welcome(s) are one all-or-nothing bundle.
    // IMMEDIATE takes the write lock at BEGIN, so concurrent submitters serialize
    // on it: one winner per epoch.
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;

    // The atomic decision, mirroring [`accepts`]: (a) continue the head lineage at
    // its head, or (b) open `head_generation + 1` at epoch 0 naming the old head in
    // `closes_epoch`, a CAS on the old lineage too. `?8 IS NOT NULL` makes an
    // omitted `closes_epoch` an explicit reject. The UNIQUE index backstops a
    // double open.
    let affected = tx
        .execute(
            "INSERT INTO mls_commit_log \
                 (conversation_id, generation, epoch, sender_id, commit_data, added_user_id, added_device_ids) \
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7 \
             WHERE \
               ( ?2 = (SELECT COALESCE(MAX(generation), 0) FROM mls_commit_log WHERE conversation_id = ?1) \
                 AND ?3 = (SELECT COALESCE(MAX(epoch), -1) + 1 FROM mls_commit_log \
                            WHERE conversation_id = ?1 AND generation = ?2) ) \
               OR \
               ( ?3 = 0 \
                 AND ?2 = (SELECT COALESCE(MAX(generation), 0) FROM mls_commit_log WHERE conversation_id = ?1) + 1 \
                 AND ?8 IS NOT NULL \
                 AND ?8 = (SELECT COALESCE(MAX(epoch), -1) + 1 FROM mls_commit_log \
                            WHERE conversation_id = ?1 \
                              AND generation = (SELECT COALESCE(MAX(generation), 0) FROM mls_commit_log \
                                                 WHERE conversation_id = ?1)) ) \
             ON CONFLICT(conversation_id, generation, epoch) DO NOTHING",
            libsql::params![
                body.conversation_id.clone(),
                body.generation,
                body.based_on_epoch,
                body.sender_id.clone(),
                commit,
                body.added_user_id.clone(),
                body.added_device_ids.clone(),
                body.closes_epoch,
            ],
        )
        .await?;

    if affected == 0 {
        // Not at the head: nothing was written. Report the head and missing commits
        // of the submitter's own generation (the lineage it must finish draining),
        // plus `head_generation` so it can tell a newer one exists.
        let head = head_epoch_in(&tx, &body.conversation_id, body.generation).await?;
        let head_gen = head_generation(&tx, &body.conversation_id).await?;
        let missing =
            fetch_commits(&tx, &body.conversation_id, body.generation, body.based_on_epoch).await?;
        tx.rollback().await?;
        return Ok(SubmitVerdict::Response(SubmitResponse::Rejected {
            head,
            head_generation: head_gen,
            missing,
        }));
    }

    // Won the epoch: publish GroupInfo and Welcomes in the same transaction, so a
    // recipient never sees a commit with no matching Welcome.
    if let Some(gi) = group_info {
        // (generation, epoch)-monotone, like `writes::upsert_group_info`. The
        // generation term matters: a successor's epoch 1 is numerically below the
        // retired lineage's last epoch.
        tx.execute(
            "INSERT INTO mls_group_info (conversation_id, generation, epoch, group_info, updated_by_device_id) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(conversation_id) DO UPDATE SET \
                 generation = excluded.generation, \
                 epoch = excluded.epoch, \
                 group_info = excluded.group_info, \
                 updated_by_device_id = excluded.updated_by_device_id, \
                 updated_at = datetime('now') \
             WHERE excluded.generation > mls_group_info.generation \
                OR (excluded.generation = mls_group_info.generation \
                    AND excluded.epoch > mls_group_info.epoch)",
            libsql::params![
                body.conversation_id.clone(),
                body.generation,
                body.based_on_epoch + 1,
                gi,
                body.sender_id.clone(),
            ],
        )
        .await?;
    }
    for (w, welcome) in welcomes {
        // Gate 3, inside the IMMEDIATE transaction so racing submitters cannot both
        // see "no conflict". Delivered rows are spent, unattributed (pre-000006) rows
        // have no owner, and a sender may refresh its own row.
        if !may_override {
            let mut rows = tx
                .query(
                    "SELECT delivered, submitted_by, generation FROM mls_welcome \
                     WHERE conversation_id = ?1 AND recipient_id = ?2 AND recipient_device_id = ?3",
                    libsql::params![
                        body.conversation_id.clone(),
                        w.recipient_id.clone(),
                        w.recipient_device_id.clone(),
                    ],
                )
                .await?;
            let conflict = match rows.next().await? {
                Some(row) => {
                    let delivered: i64 = row.get(0)?;
                    let owner: Option<String> = row.get(1)?;
                    let existing_generation: i64 = row.get(2)?;
                    delivered == 0
                        && owner.is_some()
                        && owner.as_deref() != Some(body.sender_id.as_str())
                        && existing_generation >= body.generation
                }
                None => false,
            };
            drop(rows);
            if conflict {
                tx.rollback().await?;
                return Ok(SubmitVerdict::Forbidden);
            }
        }

        // Upsert on (conversation_id, recipient_id, recipient_device_id) so a resend
        // refreshes the blob and re-arms delivery. `generation` is the lineage the
        // Welcome admits into, so the recipient can finish draining its current one
        // first (`max_past_epochs = 0`). `submitted_by` is what
        // `/v1/welcomes/resubmit` protects.
        tx.execute(
            "INSERT INTO mls_welcome \
                 (id, conversation_id, generation, recipient_id, welcome_data, recipient_device_id, delivered, submitted_by) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7) \
             ON CONFLICT(conversation_id, recipient_id, recipient_device_id) DO UPDATE SET \
                 generation = excluded.generation, \
                 welcome_data = excluded.welcome_data, \
                 submitted_by = excluded.submitted_by, \
                 delivered = 0",
            libsql::params![
                ulid::Ulid::new().to_string(),
                body.conversation_id.clone(),
                body.generation,
                w.recipient_id.clone(),
                welcome,
                w.recipient_device_id.clone(),
                body.sender_id.clone(),
            ],
        )
        .await?;
    }

    tx.commit().await?;

    Ok(SubmitVerdict::Response(SubmitResponse::Accepted {
        generation: body.generation,
        epoch: body.based_on_epoch,
    }))
}

// Commit-log retention floor (I4, #539, `specs/tla/Delivery.tla`): Tier 1 never
// prunes past the slowest current member device; Tier 2 caps the log at
// `PRUNE_MAX_BEHIND_HEAD` behind head, and a straggler pruned past recovers by
// external join (accepted loss #1). Under generations the floor is a pair: an
// epoch floor within the head generation (devices on an older lineage count as
// unreported) and a generation floor that retires closed lineages. See
// `.codesight/wiki/mls.md` → "Commit-log retention".

/// Tier-1 slack: epochs retained below the slowest member's applied epoch, to
/// absorb in-flight re-fetches.
pub const PRUNE_SLACK_EPOCHS: i64 = 8;

/// Tier-2 hard cap: the log is never retained deeper than this behind head,
/// even if a current member is further behind.
pub const PRUNE_MAX_BEHIND_HEAD: i64 = 512;

/// The retention floor (EXCLUSIVE): the prune deletes commits with `epoch < floor`.
///
/// `floor = min(max(tier1, tier2), head - 1)`, clamped to `>= 0`, where
/// `tier1 = min_since - PRUNE_SLACK_EPOCHS` only when `all_reported` (else 0)
/// and `tier2 = head - PRUNE_MAX_BEHIND_HEAD`.
///
/// The `head - 1` clamp (#681): `min_since` comes from untrusted client
/// reports, and a floor `>= head` would delete the whole live log. Clamping to
/// `head - 1` always keeps the head commit; it never bites an honest report.
pub fn prune_floor(min_since: Option<i64>, all_reported: bool, head: i64) -> i64 {
    let tier1 = match (all_reported, min_since) {
        (true, Some(m)) => m.saturating_sub(PRUNE_SLACK_EPOCHS).max(0),
        // An unreported member means `min_since` is not a safe bound: no Tier 1.
        _ => 0,
    };
    let tier2 = (head - PRUNE_MAX_BEHIND_HEAD).max(0);
    // Retain the head commit for any report, honest or forged (#681).
    let head_ceiling = (head - 1).max(0);
    tier1.max(tier2).min(head_ceiling)
}

/// The generation floor (EXCLUSIVE): lineages with `generation < floor` are
/// retired. `tier1 = min_reported_generation` when `all_reported`, `tier2 =
/// head_generation` once `head_epoch >= PRUNE_MAX_BEHIND_HEAD`; the clamp to
/// `head_generation` means the live lineage is never retired.
pub fn closed_generation_floor(
    head_generation: i64,
    head_epoch: i64,
    min_reported_generation: Option<i64>,
    all_reported: bool,
) -> i64 {
    let tier1 = match (all_reported, min_reported_generation) {
        (true, Some(g)) => g.max(0),
        _ => 0,
    };
    let tier2 = if head_epoch >= PRUNE_MAX_BEHIND_HEAD {
        head_generation
    } else {
        0
    };
    tier1.max(tier2).min(head_generation).max(0)
}

/// Record a device's commit catch-up high-water: it still needs every commit
/// `>= since` in `generation`. Monotone on the pair, compared lexicographically,
/// so a stale report can never lower it (which would prune commits it needs).
pub async fn record_commit_since(
    conn: &Connection,
    conversation_id: &str,
    user_id: &str,
    device_id: &str,
    generation: i64,
    since: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO mls_commit_since (conversation_id, user_id, device_id, generation, since_epoch) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(conversation_id, user_id, device_id) DO UPDATE SET \
             generation  = excluded.generation, \
             since_epoch = excluded.since_epoch, \
             updated_at  = datetime('now') \
         WHERE excluded.generation > mls_commit_since.generation \
            OR (excluded.generation = mls_commit_since.generation \
                AND excluded.since_epoch > mls_commit_since.since_epoch)",
        libsql::params![
            conversation_id.to_string(),
            user_id.to_string(),
            device_id.to_string(),
            generation,
            since,
        ],
    )
    .await?;
    Ok(())
}

/// Current, unrevoked member device ids for a conversation, read from the main
/// DB (group, DM or channel id). Revoked devices can never rejoin (I5), so they
/// must not pin the floor.
///
/// `pub(crate)` for the envelope-GC roster-parity test in [`crate::messages`]
/// (#722).
pub(crate) async fn current_member_devices(
    main: &Connection,
    conversation_id: &str,
) -> Result<Vec<String>> {
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

/// Every reported `device_id -> (generation, since_epoch)` for a conversation.
async fn recorded_since(
    log: &Connection,
    conversation_id: &str,
) -> Result<HashMap<String, (i64, i64)>> {
    let mut rows = log
        .query(
            "SELECT device_id, generation, since_epoch FROM mls_commit_since WHERE conversation_id = ?1",
            libsql::params![conversation_id.to_string()],
        )
        .await?;
    let mut out = HashMap::new();
    while let Some(r) = rows.next().await? {
        out.insert(r.get::<String>(0)?, (r.get::<i64>(1)?, r.get::<i64>(2)?));
    }
    Ok(out)
}

/// Delete commits with `epoch < floor` within one lineage. `floor <= 0` is a
/// no-op.
pub async fn delete_commits_below(
    conn: &Connection,
    conversation_id: &str,
    generation: i64,
    floor: i64,
) -> Result<u64> {
    if floor <= 0 {
        return Ok(0);
    }
    let deleted = conn
        .execute(
            "DELETE FROM mls_commit_log \
             WHERE conversation_id = ?1 AND generation = ?2 AND epoch < ?3",
            libsql::params![conversation_id.to_string(), generation, floor],
        )
        .await?;
    Ok(deleted)
}

/// Delete every commit in lineages below `floor`. Epoch pruning never reaches
/// closed lineages, so they are retired here. `floor <= 0` is a no-op.
pub async fn delete_generations_below(
    conn: &Connection,
    conversation_id: &str,
    floor: i64,
) -> Result<u64> {
    if floor <= 0 {
        return Ok(0);
    }
    let deleted = conn
        .execute(
            "DELETE FROM mls_commit_log WHERE conversation_id = ?1 AND generation < ?2",
            libsql::params![conversation_id.to_string(), floor],
        )
        .await?;
    Ok(deleted)
}

/// Outcome of a prune pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PruneReport {
    pub floor: i64,
    pub generation: i64,
    /// Closed lineages below this generation were retired.
    pub generation_floor: i64,
    pub deleted: u64,
}

/// Compute the retention floor and prune below it. Event-driven: called on
/// commit append and on a device's catch-up report, never on a timer.
pub async fn prune_commit_log(
    main: &Connection,
    log: &Connection,
    conversation_id: &str,
) -> Result<PruneReport> {
    let generation = head_generation(log, conversation_id).await?;
    let head = head_epoch_in(log, conversation_id, generation).await?;
    let members = current_member_devices(main, conversation_id).await?;
    let recorded = recorded_since(log, conversation_id).await?;

    // Epoch-wise, a device on an older lineage counts as unreported (disabling
    // Tier 1); generation-wise, every reported device counts.
    let (min_since, all_reported_in_head_gen, min_generation, all_reported) = if members.is_empty() {
        // No current members: only Tier 2 bounds the orphaned log.
        (None, false, None, false)
    } else {
        let mut min_epoch: Option<i64> = None;
        let mut all_epoch = true;
        let mut min_gen: Option<i64> = None;
        let mut all_gen = true;
        for d in &members {
            match recorded.get(d) {
                Some(&(g, s)) => {
                    min_gen = Some(min_gen.map_or(g, |m: i64| m.min(g)));
                    if g == generation {
                        min_epoch = Some(min_epoch.map_or(s, |m: i64| m.min(s)));
                    } else {
                        all_epoch = false;
                    }
                }
                None => {
                    all_epoch = false;
                    all_gen = false;
                }
            }
        }
        (min_epoch, all_epoch, min_gen, all_gen)
    };

    let floor = prune_floor(min_since, all_reported_in_head_gen, head);
    let generation_floor = closed_generation_floor(generation, head, min_generation, all_reported);

    let mut deleted = delete_generations_below(log, conversation_id, generation_floor).await?;
    deleted += delete_commits_below(log, conversation_id, generation, floor).await?;
    Ok(PruneReport {
        floor,
        generation,
        generation_floor,
        deleted,
    })
}

// Kani proofs of the pure head arithmetic and accept/floor decisions. Each
// property has a `should_panic` mutant that proves the harness has teeth.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// I1: the head arithmetic never wraps.
    #[kani::proof]
    fn i1_head_epoch_no_wrap() {
        assert!(head_epoch_of(None) == 0);

        let m: u64 = kani::any();
        // Exclude only the lone wrapping input.
        kani::assume(m < u64::MAX);
        let head = head_epoch_of(Some(m));
        assert!(head == m + 1);
        assert!(head > m);
    }

    /// I1 within a lineage: at a fixed head exactly one epoch of the head
    /// generation is accepted.
    #[kani::proof]
    fn i1_accept_single_epoch() {
        let head_gen: u64 = kani::any();
        let head: u64 = kani::any();
        let a: u64 = kani::any();
        let b: u64 = kani::any();
        kani::assume(head_gen <= 3);
        kani::assume(head <= 3);
        kani::assume(a <= 3);
        kani::assume(b <= 3);

        if accepts(head_gen, a, None, head_gen, head) && accepts(head_gen, b, None, head_gen, head) {
            assert!(a == b);
        }
        // `closes_epoch` is symbolic: naming a closed head must not buy a
        // same-generation submit anything.
        let closes: Option<u64> = if kani::any() {
            let c: u64 = kani::any();
            kani::assume(c <= 3);
            Some(c)
        } else {
            None
        };
        if a != head {
            assert!(!accepts(head_gen, a, closes, head_gen, head));
        }
    }

    /// Bounded symbolic submission. Bounds must allow `head_gen + 1`, or the
    /// migration branch is unreachable and the proofs are vacuous.
    fn symbolic_submission() -> (u64, u64, Option<u64>, u64, u64) {
        let g: u64 = kani::any();
        let e: u64 = kani::any();
        let head_gen: u64 = kani::any();
        let head: u64 = kani::any();
        kani::assume(g <= 4);
        kani::assume(e <= 3);
        kani::assume(head_gen <= 3);
        kani::assume(head <= 3);
        let closes: Option<u64> = if kani::any() {
            let c: u64 = kani::any();
            kani::assume(c <= 3);
            Some(c)
        } else {
            None
        };
        (g, e, closes, head_gen, head)
    }

    /// P4-L1: every accepted `(generation, epoch)` is lexicographically `>=` the
    /// head.
    #[kani::proof]
    fn p4_accept_is_lexicographically_monotone() {
        let (g, e, closes, head_gen, head) = symbolic_submission();
        if accepts(g, e, closes, head_gen, head) {
            assert!(g > head_gen || (g == head_gen && e >= head));
        }
    }

    /// P4-L2: never accepted into a closed lineage or one beyond the successor.
    #[kani::proof]
    fn p4_accept_never_downgrades_generation() {
        let (g, e, closes, head_gen, head) = symbolic_submission();
        if accepts(g, e, closes, head_gen, head) {
            assert!(g >= head_gen);
            assert!(g <= head_gen + 1);
        }
    }

    /// P4-L3: opening a lineage requires epoch 0 and naming the closed head.
    #[kani::proof]
    fn p4_opening_requires_naming_closed_head() {
        let (g, e, closes, head_gen, head) = symbolic_submission();
        if accepts(g, e, closes, head_gen, head) && g != head_gen {
            assert!(g == head_gen + 1);
            assert!(e == 0);
            assert!(closes == Some(head));
        }
    }

    /// Mutant: the SQL translated literally to unsigned, overflowing on an empty
    /// log.
    fn head_epoch_of_mutant(max_epoch: Option<u64>) -> u64 {
        max_epoch.unwrap_or(u64::MAX) + 1
    }

    #[kani::proof]
    #[kani::should_panic]
    fn i1_head_epoch_mutant_refuted() {
        let head = head_epoch_of_mutant(None);
        assert!(head == 0);
    }

    /// P4-L3 mutant: opens the next generation without naming the closed head.
    fn accepts_no_closes_mutant(
        generation: u64,
        based_on_epoch: u64,
        _closes_epoch: Option<u64>,
        head_generation: u64,
        head_epoch: u64,
    ) -> bool {
        if generation == head_generation {
            based_on_epoch == head_epoch
        } else if generation == head_generation + 1 {
            based_on_epoch == 0
        } else {
            false
        }
    }

    #[kani::proof]
    #[kani::should_panic]
    fn p4_opening_requires_naming_closed_head_mutant_refuted() {
        let (g, e, closes, head_gen, head) = symbolic_submission();
        if accepts_no_closes_mutant(g, e, closes, head_gen, head) && g != head_gen {
            assert!(closes == Some(head));
        }
    }

    /// P4-L2 mutant: accepts epoch 0 in any non-head generation.
    fn accepts_downgrade_mutant(
        generation: u64,
        based_on_epoch: u64,
        _closes_epoch: Option<u64>,
        head_generation: u64,
        head_epoch: u64,
    ) -> bool {
        if generation == head_generation {
            based_on_epoch == head_epoch
        } else {
            based_on_epoch == 0
        }
    }

    #[kani::proof]
    #[kani::should_panic]
    fn p4_accept_never_downgrades_generation_mutant_refuted() {
        let (g, e, closes, head_gen, head) = symbolic_submission();
        if accepts_downgrade_mutant(g, e, closes, head_gen, head) {
            assert!(g >= head_gen);
            assert!(g <= head_gen + 1);
        }
    }

    // I4 retention-floor harnesses. `EPOCH_MAX` must exceed
    // `PRUNE_MAX_BEHIND_HEAD` so Tier 2 can bind and P2 is not vacuous.
    const EPOCH_MAX: i64 = 1024;

    fn symbolic_min_since() -> Option<i64> {
        if kani::any() {
            let m: i64 = kani::any();
            kani::assume((0..=EPOCH_MAX).contains(&m));
            Some(m)
        } else {
            None
        }
    }

    /// P1: the floor is never negative.
    #[kani::proof]
    fn i4_floor_non_negative() {
        let min_since = symbolic_min_since();
        let all_reported: bool = kani::any();
        let head: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&head));

        let floor = prune_floor(min_since, all_reported, head);
        assert!(floor >= 0);
    }

    /// P2 (`NoLossForCurrentMember`): the floor exceeds the slowest member's epoch
    /// only when Tier 2 binds.
    #[kani::proof]
    fn i4_tier1_never_past_slowest() {
        let m: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&m));
        let head: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&head));

        let floor = prune_floor(Some(m), true, head);
        if floor > m {
            assert!(head - PRUNE_MAX_BEHIND_HEAD > m);
        }
    }

    /// P3: an unreported roster makes the floor exactly Tier 2.
    #[kani::proof]
    fn i4_unreported_disables_tier1() {
        let min_since = symbolic_min_since();
        let head: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&head));

        let floor = prune_floor(min_since, false, head);
        assert!(floor == (head - PRUNE_MAX_BEHIND_HEAD).max(0));
    }

    /// P4 (#681): the floor never reaches the head, even for a forged `min_since`
    /// above it.
    #[kani::proof]
    fn i4_floor_never_reaches_head() {
        let min_since = symbolic_min_since();
        let all_reported: bool = kani::any();
        let head: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&head));

        let floor = prune_floor(min_since, all_reported, head);
        assert!(floor <= (head - 1).max(0));
        if head > 0 {
            assert!(floor < head);
        }
    }

    /// P1 mutant: no `.max(0)` clamp.
    fn prune_floor_p1_mutant(_min_since: Option<i64>, _all_reported: bool, head: i64) -> i64 {
        head - PRUNE_MAX_BEHIND_HEAD
    }

    #[kani::proof]
    #[kani::should_panic]
    fn i4_floor_non_negative_mutant_refuted() {
        let min_since = symbolic_min_since();
        let all_reported: bool = kani::any();
        let head: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&head));

        let floor = prune_floor_p1_mutant(min_since, all_reported, head);
        assert!(floor >= 0);
    }

    /// P2 mutant: `+ PRUNE_SLACK_EPOCHS` prunes past the slowest member.
    fn prune_floor_p2_mutant(min_since: Option<i64>, all_reported: bool, head: i64) -> i64 {
        let tier1 = match (all_reported, min_since) {
            (true, Some(m)) => (m + PRUNE_SLACK_EPOCHS).max(0),
            _ => 0,
        };
        let tier2 = (head - PRUNE_MAX_BEHIND_HEAD).max(0);
        tier1.max(tier2)
    }

    #[kani::proof]
    #[kani::should_panic]
    fn i4_tier1_never_past_slowest_mutant_refuted() {
        let m: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&m));
        let head: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&head));

        let floor = prune_floor_p2_mutant(Some(m), true, head);
        if floor > m {
            assert!(head - PRUNE_MAX_BEHIND_HEAD > m);
        }
    }

    /// P3 mutant: ignores `all_reported`.
    fn prune_floor_p3_mutant(min_since: Option<i64>, all_reported: bool, head: i64) -> i64 {
        let _ = all_reported;
        let tier1 = match min_since {
            Some(m) => (m - PRUNE_SLACK_EPOCHS).max(0),
            None => 0,
        };
        let tier2 = (head - PRUNE_MAX_BEHIND_HEAD).max(0);
        tier1.max(tier2)
    }

    #[kani::proof]
    #[kani::should_panic]
    fn i4_unreported_disables_tier1_mutant_refuted() {
        let min_since = symbolic_min_since();
        let head: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&head));

        let floor = prune_floor_p3_mutant(min_since, false, head);
        assert!(floor == (head - PRUNE_MAX_BEHIND_HEAD).max(0));
    }

    /// P4 mutant (#681): no `head - 1` clamp.
    fn prune_floor_clamp_mutant(min_since: Option<i64>, all_reported: bool, head: i64) -> i64 {
        let tier1 = match (all_reported, min_since) {
            (true, Some(m)) => m.saturating_sub(PRUNE_SLACK_EPOCHS).max(0),
            _ => 0,
        };
        let tier2 = (head - PRUNE_MAX_BEHIND_HEAD).max(0);
        tier1.max(tier2)
    }

    #[kani::proof]
    #[kani::should_panic]
    fn i4_floor_never_reaches_head_mutant_refuted() {
        let min_since = symbolic_min_since();
        let all_reported: bool = kani::any();
        let head: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&head));

        let floor = prune_floor_clamp_mutant(min_since, all_reported, head);
        assert!(floor <= (head - 1).max(0));
    }

    // Closed-lineage retirement: G1 never touches the live lineage, G2 retires
    // nothing without evidence, G3 Tier 1 never retires an occupied lineage.
    const GEN_MAX: i64 = 4;

    fn symbolic_min_generation() -> Option<i64> {
        if kani::any() {
            let g: i64 = kani::any();
            kani::assume((0..=GEN_MAX).contains(&g));
            Some(g)
        } else {
            None
        }
    }

    /// G1: the generation floor stays in `0..=head_generation`.
    #[kani::proof]
    fn i4_generation_floor_never_retires_live_lineage() {
        let head_gen: i64 = kani::any();
        kani::assume((0..=GEN_MAX).contains(&head_gen));
        let head: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&head));
        let min_gen = symbolic_min_generation();
        let all_reported: bool = kani::any();

        let floor = closed_generation_floor(head_gen, head, min_gen, all_reported);
        assert!(floor >= 0);
        assert!(floor <= head_gen);
    }

    /// G2: an incomplete roster below the Tier-2 cap retires nothing.
    #[kani::proof]
    fn i4_generation_floor_needs_evidence() {
        let head_gen: i64 = kani::any();
        kani::assume((0..=GEN_MAX).contains(&head_gen));
        let head: i64 = kani::any();
        kani::assume((0..PRUNE_MAX_BEHIND_HEAD).contains(&head));
        let min_gen = symbolic_min_generation();

        let floor = closed_generation_floor(head_gen, head, min_gen, false);
        assert!(floor == 0);
    }

    /// G3: the floor exceeds the slowest device's generation only when Tier 2
    /// binds.
    #[kani::proof]
    fn i4_generation_tier1_never_retires_occupied() {
        let head_gen: i64 = kani::any();
        kani::assume((0..=GEN_MAX).contains(&head_gen));
        let head: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&head));
        let g: i64 = kani::any();
        kani::assume((0..=GEN_MAX).contains(&g));

        let floor = closed_generation_floor(head_gen, head, Some(g), true);
        if floor > g {
            assert!(head >= PRUNE_MAX_BEHIND_HEAD);
        }
    }

    /// G1 mutant: no `.min(head_generation)` clamp.
    fn closed_generation_floor_g1_mutant(
        head_generation: i64,
        head_epoch: i64,
        min_reported_generation: Option<i64>,
        all_reported: bool,
    ) -> i64 {
        let tier1 = match (all_reported, min_reported_generation) {
            (true, Some(g)) => g.max(0),
            _ => 0,
        };
        let tier2 = if head_epoch >= PRUNE_MAX_BEHIND_HEAD {
            head_generation
        } else {
            0
        };
        tier1.max(tier2).max(0)
    }

    #[kani::proof]
    #[kani::should_panic]
    fn i4_generation_floor_never_retires_live_lineage_mutant_refuted() {
        let head_gen: i64 = kani::any();
        kani::assume((0..=GEN_MAX).contains(&head_gen));
        let head: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&head));
        let min_gen = symbolic_min_generation();
        let all_reported: bool = kani::any();

        let floor = closed_generation_floor_g1_mutant(head_gen, head, min_gen, all_reported);
        assert!(floor <= head_gen);
    }

    /// G3 mutant: `g + 1` retires the slowest device's own lineage.
    fn closed_generation_floor_g3_mutant(
        head_generation: i64,
        head_epoch: i64,
        min_reported_generation: Option<i64>,
        all_reported: bool,
    ) -> i64 {
        let tier1 = match (all_reported, min_reported_generation) {
            (true, Some(g)) => (g + 1).max(0),
            _ => 0,
        };
        let tier2 = if head_epoch >= PRUNE_MAX_BEHIND_HEAD {
            head_generation
        } else {
            0
        };
        tier1.max(tier2).min(head_generation).max(0)
    }

    #[kani::proof]
    #[kani::should_panic]
    fn i4_generation_tier1_never_retires_occupied_mutant_refuted() {
        let head_gen: i64 = kani::any();
        kani::assume((0..=GEN_MAX).contains(&head_gen));
        let head: i64 = kani::any();
        kani::assume((0..PRUNE_MAX_BEHIND_HEAD).contains(&head));
        let g: i64 = kani::any();
        kani::assume((0..=GEN_MAX).contains(&g));
        // A lineage below the head must exist for `g + 1` to survive the clamp.
        kani::assume(g < head_gen);

        let floor = closed_generation_floor_g3_mutant(head_gen, head, Some(g), true);
        if floor > g {
            assert!(head >= PRUNE_MAX_BEHIND_HEAD);
        }
    }
}

#[cfg(test)]
mod retention_tests {
    use super::*;

    /// Tier 1 never exceeds the slowest member while Tier 2 does not bind.
    #[test]
    fn tier1_floor_never_above_slowest_member() {
        let head = 20;
        for min_since in 0..=20i64 {
            let floor = prune_floor(Some(min_since), true, head);
            assert!(
                floor <= min_since,
                "floor {floor} must not exceed slowest member epoch {min_since}"
            );
            assert_eq!(floor, (min_since - PRUNE_SLACK_EPOCHS).max(0));
        }
    }

    /// An unreported member disables Tier 1.
    #[test]
    fn tier1_disabled_until_whole_roster_reports() {
        assert_eq!(prune_floor(Some(1_000), false, 100), 0);
        assert_eq!(prune_floor(Some(50), false, 40), 0);
        assert_eq!(prune_floor(None, false, 40), 0);
    }

    /// Tier 2 prunes past a stuck member to bound storage (accepted loss #1).
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

    #[test]
    fn floor_never_negative() {
        assert_eq!(prune_floor(None, false, 0), 0);
        assert_eq!(prune_floor(Some(0), true, 0), 0);
        assert_eq!(prune_floor(Some(3), true, 3), 0); // min(3)-8 → clamp 0; head 3 < cap
    }

    /// #681: a forged or absurd high-water never wipes the live log.
    #[test]
    fn floor_never_reaches_head_even_for_a_bogus_report() {
        assert_eq!(prune_floor(Some(i64::MAX), true, 10), 9);
        assert!(prune_floor(Some(i64::MAX), true, 10) < 10, "never at/above head");

        assert_eq!(prune_floor(Some(1_000), true, 20), 19);

        assert_eq!(prune_floor(Some(100), true, 0), 0);
        assert_eq!(prune_floor(Some(i64::MAX), true, 0), 0);

        // An honest caught-up report: the slack keeps it below the ceiling.
        assert_eq!(prune_floor(Some(20), true, 20), 20 - PRUNE_SLACK_EPOCHS);

        assert_eq!(prune_floor(Some(i64::MAX), false, 10), 0);
    }

    /// A never-migrated conversation never retires a lineage.
    #[test]
    fn generation_floor_is_inert_before_any_migration() {
        assert_eq!(closed_generation_floor(0, 0, None, false), 0);
        assert_eq!(closed_generation_floor(0, 50, Some(0), true), 0);
        assert_eq!(
            closed_generation_floor(0, PRUNE_MAX_BEHIND_HEAD + 1, Some(0), true),
            0
        );
    }

    /// Tier 1 retires a lineage only once every device has left it.
    #[test]
    fn generation_tier1_retires_only_fully_vacated_lineages() {
        assert_eq!(closed_generation_floor(1, 3, Some(1), true), 1);
        assert_eq!(closed_generation_floor(1, 3, Some(0), true), 0);
        assert_eq!(closed_generation_floor(1, 3, Some(1), false), 0);
    }

    /// Tier 2: a device stranded on a retired lineage does not pin it forever.
    #[test]
    fn generation_tier2_bounds_a_stranded_device() {
        let stuck_in_gen = 0i64;
        let head_gen = 2i64;
        assert_eq!(
            closed_generation_floor(head_gen, PRUNE_MAX_BEHIND_HEAD - 1, Some(stuck_in_gen), true),
            0
        );
        assert_eq!(
            closed_generation_floor(head_gen, PRUNE_MAX_BEHIND_HEAD, Some(stuck_in_gen), true),
            head_gen
        );
    }

    /// The live lineage is never retired, even for a report above the head.
    #[test]
    fn generation_floor_never_exceeds_head_generation() {
        assert_eq!(closed_generation_floor(1, 3, Some(9), true), 1);
        assert_eq!(
            closed_generation_floor(2, PRUNE_MAX_BEHIND_HEAD * 4, Some(9), true),
            2
        );
    }
}
