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
//! MLS binds the ciphersuite into the group, so migrating a conversation from the
//! classic suite to the PQ-hybrid one cannot be done in place: a *successor*
//! group is stood up for the same conversation and the roster is moved into it by
//! Welcome. A successor starts at MLS epoch 0, which would read as an epoch reset
//! under the rule above.
//!
//! It is not a reset — it is a **lineage**. The monotone key widens from
//! `(conversation_id, epoch)` to `(conversation_id, generation, epoch)`, ordered
//! lexicographically, and everything above holds one key wider:
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
//! Old rows carry `generation = 0` by default, so every conversation that exists
//! today is generation 0 and nothing about its history is reinterpreted. A
//! pre-hybrid client that sends no generation field is likewise a generation-0
//! writer.

use std::collections::HashMap;

use anyhow::{anyhow, Result};
use libsql::Connection;
use crate::util::{b64, b64_decode};

// The request bodies for this module's endpoints live in `pollis-api`, the
// crate pollis-core builds its requests from — one declaration, both ends, so
// a client field that does not exist here is a compile error rather than a
// silently-absent JSON key. Re-exported so `pollis_delivery::commit::*Body`
// keeps resolving for handlers, tests and the flows harness.
pub use pollis_api::commit::*;

// ── Wire types ───────────────────────────────────────────────────────────────
//
// `SubmitResponse`, `CommitWire` and `CommitsResponse` moved to `pollis-api`
// (#922) and arrive through the `pub use` above, so this module's public
// surface is unchanged. They live there for the same reason the request bodies
// do: `pollis-core` decodes what this crate encodes, and a field declared twice
// is a field that eventually disagrees.

// ── Core logic ──────────────────────────────────────────────────────────────

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

/// The Delivery Service's accept decision as a **pure** predicate over the
/// widened `(generation, epoch)` key. This models the atomic two-branch `WHERE`
/// guard in [`submit_commit`]. It is NOT wired into `submit_commit` — the real
/// decision must stay inside the single conditional `INSERT` to be race-free (two
/// racing writers are serialized by SQLite; a Rust-side check would reintroduce a
/// read/write TOCTOU). It is proved here as the model of record.
///
/// Two accept branches, and only two:
///   * **Continue** the head lineage — `generation == head_generation` and
///     `based_on_epoch == head_epoch`. Byte-for-byte the pre-P4 rule.
///   * **Open** the next lineage — `generation == head_generation + 1`,
///     `based_on_epoch == 0`, and `closes_epoch == Some(head_epoch)`: the
///     migrator must correctly name the head it is closing, so a commit that
///     lands on the old lineage first invalidates the migration.
///
/// Note what is deliberately NOT provable here: at a fixed head BOTH branches
/// can be satisfiable (by different submissions), so unlike the pre-P4 predicate
/// this one does not admit a single key. That is correct — the atomic INSERT,
/// not the predicate, is what picks one. What IS proved (see `proofs`) is that
/// every accepted key is lexicographically `>=` the head, never a downgrade, and
/// that opening a lineage requires naming the closed head.
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

/// The conversation's newest lineage = `COALESCE(MAX(generation), 0)`. An empty
/// log is generation 0 — the same lineage a brand-new conversation starts in and
/// the one every pre-P4 conversation is already in.
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
    // `MAX(epoch)` over an empty set is SQL NULL → `None` → head 0. Epochs are
    // non-negative by construction, so the `i64 -> u64` mapping is exact.
    let max_epoch: Option<u64> = row.get::<Option<i64>>(0)?.map(|m| m as u64);
    Ok(head_epoch_of(max_epoch) as i64)
}

/// The group's head epoch, in its CURRENT lineage. Pre-P4 this was the only
/// notion of head, and for every conversation that has never migrated (head
/// generation 0) it returns exactly what it always did.
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

    // The atomic decision, one statement → no read/write race. Two accept
    // branches, exactly mirroring the pure [`accepts`] predicate:
    //
    //   (a) CONTINUE the head lineage — the submitter is in the head generation
    //       and its `based_on_epoch` is that lineage's head. Byte-for-byte the
    //       pre-P4 rule, and the branch every commit that exists today takes.
    //
    //   (b) OPEN the next lineage — `epoch = 0` in `head_generation + 1`, and the
    //       submitter names the old lineage's head in `closes_epoch`. Naming it
    //       makes the migration a compare-and-swap on the OLD head too: if any
    //       commit lands in the old generation between the migrator's read and
    //       this INSERT, `closes_epoch` is stale and the whole migration is
    //       rejected rather than silently orphaning that commit. `?8 IS NOT NULL`
    //       is what stops an omitted `closes_epoch` from being read as "closes
    //       epoch NULL" and comparing away to no rows — it is an explicit reject,
    //       not an accident of SQL NULL semantics.
    //
    // A generation can never be opened twice: the second opener re-evaluates
    // `MAX(generation)`, sees it already advanced, and matches neither branch.
    // The UNIQUE(conversation_id, generation, epoch) index is the backstop.
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
        // Not at the head: nothing was written. Read the current head + the
        // commits the client is missing within this same view, then roll back
        // (no-op — the bundle wrote nothing) and reject. The head + missing set
        // are scoped to the SUBMITTER's generation — that is the lineage it must
        // finish draining, whether or not a newer one exists — and
        // `head_generation` is what tells it a newer one does.
        let head = head_epoch_in(&tx, &body.conversation_id, body.generation).await?;
        let head_gen = head_generation(&tx, &body.conversation_id).await?;
        let missing =
            fetch_commits(&tx, &body.conversation_id, body.generation, body.based_on_epoch).await?;
        tx.rollback().await?;
        return Ok(SubmitResponse::Rejected {
            head,
            head_generation: head_gen,
            missing,
        });
    }

    // Won the epoch. Publish the resulting-epoch GroupInfo + any Welcomes so a
    // future joiner / newly-added device can come online. All part of the same
    // transaction as the commit above, so a failure here rolls the commit back
    // too — the recipient never sees a commit with no matching Welcome.
    // Moved, not borrowed (#875): the GroupInfo carries the whole ratchet tree —
    // KBs for a small roster, hundreds of KBs for a large one — and neither it
    // nor the Welcomes are read after this block, so the `.clone()` into
    // `params!` was a full copy of every blob for nothing.
    if let Some(gi) = group_info {
        // Lexicographic (generation, epoch)-monotone guard (matches the standalone
        // /v1/group-info upsert in `writes::upsert_group_info`): older GroupInfo
        // can never clobber newer, so both writers of `mls_group_info` obey one
        // rule. The generation term is load-bearing at a migration: the successor
        // lineage's epoch 1 is NUMERICALLY BELOW the retired lineage's last epoch,
        // so an epoch-only guard would reject the successor's GroupInfo and strand
        // every externally-joining device on the retired suite.
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
        // Idempotent on the UNIQUE (conversation_id, recipient_id,
        // recipient_device_id) tuple (migration 000002 (commit-log DB)): a re-sent Welcome for
        // the same recipient/device refreshes the blob and re-arms delivery
        // (`delivered = 0`) instead of erroring or stacking a duplicate row — so
        // a resubmit/retry of this commit bundle can never wedge on a dup.
        //
        // The generation stamped here is the LINEAGE THE WELCOME ADMITS INTO, so a
        // recipient can tell "you were added to the group you're already in" from
        // "you are being moved to the successor group" BEFORE it applies the blob.
        // It needs that distinction because `max_past_epochs = 0` means it must
        // finish draining its current lineage first; a Welcome it cannot place is
        // a Welcome it might apply too early, and messages get dropped.
        tx.execute(
            "INSERT INTO mls_welcome \
                 (id, conversation_id, generation, recipient_id, welcome_data, recipient_device_id, delivered) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0) \
             ON CONFLICT(conversation_id, recipient_id, recipient_device_id) DO UPDATE SET \
                 generation = excluded.generation, \
                 welcome_data = excluded.welcome_data, \
                 delivered = 0",
            libsql::params![
                ulid::Ulid::new().to_string(),
                body.conversation_id.clone(),
                body.generation,
                w.recipient_id.clone(),
                welcome,
                w.recipient_device_id.clone(),
            ],
        )
        .await?;
    }

    tx.commit().await?;

    Ok(SubmitResponse::Accepted {
        generation: body.generation,
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
//
// Under suite generations (#454 P4) both tiers keep their meaning one key wider,
// and the floor becomes a PAIR — a generation floor plus an epoch floor within
// the head generation:
//
//   * The EPOCH floor is the existing two-tier floor, computed and applied
//     strictly WITHIN the head generation, over the devices that have reported
//     themselves to be in it. A device still on an older generation is not
//     "behind at epoch X" in any comparable sense, so it counts as UNREPORTED —
//     which conservatively disables Tier 1 entirely (see `prune_floor`'s
//     `all_reported`) rather than letting a stale lower epoch look like a bound.
//
//   * The GENERATION floor retires whole CLOSED lineages, which epoch pruning
//     alone would keep forever (their commits are all below their own head, never
//     below the head generation's floor). Tier 1: delete generations below the
//     MIN reported generation — every current member device has moved past them,
//     so nobody can still need them; zero loss. Tier 2: once the head generation
//     has itself accumulated `PRUNE_MAX_BEHIND_HEAD` epochs, a device still
//     stranded on an older lineage is by construction at least that many commits
//     behind head, which is exactly the condition Tier 2 already calls accepted
//     loss — so closed generations go, and the straggler recovers by external
//     join at the head generation like any other Tier-2 casualty.

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
/// `floor = min(max(tier1, tier2), head - 1)`, clamped to `>= 0`:
///   * `tier1 = (min_since - PRUNE_SLACK_EPOCHS)` when `all_reported`, else 0.
///   * `tier2 = head - PRUNE_MAX_BEHIND_HEAD`.
///
/// The ONLY way the floor exceeds `min_since` is Tier 2 binding (a member more
/// than `PRUNE_MAX_BEHIND_HEAD` epochs behind head) — the deliberate,
/// documented accepted-loss path. Tier 1 alone is always `<= min_since`, i.e.
/// the spec's `NoLossForCurrentMember`.
///
/// ## The clamp to `head - 1` (the safety property that matters most — #681)
///
/// `tier1` is derived from client-REPORTED epochs (`record_commit_since`), which
/// this function must treat as untrusted: a member that reports a `since` far
/// above the real head — whether by a bug or a forged report — would drive
/// `tier1` (hence the floor) above `head`, and [`delete_commits_below`] deletes
/// every `epoch < floor`. An unclamped floor `>= head` therefore deletes the
/// ENTIRE live commit log for the conversation, resetting its head to 0 and
/// leaving every current member — even one sitting exactly at head — unable to
/// advance (they must all external-join, and anyone who cannot loses history).
///
/// So the floor is clamped so it can never reach the head commit. The head
/// commit sits at `epoch = head - 1` (the log holds epochs `0..=head-1`, since
/// `head = MAX(epoch) + 1`), and the floor is EXCLUSIVE, so retaining it means
/// `floor <= head - 1`. `head - 1` — not `head` — is the correct ceiling:
/// clamping to `head` would still permit `floor == head`, which deletes
/// `epoch < head` = the whole log. Keeping the head commit keeps `head_epoch_in`
/// reading the true head and keeps the group advanceable; a member that lost
/// OLDER commits still recovers via the documented Tier-2 external-join path.
/// This mirrors [`closed_generation_floor`]'s `.min(head_generation)`: the LIVE
/// lineage is never retired wholesale, whatever the reports say.
///
/// The clamp never bites a HONEST report: an honest `min_since <= head`, so
/// `tier1 <= head - PRUNE_SLACK_EPOCHS < head - 1`, and `tier2 <= head -
/// PRUNE_MAX_BEHIND_HEAD < head - 1`. It only ever narrows pruning for absurd
/// inputs — it can never widen it. `saturating_sub` keeps `min_since == i64::MAX`
/// (or a bogus deeply-negative report) from overflowing the Tier-1 subtraction.
pub fn prune_floor(min_since: Option<i64>, all_reported: bool, head: i64) -> i64 {
    let tier1 = match (all_reported, min_since) {
        (true, Some(m)) => m.saturating_sub(PRUNE_SLACK_EPOCHS).max(0),
        // A current member has not reported: `min_since` is not a safe lower
        // bound over the roster, so Tier 1 must not prune. Tier 2 still applies.
        _ => 0,
    };
    let tier2 = (head - PRUNE_MAX_BEHIND_HEAD).max(0);
    // The head commit (`epoch = head - 1`) is NEVER pruned: the floor is
    // exclusive, so clamping to `head - 1` retains it and keeps the live log
    // non-empty for ANY report, honest or forged (#681).
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
///   * `tier1 = min_reported_generation` when `all_reported`, else 0. Every
///     current member device is at or above it, so nothing below it is needed by
///     anyone — zero loss.
///   * `tier2 = head_generation` once `head_epoch >= PRUNE_MAX_BEHIND_HEAD`. A
///     device still on a closed lineage when the successor has run that far is at
///     least `PRUNE_MAX_BEHIND_HEAD` commits behind head, which is already the
///     accepted-loss condition Tier 2 exists to bound.
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
/// lineage. Monotone on the PAIR, compared lexicographically: a stale/reordered
/// report can never LOWER a device's recorded position (which would raise the
/// floor and prune commits it still needs). The pair — not `MAX` on each column
/// independently — is what keeps it honest across a migration, where the
/// successor lineage's epoch 0 is numerically below the retired lineage's last.
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
/// A `conversation_id` is a group id, a DM channel id, or a channel id (mirrors
/// [`crate::writes::is_member`]'s three membership shapes). Revoked devices are
/// excluded — a revoked device can never rejoin (I5), so it must not pin the
/// floor down. Cross-DB: membership lives on MAIN, the high-water on LOG, so the
/// floor is composed in Rust across the two connections (no single SQL join).
///
/// `pub(crate)` — not part of the DS's public surface, but the envelope-GC
/// roster-parity test in [`crate::messages`] asserts this function and the
/// `CLEANUP_*` SQL resolve the SAME roster for a shared fixture (#722, I5).
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
/// epoch-retention write, kept small + public so it can be driven with an
/// explicit floor from tests. Never touches the
/// UNIQUE(conversation_id, generation, epoch) index (fork-dedup) — a pure row
/// DELETE leaves the remaining epochs and their one-per-epoch guarantee intact.
/// `floor <= 0` is a no-op (nothing to prune below epoch 0).
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
/// Separate from [`delete_commits_below`] because it retires whole generations
/// rather than a prefix of one: epoch pruning can never reach them (their epochs
/// are all below their own head, never below the head generation's floor).
/// `floor <= 0` is a no-op — generation 0 is never retired by this rule, since
/// there is no closed lineage below it.
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
/// prune the commit log below it. EVENT-DRIVEN — called on commit-append
/// (`submit`) and on a device's catch-up report (`commits` GET), never on a
/// timer (repo rule: no periodic polling). `main` supplies membership, `log`
/// supplies the high-waters + the commit log to prune.
pub async fn prune_commit_log(
    main: &Connection,
    log: &Connection,
    conversation_id: &str,
) -> Result<PruneReport> {
    let generation = head_generation(log, conversation_id).await?;
    let head = head_epoch_in(log, conversation_id, generation).await?;
    let members = current_member_devices(main, conversation_id).await?;
    let recorded = recorded_since(log, conversation_id).await?;

    // Two independent roster summaries, both over the SAME device list:
    //   * epoch-wise, restricted to devices reported IN the head generation — a
    //     device on an older lineage has no comparable epoch, so it is treated as
    //     unreported (which disables Tier 1) rather than as a bound;
    //   * generation-wise, over every device that has reported at all.
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

    /// I1 (within a lineage): at a fixed head the accept decision admits exactly
    /// one epoch of the HEAD generation — no two distinct same-generation epochs
    /// are ever both accepted (no fork), and any stale/forward submit in that
    /// generation is rejected. This is the pre-P4 property, restated one key
    /// wider and unchanged in substance.
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
        // A stale (`based_on < head`) or forward-gap (`based_on > head`) submit is
        // rejected — the only accepted epoch in the head generation is the head.
        // `closes_epoch` is symbolic here: naming a closed head must not buy a
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
    /// `>=` the head pair in lexicographic order. This is the widened form of the
    /// "the head only ever moves forward" property — a successor lineage's epoch 0
    /// is numerically below the old head but lexicographically above it, which is
    /// exactly the distinction generations exist to make.
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
    /// commit in a NEW generation forces it to be that generation's epoch 0 AND to
    /// correctly name the closed head of the previous generation. Without the
    /// `closes_epoch` term a migrator that read the roster before someone else's
    /// commit landed would orphan that commit; with it, the migration is rejected
    /// instead.
    #[kani::proof]
    fn p4_opening_requires_naming_closed_head() {
        let (g, e, closes, head_gen, head) = symbolic_submission();
        if accepts(g, e, closes, head_gen, head) && g != head_gen {
            assert!(g == head_gen + 1);
            assert!(e == 0);
            assert!(closes == Some(head));
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

    /// P4-L3 mutant: opens the next generation on `epoch == 0` alone, without
    /// requiring the submitter to name the closed head. This is the "stale
    /// migrator" bug — a migration built before someone else's commit landed still
    /// succeeds, orphaning that commit in a lineage nobody will ever read again.
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
            // A migration with `closes_epoch = None` (or naming the wrong epoch)
            // is accepted by the mutant → Kani finds the violation.
            assert!(closes == Some(head));
        }
    }

    /// P4-L2 mutant: accepts epoch 0 in ANY generation other than the head's,
    /// including a closed one. Models "a successor may start at epoch 0" written
    /// without pinning WHICH successor — a device still running the retired suite
    /// could then append to the lineage everyone else has left.
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

    // ─── I4 — retention-floor harnesses (prune_floor) ─────────────────────────
    //
    // Prove the REAL `prune_floor` (no forked copy): the floor is non-negative
    // (P1), Tier 1 never prunes past the slowest current member except when the
    // documented Tier-2 cap binds (P2 — the code-level Spec-B
    // `NoLossForCurrentMember`), and an unreported roster disables Tier 1
    // entirely (P3). Pure integer arithmetic — no `Vec`/`String`, no loops — so
    // CBMC is instantaneous. Each harness is paired with a `#[kani::should_panic]`
    // mutant (a deliberately-broken local copy of the floor logic) that VIOLATES
    // the property, proving the harness has teeth.
    //
    // BOUND NOTE: symbolic epochs sit in `0..=EPOCH_MAX`. `EPOCH_MAX` MUST exceed
    // `PRUNE_MAX_BEHIND_HEAD` (512) so Tier 2 can actually bind above a member's
    // epoch — otherwise P2's `floor > m` antecedent is unreachable and the proof
    // goes vacuous. `1024` keeps the space small (CBMC reasons symbolically over
    // pure arithmetic, so the range costs nothing) while exercising both tiers.
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

    /// P1 (floor_non_negative): `prune_floor(..) >= 0` for ALL inputs — the
    /// `.max(0)` clamps mean a short log or an empty roster never yields a
    /// negative floor (a negative floor would be a meaningless/underflowing
    /// `epoch < floor` DELETE bound).
    #[kani::proof]
    fn i4_floor_non_negative() {
        let min_since = symbolic_min_since();
        let all_reported: bool = kani::any();
        let head: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&head));

        let floor = prune_floor(min_since, all_reported, head);
        assert!(floor >= 0);
    }

    /// P2 (tier1_never_past_slowest = Spec-B `NoLossForCurrentMember`): with the
    /// whole roster reported and `min_since == Some(m)`, the ONLY way the floor
    /// can exceed `m` — the slowest current member's applied epoch — is Tier 2
    /// binding (`head - PRUNE_MAX_BEHIND_HEAD > m`, the documented accepted-loss
    /// path). Tier 1 alone is always `<= m`, so no commit a current member still
    /// needs is ever pruned by Tier 1.
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

    /// P3 (unreported_disables_tier1): when `all_reported == false`, the floor is
    /// EXACTLY Tier 2 (`(head - PRUNE_MAX_BEHIND_HEAD).max(0)`) for ANY
    /// `min_since` — an unreported current member can never contribute to
    /// pruning, because `min_since` is not then a safe lower bound over the
    /// roster.
    #[kani::proof]
    fn i4_unreported_disables_tier1() {
        let min_since = symbolic_min_since();
        let head: i64 = kani::any();
        kani::assume((0..=EPOCH_MAX).contains(&head));

        let floor = prune_floor(min_since, false, head);
        assert!(floor == (head - PRUNE_MAX_BEHIND_HEAD).max(0));
    }

    /// P4 (#681, floor_never_reaches_head): the floor NEVER reaches the head, for
    /// ANY reported `min_since` — including a forged one far above head. Because
    /// the floor is exclusive and the head commit sits at `head - 1`, `floor <=
    /// head - 1` is exactly "the live commit log is never wiped": whatever a
    /// device reports, the head commit survives and the group stays advanceable.
    /// The symbolic `min_since` deliberately ranges independently of `head`, so
    /// `min_since > head` (the log-wipe input) is covered.
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

    // ─── Mutants (teeth) ──────────────────────────────────────────────────────

    /// P1 mutant: the raw Tier-2 expression with NO `.max(0)` clamp and no Tier 1
    /// — a short log (`head < PRUNE_MAX_BEHIND_HEAD`) yields a negative floor.
    fn prune_floor_p1_mutant(_min_since: Option<i64>, _all_reported: bool, head: i64) -> i64 {
        // BUG: dropped the `.max(0)` floor — returns `head - 512`, negative for a
        // short log.
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
    /// the slowest member, so Tier 1 alone exceeds `m` and prunes commits a
    /// current member still needs, with Tier 2 not even binding. (This is the
    /// spec's refuted "prune past the member" variant.)
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
        // With a small head (Tier 2 idle) the mutant floor is `m + 8 > m`, so the
        // Tier-2 justification is false → Kani finds the violation.
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
        // A reported `min_since` above the Tier-2 cap makes the mutant floor
        // exceed `(head - 512).max(0)` → Kani finds the violation.
        assert!(floor == (head - PRUNE_MAX_BEHIND_HEAD).max(0));
    }

    /// P4 mutant (#681): the pre-fix floor with NO `.min(head - 1)` clamp — Tier 1
    /// is driven straight off the reported `min_since`. A reported epoch above the
    /// head (`min_since > head + SLACK`) pushes the floor to/above head, so
    /// `delete_commits_below(epoch < floor)` deletes the whole live log. `Kani`
    /// must find the input that makes the floor reach head.
    fn prune_floor_clamp_mutant(min_since: Option<i64>, all_reported: bool, head: i64) -> i64 {
        let tier1 = match (all_reported, min_since) {
            (true, Some(m)) => m.saturating_sub(PRUNE_SLACK_EPOCHS).max(0),
            _ => 0,
        };
        let tier2 = (head - PRUNE_MAX_BEHIND_HEAD).max(0);
        // BUG: dropped the `.min((head - 1).max(0))` clamp — a forged high-water
        // drives the floor above head and wipes the live commit log.
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
        // A reported `min_since` above head drives the mutant floor to/above head
        // (the log wipe) → Kani finds the violation of the retention property.
        assert!(floor <= (head - 1).max(0));
    }

    // ─── I4 under generations — closed-lineage retirement ─────────────────────
    //
    // `closed_generation_floor` is the one rule that deletes commits WHOLESALE
    // rather than a prefix, so its safety properties are the ones worth machine
    // checking: it never touches the live lineage (G1), it retires nothing without
    // evidence (G2), and Tier 1 never retires a lineage a current member device is
    // still in (G3). `GEN_MAX` is small — generations advance once per suite
    // migration, not per commit.
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
    /// `0..=head_generation`, so the lineage the group is actually running is
    /// never deleted out from under it, for ANY combination of reports.
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
    /// Neither tier has grounds, so the floor is exactly 0.
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
    /// and the slowest device in generation `g`, the ONLY way the floor can exceed
    /// `g` — i.e. delete the lineage that device is still living in — is the
    /// documented Tier-2 accepted-loss path (`head_epoch >= PRUNE_MAX_BEHIND_HEAD`).
    /// This is `NoLossForCurrentMember` lifted from epochs to lineages.
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
    /// generation above the head (or Tier 2 on a head that has since been
    /// superseded) retires the LIVE lineage — every commit in the group, gone.
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
        // A reported generation above the head drives the mutant past it → Kani
        // finds the violation.
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

    /// #681 — the retention floor NEVER reaches the head, so a forged/absurd
    /// high-water can never wipe the live commit log. The floor is clamped to
    /// `head - 1` (the head commit at `epoch = head - 1` is always retained), and
    /// the Tier-1 subtraction saturates so `min_since == i64::MAX` does not
    /// overflow. Covers head==0, min_since==head, min_since>head, the i64::MAX
    /// blow-up, and the (already Tier-1-disabling) unreported case.
    #[test]
    fn floor_never_reaches_head_even_for_a_bogus_report() {
        // The log-wipe input: a device reports an epoch far above the real head.
        // Pre-fix this returned i64::MAX - 8 (⇒ delete every epoch < that ⇒ wipe);
        // clamped it retains the head commit at epoch 9.
        assert_eq!(prune_floor(Some(i64::MAX), true, 10), 9);
        assert!(prune_floor(Some(i64::MAX), true, 10) < 10, "never at/above head");

        // min_since a plain amount above head → still clamped to head - 1.
        assert_eq!(prune_floor(Some(1_000), true, 20), 19);

        // head == 0 (empty log): nothing to retain, floor collapses to 0.
        assert_eq!(prune_floor(Some(100), true, 0), 0);
        assert_eq!(prune_floor(Some(i64::MAX), true, 0), 0);

        // min_since == head is an honest "caught up to head" report; the slack
        // keeps it well below the ceiling, so the clamp does not bite.
        assert_eq!(prune_floor(Some(20), true, 20), 20 - PRUNE_SLACK_EPOCHS);

        // An unreported roster already disables Tier 1, so a bogus min_since is
        // inert regardless of the clamp — dropping a report can only NARROW
        // pruning, never widen it.
        assert_eq!(prune_floor(Some(i64::MAX), false, 10), 0);
    }

    /// A conversation that has never migrated (head generation 0) never retires a
    /// lineage — there is none below it to retire — no matter what the roster
    /// reports or how long the log gets. This is the ONLY case that exists in the
    /// deployed fleet today, so it is the one that must be exactly a no-op.
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
    /// the successor lineage, the retired one goes. Until then it stays, even
    /// though the head has moved on — one member still draining generation 0 is
    /// enough to hold it.
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
    /// forever. Once the successor has itself run `PRUNE_MAX_BEHIND_HEAD` epochs
    /// that device is at least that far behind head — already the accepted-loss
    /// condition — so the closed lineages go and it recovers by external join.
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
