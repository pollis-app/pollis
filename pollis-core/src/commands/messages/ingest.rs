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

    // Pull un-ingested envelopes for each bound conversation (strictly past THAT
    // conversation's own watermark), grouped per conversation so each watermark
    // advances independently. Steady state returns zero rows across the board.
    let mut per_conv: Vec<(String, Vec<EnvelopeRow>)> =
        Vec::with_capacity(conversation_ids.len());
    for cid in &conversation_ids {
        let mut envs: Vec<EnvelopeRow> = Vec::new();
        let mut rows = conn.query(
            "SELECT id, sender_id, ciphertext, reply_to_id, target_message_id, sent_at, type
             FROM message_envelope
             WHERE conversation_id = ?1
               AND sent_at > COALESCE(
                   (SELECT last_fetched_at FROM conversation_watermark
                    WHERE conversation_id = ?1 AND user_id = ?2 AND device_id = ?3),
                   ''
               )
             ORDER BY sent_at ASC, id ASC",
            libsql::params![cid.clone(), user_id.to_string(), did_param.clone()],
        ).await?;
        while let Some(row) = rows.next().await? {
            envs.push((
                row.get::<String>(0)?,
                row.get::<String>(1)?,
                row.get::<String>(2)?,
                row.get::<Option<String>>(3)?,
                row.get::<Option<String>>(4)?,
                row.get::<String>(5)?,
                row.get::<String>(6)?,
            ));
        }
        per_conv.push((cid.clone(), envs));
    }

    // Drive the shared group's replay once, decrypting each conversation's
    // envelopes as the group reaches their epoch. Returns the per-conversation
    // watermark each device may advance to.
    let watermarks: Vec<(String, Option<String>)> =
        ingest_group_envelopes_interleaved(state, user_id, mls_group_id, &per_conv).await?;

    // Advance each conversation's watermark through the Delivery Service (best
    // effort — DS failures are logged and ignored). Envelope GC is NOT fired from
    // here any more (#689): the trigger moved server-side to the DS's own sweep
    // (`pollis-delivery` `sweep_envelope_gc`), so a conversation whose members all
    // go quiet is still collected and a chatty one no longer runs GC on every
    // ingest. This path only reports the watermark that GC reads.
    for (cid, ts_opt) in &watermarks {
        if let (Some(ts), Some(did)) = (ts_opt.as_ref(), device_id.as_ref()) {
            let body = serde_json::json!({
                "conversation_id": cid,
                "user_id": user_id,
                "device_id": did,
                "last_fetched_at": ts,
            });
            if let Err(e) =
                crate::commands::mls::ds_post_ok(state, "/v1/watermarks/advance", &body).await
            {
                eprintln!("[watermark] catch_up_group: DS advance failed for {cid}: {e}");
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
) -> Result<Vec<(String, Option<String>)>> {
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
    for (ci, (_cid, envs)) in per_conv.iter().enumerate() {
        let mut this: Vec<Option<(i64, u64)>> = Vec::with_capacity(envs.len());
        for (ei, env) in envs.iter().enumerate() {
            let (_, _, ciphertext, _, _, _, env_type) = env;
            let epoch = match env_type.as_str() {
                "message" | "edit" => ciphertext
                    .strip_prefix("mls:")
                    .and_then(|h| hex::decode(h).ok())
                    .and_then(|b| crate::commands::mls::envelope_lineage(&b)),
                _ => None,
            };
            this.push(epoch);
            if let Some(e) = epoch {
                by_epoch.entry(e).or_default().push((ci, ei));
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
    {
        let mut on_epoch = |conn: &rusqlite::Connection, generation: i64, epoch: u64| {
            // Lexicographic on `(generation, epoch)`: a successor lineage restarts
            // at epoch 0, numerically BELOW the retired lineage's last epoch, so a
            // scalar max would run backwards across a migration.
            let key = (generation, epoch);
            max_fired_epoch = Some(max_fired_epoch.map_or(key, |m| m.max(key)));
            if let Some(indices) = by_epoch.get(&key) {
                for &(ci, ei) in indices {
                    decrypt_and_persist_one(
                        conn,
                        &per_conv[ci].0,
                        mls_group_id,
                        &per_conv[ci].1[ei],
                    );
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
    let mut out: Vec<(String, Option<String>)> = Vec::with_capacity(per_conv.len());
    for (ci, (cid, envs)) in per_conv.iter().enumerate() {
        let items: Vec<(&str, super::watermark::EnvKind, Option<(i64, u64)>)> = envs
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
        out.push((cid.clone(), watermark));
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
fn decrypt_and_persist_one(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    mls_group_id: &str,
    env: &EnvelopeRow,
) {
    // `sender_id` (the server-writable envelope column) is intentionally NOT
    // read for attribution — the sender is taken from the MLS credential inside
    // the ciphertext (sealed sender, `docs/metadata-minimization-design.md` §2).
    let (id, _sender_id, ciphertext, reply_to_id, target_id, sent_at, env_type) = env;
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
                return;
            }
            let Some(bytes) = ciphertext
                .strip_prefix("mls:")
                .and_then(|h| hex::decode(h).ok())
            else {
                return;
            };
            // Attribute from the MLS-authenticated credential inside the
            // ciphertext, NOT the server-writable `message_envelope.sender_id`
            // tuple field (which may be a blinded sentinel under sealed sender).
            // See `docs/metadata-minimization-design.md` §2.
            let Some((plain, cred_sender)) =
                crate::commands::mls::try_mls_decrypt(conn, mls_group_id, &bytes)
            else {
                // Decrypt failed — leave the envelope in message_envelope for a
                // future retry; the watermark is computed to not skip past it.
                return;
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
                super::framing::Frame::Text(plaintext) => {
                    if let Ok(text) = String::from_utf8(plaintext) {
                        let _ = conn.execute(
                            "INSERT OR IGNORE INTO message
                             (id, conversation_id, sender_id, ciphertext, content, reply_to_id, sent_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            rusqlite::params![id, conversation_id, cred_sender, bytes, text, reply_to_id, sent_at],
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
                let Some((plain, cred_sender)) = ciphertext
                    .strip_prefix("mls:")
                    .and_then(|h| hex::decode(h).ok())
                    .and_then(|b| crate::commands::mls::try_mls_decrypt(conn, mls_group_id, &b))
                else {
                    return;
                };
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
                    return;
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
    for peer_id in &peer_ids {
        if let Err(e) = crate::commands::safety::check_and_pin_account_key(state, peer_id).await {
            eprintln!("[ingest] check_and_pin_account_key for {peer_id}: {e}");
        }
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
