use rusqlite::OptionalExtension;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

/// One row from `message_envelope`:
/// `(id, sender_id, ciphertext, reply_to_id, target_message_id, sent_at, type, seq)`.
///
/// `seq` is the DS delivery position (#1087), the cursor; `sent_at` only fills the legacy `last_fetched_at`.
type EnvelopeRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
    i64,
);

/// One catch-up's envelopes bucketed per bound conversation, in listing order.
type PerConversation = Vec<(String, Vec<EnvelopeRow>)>;

/// The DS's group-wide arrival position of each envelope in a
/// [`PerConversation`], same shape: `arrival[ci][ei]` is `per_conv[ci].1[ei]`'s.
type Arrival = Vec<Vec<Option<i64>>>;

/// Whether a tombstone's redaction is done here or must be retried (#693 / #661 WS1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DeleteResolution {
    /// Done: applied now, already applied, or the target will never arrive (pre-join).
    Handled,
    /// Retry later: a transient DB error, or the target is not ingested yet.
    Pending,
}

/// One conversation's pass: its watermark and newly decrypted inbound messages (#857).
struct ConvIngest {
    conversation_id: String,
    /// The cursor this device may advance to: `(seq, sent_at)` of the highest handled envelope (#1087).
    watermark: Option<(i64, String)>,
    /// Ids of messages persisted for the FIRST time on this pass, authored by
    /// someone else. Empty for group channels, which never emit receipts.
    newly_delivered: Vec<String>,
}

/// What one [`decrypt_and_persist_one`] attempt came to (#1312).
#[derive(Debug, PartialEq, Eq)]
enum Ingested {
    /// Opened — or already held, or nothing to open. Carries the message id
    /// when this persisted a NEW inbound DM message (the receipt batch, #857).
    Opened(Option<String>),
    /// The bytes would not open at the epoch they claim; carries why (#1520).
    Failed(crate::commands::mls::DecryptFailure),
}

/// The last failure reason logged per envelope id (#1520), so each is logged once per reason.
static LOGGED_FAILURES: std::sync::Mutex<
    Option<HashMap<String, crate::commands::mls::DecryptFailureKind>>,
> = std::sync::Mutex::new(None);

/// Past this many remembered envelopes the memory is cleared rather than
/// grown: the worst case is one repeated line per envelope, not a leak.
const LOGGED_FAILURES_CAP: usize = 4096;

/// Whether this failure of envelope `id` is new to this process — never seen,
/// or seen with a different reason. Records it either way.
fn first_sighting(id: &str, kind: crate::commands::mls::DecryptFailureKind) -> bool {
    let Ok(mut guard) = LOGGED_FAILURES.lock() else {
        return true;
    };
    let seen = guard.get_or_insert_with(HashMap::new);
    if seen.get(id) == Some(&kind) {
        return false;
    }
    if seen.len() >= LOGGED_FAILURES_CAP {
        seen.clear();
    }
    seen.insert(id.to_string(), kind);
    true
}

/// Log a decrypt failure as it happens (#1520), once per envelope and reason.
/// Ids, positions and the failing step only — never content or key material.
fn note_decrypt_failure(
    conversation_id: &str,
    env_type: &str,
    id: &str,
    why: &crate::commands::mls::DecryptFailure,
) -> Ingested {
    if first_sighting(id, why.kind) {
        crate::diag!("[decrypt] {conversation_id}: {env_type} {id} would not open — {why}");
    }
    Ingested::Failed(*why)
}

/// Decide a delete tombstone's fate from its redaction outcome (pure; see `delete_resolution_tests`):
///   * `applied_rows` — `None` if the redaction `UPDATE` errored (transient →
///     retry), else `Some(n)` rows redacted now.
///   * `target_present` — a `message` row with this id exists locally (redacted
///     or not). Only consulted on the zero-row path.
///   * `target_awaiting_ingest` — the target's envelope is still in this
///     conversation's fetch set at an epoch the replay has not reached, i.e.
///     absent *because we have not caught up yet* (retry) rather than absent
///     *because it is a pre-join message we will never receive* (terminal).
///
/// The only silent advance is over a target this device provably never receives (an accepted loss).
fn resolve_delete(
    applied_rows: Option<usize>,
    target_present: bool,
    target_awaiting_ingest: bool,
) -> DeleteResolution {
    match applied_rows {
        // The UPDATE errored: treat as transient and retry rather than drop the redaction.
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
                // Absent for good (pre-join): vacuously satisfied.
                DeleteResolution::Handled
            }
        }
    }
}

/// Pull new envelopes for a channel from remote, decrypt, and persist into the
/// local `message` table (the authoritative history for this device).
///
/// Catches up the whole MLS group, since a group's channels share it (see [`catch_up_mls_group_interleaved`]).
pub async fn ingest_channel_envelopes_inner(
    state: &Arc<AppState>,
    user_id: &str,
    channel_id: &str,
) -> Result<()> {
    // One round trip resolves the group and membership; unauthorized falls back to local.
    let resolved =
        crate::commands::ds_reads::catch_up(state, channel_id, false, false).await?;
    if !resolved.authorized {
        return Ok(());
    }
    let mls_group_id = resolved.mls_group_id;

    let device_id = state.device_id.lock().await.clone();

    // Drain welcomes first so a just-joined member has a local group.
    if let Some(ref did) = device_id {
        if let Err(e) = crate::commands::mls::poll_mls_welcomes_inner(state, user_id, did).await {
            crate::diag!("[ingest] poll_mls_welcomes for {mls_group_id}: {e}");
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
/// One MLS group backs all of a group's channels, so the replay runs once and decrypts each epoch across all of them:
/// ```text
///   decrypt (any conversation) envelopes whose MLS epoch == initial_epoch
///   for each pending commit in epoch order:
///       apply the commit            (shared group advances to the next epoch)
///       decrypt (any conversation) envelopes whose MLS epoch == the new epoch
/// ```
/// Each envelope's epoch is parsed, not inferred; the replay runs even with zero envelopes (#371).
pub async fn catch_up_mls_group_interleaved(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
) -> Result<()> {
    let held = |state: &Arc<AppState>| {
        let state = state.clone();
        let id = mls_group_id.to_string();
        async move {
            let guard = state.local_db.lock().await;
            guard
                .as_ref()
                .is_some_and(|db| crate::commands::mls::has_local_group(db.conn(), &id))
        }
    };
    let held_before = held(state).await;
    // Boxed: the largest future on the receive path (#1571 overflowed a test stack).
    Box::pin(catch_up_once(state, mls_group_id, user_id)).await?;
    // A pass that started with no group could only join; run one more pass now (#1571) to show what it holds.
    if !held_before && held(state).await {
        Box::pin(catch_up_once(state, mls_group_id, user_id)).await?;
        // A just-joined device runs the re-add backstop once (#1571). Boxed: reconcile can re-enter this catch-up.
        if let Err(e) = Box::pin(crate::commands::mls::reconcile_backstop_one(
            state,
            mls_group_id,
            user_id,
        ))
        .await
        {
            crate::diag!("[mls-backstop] after joining {mls_group_id}: {e}");
        }
    }
    Ok(())
}

async fn catch_up_once(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
) -> Result<()> {
    // One request lists the bound conversations and their envelopes past each watermark (#875, #987).
    let resolved =
        crate::commands::ds_reads::catch_up(state, mls_group_id, true, false).await?;
    if !resolved.authorized {
        // Not a member (any more): nothing to do.
        return Ok(());
    }
    let is_dm = resolved.kind == pollis_api::directory::ConversationKind::Dm;
    // The log head the DS read BEFORE it scanned the envelopes (#1041): the
    // replay below must not apply a commit at or beyond it. See `ReplayBound`.
    let bound = resolved
        .head
        .map(|h| crate::commands::mls::ReplayBound {
            generation: h.generation,
            epoch: h.epoch.max(0) as u64,
        });

    let device_id = state.device_id.lock().await.clone();

    let (per_conv, arrival) = partition_envelopes(resolved.conversation_ids, resolved.envelopes);

    // Drive the replay once; returns each conversation's watermark and newly decrypted messages.
    let results: Vec<ConvIngest> = ingest_group_envelopes_interleaved(
        state,
        user_id,
        mls_group_id,
        &per_conv,
        &arrival,
        is_dm,
        bound,
    )
    .await?;

    // Advance each watermark via the DS (best-effort). Envelope GC runs server-side (#689).
    for res in &results {
        let cid = &res.conversation_id;
        if let (Some((seq, sent_at)), Some(did)) = (res.watermark.as_ref(), device_id.as_ref()) {
            let body = pollis_api::messages::WatermarkBody {
                conversation_id: cid.clone(),
                user_id: Some(user_id.to_string()),
                device_id: did.clone(),
                // The cursor (#1087). `last_fetched_at` rides along for the
                // legacy column only; delivery and retention read `last_seq`.
                last_seq: Some(*seq),
                last_fetched_at: sent_at.clone(),
            };
            if let Err(e) =
                crate::commands::mls::ds_post_ok(state, &body).await
            {
                crate::diag!("[watermark] catch_up_group: DS advance failed for {cid}: {e}");
            }
        }
    }

    // Delivery receipts (#857), batched per conversation; `emit_receipt` re-checks the gates. Best-effort.
    if is_dm {
        emit_delivered_batches(
            state,
            results
                .iter()
                .map(|r| (r.conversation_id.as_str(), r.newly_delivered.as_slice())),
        )
        .await;
    }

    Ok(())
}

/// Bucket one catch-up's envelopes per bound conversation, in the order the
/// conversations were listed.
///
/// The DS's `ORDER BY conversation_id, seq` keeps each conversation's order.
///
/// Also returns each envelope's group-wide arrival position, for [`into_sealing_order`].
pub(super) fn partition_envelopes(
    conversation_ids: Vec<String>,
    envelopes: Vec<pollis_api::directory::EnvelopeWire>,
) -> (PerConversation, Arrival) {
    let mut per_conv: Vec<(String, Vec<EnvelopeRow>)> = conversation_ids
        .iter()
        .map(|cid| (cid.clone(), Vec::new()))
        .collect();
    let mut arrival: Vec<Vec<Option<i64>>> = vec![Vec::new(); conversation_ids.len()];
    // Index by conversation id so a row lands in the right bucket regardless
    // of the order the ids were listed in.
    let slot: HashMap<&str, usize> = conversation_ids
        .iter()
        .enumerate()
        .map(|(i, cid)| (cid.as_str(), i))
        .collect();
    for env in envelopes {
        let Some(&i) = slot.get(env.conversation_id.as_str()) else {
            continue;
        };
        arrival[i].push(env.arrival);
        per_conv[i].1.push((
            env.id,
            env.sender_id,
            env.ciphertext,
            env.reply_to_id,
            env.target_message_id,
            env.sent_at,
            env.kind,
            // 0 is the fail-safe: the cursor then never advances past the row.
            env.seq.unwrap_or(0),
        ));
    }
    (per_conv, arrival)
}

/// Put one epoch's run of envelopes — drawn from EVERY conversation of the
/// group — into the order the DS accepted them, which is the order their
/// senders sealed them in.
///
/// One sender's generations span all channels and openmls keeps only 5 skipped, so decrypt in arrival order.
///
/// Stable sort on the DS position; an older DS sends none and the run is unchanged.
pub(super) fn into_sealing_order(run: &mut [(usize, usize)], arrival: &[Vec<Option<i64>>]) {
    run.sort_by_key(|&(ci, ei)| arrival.get(ci).and_then(|a| a.get(ei)).copied().flatten());
}

/// Emit one delivery receipt per conversation for newly persisted messages (#857). Best-effort.
async fn emit_delivered_batches<'a>(
    state: &Arc<AppState>,
    batches: impl Iterator<Item = (&'a str, &'a [String])>,
) {
    for (conversation_id, ids) in batches {
        if ids.is_empty() {
            continue;
        }
        if let Err(e) = super::receipts::emit_delivered(state, conversation_id, ids).await {
            crate::diag!("[receipts] delivered emit failed for {conversation_id}: {e}");
        }
    }
}

/// What the committer's sweep ([`sweep_current_epoch`]) persisted, handed back
/// so the receipts it owes are sent only once the won commit is merged.
pub(crate) struct SweptEnvelopes {
    is_dm: bool,
    newly_delivered: Vec<(String, Vec<String>)>,
}

impl SweptEnvelopes {
    /// Send the delivery receipts for what the sweep persisted. Deferred to
    /// after the merge because a receipt is itself an MLS send, and encrypting
    /// merges any staged commit first — before the committer has finalized it.
    pub(crate) async fn emit_receipts(self, state: &Arc<AppState>) {
        if !self.is_dm {
            return;
        }
        emit_delivered_batches(
            state,
            self.newly_delivered
                .iter()
                .map(|(cid, ids)| (cid.as_str(), ids.as_slice())),
        )
        .await;
    }
}

/// The committer's sweep (#1041): after a staged commit has WON its epoch on
/// the log and BEFORE it is merged locally, decrypt every envelope sealed at the
/// epoch the commit is about to leave.
///
/// A merge discards the leaving epoch's keys, so envelopes posted after this device's catch-up need this second read.
///
/// Must run under the MLS lock with the won commit still staged. Advances no watermark.
pub(crate) async fn sweep_current_epoch(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
) -> Result<SweptEnvelopes> {
    let resolved =
        crate::commands::ds_reads::catch_up(state, mls_group_id, true, false).await?;
    let is_dm = resolved.kind == pollis_api::directory::ConversationKind::Dm;
    let mut swept = SweptEnvelopes {
        is_dm,
        newly_delivered: Vec::new(),
    };
    if !resolved.authorized {
        return Ok(swept);
    }
    let (per_conv, arrival) = partition_envelopes(resolved.conversation_ids, resolved.envelopes);

    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
    let conn = db.conn();
    let Some(mut decryptor) = crate::commands::mls::MlsDecryptor::open(conn, mls_group_id) else {
        return Ok(swept);
    };
    let at = decryptor.lineage();
    // Every conversation's envelopes sealed at `at`, decrypted as ONE run in the
    // order they were sealed — the catch-up hook's rule (`into_sealing_order`).
    let mut run: Vec<(usize, usize)> = Vec::new();
    let mut decoded: HashMap<(usize, usize), Vec<u8>> = HashMap::new();
    for (ci, (_cid, envs)) in per_conv.iter().enumerate() {
        for (ei, env) in envs.iter().enumerate() {
            let (_, _, ciphertext, _, _, _, env_type, _) = env;
            if env_type != "message" && env_type != "edit" {
                continue;
            }
            let Some(bytes) = ciphertext
                .strip_prefix("mls:")
                .and_then(|h| hex::decode(h).ok())
            else {
                continue;
            };
            if crate::commands::mls::envelope_lineage(&bytes) != Some(at) {
                continue;
            }
            run.push((ci, ei));
            decoded.insert((ci, ei), bytes);
        }
    }
    into_sealing_order(&mut run, &arrival);
    let mut delivered_per_conv: Vec<Vec<String>> = vec![Vec::new(); per_conv.len()];
    for (ci, ei) in run {
        let Some(bytes) = decoded.get(&(ci, ei)) else {
            continue;
        };
        let (cid, envs) = &per_conv[ci];
        if let Ingested::Opened(Some(id)) =
            decrypt_and_persist_one(conn, &mut decryptor, cid, &envs[ei], bytes, user_id, is_dm)
        {
            delivered_per_conv[ci].push(id);
        }
    }
    for ((cid, _), delivered) in per_conv.iter().zip(delivered_per_conv) {
        if !delivered.is_empty() {
            crate::diag!(
                "[mls] pre-merge sweep of {cid} decrypted {} envelope(s) sealed at ({}, {}) — \
                 the epoch this commit closes (#1041)",
                delivered.len(),
                at.0,
                at.1
            );
            swept.newly_delivered.push((cid.clone(), delivered));
        }
    }
    Ok(swept)
}

/// Core of [`catch_up_mls_group_interleaved`]: drive the shared MLS group's
/// commit replay ONCE, decrypting the envelopes (from ANY bound conversation)
/// sealed at each epoch while the group is still AT that epoch, and compute the
/// watermark each conversation may independently advance to.
///
/// Each watermark stops strictly before the first envelope needing a retry (epoch beyond `max_fired_epoch`).
async fn ingest_group_envelopes_interleaved(
    state: &Arc<AppState>,
    user_id: &str,
    mls_group_id: &str,
    per_conv: &[(String, Vec<EnvelopeRow>)],
    // `arrival[ci][ei]`: the DS's group-wide position of `per_conv[ci].1[ei]`
    // (see `partition_envelopes`), which orders each epoch's decrypt run.
    arrival: &[Vec<Option<i64>>],
    // Whether this MLS group backs a DM (receipts are DMs-only, #857).
    is_dm: bool,
    // The log head read before the envelope fetch (#1041); the replay stops
    // short of it. `None` replays to head.
    bound: Option<crate::commands::mls::ReplayBound>,
) -> Result<Vec<ConvIngest>> {
    // Index message/edit envelopes by `(generation, epoch)` (not epoch alone, #454 P4) for the per-epoch hook.
    let mut by_epoch: HashMap<(i64, u64), Vec<(usize, usize)>> = HashMap::new();
    // epoch_of[ci][ei] mirrors per_conv[ci].1[ei]'s parsed position.
    let mut epoch_of: Vec<Vec<Option<(i64, u64)>>> = Vec::with_capacity(per_conv.len());
    // Hex-decoded once here and reused in the hook (#875).
    let mut decoded: HashMap<(usize, usize), Vec<u8>> = HashMap::new();
    for (ci, (_cid, envs)) in per_conv.iter().enumerate() {
        let mut this: Vec<Option<(i64, u64)>> = Vec::with_capacity(envs.len());
        for (ei, env) in envs.iter().enumerate() {
            let (_, _, ciphertext, _, _, _, env_type, _) = env;
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
    // Each epoch's run is decrypted in the order its envelopes were sealed, not
    // conversation by conversation — the loss `into_sealing_order` describes.
    for run in by_epoch.values_mut() {
        into_sealing_order(run, arrival);
    }

    // Drive the replay; `max_fired_epoch` is the highest epoch reached. Runs even with zero envelopes.
    let mut max_fired_epoch: Option<(i64, u64)> = None;
    // Where the replay started, to tell never-attempted envelopes from failed ones (#1312).
    let mut first_fired_epoch: Option<(i64, u64)> = None;
    let mut fired: HashSet<(i64, u64)> = HashSet::new();
    let mut failed: HashMap<(usize, usize), crate::commands::mls::DecryptFailure> =
        HashMap::new();
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
            first_fired_epoch.get_or_insert(key);
            fired.insert(key);
            if let Some(indices) = by_epoch.get(&key) {
                // One group load per epoch run (#875), valid because every envelope here shares the epoch's key schedule.
                let Some(mut decryptor) =
                    crate::commands::mls::MlsDecryptor::open(conn, mls_group_id)
                else {
                    // No local group: the epoch counts as reached and these record as failed (#1312).
                    let why = crate::commands::mls::DecryptFailure::no_local_group();
                    failed.extend(indices.iter().map(|&at| (at, why)));
                    return;
                };
                for &(ci, ei) in indices {
                    // Present for every index in `by_epoch` by construction.
                    let Some(bytes) = decoded.get(&(ci, ei)) else {
                        continue;
                    };
                    match decrypt_and_persist_one(
                        conn,
                        &mut decryptor,
                        &per_conv[ci].0,
                        &per_conv[ci].1[ei],
                        bytes,
                        user_id,
                        is_dm,
                    ) {
                        Ingested::Opened(Some(delivered_id)) => {
                            newly_delivered[ci].push(delivered_id);
                        }
                        Ingested::Opened(None) => {}
                        Ingested::Failed(why) => {
                            failed.insert((ci, ei), why);
                        }
                    }
                }
            }
        };
        if let Err(e) = crate::commands::mls::process_pending_commits_inner_with_hook(
            state,
            mls_group_id,
            user_id,
            &mut on_epoch,
            bound,
        )
        .await
        {
            crate::diag!("[ingest] process_pending_commits for {mls_group_id}: {e}");
        }
    }

    // Apply tombstones after decryption; an unapplied-but-applicable one becomes `DeletePending` (#693, see `resolve_delete`).
    let mut pending_deletes: HashSet<(usize, usize)> = HashSet::new();
    {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        for (ci, (_cid, envs)) in per_conv.iter().enumerate() {
            for (ei, (_, _, _, _, target_id, _, env_type, _)) in envs.iter().enumerate() {
                if env_type != "delete" {
                    continue;
                }
                // No target: nothing to apply or retry, so handled.
                let Some(tid) = target_id.as_ref() else {
                    continue;
                };
                let now = chrono::Utc::now().to_rfc3339();
                // `None` = the redaction UPDATE errored (treated as transient →
                // retry). `Some(n)` = n rows redacted now.
                let applied_rows: Option<usize> = db
                    .conn()
                    .execute(super::outbox::SOFT_DELETE_SQL, rusqlite::params![now, tid])
                    .ok();
                // Only needed on the zero-row path: does a row with this id exist at all?
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
                // Is the target still awaiting ingest here (retry) rather than pre-join (terminal)?
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

    // Each watermark goes through the Kani-proved `next_watermark`; the key `(seq, sent_at)` orders by `seq` (#1087).
    let mut out: Vec<ConvIngest> = Vec::with_capacity(per_conv.len());
    for (ci, (cid, envs)) in per_conv.iter().enumerate() {
        let items: Vec<super::watermark::EnvView<(i64, String), (i64, u64)>> = envs
            .iter()
            .enumerate()
            .map(|(ei, env)| {
                // An unapplied tombstone is held back as `DeletePending` (#693).
                let kind = if env.6 == "delete" && pending_deletes.contains(&(ci, ei)) {
                    super::watermark::EnvKind::DeletePending
                } else {
                    super::watermark::EnvKind::from_type(env.6.as_str())
                };
                ((env.7, env.5.clone()), kind, epoch_of[ci][ei])
            })
            .collect();
        let watermark = super::watermark::next_watermark(&items, max_fired_epoch);
        out.push(ConvIngest {
            conversation_id: cid.clone(),
            watermark,
            newly_delivered: std::mem::take(&mut newly_delivered[ci]),
        });
    }

    // Make every loss the cursor is about to commit to visible (#1312).
    if let (Some(start), Some(reached)) = (first_fired_epoch, max_fired_epoch) {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        record_losses(
            db.conn(),
            mls_group_id,
            per_conv,
            &epoch_of,
            &out,
            Replay {
                start,
                reached,
                fired: &fired,
                failed: &failed,
            },
        );
    }
    Ok(out)
}

/// What one pass's commit replay did, as [`record_losses`] needs it.
struct Replay<'a> {
    /// The position the local group was at when the replay began.
    start: (i64, u64),
    /// The highest position it reached.
    reached: (i64, u64),
    /// Every position it reached.
    fired: &'a HashSet<(i64, u64)>,
    /// `(conversation, envelope)` indices attempted and not opened, with why.
    failed: &'a HashMap<(usize, usize), crate::commands::mls::DecryptFailure>,
}

/// Write a placeholder (and a log line) for every message envelope this pass's
/// cursor moves past that this device was entitled to read and did not.
/// See [`super::loss`] for what "entitled" excludes.
fn record_losses(
    conn: &rusqlite::Connection,
    mls_group_id: &str,
    per_conv: &[(String, Vec<EnvelopeRow>)],
    epoch_of: &[Vec<Option<(i64, u64)>>],
    advanced: &[ConvIngest],
    replay: Replay<'_>,
) {
    let lineage = replay.reached.0;
    // A lineage held since before #1312 has no recorded join. Its floor is
    // where this first look at it started (see `loss::fallback_floor`).
    if crate::commands::mls::first_join(conn, mls_group_id, lineage).is_none() {
        let start_epoch = if replay.start.0 == lineage { replay.start.1 } else { 0 };
        crate::commands::mls::record_first_join(
            conn,
            mls_group_id,
            lineage,
            super::loss::fallback_floor(start_epoch),
        );
    }
    let mut pre_join = 0usize;
    let mut removed = 0usize;
    for (ci, (cid, envs)) in per_conv.iter().enumerate() {
        let Some((cursor, _)) = advanced.get(ci).and_then(|a| a.watermark.as_ref()) else {
            continue;
        };
        for (ei, env) in envs.iter().enumerate() {
            let (id, sender_id, _, _, _, sent_at, env_type, seq) = env;
            if seq > cursor || (env_type != "message" && env_type != "edit") {
                continue;
            }
            let Some(sealed_at) = epoch_of[ci][ei] else {
                crate::diag!("[loss] {cid}: envelope {id} is not a readable MLS message — skipped");
                continue;
            };
            let attempted = replay.fired.contains(&sealed_at);
            let why = replay.failed.get(&(ci, ei));
            // Opened (or a control frame, or already held).
            if attempted && why.is_none() {
                continue;
            }
            if super::loss::has_real_row(conn, id) || super::loss::is_known_envelope(conn, id) {
                continue;
            }
            // Sealed at an epoch this device had been removed from: never
            // addressed to it — the "while you were out" half of the accepted
            // pre-join loss, reached before any re-add moves the floor (#1524).
            if why.is_some_and(|w| w.kind == crate::commands::mls::DecryptFailureKind::Evicted) {
                removed += 1;
                continue;
            }
            let floor = crate::commands::mls::first_join(conn, mls_group_id, sealed_at.0);
            let reason = match super::loss::classify(sealed_at, lineage, floor, attempted) {
                Some(reason) => reason,
                // Below the floor: sent before this device held the group. That
                // is the accepted loss for a new device or for anything sent
                // before its user joined — but NOT for a device that was
                // registered all along and was left out of the tree (#1571).
                None => {
                    let entitled = crate::commands::mls::entitled_since(conn, mls_group_id, sealed_at.0);
                    if !super::loss::sealed_while_entitled(floor, entitled, sealed_at.1, sent_at) {
                        pre_join += 1;
                        continue;
                    }
                    crate::diag!(
                        "[loss] {cid}: {env_type} {id} sealed at ({}, {}) while this device was \
                         registered but outside the tree (joined at {floor:?}, entitled since \
                         {entitled:?}) — shown, not passed (#1571)",
                        sealed_at.0,
                        sealed_at.1,
                    );
                    if env_type == "message" {
                        super::loss::write_placeholder(
                            conn,
                            cid,
                            id,
                            sender_id,
                            sent_at,
                            super::loss::Reason::Stranded,
                        );
                    }
                    continue;
                }
            };
            // `failed: secret reuse (...)` — the openmls step that refused it (#1520).
            let label = match why {
                Some(why) => format!("{}: {why}", reason.as_str()),
                None => reason.as_str().to_string(),
            };
            crate::diag!(
                "[loss] {cid}: {env_type} {id} sealed at ({}, {}) lost on this device — {label} \
                 (device went from ({}, {}) to ({}, {}), joined at {floor:?})",
                sealed_at.0,
                sealed_at.1,
                replay.start.0,
                replay.start.1,
                replay.reached.0,
                replay.reached.1,
            );
            // A lost edit or delete has no row of its own to stand in for.
            if env_type == "message" {
                super::loss::write_placeholder(conn, cid, id, sender_id, sent_at, reason);
            }
        }
    }
    if pre_join > 0 {
        crate::diag!(
            "[loss] {mls_group_id}: passed {pre_join} envelope(s) sealed before this device joined"
        );
    }
    if removed > 0 {
        crate::diag!(
            "[ingest] {mls_group_id}: passed {removed} envelope(s) sealed after this device was removed"
        );
    }
}

/// Decrypt one `message` or `edit` envelope at the group's CURRENT epoch and
/// persist it. Invoked by [`ingest_group_envelopes_interleaved`]'s per-epoch
/// hook, so the caller has already positioned the local group at this envelope's
/// epoch.
/// Infallible: a failure leaves nothing persisted, for a later retry.
///
/// Returns the id of a new inbound DM message (what a receipt acknowledges, #857); `None` otherwise.
/// The ratchet step and the row insert share one savepoint (#1524), so a death can't spend a key without a row.
fn decrypt_and_persist_one(
    conn: &rusqlite::Connection,
    decryptor: &mut crate::commands::mls::MlsDecryptor<'_>,
    conversation_id: &str,
    env: &EnvelopeRow,
    bytes: &[u8],
    user_id: &str,
    is_dm: bool,
) -> Ingested {
    let one = OneEnvelope::begin(conn);
    let out = open_and_persist(conn, decryptor, conversation_id, env, bytes, user_id, is_dm);
    one.release();
    out
}

/// The savepoint [`decrypt_and_persist_one`] holds across one envelope. Rolled
/// back unless released — on an unwind, and, the case it exists for, never
/// released at all when the process dies, which SQLite rolls back on the next
/// open.
struct OneEnvelope<'c> {
    conn: Option<&'c rusqlite::Connection>,
}

impl<'c> OneEnvelope<'c> {
    const SAVEPOINT: &'static str = "SAVEPOINT ingest_envelope";

    fn begin(conn: &'c rusqlite::Connection) -> Self {
        match conn.execute_batch(Self::SAVEPOINT) {
            Ok(()) => Self { conn: Some(conn) },
            // Not fatal: the envelope is still opened, only without the
            // guarantee — what every pass did before #1524.
            Err(e) => {
                crate::diag!("[ingest] could not open the per-envelope savepoint: {e}");
                Self { conn: None }
            }
        }
    }

    fn release(mut self) {
        if let Some(conn) = self.conn.take() {
            if let Err(e) = conn.execute_batch("RELEASE ingest_envelope") {
                crate::diag!("[ingest] could not release the per-envelope savepoint: {e}");
            }
        }
    }
}

impl Drop for OneEnvelope<'_> {
    fn drop(&mut self) {
        if let Some(conn) = self.conn.take() {
            let _ = conn.execute_batch("ROLLBACK TO ingest_envelope; RELEASE ingest_envelope");
        }
    }
}

/// The body of [`decrypt_and_persist_one`], inside its savepoint.
fn open_and_persist(
    conn: &rusqlite::Connection,
    // The epoch's already-loaded group (#875).
    decryptor: &mut crate::commands::mls::MlsDecryptor<'_>,
    conversation_id: &str,
    env: &EnvelopeRow,
    // The ciphertext, hex-decoded once by the caller (#875).
    bytes: &[u8],
    user_id: &str,
    is_dm: bool,
) -> Ingested {
    // `sender_id` is not used for attribution; the MLS credential is (sealed sender).
    let (id, _sender_id, _ciphertext, reply_to_id, target_id, sent_at, env_type, _seq) = env;
    match env_type.as_str() {
        "message" => {
            // A placeholder (#1312) does not count: if this ever opens after
            // all, the real message replaces it.
            if super::loss::has_real_row(conn, id) {
                return Ingested::Opened(None);
            }
            // Attribute from the MLS credential, never the server-writable `sender_id`.
            let (plain, cred_sender) = match decryptor.decrypt(bytes) {
                Ok(opened) => opened,
                // Whether this is a loss is decided once the cursor's position
                // is known (`record_losses`), not here; why it failed is logged
                // now (#1520).
                Err(why) => return note_decrypt_failure(conversation_id, env_type, id, &why),
            };
            // Test-harness only: the process dies with the key spent and the
            // row not yet written (#1524).
            if super::send::test_hooks::take(super::send::test_hooks::Crash::AfterDecrypt) {
                super::send::test_hooks::die();
            }
            // Whatever it turns out to be, it opened. A control frame leaves no
            // row: remember it opened, and drop any placeholder standing in
            // for it.
            if !matches!(super::framing::classify(&plain), super::framing::Frame::Text { .. }) {
                super::loss::record_opened_envelope(conn, id);
                let _ = conn.execute(
                    "DELETE FROM message WHERE id = ?1 AND undecryptable IS NOT NULL",
                    rusqlite::params![id],
                );
            }
            match super::framing::classify(&plain) {
                // Redaction: honored only when its MLS author matches the target's author; never stored as a row.
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
                            super::outbox::SOFT_DELETE_SQL,
                            rusqlite::params![now, target_message_id],
                        );
                    }
                }
                // Text: strip padding (no-op for legacy/attachments); `thread_id` (#825) comes only from the ciphertext.
                super::framing::Frame::Text { text, thread_id } => {
                    if let Ok(text) = String::from_utf8(text) {
                        // Replaces a placeholder for the same id (#1312), never
                        // a real row.
                        let inserted = conn.execute(
                            "INSERT INTO message
                             (id, conversation_id, sender_id, ciphertext, content, reply_to_id, thread_id, sent_at)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                             ON CONFLICT(id) DO UPDATE SET
                               sender_id = excluded.sender_id, ciphertext = excluded.ciphertext,
                               content = excluded.content, reply_to_id = excluded.reply_to_id,
                               thread_id = excluded.thread_id, sent_at = excluded.sent_at,
                               undecryptable = NULL
                             WHERE message.undecryptable IS NOT NULL",
                            rusqlite::params![id, conversation_id, cred_sender, bytes, text, reply_to_id, thread_id, sent_at],
                        );
                        // The key is spent either way; a row that won't store is a loss to record (#1524).
                        let inserted = match inserted {
                            Ok(n) => n,
                            Err(e) => {
                                crate::diag!("[ingest] {conversation_id}: message {id} opened but its row would not store: {e}");
                                return Ingested::Failed(crate::commands::mls::DecryptFailure {
                                    kind: crate::commands::mls::DecryptFailureKind::Storage,
                                    sealed_at: None,
                                    group_at: Some(decryptor.lineage()),
                                });
                            }
                        };
                        // Delivered (#857): fetched, decrypted and stored, first time, someone else's, DM only.
                        if inserted > 0 && is_dm && cred_sender != user_id {
                            return Ingested::Opened(Some(id.clone()));
                        }
                    }
                }
                // A receipt (#857) is a control message: never a row, never acknowledged, so receipts can't ping-pong.
                super::framing::Frame::Receipt { kind, at, message_ids } => {
                    // Our own receipt, echoed back to us from the conversation
                    // we posted it to. Nothing to learn from it.
                    if cred_sender == user_id {
                        return Ingested::Opened(None);
                    }
                    // `receipts_permitted` drops group-channel receipts; `cred_sender` means a member records only in their own name.
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
                // Edit: honored only when its MLS author matches the target's author (#607), as with redactions.
                let (plain, cred_sender) = match decryptor.decrypt(bytes) {
                    Ok(opened) => opened,
                    Err(why) => {
                        return note_decrypt_failure(conversation_id, env_type, id, &why);
                    }
                };
                // An edit leaves no row under its own id.
                super::loss::record_opened_envelope(conn, id);
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
                    return Ingested::Opened(None);
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
    Ingested::Opened(None)
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
    // Membership and peers in one request; the catch-up fetches the envelopes.
    let resolved =
        crate::commands::ds_reads::catch_up(state, dm_channel_id, false, true).await?;
    if !resolved.authorized {
        return Ok(());
    }

    let device_id = state.device_id.lock().await.clone();

    if let Some(ref did) = device_id {
        if let Err(e) = crate::commands::mls::poll_mls_welcomes_inner(state, user_id, did).await {
            crate::diag!("[ingest] poll_mls_welcomes for DM {dm_channel_id}: {e}");
        }
    }

    // Re-verify each peer's `account_id_pub` against the TOFU pin; a change emits `KeyChanged`.
    let peer_ids: Vec<String> = resolved
        .dm
        .map(|dm| {
            dm.members
                .into_iter()
                .map(|m| m.user_id)
                .filter(|id| id != user_id)
                .collect()
        })
        .unwrap_or_default();
    // Batched (#875): one query for the whole peer set.
    if let Err(e) = crate::commands::safety::batch_check_and_pin_account_keys(state, &peer_ids).await
    {
        crate::diag!("[ingest] batch_check_and_pin_account_keys for {dm_channel_id}: {e}");
    }

    // A DM's MLS group backs exactly one conversation.
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
mod sealing_order_tests {
    //! An epoch's decrypt run follows the DS's group-wide arrival order, keeps
    //! each conversation's own order, and is untouched by an older DS.
    use super::into_sealing_order;

    #[test]
    fn a_run_interleaves_conversations_in_arrival_order() {
        // Conversation 0 holds arrivals 1 and 8; conversation 1 holds 2..=7.
        let arrival = vec![vec![Some(1), Some(8)], (2..=7).map(Some).collect()];
        let mut run: Vec<(usize, usize)> =
            vec![(0, 0), (0, 1), (1, 0), (1, 1), (1, 2), (1, 3), (1, 4), (1, 5)];
        into_sealing_order(&mut run, &arrival);
        assert_eq!(
            run,
            vec![(0, 0), (1, 0), (1, 1), (1, 2), (1, 3), (1, 4), (1, 5), (0, 1)]
        );
    }

    #[test]
    fn an_older_ds_without_positions_keeps_the_run_as_it_was() {
        let arrival = vec![vec![None, None], vec![None]];
        let mut run = vec![(0, 0), (0, 1), (1, 0)];
        into_sealing_order(&mut run, &arrival);
        assert_eq!(run, vec![(0, 0), (0, 1), (1, 0)]);
    }
}

#[cfg(test)]
mod failure_log_tests {
    //! #1520: a decrypt failure is logged when it first happens, and again only
    //! when its reason changes — not on every retry pass of the same envelope.
    use super::first_sighting;
    use crate::commands::mls::DecryptFailureKind as K;

    #[test]
    fn a_failure_is_logged_once_per_envelope_and_reason() {
        let id = "01JT1520DEDUPEENVELOPE0000";
        assert!(first_sighting(id, K::AeadFailure), "first failure is logged");
        assert!(!first_sighting(id, K::AeadFailure), "the same reason on a retry is not");
        assert!(first_sighting(id, K::SecretReuse), "a changed reason is");
        assert!(!first_sighting(id, K::SecretReuse));
        assert!(first_sighting("01JT1520OTHERENVELOPE00000", K::SecretReuse), "per envelope");
    }
}

#[cfg(test)]
mod delete_resolution_tests {
    //! #693 / #661 (WS1): the full redaction-outcome → tombstone-fate matrix.

    use super::{resolve_delete, DeleteResolution};

    /// The redaction matched a live row this pass — done.
    #[test]
    fn applied_now_is_handled() {
        assert_eq!(
            resolve_delete(Some(1), true, false),
            DeleteResolution::Handled
        );
    }

    /// Zero rows but the row exists (already redacted): handled.
    #[test]
    fn already_redacted_is_handled() {
        assert_eq!(
            resolve_delete(Some(0), true, false),
            DeleteResolution::Handled
        );
    }

    /// Zero rows, no local row, target awaiting ingest: retry.
    #[test]
    fn absent_but_awaiting_ingest_is_pending() {
        assert_eq!(
            resolve_delete(Some(0), false, true),
            DeleteResolution::Pending
        );
    }

    /// Zero rows, no local row, target not awaiting ingest (pre-join): handled.
    #[test]
    fn absent_pre_join_is_handled_terminally() {
        assert_eq!(
            resolve_delete(Some(0), false, false),
            DeleteResolution::Handled
        );
    }

    /// The UPDATE errored: retry, whatever the target state.
    #[test]
    fn transient_db_error_is_pending() {
        assert_eq!(resolve_delete(None, false, false), DeleteResolution::Pending);
        assert_eq!(resolve_delete(None, true, false), DeleteResolution::Pending);
    }
}

