use rusqlite::OptionalExtension;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

/// One row from `message_envelope`:
/// `(id, sender_id, ciphertext, reply_to_id, target_message_id, sent_at, type, seq)`.
///
/// `seq` is the cursor (#1087); `sent_at` only fills the legacy `last_fetched_at`.
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

/// Whether a delete tombstone is done on this device or must hold the cursor
/// below it for a later pass (#693).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DeleteResolution {
    /// Applied now, already applied, or the target will never arrive (pre-join).
    Handled,
    /// A transient DB error, or the target is not ingested yet.
    Pending,
}

/// One conversation's result from [`ingest_group_envelopes_interleaved`].
struct ConvIngest {
    conversation_id: String,
    /// `(seq, sent_at)` of the highest handled envelope; ordered by `seq` alone
    /// (#1087).
    watermark: Option<(i64, String)>,
    /// Others' messages persisted for the first time on this pass, for the DM
    /// delivery receipt (#857). Empty for group channels.
    newly_delivered: Vec<String>,
}

/// What one [`decrypt_and_persist_one`] attempt came to (#1312).
#[derive(Debug, PartialEq, Eq)]
enum Ingested {
    /// Opened, already held, or nothing to open; the id when a new inbound DM
    /// message was persisted (#857).
    Opened(Option<String>),
    /// Would not open at its claimed epoch; lost once the cursor passes it.
    Failed(crate::commands::mls::DecryptFailure),
}

/// Last failure reason logged per envelope id, so a retried failure is logged
/// again only when its reason changes (#1520).
static LOGGED_FAILURES: std::sync::Mutex<
    Option<HashMap<String, crate::commands::mls::DecryptFailureKind>>,
> = std::sync::Mutex::new(None);

/// Cleared rather than grown past this: the worst case is a repeated line.
const LOGGED_FAILURES_CAP: usize = 4096;

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

/// Log a decrypt failure once per envelope and reason (#1520). Ids, positions
/// and the failing step only — never content or key material.
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

/// Decide a delete tombstone's fate from its redaction outcome. `applied_rows`
/// is `None` when the `UPDATE` errored; `target_present` means a local row
/// exists (redacted or not); `target_awaiting_ingest` means its envelope is in
/// this fetch at an epoch the replay has not reached. The only silent advance
/// is over a target this device provably never receives (pre-join).
fn resolve_delete(
    applied_rows: Option<usize>,
    target_present: bool,
    target_awaiting_ingest: bool,
) -> DeleteResolution {
    match applied_rows {
        // Holding the cursor beats silently dropping the redaction.
        None => DeleteResolution::Pending,
        Some(n) if n >= 1 => DeleteResolution::Handled,
        Some(_) => {
            if target_present {
                DeleteResolution::Handled
            } else if target_awaiting_ingest {
                DeleteResolution::Pending
            } else {
                DeleteResolution::Handled
            }
        }
    }
}

/// Pull, decrypt and persist a channel's new envelopes. Catches up the whole MLS
/// group (every sibling channel) via [`catch_up_mls_group_interleaved`].
pub async fn ingest_channel_envelopes_inner(
    state: &Arc<AppState>,
    user_id: &str,
    channel_id: &str,
) -> Result<()> {
    // Not a member: the read path falls back to what is local.
    let resolved =
        crate::commands::ds_reads::catch_up(state, channel_id, false, false).await?;
    if !resolved.authorized {
        return Ok(());
    }
    let mls_group_id = resolved.mls_group_id;

    let device_id = state.device_id.lock().await.clone();

    // Welcomes first, so a just-joined member has a group before the replay.
    if let Some(ref did) = device_id {
        if let Err(e) = crate::commands::mls::poll_mls_welcomes_inner(state, user_id, did).await {
            crate::diag!("[ingest] poll_mls_welcomes for {mls_group_id}: {e}");
        }
    }

    catch_up_mls_group_interleaved(state, &mls_group_id, user_id).await
}

/// The catch-up entry point: advance the shared MLS group to head, decrypting
/// every bound conversation's envelopes at each epoch before the group leaves
/// it (`max_past_epochs = 0`). See `.codesight/wiki/mls.md` (group-level
/// interleaved catch-up).
///
/// ```text
///   decrypt (any conversation) envelopes whose MLS epoch == initial_epoch
///   for each pending commit in epoch order:
///       apply the commit            (shared group advances to the next epoch)
///       decrypt (any conversation) envelopes whose MLS epoch == the new epoch
/// ```
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
    // Boxed: inline, both awaits of this large future sat in every caller's
    // state machine and overflowed a 2 MiB stack (#1571).
    Box::pin(catch_up_once(state, mls_group_id, user_id)).await?;
    // A pass that started with no group only joined and decrypted nothing, so
    // run the decrypting pass now instead of at the next wake-up (#1571).
    if !held_before && held(state).await {
        Box::pin(catch_up_once(state, mls_group_id, user_id)).await?;
        // A just-joined device is the member most likely online: run the re-add
        // backstop now (#1571). Boxed: reconcile re-enters this catch-up (with a
        // group held, so not this branch).
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
    // One request: the group's conversations and each one's envelopes past its
    // own watermark for this device.
    let resolved =
        crate::commands::ds_reads::catch_up(state, mls_group_id, true, false).await?;
    if !resolved.authorized {
        return Ok(());
    }
    let is_dm = resolved.kind == pollis_api::directory::ConversationKind::Dm;
    // The log head read before the envelope scan; the replay stops short of it
    // (#1041, `ReplayBound`).
    let bound = resolved
        .head
        .map(|h| crate::commands::mls::ReplayBound {
            generation: h.generation,
            epoch: h.epoch.max(0) as u64,
        });

    let device_id = state.device_id.lock().await.clone();

    let (per_conv, arrival) = partition_envelopes(resolved.conversation_ids, resolved.envelopes);

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

    // Best effort. Envelope GC runs in the DS's own sweep (#689); this only
    // reports the watermark it reads.
    for res in &results {
        let cid = &res.conversation_id;
        if let (Some((seq, sent_at)), Some(did)) = (res.watermark.as_ref(), device_id.as_ref()) {
            let body = pollis_api::messages::WatermarkBody {
                conversation_id: cid.clone(),
                user_id: Some(user_id.to_string()),
                device_id: did.clone(),
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

    // Delivery receipts (#857): "delivered" means this device decrypted it.
    // `emit_receipt` re-checks the DM-only and opt-out gates; `is_dm` here only
    // skips a round trip.
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

/// Bucket one catch-up's envelopes per bound conversation (each in the DS's
/// `seq` order), with each envelope's group-wide arrival position alongside.
/// Bucket order is the cursor's order, not the decrypt order
/// ([`into_sealing_order`]).
pub(super) fn partition_envelopes(
    conversation_ids: Vec<String>,
    envelopes: Vec<pollis_api::directory::EnvelopeWire>,
) -> (PerConversation, Arrival) {
    let mut per_conv: Vec<(String, Vec<EnvelopeRow>)> = conversation_ids
        .iter()
        .map(|cid| (cid.clone(), Vec::new()))
        .collect();
    let mut arrival: Vec<Vec<Option<i64>>> = vec![Vec::new(); conversation_ids.len()];
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
            // 0 fails safe: the cursor never advances past it.
            env.seq.unwrap_or(0),
        ));
    }
    (per_conv, arrival)
}

/// Order one epoch's run (from every conversation of the group) as its senders
/// sealed it: a sender's ratchet generations span all channels and openmls's
/// `out_of_order_tolerance` is bounded (see `.codesight/wiki/mls.md`). A stable
/// sort, so each conversation's own order holds; an older DS sends no position
/// and the run is unchanged.
pub(super) fn into_sealing_order(run: &mut [(usize, usize)], arrival: &[Vec<Option<i64>>]) {
    run.sort_by_key(|&(ci, ei)| arrival.get(ci).and_then(|a| a.get(ei)).copied().flatten());
}

/// One delivery receipt per conversation (#857). Best-effort: never fails the
/// ingest that already landed the message.
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
    /// Deferred to after the merge: a receipt is an MLS send, and encrypting
    /// merges any staged commit before the committer has finalized it.
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

/// The committer's pre-merge sweep (#1041): once a staged commit has won its
/// epoch, decrypt every envelope sealed at the epoch it is about to leave.
///
/// Must run under the conversation's MLS lock with the commit still staged. No
/// watermark advances here; the next pass dedupes these. See
/// `.codesight/wiki/mls.md` (committer pre-merge sweep).
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
    // One run across conversations, in sealing order (`into_sealing_order`).
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

/// Core of [`catch_up_mls_group_interleaved`]: one commit replay for the shared
/// group, decrypting each epoch's envelopes while the group is at it, then each
/// conversation's watermark — the handled prefix, stopping before the first
/// envelope at an epoch the replay has not reached ([`super::watermark`]).
async fn ingest_group_envelopes_interleaved(
    state: &Arc<AppState>,
    user_id: &str,
    mls_group_id: &str,
    per_conv: &[(String, Vec<EnvelopeRow>)],
    arrival: &[Vec<Option<i64>>],
    // Receipts (#857) are DMs-only, on both the recording and emitting side.
    is_dm: bool,
    // `None` replays to head (#1041).
    bound: Option<crate::commands::mls::ReplayBound>,
) -> Result<Vec<ConvIngest>> {
    // Keyed on `(generation, epoch)`, not the epoch: a migrated conversation
    // restarts at epoch 0 in the next generation (#454).
    let mut by_epoch: HashMap<(i64, u64), Vec<(usize, usize)>> = HashMap::new();
    let mut epoch_of: Vec<Vec<Option<(i64, u64)>>> = Vec::with_capacity(per_conv.len());
    // Decoded once here and reused by the hook (#875).
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
                if let Some(b) = bytes {
                    decoded.insert((ci, ei), b);
                }
            }
        }
        epoch_of.push(this);
    }
    for run in by_epoch.values_mut() {
        into_sealing_order(run, arrival);
    }

    // Runs even with zero envelopes, so the group still reaches head (#371).
    let mut max_fired_epoch: Option<(i64, u64)> = None;
    // Tells an envelope never attempted (below the start) from one attempted
    // and failed (#1312).
    let mut first_fired_epoch: Option<(i64, u64)> = None;
    let mut fired: HashSet<(i64, u64)> = HashSet::new();
    let mut failed: HashMap<(usize, usize), crate::commands::mls::DecryptFailure> =
        HashMap::new();
    let mut newly_delivered: Vec<Vec<String>> = vec![Vec::new(); per_conv.len()];
    {
        let mut on_epoch = |conn: &rusqlite::Connection, generation: i64, epoch: u64| {
            // Lexicographic: a successor lineage restarts at epoch 0, so a scalar
            // max would run backwards across a migration.
            let key = (generation, epoch);
            max_fired_epoch = Some(max_fired_epoch.map_or(key, |m| m.max(key)));
            first_fired_epoch.get_or_insert(key);
            fired.insert(key);
            if let Some(indices) = by_epoch.get(&key) {
                // One group load per epoch run (#875): valid only because every
                // envelope here is at the epoch just reached, and dropped before
                // the replay applies the next commit.
                let Some(mut decryptor) =
                    crate::commands::mls::MlsDecryptor::open(conn, mls_group_id)
                else {
                    // The epoch counts as reached, so these are failures (#1312).
                    let why = crate::commands::mls::DecryptFailure::no_local_group();
                    failed.extend(indices.iter().map(|&at| (at, why)));
                    return;
                };
                for &(ci, ei) in indices {
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

    // Admin delete tombstones, applied after decryption so a target decrypted
    // this pass still lands. One that may still apply later holds the cursor
    // (`resolve_delete`, #693).
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
                // No target: nothing a retry could fix, so it is handled.
                let Some(tid) = target_id.as_ref() else {
                    continue;
                };
                let now = chrono::Utc::now().to_rfc3339();
                let applied_rows: Option<usize> = db
                    .conn()
                    .execute(super::outbox::SOFT_DELETE_SQL, rusqlite::params![now, tid])
                    .ok();
                // Only consulted on the zero-row path.
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
                // Same `is_handled` the watermark uses.
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

    // Through the Kani-proved `watermark::next_watermark`, never a copy: it is
    // the delivery safety property. Key order is `seq` alone (#1087).
    let mut out: Vec<ConvIngest> = Vec::with_capacity(per_conv.len());
    for (ci, (cid, envs)) in per_conv.iter().enumerate() {
        let items: Vec<super::watermark::EnvView<(i64, String), (i64, u64)>> = envs
            .iter()
            .enumerate()
            .map(|(ei, env)| {
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
    // No recorded join: the floor is where this first look started
    // (`loss::fallback_floor`).
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
            if attempted && why.is_none() {
                continue;
            }
            if super::loss::has_real_row(conn, id) || super::loss::is_known_envelope(conn, id) {
                continue;
            }
            // Sealed while this device was removed: never addressed to it (#1524).
            if why.is_some_and(|w| w.kind == crate::commands::mls::DecryptFailureKind::Evicted) {
                removed += 1;
                continue;
            }
            let floor = crate::commands::mls::first_join(conn, mls_group_id, sealed_at.0);
            let reason = match super::loss::classify(sealed_at, lineage, floor, attempted) {
                Some(reason) => reason,
                // Below the floor: an accepted loss, unless the device was
                // registered all along and left out of the tree (#1571).
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

/// Decrypt one `message` or `edit` envelope at the group's current epoch and
/// persist it. Only a new inbound DM message yields an id (what a receipt
/// acknowledges); control frames never do, so a receipt never triggers one.
///
/// The ratchet step and the row share one savepoint, so a process death between
/// them cannot spend the key without storing the message (#1524).
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

/// The per-envelope savepoint: rolled back unless released, including by SQLite
/// on the next open after a process death.
struct OneEnvelope<'c> {
    conn: Option<&'c rusqlite::Connection>,
}

impl<'c> OneEnvelope<'c> {
    const SAVEPOINT: &'static str = "SAVEPOINT ingest_envelope";

    fn begin(conn: &'c rusqlite::Connection) -> Self {
        match conn.execute_batch(Self::SAVEPOINT) {
            Ok(()) => Self { conn: Some(conn) },
            // Not fatal: the envelope still opens, without the guarantee.
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
    decryptor: &mut crate::commands::mls::MlsDecryptor<'_>,
    conversation_id: &str,
    env: &EnvelopeRow,
    bytes: &[u8],
    user_id: &str,
    is_dm: bool,
) -> Ingested {
    // Attribution comes from the MLS credential, never the server-writable
    // `sender_id` (sealed sender, `docs/metadata-minimization-design.md` §2).
    let (id, _sender_id, _ciphertext, reply_to_id, target_id, sent_at, env_type, _seq) = env;
    match env_type.as_str() {
        "message" => {
            // A placeholder (#1312) does not count; the real message replaces it.
            if super::loss::has_real_row(conn, id) {
                return Ingested::Opened(None);
            }
            let (plain, cred_sender) = match decryptor.decrypt(bytes) {
                Ok(opened) => opened,
                // Whether it is a loss is decided in `record_losses`.
                Err(why) => return note_decrypt_failure(conversation_id, env_type, id, &why),
            };
            // Test harness: die with the key spent and no row (#1524).
            if super::send::test_hooks::take(super::send::test_hooks::Crash::AfterDecrypt) {
                super::send::test_hooks::die();
            }
            // A control frame leaves no row: record that it opened and drop any
            // placeholder for it.
            if !matches!(super::framing::classify(&plain), super::framing::Frame::Text { .. }) {
                super::loss::record_opened_envelope(conn, id);
                let _ = conn.execute(
                    "DELETE FROM message WHERE id = ?1 AND undecryptable IS NOT NULL",
                    rusqlite::params![id],
                );
            }
            match super::framing::classify(&plain) {
                // Honored only when the redaction's MLS-authenticated author is
                // the target's stored author, so neither the server nor another
                // member can redact someone else's message.
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
                super::framing::Frame::Text { text, thread_id } => {
                    if let Ok(text) = String::from_utf8(text) {
                        // Replaces a placeholder (#1312), never a real row.
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
                        // The savepoint is still released (a rollback would only
                        // repeat the write failure); this is a loss (#1524).
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
                        // Delivered (#857): stored for the first time, in a DM,
                        // from someone else.
                        if inserted > 0 && is_dm && cred_sender != user_id {
                            return Ingested::Opened(Some(id.clone()));
                        }
                    }
                }
                // A receipt (#857) writes no `message` row, so it is never
                // itself acknowledged and two clients cannot ping-pong.
                super::framing::Frame::Receipt { kind, at, message_ids } => {
                    if cred_sender == user_id {
                        return Ingested::Opened(None);
                    }
                    // Recorded under the MLS-authenticated author, so no one can
                    // forge another member's receipt. The DM-only gate lives in
                    // `receipts_permitted`.
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
                // Honored only when its MLS-authenticated author is the target's
                // stored author: the DS accepts an edit from any member, so
                // authorship is enforced here (#607).
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
    Ingested::Opened(None)
}

/// Ingest a channel without reading a page (realtime hints, focus pre-warm).
pub async fn ingest_channel_envelopes(
    user_id: String,
    channel_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    ingest_channel_envelopes_inner(state, &user_id, &channel_id).await
}

/// Pull, decrypt and persist a DM's new envelopes; its MLS group id is the
/// conversation id.
pub async fn ingest_dm_envelopes_inner(
    state: &Arc<AppState>,
    user_id: &str,
    dm_channel_id: &str,
) -> Result<()> {
    // Membership and peers only; the catch-up below fetches the envelopes.
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

    // Re-check each peer's key against the TOFU pin on every ingest, so a
    // changed key shows its banner as soon as it is observed.
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
    if let Err(e) = crate::commands::safety::batch_check_and_pin_account_keys(state, &peer_ids).await
    {
        crate::diag!("[ingest] batch_check_and_pin_account_keys for {dm_channel_id}: {e}");
    }

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
    //! The watermark half is `watermark::delete_consumption_tests`.

    use super::{resolve_delete, DeleteResolution};

    #[test]
    fn applied_now_is_handled() {
        assert_eq!(
            resolve_delete(Some(1), true, false),
            DeleteResolution::Handled
        );
    }

    #[test]
    fn already_redacted_is_handled() {
        assert_eq!(
            resolve_delete(Some(0), true, false),
            DeleteResolution::Handled
        );
    }

    #[test]
    fn absent_but_awaiting_ingest_is_pending() {
        assert_eq!(
            resolve_delete(Some(0), false, true),
            DeleteResolution::Pending
        );
    }

    #[test]
    fn absent_pre_join_is_handled_terminally() {
        assert_eq!(
            resolve_delete(Some(0), false, false),
            DeleteResolution::Handled
        );
    }

    #[test]
    fn transient_db_error_is_pending() {
        assert_eq!(resolve_delete(None, false, false), DeleteResolution::Pending);
        assert_eq!(resolve_delete(None, true, false), DeleteResolution::Pending);
    }
}

