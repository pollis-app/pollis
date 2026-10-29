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
//!
//! ## Suite generations (issue #454 P4)
//!
//! A suite migration stands up a successor group at MLS epoch 0: a new **lineage**, not a reset.
//!
//! The key widens to `(conversation_id, generation, epoch)`, ordered lexicographically:
//!   - **Within** a generation the rule is byte-for-byte the head+1 rule above.
//!   - Epoch 0 is accepted **only** as the opening commit of generation `N + 1`,
//!     and only when the submitter also names the **closed head of generation
//!     `N`** (`closes_epoch`) and that name is still correct. So a migration is a
//!     compare-and-swap on the OLD lineage's head as well as a claim on the new
//!     one: a commit that lands in generation `N` between the migrator's read and
//!     its submit invalidates the migration rather than being orphaned by it.
//!   - A generation can never be opened twice — the second opener sees
//!     `MAX(generation)` already advanced, and the unique index is the backstop.
//!
//! Old rows and pre-hybrid clients are generation 0.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use libsql::Connection;
use crate::util::{b64, b64_decode};

// Request bodies live in `pollis-api`, shared with pollis-core.
pub use pollis_api::commit::*;

/// The group's head epoch as a **pure** function of the log's current
/// `MAX(epoch)`: `None` (empty log) → `0`, `Some(m)` → `m + 1`. This is the
/// `COALESCE(MAX(epoch), -1) + 1` lifted out of [`head_epoch`] so Kani can prove no underflow or wrap (I1).
pub fn head_epoch_of(max_epoch: Option<u64>) -> u64 {
    match max_epoch {
        // Empty/unknown group: the first commit is built from epoch 0.
        None => 0,
        Some(m) => m + 1,
    }
}

/// The Delivery Service's accept decision as a **pure** predicate over the
/// widened `(generation, epoch)` key. This models the atomic two-branch `WHERE`
/// guard in [`submit_commit`]; not wired in, as a Rust-side check would be a TOCTOU.
///
/// Two accept branches, and only two:
///   * **Continue** the head lineage — `generation == head_generation` and
///     `based_on_epoch == head_epoch`. Byte-for-byte the pre-P4 rule.
///   * **Open** the next lineage — `generation == head_generation + 1`,
///     `based_on_epoch == 0`, and `closes_epoch == Some(head_epoch)`: the
///     migrator must correctly name the head it is closing, so a commit that
///     lands on the old lineage first invalidates the migration.
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
        // Neither the head lineage nor its immediate successor: a downgrade to a
        // closed generation, or a jump over one that was never opened.
        false
    }
}

/// The conversation's newest lineage = `COALESCE(MAX(generation), 0)`.
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

/// The head epoch WITHIN one lineage = `MAX(epoch) + 1` over that generation's
/// rows (0 for an empty/unknown one). Reads `MAX(epoch)` and applies the pure,
/// Kani-proved [`head_epoch_of`].
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
    // Epochs are non-negative, so `i64 -> u64` is exact.
    let max_epoch: Option<u64> = row.get::<Option<i64>>(0)?.map(|m| m as u64);
    Ok(head_epoch_of(max_epoch) as i64)
}

/// The group's head epoch, in its CURRENT lineage.
pub async fn head_epoch(conn: &Connection, conversation_id: &str) -> Result<i64> {
    let gen = head_generation(conn, conversation_id).await?;
    head_epoch_in(conn, conversation_id, gen).await
}

/// Commits in `generation` with `epoch >= since`, contiguous, in apply order.
/// Scoped to ONE lineage: commits from different generations are encrypted under
/// different suites and different groups, so they are never interleaved.
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

/// The verdict of `POST /v1/commits`.
///
/// Wider than [`SubmitResponse`], a shipped wire type that must not grow variants.
#[derive(Debug)]
pub enum SubmitVerdict {
    /// The ordinary CAS outcome — `Accepted` or `Rejected`.
    Response(SubmitResponse),
    /// A Welcome in the bundle is not one this submitter may write (C1). The
    /// whole bundle is refused; nothing was written.
    Forbidden,
    /// The bundle is malformed. `&'static str` so no caller input is echoed.
    Invalid(&'static str),
}

/// Submit a commit. Accepts iff `based_on_epoch` is the current head; otherwise
/// rejects with the head + the missing commits. See the module docs for why
/// this is race-free / fork-free / gap-free / append-only.
///
/// Modeled by `Submit` in `specs/tla/CommitLog.tla` (Spec A, I1/I2).
///
/// ## Why this takes TWO connections (C1)
///
/// Welcome recipients are checked against MEMBERSHIP on `main_conn`; the log is on `log_conn`.
///
/// Three gates, on the ROWS written, so they also run on the no-auth path:
///
///   1. **Bounded blob.** A Welcome may not exceed
///      [`crate::writes::WELCOME_MAX_BYTES`], the same ceiling a resubmit obeys.
///   2. **The RECIPIENT is in the DESIRED ROSTER** ([`crate::writes::in_desired_roster`]).
///   3. **One member may not steal another's pending Welcome**, unless admin; a NEWER generation always wins.
pub async fn submit_commit(
    main_conn: &Connection,
    log_conn: &Connection,
    body: &SubmitBody,
) -> Result<SubmitVerdict> {
    let conn = log_conn;
    let commit = b64_decode(&body.commit)?;

    // Decode BEFORE opening the transaction, so a malformed payload is a clean `Err`.
    let group_info = body.group_info.as_deref().map(b64_decode).transpose()?;
    let welcomes = body
        .welcomes
        .iter()
        .map(|w| Ok((w, b64_decode(&w.welcome)?)))
        .collect::<Result<Vec<_>>>()?;

    // Gates 1 + 2 run on inputs alone, before any write; membership is cached per recipient user.
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

    // Gate 3's admin escape hatch, read on the MAIN DB before the log-DB transaction.
    let may_override = if welcomes.is_empty() {
        true
    } else {
        crate::writes::is_conversation_admin(main_conn, &body.conversation_id, &body.sender_id)
            .await?
    };

    // One IMMEDIATE bundle: commit, GroupInfo and Welcomes all commit or roll back; one winner per epoch.
    let tx = conn
        .transaction_with_behavior(libsql::TransactionBehavior::Immediate)
        .await?;

    // Atomic two-branch decision mirroring [`accepts`]: (a) continue the head lineage, (b) open the next.
    // `?8 IS NOT NULL` makes an omitted `closes_epoch` an explicit reject, not a NULL-compare accident.
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
        // Not at the head: nothing written. Reject with the head + missing commits of the SUBMITTER's generation.
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

    // Won the epoch: GroupInfo + Welcomes in the same transaction. Moved, not cloned (#875).
    if let Some(gi) = group_info {
        // (generation, epoch)-monotone, matching `writes::upsert_group_info`.
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
        // Gate 3, inside the IMMEDIATE tx: another member's PENDING Welcome at this or a later generation is defended.
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

        // Idempotent on the UNIQUE recipient tuple (migration 000002): a resend refreshes and re-arms.
        //
        // The generation is the lineage the Welcome admits INTO, so a recipient can drain its current one first.
        //
        // `submitted_by` (000006) is the Add's author, whom a resubmit may not overwrite.
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

// Commit-log retention (I4, #539): Tier 1 is bounded by the SLOWEST member; Tier 2 caps depth (accepted loss #1).

/// Tier-1 slack: retain this many epochs BELOW the slowest current member's
/// Pure buffer: keeping `min_since - K` never drops anything a member awaits.
pub const PRUNE_SLACK_EPOCHS: i64 = 8;

/// Tier-2 hard cap on depth behind head, even past a straggler (accepted loss #1).
pub const PRUNE_MAX_BEHIND_HEAD: i64 = 512;

/// The retention floor (EXCLUSIVE): the prune deletes commits with `epoch < floor`.
///
/// Pure, audited against `specs/tla/Delivery.tla`. Inputs:
///   * `min_since` — MIN reported applied epoch over current member devices, or
///     `None` when no member has reported.
///   * `all_reported` — whether EVERY current member device has a reported
///     high-water. Tier 1 only prunes when the whole roster is accounted for; a
///     single unreported member means `min_since` is not a safe lower bound, so
///     Tier 1 contributes nothing (floor 0) and only Tier 2's cap applies.
///   * `head` — the group head epoch (`MAX(epoch)+1`).
///
/// `floor = min(max(tier1, tier2), head - 1)`, clamped to `>= 0`:
///   * `tier1 = (min_since - PRUNE_SLACK_EPOCHS)` when `all_reported`, else 0.
///   * `tier2 = head - PRUNE_MAX_BEHIND_HEAD`.
///
/// ## The clamp to `head - 1` (the safety property that matters most — #681)
///
/// `tier1` comes from untrusted client reports; an unclamped floor `>= head` deletes the whole live log.
///
/// Clamping to `head - 1` (not `head`; the floor is exclusive) always keeps the head commit.
pub fn prune_floor(min_since: Option<i64>, all_reported: bool, head: i64) -> i64 {
    let tier1 = match (all_reported, min_since) {
        (true, Some(m)) => m.saturating_sub(PRUNE_SLACK_EPOCHS).max(0),
        // A current member has not reported: `min_since` is not a safe lower
        // bound over the roster, so Tier 1 must not prune. Tier 2 still applies.
        _ => 0,
    };
    let tier2 = (head - PRUNE_MAX_BEHIND_HEAD).max(0);
    let head_ceiling = (head - 1).max(0);
    tier1.max(tier2).min(head_ceiling)
}

/// The generation floor (EXCLUSIVE): whole lineages with `generation < floor` are
/// closed and safe to delete outright. Pure, for the same reasons as
/// [`prune_floor`]. Inputs:
///   * `head_generation` — the conversation's newest lineage.
///   * `head_epoch` — the head epoch WITHIN that lineage.
///   * `min_reported_generation` — MIN reported generation over current member
///     devices, or `None` when no member has reported.
///   * `all_reported` — whether EVERY current member device has reported.
///
/// `floor = min(max(tier1, tier2), head_generation)`, clamped `>= 0`:
///   * `tier1 = min_reported_generation` when `all_reported`, else 0 (zero loss).
///   * `tier2 = head_generation` once `head_epoch >= PRUNE_MAX_BEHIND_HEAD` (Tier 2's accepted loss).
///
/// The final clamp to `head_generation` is the safety property that matters most:
/// the LIVE lineage is never retired wholesale, whatever the reports say.
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

/// Record device `device_id`'s commit-catch-up high-water for a conversation.
/// `since` is the epoch the client is caught up FROM within `generation` — its
/// current local MLS epoch — so it still needs every commit `>= since` in that
/// lineage. Monotone on the (generation, since) PAIR, so a stale report can never LOWER it.
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

/// The CURRENT member device ids for a conversation, read from the MAIN DB.
/// Revoked devices are excluded (I5) so they never pin the floor down.
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

/// Every reported `(device_id -> (generation, since_epoch))` high-water for a
/// conversation, read from the LOG DB.
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

/// Delete commits below `floor` (EXCLUSIVE) WITHIN one lineage. The single
/// epoch-retention write; a pure row DELETE, so the UNIQUE index guarantee holds.
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

/// Delete every commit in a CLOSED lineage below `floor` (EXCLUSIVE generation).
/// Epoch pruning never reaches closed lineages; `floor <= 0` is a no-op.
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
    /// The computed epoch floor (exclusive) within the head generation.
    pub floor: i64,
    /// The head generation the epoch floor was applied to.
    pub generation: i64,
    /// The computed generation floor (exclusive) — closed lineages below it were
    /// retired outright.
    pub generation_floor: i64,
    /// Commit rows deleted this pass, across both rules.
    pub deleted: u64,
}

/// Compute the retention floor from live membership + reported high-waters and
/// prune the commit log below it. Event-driven (submit, catch-up report), never a timer.
pub async fn prune_commit_log(
    main: &Connection,
    log: &Connection,
    conversation_id: &str,
) -> Result<PruneReport> {
    let generation = head_generation(log, conversation_id).await?;
    let head = head_epoch_in(log, conversation_id, generation).await?;
    let members = current_member_devices(main, conversation_id).await?;
    let recorded = recorded_since(log, conversation_id).await?;

    // Epoch-wise over devices in the head generation (others count as unreported); generation-wise over all.
    let (min_since, all_reported_in_head_gen, min_generation, all_reported) = if members.is_empty() {
        // No current members (everyone left / the group was deleted): Tier 1 has
        // no lower bound to protect, so only Tier 2 bounds the orphaned log.
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
                        // Behind on the lineage itself: its epoch says nothing
                        // about what it still needs from THIS one.
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

// Kani harnesses (I1): `head_epoch_of` never wraps; `accepts` admits one epoch per head.
#[cfg(kani)]
mod proofs {
    use super::*;

    /// I1: the head arithmetic never underflows or wraps.
    #[kani::proof]
    fn i1_head_epoch_no_wrap() {
        // Empty log → head 0, no underflow.
        assert!(head_epoch_of(None) == 0);

        let m: u64 = kani::any();
        // Exclude only the lone wrapping input.
        kani::assume(m < u64::MAX);
        let head = head_epoch_of(Some(m));
        assert!(head == m + 1);
        // The head strictly exceeds the max epoch: monotone head, no wrap.
        assert!(head > m);
    }

    /// I1 (within a lineage): at a fixed head exactly one HEAD-generation epoch is accepted (no fork).
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

        // Two accepted same-generation submits at one head must be the same epoch.
        if accepts(head_gen, a, None, head_gen, head) && accepts(head_gen, b, None, head_gen, head) {
            assert!(a == b);
        }
        // Stale or forward-gap submits are rejected; a symbolic `closes_epoch` buys nothing.
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

    /// A symbolic `(generation, epoch, closes_epoch)` submission and a symbolic
    /// `(head_generation, head_epoch)`, all bounded so CBMC stays instantaneous.
    /// Bounds must allow `head_gen + 1` to be expressible, or the migration branch
    /// goes unreachable and every property below is vacuous.
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

    /// P4-L1 (lexicographic monotonicity): every accepted `(generation, epoch)` is
    /// `>=` the head pair lexicographically.
    #[kani::proof]
    fn p4_accept_is_lexicographically_monotone() {
        let (g, e, closes, head_gen, head) = symbolic_submission();
        if accepts(g, e, closes, head_gen, head) {
            assert!(g > head_gen || (g == head_gen && e >= head));
        }
    }

    /// P4-L2 (no downgrade): a commit is never accepted into a CLOSED lineage, nor
    /// into one that was never opened. The accepted generation is the head's or
    /// its immediate successor — never below, never a jump.
    #[kani::proof]
    fn p4_accept_never_downgrades_generation() {
        let (g, e, closes, head_gen, head) = symbolic_submission();
        if accepts(g, e, closes, head_gen, head) {
            assert!(g >= head_gen);
            assert!(g <= head_gen + 1);
        }
    }

    /// P4-L3 (opening a lineage is a compare-and-swap on the old one): accepting a
    /// commit in a NEW generation requires epoch 0 AND naming the previous closed head.
    #[kani::proof]
    fn p4_opening_requires_naming_closed_head() {
        let (g, e, closes, head_gen, head) = symbolic_submission();
        if accepts(g, e, closes, head_gen, head) && g != head_gen {
            assert!(g == head_gen + 1);
            assert!(e == 0);
            assert!(closes == Some(head));
        }
    }

    /// Negative harness: the literal unsigned `COALESCE(MAX, -1) + 1`, which overflows on an empty log.
    fn head_epoch_of_mutant(max_epoch: Option<u64>) -> u64 {
        // BUG: `u64::MAX + 1` overflows on an empty log.
        max_epoch.unwrap_or(u64::MAX) + 1
    }

    #[kani::proof]
    #[kani::should_panic]
    fn i1_head_epoch_mutant_refuted() {
        // Empty log drives the mutant's `u64::MAX + 1` overflow.
        let head = head_epoch_of_mutant(None);
        assert!(head == 0);
    }

    /// P4-L3 mutant: opens the next generation on `epoch == 0` alone, without
    /// requiring the closed head to be named (the stale-migrator bug).
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
            // BUG: dropped the `closes_epoch == Some(head_epoch)` compare-and-swap.
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
            // A migration not naming the closed head is accepted by the mutant.
            assert!(closes == Some(head));
        }
    }

    /// P4-L2 mutant: accepts epoch 0 in ANY generation other than the head's,
    /// including a closed one.
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
            // BUG: no `generation == head_generation + 1` bound, so a LOWER
            // generation is opened just as readily as the next one.
            based_on_epoch == 0
        }
    }

    #[kani::proof]
    #[kani::should_panic]
    fn p4_accept_never_downgrades_generation_mutant_refuted() {
        let (g, e, closes, head_gen, head) = symbolic_submission();
        if accepts_downgrade_mutant(g, e, closes, head_gen, head) {
            // `g < head_gen` with `e == 0` is accepted by the mutant → violation.
            assert!(g >= head_gen);
            assert!(g <= head_gen + 1);
        }
    }

    // I4 retention-floor harnesses over the REAL `prune_floor`, each paired with a `should_panic` mutant.
    // `EPOCH_MAX` must exceed `PRUNE_MAX_BEHIND_HEAD` or P2's antecedent is unreachable (vacuous).
    const EPOCH_MAX: i64 = 1024;

    /// A symbolic `min_since`: `None`, or `Some(m)` with `0 <= m <= EPOCH_MAX`
    /// (epochs are non-negative by construction).
    fn symbolic_min_since() -> Option<i64> {
        if kani::any() {
            let m: i64 = kani::any();
            kani::assume((0..=EPOCH_MAX).contains(&m));
            Some(m)
        } else {
            None
        }
    }

    /// P1 (floor_non_negative): `prune_floor(..) >= 0` for ALL inputs.
    #[kani::proof]
    fn i4_floor_non_negative() {
        let min_since = symbolic_min_since();
        let all_reported: bool = kani::any();
        let head: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&head));

        let floor = prune_floor(min_since, all_reported, head);
        assert!(floor >= 0);
    }

    /// P2 (Spec-B `NoLossForCurrentMember`): the floor exceeds the slowest member's epoch only when Tier 2 binds.
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

    /// P3 (unreported_disables_tier1): when `all_reported == false`, the floor is exactly Tier 2.
    #[kani::proof]
    fn i4_unreported_disables_tier1() {
        let min_since = symbolic_min_since();
        let head: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&head));

        let floor = prune_floor(min_since, false, head);
        assert!(floor == (head - PRUNE_MAX_BEHIND_HEAD).max(0));
    }

    /// P4 (#681, floor_never_reaches_head): for ANY `min_since`, even forged above head, `floor <= head - 1`.
    #[kani::proof]
    fn i4_floor_never_reaches_head() {
        let min_since = symbolic_min_since();
        let all_reported: bool = kani::any();
        let head: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&head));

        let floor = prune_floor(min_since, all_reported, head);
        // Never at or above head — the head commit (epoch head-1) is retained.
        assert!(floor <= (head - 1).max(0));
        // A non-empty log therefore always keeps at least its head commit.
        if head > 0 {
            assert!(floor < head);
        }
    }

    /// P1 mutant: the raw Tier-2 expression with NO `.max(0)` clamp and no Tier 1
    /// — a short log (`head < PRUNE_MAX_BEHIND_HEAD`) yields a negative floor.
    fn prune_floor_p1_mutant(_min_since: Option<i64>, _all_reported: bool, head: i64) -> i64 {
        // BUG: no `.max(0)`.
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
        // A short log drives the mutant negative → Kani finds the violation.
        assert!(floor >= 0);
    }

    /// P2 mutant: `+ PRUNE_SLACK_EPOCHS` instead of `-` — retains the slack ABOVE
    /// the slowest member (the spec's refuted variant).
    fn prune_floor_p2_mutant(min_since: Option<i64>, all_reported: bool, head: i64) -> i64 {
        let tier1 = match (all_reported, min_since) {
            // BUG: `+ SLACK` prunes SLACK epochs PAST the slowest member.
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
        // Tier 2 idle, so the mutant floor `m + 8 > m` violates.
        if floor > m {
            assert!(head - PRUNE_MAX_BEHIND_HEAD > m);
        }
    }

    /// P3 mutant: ignores `all_reported` — an unreported roster still lets Tier 1
    /// prune off `min_since`, so a member we cannot bound gets pruned past.
    fn prune_floor_p3_mutant(min_since: Option<i64>, all_reported: bool, head: i64) -> i64 {
        // BUG: dropped the `all_reported` guard.
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
        // `min_since` above the Tier-2 cap breaks the mutant.
        assert!(floor == (head - PRUNE_MAX_BEHIND_HEAD).max(0));
    }

    /// P4 mutant (#681): the pre-fix floor with NO `.min(head - 1)` clamp — Tier 1
    /// is driven straight off the reported `min_since`, so a high report wipes the log.
    fn prune_floor_clamp_mutant(min_since: Option<i64>, all_reported: bool, head: i64) -> i64 {
        let tier1 = match (all_reported, min_since) {
            (true, Some(m)) => m.saturating_sub(PRUNE_SLACK_EPOCHS).max(0),
            _ => 0,
        };
        let tier2 = (head - PRUNE_MAX_BEHIND_HEAD).max(0);
        // BUG: dropped the `.min((head - 1).max(0))` clamp.
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
        // `min_since` above head breaks the mutant.
        assert!(floor <= (head - 1).max(0));
    }

    // Closed-lineage retirement (G1-G3); `GEN_MAX` is small as generations change once per migration.
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

    /// G1 (live_lineage_never_retired): the generation floor is always in
    /// `0..=head_generation`, for ANY reports.
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

    /// G2 (no_retirement_without_evidence): with the roster NOT fully reported and
    /// the head lineage still short of the Tier-2 cap, nothing is retired at all.
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

    /// G3 (tier1_never_retires_an_occupied_lineage): with the whole roster reported
    /// and the slowest device in generation `g`, the floor exceeds `g` only on the Tier-2 path.
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

    /// G1 mutant: drops the `.min(head_generation)` clamp, so a bogus report at a
    /// generation above the head retires the LIVE lineage.
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
        // BUG: dropped `.min(head_generation)`.
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
        // A generation reported above the head breaks the mutant.
        assert!(floor <= head_gen);
    }

    /// G3 mutant: uses `+ 1` on the min reported generation — retires the lineage
    /// the slowest device is CURRENTLY in, with Tier 2 not even binding.
    fn closed_generation_floor_g3_mutant(
        head_generation: i64,
        head_epoch: i64,
        min_reported_generation: Option<i64>,
        all_reported: bool,
    ) -> i64 {
        let tier1 = match (all_reported, min_reported_generation) {
            // BUG: `g + 1` retires generation `g` itself.
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
        // Keep Tier 2 idle so the mutant's Tier 1 is the only thing pruning.
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

    /// TIER 1 (the spec's `NoLossForCurrentMember`): with the whole roster
    /// reported and Tier 2 idle, the floor never exceeds the slowest member's epoch.
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
            assert_eq!(floor, (min_since - PRUNE_SLACK_EPOCHS).max(0));
        }
    }

    /// An unreported member disables Tier 1: the floor collapses to Tier 2.
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
    /// so storage stays bounded (accepted loss #1).
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

    /// #681 — the retention floor NEVER reaches the head, so a forged/absurd
    /// high-water can never wipe the live commit log.
    #[test]
    fn floor_never_reaches_head_even_for_a_bogus_report() {
        // A report far above the real head still keeps the head commit (epoch 9).
        assert_eq!(prune_floor(Some(i64::MAX), true, 10), 9);
        assert!(prune_floor(Some(i64::MAX), true, 10) < 10, "never at/above head");

        // min_since a plain amount above head → still clamped to head - 1.
        assert_eq!(prune_floor(Some(1_000), true, 20), 19);

        // head == 0 (empty log): nothing to retain, floor collapses to 0.
        assert_eq!(prune_floor(Some(100), true, 0), 0);
        assert_eq!(prune_floor(Some(i64::MAX), true, 0), 0);

        // An honest caught-up report: the slack keeps the clamp from biting.
        assert_eq!(prune_floor(Some(20), true, 20), 20 - PRUNE_SLACK_EPOCHS);

        // An unreported roster already disables Tier 1.
        assert_eq!(prune_floor(Some(i64::MAX), false, 10), 0);
    }

    /// A conversation that has never migrated (head generation 0) never retires a
    /// lineage, whatever the roster reports or the log length.
    #[test]
    fn generation_floor_is_inert_before_any_migration() {
        assert_eq!(closed_generation_floor(0, 0, None, false), 0);
        assert_eq!(closed_generation_floor(0, 50, Some(0), true), 0);
        // Even with Tier 2 wide open.
        assert_eq!(
            closed_generation_floor(0, PRUNE_MAX_BEHIND_HEAD + 1, Some(0), true),
            0
        );
    }

    /// TIER 1 (zero loss): once every current member device has reported itself in
    /// the successor lineage, the retired one goes; one straggler holds it.
    #[test]
    fn generation_tier1_retires_only_fully_vacated_lineages() {
        // Everyone has moved to generation 1 → generation 0 is retired.
        assert_eq!(closed_generation_floor(1, 3, Some(1), true), 1);
        // One device still in generation 0 → nothing retired.
        assert_eq!(closed_generation_floor(1, 3, Some(0), true), 0);
        // A device that has not reported at all → no bound, nothing retired.
        assert_eq!(closed_generation_floor(1, 3, Some(1), false), 0);
    }

    /// TIER 2 (hard cap): a device stranded on a retired lineage does not pin it
    /// forever: once the successor runs `PRUNE_MAX_BEHIND_HEAD` epochs, closed lineages go.
    #[test]
    fn generation_tier2_bounds_a_stranded_device() {
        let stuck_in_gen = 0i64;
        let head_gen = 2i64;
        // Just below the cap: Tier 2 idle, the straggler still holds its lineage.
        assert_eq!(
            closed_generation_floor(head_gen, PRUNE_MAX_BEHIND_HEAD - 1, Some(stuck_in_gen), true),
            0
        );
        // At the cap: everything below the live lineage is retired.
        assert_eq!(
            closed_generation_floor(head_gen, PRUNE_MAX_BEHIND_HEAD, Some(stuck_in_gen), true),
            head_gen
        );
    }

    /// The live lineage is never retired wholesale, whatever the reports say —
    /// including a nonsensical report from a generation above the head.
    #[test]
    fn generation_floor_never_exceeds_head_generation() {
        assert_eq!(closed_generation_floor(1, 3, Some(9), true), 1);
        assert_eq!(
            closed_generation_floor(2, PRUNE_MAX_BEHIND_HEAD * 4, Some(9), true),
            2
        );
    }
}
