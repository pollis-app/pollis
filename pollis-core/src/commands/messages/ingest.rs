use rusqlite::OptionalExtension;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

/// One row from `message_envelope`:
/// `(id, sender_id, ciphertext, reply_to_id, target_message_id, sent_at, type)`.
type EnvelopeRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
);

/// Whether a delete tombstone's redaction is done on this device, or must be
/// retried on a later pass (#693 / #661 WS1). Consumed by
/// [`ingest_group_envelopes_interleaved`] to choose the tombstone's `EnvKind`
/// (`Delete` vs `DeletePending`) and thus whether the watermark advances past it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DeleteResolution {
    /// Terminally handled — the cursor may advance past the tombstone. Covers:
    /// the redaction was applied now; it was already applied (idempotent
    /// re-application); or the target is a message this device will never receive
    /// (a pre-join loss), so the redaction is vacuously satisfied.
    Handled,
    /// Not done, but still applicable on a later pass — the cursor must stop below
    /// the tombstone so the next fetch re-applies it. Covers: a transient local-DB
    /// error, or the target has not been ingested yet (still awaiting decryption).
    Pending,
}

/// What one conversation's pass through [`ingest_group_envelopes_interleaved`]
/// produced: how far its watermark may advance, and which inbound messages this
/// device newly decrypted (the batch a DM acknowledges with one delivery
/// receipt, #857).
struct ConvIngest {
    conversation_id: String,
    watermark: Option<String>,
    /// Ids of messages persisted for the FIRST time on this pass, authored by
    /// someone else. Empty for group channels, which never emit receipts.
    newly_delivered: Vec<String>,
}

/// The un-ingested-envelope fetch for `n` conversations at once (#875).
///
/// Bound by POSITION, never by string interpolation of the ids: `?1` is the
/// user, `?2` the device, and the conversations occupy `?3..?{n+2}` in the order
/// the caller pushes them. Pure and separately tested (`envelope_fetch_sql_*`)
/// because an off-by-one between the placeholders emitted here and the params
/// pushed at the call site is a runtime SQL error on the receive path, and that
/// arithmetic is exactly the kind that is wrong once and never again.
///
/// The watermark subquery correlates on `message_envelope.conversation_id`
/// rather than a bound id, which is what keeps "strictly past THAT
/// conversation's own watermark" true now that one query serves all of them.
fn envelope_fetch_sql(n: usize) -> String {
    let placeholders = crate::db::chunk::placeholders(n, 3);
    format!(
        "SELECT conversation_id, id, sender_id, ciphertext, reply_to_id, target_message_id, sent_at, type
         FROM message_envelope
         WHERE conversation_id IN ({placeholders})
           AND sent_at > COALESCE(
               (SELECT last_fetched_at FROM conversation_watermark
                WHERE conversation_id = message_envelope.conversation_id
                  AND user_id = ?1 AND device_id = ?2),
               ''
           )
         ORDER BY conversation_id ASC, sent_at ASC, id ASC"
    )
}

/// Decide a delete tombstone's fate from its redaction outcome. Pure so the
/// edge-case matrix is unit-testable offline (`delete_resolution_tests`) without
/// a DB:
///   * `applied_rows` — `None` if the redaction `UPDATE` errored (transient →
///     retry), else `Some(n)` rows redacted now.
///   * `target_present` — a `message` row with this id exists locally (redacted
///     or not). Only consulted on the zero-row path.
///   * `target_awaiting_ingest` — the target's envelope is still in this
///     conversation's fetch set at an epoch the replay has not reached, i.e.
///     absent *because we have not caught up yet* (retry) rather than absent
///     *because it is a pre-join message we will never receive* (terminal).
///
/// A tombstone that can never be applied on this device (target permanently
/// absent) has a DEFINED terminal state — `Handled` — and is NOT silently
/// consumed while still applicable: the only silent advance is over a target we
/// provably will never receive, which is an accepted loss (a message sent before
/// we joined the MLS tree). Ordering is a second guard: the target message is
/// always stamped below its tombstone, so an awaiting target already holds the
/// cursor below itself — this predicate makes that intent explicit rather than
/// emergent.
fn resolve_delete(
    applied_rows: Option<usize>,
    target_present: bool,
    target_awaiting_ingest: bool,
) -> DeleteResolution {
    match applied_rows {
        // The redaction UPDATE errored — treat as transient and retry. A truly
        // permanent local-DB fault (corruption / disk-full) wedges far more than
        // one tombstone and is out of scope; holding the cursor is the safe choice
        // over silently dropping the redaction.
        None => DeleteResolution::Pending,
        // Rows redacted now — applied.
        Some(n) if n >= 1 => DeleteResolution::Handled,
        // Zero rows: nothing un-redacted matched.
        Some(_) => {
            if target_present {
                // The row exists but was already redacted — idempotent, handled.
                DeleteResolution::Handled
            } else if target_awaiting_ingest {
                // Absent only because we have not decrypted the target yet.
                DeleteResolution::Pending
            } else {
                // Absent for good — a pre-join message we will never receive. The
                // redaction is vacuously satisfied; retrying forever can't help.
                DeleteResolution::Handled
            }
        }
    }
}

/// Pull new envelopes for a channel from remote, decrypt, and persist into the
/// local `message` table (the authoritative history for this device).
///
/// Opening ANY channel catches up the WHOLE MLS group — every sibling channel —
/// via [`catch_up_mls_group_interleaved`]. Because all of a group's channels
/// share ONE MLS group (`mls_group_id == group_id`) while message ingest is
/// per-conversation, advancing the shared local group for one channel could
/// otherwise leap past an epoch at which a *sibling* channel holds an
/// un-ingested message — and with `max_past_epochs = 0` that message's keys are
/// then gone forever (the cross-channel strand). Catching up at MLS-group
/// granularity makes that invalid state unrepresentable: the shared group never
/// advances past an epoch without first decrypting every bound conversation's
/// messages sealed at it.
pub async fn ingest_channel_envelopes_inner(
    state: &Arc<AppState>,
    user_id: &str,
    channel_id: &str,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Single round-trip: resolve mls_group_id AND confirm membership. Returns
    // a row only when the channel exists and the user is a member; otherwise
    // short-circuit so the read path falls back to whatever is local.
    let mls_group_id: String = {
        let mut rows = conn.query(
            "SELECT c.group_id FROM channels c
             JOIN group_member gm ON gm.group_id = c.group_id
             WHERE c.id = ?1 AND gm.user_id = ?2
             LIMIT 1",
            libsql::params![channel_id.to_string(), user_id.to_string()],
        ).await?;
        match rows.next().await? {
            Some(row) => row.get::<String>(0)?,
            None => return Ok(()),
        }
    };

    let device_id = state.device_id.lock().await.clone();

    // Drain welcomes first so a just-joined member has a local group before the
    // commit replay runs. Commit replay + decryption are driven by the
    // group-level interleaved catch-up below, NOT here.
    if let Some(ref did) = device_id {
        if let Err(e) = crate::commands::mls::poll_mls_welcomes_inner(state, user_id, did).await {
            eprintln!("[ingest] poll_mls_welcomes for {mls_group_id}: {e}");
        }
    }

    // Catch up the whole group (all sibling channels) in one epoch-stepped pass.
    catch_up_mls_group_interleaved(state, &mls_group_id, user_id).await
}

/// Group-level interleaved catch-up: advance the SHARED MLS group for
/// `mls_group_id` to head while decrypting EVERY bound conversation's messages
/// at each epoch BEFORE the group advances past it. This is THE catch-up entry
/// point — channel ingest, DM ingest, the cold-launch sweep, and the realtime
/// membership signal all route through it.
///
/// ## Why group-level (the cross-channel / sweep / realtime strand)
///
/// One MLS group backs ALL of a group's channels (`mls_group_id == group_id`),
/// but message ingest is per-conversation. A per-channel catch-up advances the
/// shared local group for one channel's epochs only, so it can leap past an
/// epoch at which a *sibling* channel — or, on the cold-launch sweep / realtime
/// commit-only paths, the very same channel — holds an un-ingested message.
/// With `max_past_epochs = 0` the ratchet keys for that epoch are then discarded
/// and the message is PERMANENTLY LOST, violating the core invariant that a
/// current member reads every message sent while they were a member.
///
/// The fix drives the commit replay ONCE for the shared group and, at each epoch
/// the replay reaches, decrypts the envelopes sealed at that epoch across ALL
/// bound conversations — before the next commit advances past it:
/// ```text
///   decrypt (any conversation) envelopes whose MLS epoch == initial_epoch
///   for each pending commit in epoch order:
///       apply the commit            (shared group advances to the next epoch)
///       decrypt (any conversation) envelopes whose MLS epoch == the new epoch
/// ```
/// Each envelope's epoch is read by PARSING it (not inferred from `sent_at`).
///
/// The commit replay runs even when there are zero envelopes, so the sweep's
/// cold-launch guarantee (advance every group to head, issue #371) is preserved
/// — interleaved ingest advances to head too, just also decrypting en route.
/// Steady state is cheap: per-conversation watermarks make repeat catch-ups
/// return zero envelopes, and enumerating the conversations is one small query.
pub async fn catch_up_mls_group_interleaved(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Enumerate every conversation bound to this MLS group. Two shapes (mirrors
    // `voice_e2ee::catch_up_mls_group`):
    //   DM    → `mls_group_id` IS the dm_channel id (single conversation).
    //   group → every `channels.id` where `group_id = mls_group_id`.
    let is_dm: bool = {
        let mut rows = conn.query(
            "SELECT 1 FROM dm_channel WHERE id = ?1 LIMIT 1",
            libsql::params![mls_group_id.to_string()],
        ).await?;
        rows.next().await?.is_some()
    };

    let conversation_ids: Vec<String> = if is_dm {
        vec![mls_group_id.to_string()]
    } else {
        let mut out = Vec::new();
        let mut rows = conn.query(
            "SELECT id FROM channels WHERE group_id = ?1",
            libsql::params![mls_group_id.to_string()],
        ).await?;
        while let Some(row) = rows.next().await? {
            out.push(row.get::<String>(0)?);
        }
        out
    };

    let device_id = state.device_id.lock().await.clone();
    let did_param = device_id.clone().unwrap_or_default();

    // Pull un-ingested envelopes for every bound conversation in ONE query
    // (strictly past THAT conversation's own watermark), then group per
    // conversation so each watermark still advances independently. Steady state
    // returns zero rows across the board.
    //
    // #875: this was a query PER CHANNEL. `conversation_ids` is every channel of
    // the group, and this function is the central receive path — every outbound
    // message, every realtime hint, every sweep and every welcome runs it — so
    // sending one message in a 20-channel group cost 20 sequential Turso round
    // trips before anything else could happen. The correlated watermark subquery
    // is unchanged: it keys off the row's own `conversation_id`, so it still
    // resolves per conversation rather than group-wide.
    //
    // The composite `ORDER BY conversation_id, sent_at ASC, id ASC` is what lets
    // one result set be partitioned into the same per-conversation lists the old
    // loop built: within a conversation the order is byte-for-byte what it was.
    let mut per_conv: Vec<(String, Vec<EnvelopeRow>)> = conversation_ids
        .iter()
        .map(|cid| (cid.clone(), Vec::new()))
        .collect();
    if !conversation_ids.is_empty() {
        // Index by conversation id so a row lands in the right bucket regardless
        // of the order the ids were listed in — and, since #916, regardless of
        // which chunk it came back in.
        let slot: HashMap<&str, usize> = conversation_ids
            .iter()
            .enumerate()
            .map(|(i, cid)| (cid.as_str(), i))
            .collect();
        // TWO slots reserved: `?1` user, `?2` device (see `envelope_fetch_sql`).
        for chunk in crate::db::chunk::bind_chunks(&conversation_ids, 2) {
            let sql = envelope_fetch_sql(chunk.len());
            let mut params: Vec<libsql::Value> = Vec::with_capacity(chunk.len() + 2);
            params.push(user_id.to_string().into());
            params.push(did_param.clone().into());
            for cid in chunk {
                params.push(cid.clone().into());
            }
            let mut rows = conn.query(&sql, params).await?;
            while let Some(row) = rows.next().await? {
                let cid: String = row.get::<String>(0)?;
                let Some(&i) = slot.get(cid.as_str()) else {
                    continue;
                };
                per_conv[i].1.push((
                    row.get::<String>(1)?,
                    row.get::<String>(2)?,
                    row.get::<String>(3)?,
                    row.get::<Option<String>>(4)?,
                    row.get::<Option<String>>(5)?,
                    row.get::<String>(6)?,
                    row.get::<String>(7)?,
                ));
            }
        }
    }

    // Drive the shared group's replay once, decrypting each conversation's
    // envelopes as the group reaches their epoch. Returns the per-conversation
    // watermark each device may advance to, plus the messages newly decrypted on
    // this pass (the delivery-receipt batch).
    let results: Vec<ConvIngest> =
        ingest_group_envelopes_interleaved(state, user_id, mls_group_id, &per_conv, is_dm).await?;

    // Advance each conversation's watermark through the Delivery Service (best
    // effort — DS failures are logged and ignored). Envelope GC is NOT fired from
    // here any more (#689): the trigger moved server-side to the DS's own sweep
    // (`pollis-delivery` `sweep_envelope_gc`), so a conversation whose members all
    // go quiet is still collected and a chatty one no longer runs GC on every
    // ingest. This path only reports the watermark that GC reads.
    for res in &results {
        let cid = &res.conversation_id;
        if let (Some(ts), Some(did)) = (res.watermark.as_ref(), device_id.as_ref()) {
            let body = pollis_api::messages::WatermarkBody {
                conversation_id: cid.clone(),
                user_id: Some(user_id.to_string()),
                device_id: did.clone(),
                last_fetched_at: ts.clone(),
            };
            if let Err(e) =
                crate::commands::mls::ds_post_ok(state, &body).await
            {
                eprintln!("[watermark] catch_up_group: DS advance failed for {cid}: {e}");
            }
        }
    }

    // Delivery receipts (#857): this device fetched and DECRYPTED these
    // messages, which is precisely what "delivered" means — a statement about a
    // device, never about a human having seen anything. One batched control
    // frame per conversation covers a whole catch-up burst.
    //
    // `emit_receipt` re-checks both gates (DMs only, and the sending-side
    // opt-out), so this call site cannot widen the feature's scope by mistake;
    // the `is_dm` short-circuit here just avoids a pointless round-trip for the
    // group-channel case. Best-effort — a receipt that fails to send must never
    // fail an ingest pass, because the message itself already landed.
    if is_dm {
        for res in &results {
            if res.newly_delivered.is_empty() {
                continue;
            }
            if let Err(e) = super::receipts::emit_delivered(
                state,
                &res.conversation_id,
                &res.newly_delivered,
            )
            .await
            {
                eprintln!(
                    "[receipts] delivered emit failed for {}: {e}",
                    res.conversation_id
                );
            }
        }
    }

    Ok(())
}

/// Core of [`catch_up_mls_group_interleaved`]: drive the shared MLS group's
/// commit replay ONCE, decrypting the envelopes (from ANY bound conversation)
/// sealed at each epoch while the group is still AT that epoch, and compute the
/// watermark each conversation may independently advance to.
///
/// ## Watermark (per conversation)
///
/// For each conversation, returns the sent_at its watermark should advance to:
/// the max over the contiguous (sent_at-ordered) prefix of THAT conversation's
/// envelopes that are definitively handled, stopping STRICTLY BEFORE the first
/// one that must be retried later. "Handled" = decrypted this pass, OR
/// permanently undeliverable to us (an unreachable pre-join epoch, or unparseable
/// bytes), OR an epoch-independent tombstone. "Retry later" = an MLS epoch beyond
/// the highest epoch this pass reached (`max_fired_epoch`) — the commit that
/// would let us decrypt it hasn't arrived yet. `max_fired_epoch` is group-wide:
/// all bound conversations share the one MLS group, so "have we reached this
/// envelope's epoch yet?" is answered by the single shared replay. Stopping
/// before such an envelope (rather than taking a global max) is what guarantees
/// an undecryptable-for-now message is re-fetched on a later pass instead of
/// being skipped. Pre-join messages do NOT stop the watermark; the Delivery
/// Service's envelope GC is the backstop that removes them from the fetch set.
async fn ingest_group_envelopes_interleaved(
    state: &Arc<AppState>,
    user_id: &str,
    mls_group_id: &str,
    per_conv: &[(String, Vec<EnvelopeRow>)],
    // Whether this MLS group backs a DM. Receipts (#857) are DMs-only, so this
    // gates both recording an inbound receipt frame and collecting the
    // delivered batch.
    is_dm: bool,
) -> Result<Vec<ConvIngest>> {
    // Pre-parse each message/edit envelope's MLS position across ALL bound
    // conversations (delete/unknown carry none) and index `(conv_idx, env_idx)`
    // by position so the per-epoch hook decrypts exactly the ones sealed at the
    // point the replay just reached, regardless of which conversation they're in.
    //
    // The position is the `(generation, epoch)` PAIR, not the epoch (#454 P4).
    // A migrated conversation restarts at epoch 0 in the next generation, so
    // keying on the epoch alone would collide envelopes from two different
    // lineages — sealed under different suites and different key schedules — into
    // one bucket, and hand the successor group ciphertext only the retired group
    // could ever open.
    let mut by_epoch: HashMap<(i64, u64), Vec<(usize, usize)>> = HashMap::new();
    // epoch_of[ci][ei] mirrors per_conv[ci].1[ei]'s parsed position.
    let mut epoch_of: Vec<Vec<Option<(i64, u64)>>> = Vec::with_capacity(per_conv.len());
    // The hex-decoded ciphertext of every envelope that HAS a position, kept
    // from this pass rather than decoded a second time in the hook (#875). The
    // decode is a per-envelope allocation and parse of an ~1KB hex string, and
    // it used to happen twice for every message and edit on every pass — once
    // here to read the lineage, then again inside `decrypt_and_persist_one` on
    // the identical string. Keying on `(ci, ei)` means only envelopes that
    // actually reached this map are held, and they are dropped with it.
    let mut decoded: HashMap<(usize, usize), Vec<u8>> = HashMap::new();
    for (ci, (_cid, envs)) in per_conv.iter().enumerate() {
        let mut this: Vec<Option<(i64, u64)>> = Vec::with_capacity(envs.len());
        for (ei, env) in envs.iter().enumerate() {
            let (_, _, ciphertext, _, _, _, env_type) = env;
            let bytes = match env_type.as_str() {
                "message" | "edit" => ciphertext
                    .strip_prefix("mls:")
                    .and_then(|h| hex::decode(h).ok()),
                _ => None,
            };
            let epoch = bytes
                .as_deref()
                .and_then(crate::commands::mls::envelope_lineage);
            this.push(epoch);
            if let Some(e) = epoch {
                by_epoch.entry(e).or_default().push((ci, ei));
                // Only reachable when `bytes` is `Some` — `envelope_lineage`
                // parses those very bytes.
                if let Some(b) = bytes {
                    decoded.insert((ci, ei), b);
                }
            }
        }
        epoch_of.push(this);
    }

    // Drive the commit replay, decrypting each epoch's messages/edits (any
    // conversation) as the shared group reaches it. `max_fired_epoch` records the
    // highest epoch the replay actually reached — the watermark's "have we caught
    // up to it yet?" boundary. Scoped so the hook's borrows end before the
    // watermark loop. Runs even with zero envelopes so the group still advances
    // to head (the cold-launch sweep guarantee).
    let mut max_fired_epoch: Option<(i64, u64)> = None;
    // Per-conversation batch of messages newly persisted on this pass — the
    // delivery receipts (#857) this device owes once the replay is done.
    let mut newly_delivered: Vec<Vec<String>> = vec![Vec::new(); per_conv.len()];
    {
        let mut on_epoch = |conn: &rusqlite::Connection, generation: i64, epoch: u64| {
            // Lexicographic on `(generation, epoch)`: a successor lineage restarts
            // at epoch 0, numerically BELOW the retired lineage's last epoch, so a
            // scalar max would run backwards across a migration.
            let key = (generation, epoch);
            max_fired_epoch = Some(max_fired_epoch.map_or(key, |m| m.max(key)));
            if let Some(indices) = by_epoch.get(&key) {
                // One group load for the whole epoch's run (#875). Every
                // envelope in `indices` is sealed at the epoch the replay has
                // just reached, so they all decrypt against the same key
                // schedule — the reason a single `MlsDecryptor` is valid here
                // and only here. It is dropped at the end of this hook, before
                // the replay applies the commit that moves the group on.
                let Some(mut decryptor) =
                    crate::commands::mls::MlsDecryptor::open(conn, mls_group_id)
                else {
                    // No local group: nothing at this epoch is decryptable.
                    // The envelopes stay put for a later pass, exactly as a
                    // per-envelope decrypt failure would leave them.
                    return;
                };
                for &(ci, ei) in indices {
                    // Present for every index in `by_epoch` by construction.
                    let Some(bytes) = decoded.get(&(ci, ei)) else {
                        continue;
                    };
                    if let Some(delivered_id) = decrypt_and_persist_one(
                        conn,
                        &mut decryptor,
                        &per_conv[ci].0,
                        &per_conv[ci].1[ei],
                        bytes,
                        user_id,
                        is_dm,
                    ) {
                        newly_delivered[ci].push(delivered_id);
                    }
                }
            }
        };
        if let Err(e) = crate::commands::mls::process_pending_commits_inner_with_hook(
            state,
            mls_group_id,
            user_id,
            &mut on_epoch,
        )
        .await
        {
            eprintln!("[ingest] process_pending_commits for {mls_group_id}: {e}");
        }
    }

    // Apply the epoch-independent envelopes (admin-issued delete tombstones)
    // across all conversations after decryption, so a tombstone whose target was
    // just decrypted this pass still lands. The ciphertext is empty — there is
    // nothing to decrypt.
    //
    // The redaction's OUTCOME decides whether the tombstone is consumed (#693 /
    // #661 WS1). A tombstone whose `UPDATE` did not take effect but still can on a
    // later pass — the target message has not been ingested yet, or a transient
    // local-DB error — is recorded in `pending_deletes` and re-classified to
    // `EnvKind::DeletePending` below, so the watermark holds strictly below it and
    // the next fetch re-applies it (mirroring the undecryptable-message retry
    // path). A tombstone whose redaction landed, was already applied, or targets a
    // message this device will never receive (a pre-join loss) is terminally
    // handled and the cursor advances past it. See `resolve_delete`.
    let mut pending_deletes: HashSet<(usize, usize)> = HashSet::new();
    {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        for (ci, (_cid, envs)) in per_conv.iter().enumerate() {
            for (ei, (_, _, _, _, target_id, _, env_type)) in envs.iter().enumerate() {
                if env_type != "delete" {
                    continue;
                }
                // A tombstone with no target cannot redact anything — there is
                // nothing to apply and nothing a retry could fix. Terminally
                // handled: leave it to advance the cursor.
                let Some(tid) = target_id.as_ref() else {
                    continue;
                };
                let now = chrono::Utc::now().to_rfc3339();
                // `None` = the redaction UPDATE errored (treated as transient →
                // retry). `Some(n)` = n rows redacted now.
                let applied_rows: Option<usize> = db
                    .conn()
                    .execute(
                        "UPDATE message SET content = NULL, deleted_at = ?1
                         WHERE id = ?2 AND deleted_at IS NULL",
                        rusqlite::params![now, tid],
                    )
                    .ok();
                // Only meaningful on the zero-row path: does a row with this id
                // exist at all (already redacted → present; genuinely absent →
                // not)? Skip the query when the UPDATE already told us (>=1 row
                // matched ⇒ present; error ⇒ Pending regardless).
                let target_present = if applied_rows == Some(0) {
                    db.conn()
                        .query_row(
                            "SELECT 1 FROM message WHERE id = ?1",
                            rusqlite::params![tid],
                            |_| Ok(true),
                        )
                        .optional()
                        .ok()
                        .flatten()
                        .unwrap_or(false)
                } else {
                    true
                };
                // Is the target still awaiting ingest in THIS conversation's fetch
                // set — a message/edit at an epoch the replay has not reached? That
                // is "absent because we have not caught up yet" (must retry), as
                // distinct from "absent because it is a pre-join message we will
                // never receive" (terminal). Uses the same `is_handled` the
                // watermark does, over the target envelope's parsed epoch.
                let target_awaiting = envs.iter().enumerate().any(|(ej, e)| {
                    &e.0 == tid
                        && !super::watermark::is_handled(
                            super::watermark::EnvKind::from_type(e.6.as_str()),
                            epoch_of[ci][ej],
                            max_fired_epoch,
                        )
                });
                if let DeleteResolution::Pending =
                    resolve_delete(applied_rows, target_present, target_awaiting)
                {
                    pending_deletes.insert((ci, ei));
                }
            }
        }
    }

    // Compute each conversation's watermark independently over its own
    // envelopes, delegating to the PROVEN pure function (Kani P1/P2/P3 in
    // `super::watermark`) — the "is this handled / how far may the cursor
    // advance" decision is the message-delivery safety property, so the runtime
    // path goes through the verified function, not a copy. Build the
    // `(sent_at, EnvKind, Option<epoch>)` view from the existing envelope rows +
    // pre-parsed `epoch_of`; `&str` keys avoid cloning every `sent_at`.
    let mut out: Vec<ConvIngest> = Vec::with_capacity(per_conv.len());
    for (ci, (cid, envs)) in per_conv.iter().enumerate() {
        let items: Vec<super::watermark::EnvView<&str, (i64, u64)>> = envs
            .iter()
            .enumerate()
            .map(|(ei, env)| {
                // A tombstone whose redaction did not take effect this pass is
                // held back as `DeletePending` (not `Delete`) so the cursor stops
                // strictly below it — the fix for #693 / #661 WS1.
                let kind = if env.6 == "delete" && pending_deletes.contains(&(ci, ei)) {
                    super::watermark::EnvKind::DeletePending
                } else {
                    super::watermark::EnvKind::from_type(env.6.as_str())
                };
                (env.5.as_str(), kind, epoch_of[ci][ei])
            })
            .collect();
        let watermark =
            super::watermark::next_watermark(&items, max_fired_epoch).map(str::to_string);
        out.push(ConvIngest {
            conversation_id: cid.clone(),
            watermark,
            newly_delivered: std::mem::take(&mut newly_delivered[ci]),
        });
    }
    Ok(out)
}

/// Decrypt one `message` or `edit` envelope at the group's CURRENT epoch and
/// persist it. Invoked by [`ingest_group_envelopes_interleaved`]'s per-epoch
/// hook, so the caller has already positioned the local group at this envelope's
/// epoch.
/// Infallible: a failed decrypt or a transient DB error simply leaves nothing
/// persisted (the envelope stays in `message_envelope` for a later retry), the
/// same outcome the watermark logic accounts for.
///
/// Returns the message id when this call persisted a NEW inbound message in a
/// DM — i.e. exactly what a delivery receipt (#857) acknowledges. `None` for
/// everything else: our own messages, re-ingested duplicates, edits, tombstones,
/// group channels, and control frames (which are never themselves acknowledged,
/// so a receipt can never trigger a receipt).
fn decrypt_and_persist_one(
    conn: &rusqlite::Connection,
    // The epoch's already-loaded group (#875). Taken rather than re-derived so
    // a run of envelopes at one epoch costs one group load, not one per message.
    decryptor: &mut crate::commands::mls::MlsDecryptor<'_>,
    conversation_id: &str,
    env: &EnvelopeRow,
    // The envelope's already-hex-decoded ciphertext (#875). Decoded once by the
    // caller's lineage pre-parse; decoding it again here was ~1KB of hex parsed
    // twice for every message and edit on every ingest pass.
    bytes: &[u8],
    user_id: &str,
    is_dm: bool,
) -> Option<String> {
    // `sender_id` (the server-writable envelope column) is intentionally NOT
    // read for attribution — the sender is taken from the MLS credential inside
    // the ciphertext (sealed sender, `docs/metadata-minimization-design.md` §2).
    let (id, _sender_id, _ciphertext, reply_to_id, target_id, sent_at, env_type) = env;
    match env_type.as_str() {
        "message" => {
            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM message WHERE id = ?1",
                    rusqlite::params![id],
                    |_| Ok(true),
                )
                .optional()
                .ok()
                .flatten()
                .unwrap_or(false);
            if exists {
                return None;
            }
            // Attribute from the MLS-authenticated credential inside the
            // ciphertext, NOT the server-writable `message_envelope.sender_id`
            // tuple field (which may be a blinded sentinel under sealed sender).
            // See `docs/metadata-minimization-design.md` §2.
            let Some((plain, cred_sender)) = decryptor.decrypt(bytes) else {
                // Decrypt failed — leave the envelope in message_envelope for a
                // future retry; the watermark is computed to not skip past it.
                return None;
            };
            match super::framing::classify(&plain) {
                // "Delete for everyone" (E2EE redaction). Honor it ONLY when the
                // redaction's MLS-authenticated author (`cred_sender`) is the
                // SAME as the target message's author (its stored `sender_id`,
                // itself the MLS credential recorded at ingest). This makes the
                // invariant cryptographic: neither the server nor another member
                // can redact a message they did not author — a mismatched or
                // unknown target is silently ignored. Admin moderation uses the
                // separate server-issued `type='delete'` tombstone instead. The
                // redaction is a control message and is NEVER stored as a
                // visible `message` row.
                super::framing::Frame::Redaction(target_message_id) => {
                    let author: Option<String> = conn
                        .query_row(
                            "SELECT sender_id FROM message WHERE id = ?1",
                            rusqlite::params![target_message_id],
                            |row| row.get(0),
                        )
                        .optional()
                        .ok()
                        .flatten();
                    if author.as_deref() == Some(cred_sender.as_str()) {
                        let now = chrono::Utc::now().to_rfc3339();
                        let _ = conn.execute(
                            "UPDATE message SET content = NULL, deleted_at = ?1
                             WHERE id = ?2 AND deleted_at IS NULL",
                            rusqlite::params![now, target_message_id],
                        );
                    }
                }
                // Ordinary text / attachment message. Strip size padding (issue
                // #331 v2, §4.1) — a no-op for legacy unpadded sends and for
                // attachment envelopes, so old and new clients interoperate.
                // `thread_id` (#825) rides inside the ciphertext as a trailer on
                // this same frame, so it is recovered here and nowhere else —
                // the DS and Turso never see it. `None` for every unthreaded
                // send and for every message from a threads-unaware client.
                super::framing::Frame::Text { text, thread_id } => {
                    if let Ok(text) = String::from_utf8(text) {
                        let inserted = conn.execute(
                            "INSERT OR IGNORE INTO message
                             (id, conversation_id, sender_id, ciphertext, content, reply_to_id, thread_id, sent_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                            rusqlite::params![id, conversation_id, cred_sender, bytes, text, reply_to_id, thread_id, sent_at],
                        ).unwrap_or(0);
                        // Delivered (#857) means exactly this: the envelope was
                        // fetched AND decrypted AND stored, here, now. Only a
                        // DM, only someone else's message, and only the first
                        // time — a re-ingest returns early above, so a message
                        // is never acknowledged twice.
                        if inserted > 0 && is_dm && cred_sender != user_id {
                            return Some(id.clone());
                        }
                    }
                }
                // A delivery/read receipt from another member (#857). It is a
                // CONTROL message: no `message` row is ever written for it, on
                // either side, so it can never be rendered and — crucially —
                // never itself acknowledged. That is what keeps two clients from
                // ping-ponging receipts forever.
                super::framing::Frame::Receipt { kind, at, message_ids } => {
                    // Our own receipt, echoed back to us from the conversation
                    // we posted it to. Nothing to learn from it.
                    if cred_sender == user_id {
                        return None;
                    }
                    // `is_dm` is passed through to the recorder rather than
                    // checked here: #857 excludes group channels on the
                    // recording side as well as the emitting side, and putting
                    // that decision in `receipts_permitted` keeps it one
                    // testable predicate instead of two call sites that have to
                    // agree. A receipt frame that somehow reaches a group
                    // channel is therefore dropped, not honoured.
                    //
                    // `cred_sender` is the MLS-authenticated author of the
                    // frame, so a member can only ever record a receipt in their
                    // OWN name — neither the server nor another member can
                    // forge "carol read this".
                    for target in &message_ids {
                        super::receipts::record_receipt(
                            conn,
                            target,
                            &cred_sender,
                            kind,
                            &at,
                            is_dm,
                        );
                    }
                }
            }
        }
        "edit" => {
            if let Some(tid) = target_id.as_ref() {
                // Honor the edit ONLY when its MLS-authenticated author
                // (`cred_sender`) equals the target message's stored author. This
                // MIRRORS the Redaction branch and is the client-side enforcement
                // that REPLACES the DS author check dropped under Solution A
                // (#607): the DS now accepts an edit envelope from any member, so
                // authorship is proven here, cryptographically — neither the
                // server nor another member can edit a message they did not write.
                // A mismatched or unknown-target edit is silently dropped.
                let (plain, cred_sender) = decryptor.decrypt(bytes)?;
                let author: Option<String> = conn
                    .query_row(
                        "SELECT sender_id FROM message WHERE id = ?1",
                        rusqlite::params![tid],
                        |row| row.get(0),
                    )
                    .optional()
                    .ok()
                    .flatten();
                if author.as_deref() != Some(cred_sender.as_str()) {
                    return None;
                }
                // Strip size padding (§4.1); no-op for legacy/unpadded edits.
                if let Ok(text) = String::from_utf8(super::framing::strip(&plain)) {
                    let now = chrono::Utc::now().to_rfc3339();
                    let _ = conn.execute(
                        "UPDATE message SET content = ?1, edited_at = ?2
                         WHERE id = ?3 AND deleted_at IS NULL",
                        rusqlite::params![text, now, tid],
                    );
                }
            }
        }
        _ => {}
    }
    // Only the "persisted a new inbound DM message" path above returns an id;
    // every other outcome is nothing to acknowledge.
    None
}

/// Frontend-triggerable ingest for a channel. Used by LiveKit real-time hints
/// and channel-focus pre-warm paths that want to persist new envelopes without
/// reading a page.
pub async fn ingest_channel_envelopes(
    user_id: String,
    channel_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    ingest_channel_envelopes_inner(state, &user_id, &channel_id).await
}

/// Pull new envelopes for a DM from remote, decrypt, and persist into the local
/// `message` table. DM MLS group is keyed by conversation_id directly.
pub async fn ingest_dm_envelopes_inner(
    state: &Arc<AppState>,
    user_id: &str,
    dm_channel_id: &str,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    let is_member: bool = {
        let mut rows = conn.query(
            "SELECT 1 FROM dm_channel_member
             WHERE dm_channel_id = ?1 AND user_id = ?2
             LIMIT 1",
            libsql::params![dm_channel_id.to_string(), user_id.to_string()],
        ).await?;
        rows.next().await?.is_some()
    };
    if !is_member {
        return Ok(());
    }

    let device_id = state.device_id.lock().await.clone();

    if let Some(ref did) = device_id {
        if let Err(e) = crate::commands::mls::poll_mls_welcomes_inner(state, user_id, did).await {
            eprintln!("[ingest] poll_mls_welcomes for DM {dm_channel_id}: {e}");
        }
    }

    // Re-verify each peer's `account_id_pub` against the local TOFU pin on
    // every ingest. If a server-side swap or peer account reset has changed
    // the key, this emits a `KeyChanged` realtime event so the conversation
    // gets an inline banner (Signal-style) the moment we observe the change,
    // not only when the user opens the peer's profile.
    let peer_ids: Vec<String> = {
        let mut out = Vec::new();
        let mut rows = conn.query(
            "SELECT user_id FROM dm_channel_member \
             WHERE dm_channel_id = ?1 AND user_id <> ?2",
            libsql::params![dm_channel_id.to_string(), user_id.to_string()],
        ).await?;
        while let Some(row) = rows.next().await? {
            out.push(row.get::<String>(0)?);
        }
        out
    };
    // Batched (#875): one Turso query for the whole peer set instead of one per
    // peer. Identical semantics — `batch_check_and_pin_account_keys` pins new
    // peers, updates and un-verifies a changed pin, and emits the same
    // `KeyChanged` event — and it is what `mls::reconcile` already uses. This
    // runs on EVERY ingest pass, so for a multi-party DM it was a round trip per
    // peer per realtime hint.
    if let Err(e) = crate::commands::safety::batch_check_and_pin_account_keys(state, &peer_ids).await
    {
        eprintln!("[ingest] batch_check_and_pin_account_keys for {dm_channel_id}: {e}");
    }

    // A DM's MLS group backs exactly one conversation (mls_group_id ==
    // dm_channel_id), so the group-level catch-up degenerates to the single-
    // conversation case — same interleaved decrypt + watermark + GC path.
    catch_up_mls_group_interleaved(state, dm_channel_id, user_id).await
}

/// Frontend-triggerable ingest for a DM channel.
pub async fn ingest_dm_envelopes(
    user_id: String,
    dm_channel_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    ingest_dm_envelopes_inner(state, &user_id, &dm_channel_id).await
}

#[cfg(test)]
mod delete_resolution_tests {
    //! #693 / #661 (WS1): the redaction-outcome → tombstone-fate decision, the
    //! fix for a delete that was consumed-and-lost when its `UPDATE` matched zero
    //! rows or errored. These pin the full edge-case matrix the reviewer called
    //! out — applied, already-redacted, transient error, and the two flavours of
    //! "target absent" that MUST be distinguished (not-caught-up vs pre-join). The
    //! complementary watermark half is `watermark::delete_consumption_tests`.

    use super::{resolve_delete, DeleteResolution};

    /// The redaction matched a live row this pass — done.
    #[test]
    fn applied_now_is_handled() {
        assert_eq!(
            resolve_delete(Some(1), true, false),
            DeleteResolution::Handled
        );
    }

    /// Zero rows but the row exists (already redacted on a prior pass) — idempotent
    /// re-application is handled, NOT a zero-row failure that wedges the cursor.
    #[test]
    fn already_redacted_is_handled() {
        assert_eq!(
            resolve_delete(Some(0), true, false),
            DeleteResolution::Handled
        );
    }

    /// Zero rows, no local row, but the target is still awaiting ingest (an
    /// unreached epoch in this fetch set) — absent because we have not caught up
    /// yet, so retry rather than consume.
    #[test]
    fn absent_but_awaiting_ingest_is_pending() {
        assert_eq!(
            resolve_delete(Some(0), false, true),
            DeleteResolution::Pending
        );
    }

    /// Zero rows, no local row, and the target is NOT awaiting ingest — a pre-join
    /// message this device will never receive. Terminal: the redaction is
    /// vacuously satisfied and retrying forever cannot help, so it is handled (and
    /// does NOT spin).
    #[test]
    fn absent_pre_join_is_handled_terminally() {
        assert_eq!(
            resolve_delete(Some(0), false, false),
            DeleteResolution::Handled
        );
    }

    /// The redaction UPDATE errored (a transient local-DB fault) — retry, never
    /// silently consume. Holds regardless of the (unknown) target state.
    #[test]
    fn transient_db_error_is_pending() {
        assert_eq!(resolve_delete(None, false, false), DeleteResolution::Pending);
        assert_eq!(resolve_delete(None, true, false), DeleteResolution::Pending);
    }
}

#[cfg(test)]
mod envelope_fetch_sql_tests {
    use super::envelope_fetch_sql;

    /// The invariant the call site depends on: exactly `n` conversation
    /// placeholders, numbered `?3..?{n+2}`, leaving `?1`/`?2` for the user and
    /// device. Off by one and the receive path takes a runtime SQL error.
    #[test]
    fn placeholders_start_at_three_and_are_contiguous() {
        for n in 1..=25usize {
            let sql = envelope_fetch_sql(n);
            let list = sql
                .split_once("conversation_id IN (")
                .and_then(|(_, rest)| rest.split_once(')'))
                .map(|(inner, _)| inner.to_string())
                .expect("the IN list is present");
            let got: Vec<&str> = list.split(',').collect();
            let want: Vec<String> = (0..n).map(|i| format!("?{}", i + 3)).collect();
            assert_eq!(got, want, "placeholder list wrong for n={n}");
            assert!(sql.contains("user_id = ?1"), "?1 must stay the user");
            assert!(sql.contains("device_id = ?2"), "?2 must stay the device");
        }
    }

    /// One query, not one per conversation: the whole point of #875 here. A
    /// re-introduced per-conversation loop would bind exactly one id, so a
    /// 20-channel group producing a single-placeholder statement is the shape
    /// this forbids.
    #[test]
    fn one_statement_covers_every_conversation() {
        let sql = envelope_fetch_sql(20);
        assert!(sql.contains("?22"), "all 20 conversations must be in ONE statement");
        assert_eq!(sql.matches("FROM message_envelope").count(), 1);
    }

    /// The watermark must stay PER CONVERSATION. Correlating on the outer row's
    /// own `conversation_id` is what preserves "strictly past THAT
    /// conversation's watermark" — a bound id here would apply one
    /// conversation's watermark to all of them and silently skip messages.
    #[test]
    fn the_watermark_subquery_correlates_per_conversation() {
        let sql = envelope_fetch_sql(3);
        assert!(
            sql.contains("conversation_id = message_envelope.conversation_id"),
            "the watermark must be resolved per row, not once for the batch:\n{sql}"
        );
    }

    /// Rows come back grouped by conversation and, within one, in the exact
    /// order the per-conversation query returned them — that ordering is what
    /// the epoch interleave and the watermark advance both assume.
    #[test]
    fn ordering_groups_by_conversation_then_preserves_the_old_order() {
        let sql = envelope_fetch_sql(2);
        assert!(sql.contains("ORDER BY conversation_id ASC, sent_at ASC, id ASC"), "{sql}");
    }
}
