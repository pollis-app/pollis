use rusqlite::OptionalExtension;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

/// One row from `message_envelope`:
/// `(id, sender_id, ciphertext, reply_to_id, target_message_id, sent_at, type, seq)`.
///
/// `seq` (#1087) is the cursor and fetch order; `sent_at` only fills the legacy
/// `last_fetched_at` column.
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

/// Whether a delete tombstone's redaction is done on this device, or must be
/// retried on a later pass (#693).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DeleteResolution {
    /// Applied now, already applied, or the target is a pre-join message this
    /// device never receives. The cursor may pass it.
    Handled,
    /// Still applicable later (transient DB error, or target not ingested yet).
    /// The cursor must stop below it.
    Pending,
}

/// One conversation's result from [`ingest_group_envelopes_interleaved`]: how
/// far its watermark may advance, and the inbound messages newly decrypted (the
/// receipt batch, #857).
struct ConvIngest {
    conversation_id: String,
    /// `(seq, sent_at)` of the highest handled envelope, ordered by `seq` alone
    /// (#1087); `sent_at` only fills the legacy `last_fetched_at` column.
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
    /// The bytes would not open at the epoch they claim. Carries why (#1520), for
    /// the `[loss]` line.
    Failed(crate::commands::mls::DecryptFailure),
}

/// The last failure reason logged per envelope id (#1520), so a failure is
/// logged once per reason rather than on every retry pass.
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

/// Decide a delete tombstone's fate from its redaction outcome (#693). Pure so
/// the edge cases are unit-testable without a DB.
///
/// - `applied_rows`: `None` if the redaction `UPDATE` errored (retry), else rows
///   redacted now.
/// - `target_present`: a local `message` row with this id exists, redacted or not.
/// - `target_awaiting_ingest`: the target is still in this fetch set at an epoch
///   the replay has not reached (retry), as opposed to a pre-join message
///   (terminal, an accepted loss).
fn resolve_delete(
    applied_rows: Option<usize>,
    target_present: bool,
    target_awaiting_ingest: bool,
) -> DeleteResolution {
    match applied_rows {
        // Treat a failed UPDATE as transient: holding the cursor beats silently
        // dropping the redaction.
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

/// Pull new envelopes for a channel, decrypt, and persist into the local
/// `message` table.
///
/// Catches up the whole MLS group (every sibling channel), so the shared group
/// never advances past an epoch a sibling channel still needs; see
/// `.codesight/wiki/mls.md` → "Group-level interleaved catch-up".
pub async fn ingest_channel_envelopes_inner(
    state: &Arc<AppState>,
    user_id: &str,
    channel_id: &str,
) -> Result<()> {
    // Resolves `mls_group_id` and membership in one round trip. Not a member:
    // the read path falls back to whatever is local.
    let resolved =
        crate::commands::ds_reads::catch_up(state, channel_id, false, false).await?;
    if !resolved.authorized {
        return Ok(());
    }
    let mls_group_id = resolved.mls_group_id;

    let device_id = state.device_id.lock().await.clone();

    // Drain welcomes first so a just-joined member has a local group before the
    // commit replay runs.
    if let Some(ref did) = device_id {
        if let Err(e) = crate::commands::mls::poll_mls_welcomes_inner(state, user_id, did).await {
            crate::diag!("[ingest] poll_mls_welcomes for {mls_group_id}: {e}");
        }
    }

    catch_up_mls_group_interleaved(state, &mls_group_id, user_id).await
}

/// Advance the shared MLS group for `mls_group_id` to head while decrypting
/// every bound conversation's envelopes at each epoch before the group moves
/// past it. The single catch-up entry point: channel ingest, DM ingest, the
/// cold-launch sweep and the realtime membership signal all route through it.
///
/// ```text
///   decrypt (any conversation) envelopes whose MLS epoch == initial_epoch
///   for each pending commit in epoch order:
///       apply the commit            (shared group advances to the next epoch)
///       decrypt (any conversation) envelopes whose MLS epoch == the new epoch
/// ```
///
/// Runs the replay even with zero envelopes, so the cold-launch sweep still
/// reaches head (#371). Why group-level: `.codesight/wiki/mls.md` →
/// "Group-level interleaved catch-up".
pub async fn catch_up_mls_group_interleaved(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
) -> Result<()> {
    // One request (#987): the conversations bound to this MLS group, and each
    // one's envelopes strictly past that conversation's own watermark.
    let resolved =
        crate::commands::ds_reads::catch_up(state, mls_group_id, true, false).await?;
    if !resolved.authorized {
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

    // Best effort. Envelope GC runs server-side (#689); this only reports the
    // watermark it reads.
    for res in &results {
        let cid = &res.conversation_id;
        if let (Some((seq, sent_at)), Some(did)) = (res.watermark.as_ref(), device_id.as_ref()) {
            let body = pollis_api::messages::WatermarkBody {
                conversation_id: cid.clone(),
                user_id: Some(user_id.to_string()),
                device_id: did.clone(),
                // The cursor (#1087); `last_fetched_at` fills the legacy column only.
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

    // Delivery receipts (#857): this device decrypted these messages, which is
    // what "delivered" means. Best effort; `emit_receipt` re-checks the DM-only
    // and opt-out gates.
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

/// Bucket one catch-up's envelopes per bound conversation, in listing order,
/// with each envelope's group-wide arrival position alongside
/// (`arrival[ci][ei]` belongs to `per_conv[ci].1[ei]`; see [`into_sealing_order`]).
fn partition_envelopes(
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
            // Fail-safe for a row without a seq: the cursor never advances past it.
            env.seq.unwrap_or(0),
        ));
    }
    (per_conv, arrival)
}

/// Order one epoch's run of envelopes, drawn from every conversation of the
/// group, by the DS's group-wide arrival position: the order their senders
/// sealed them in.
///
/// One sender's ratchet generations run across all of a group's channels, and
/// openmls keeps only 5 skipped generations (`out_of_order_tolerance`), so
/// decrypting conversation by conversation loses messages. Stable sort: each
/// conversation's own order is untouched, and an older DS (no position) leaves
/// the run as it was.
fn into_sealing_order(run: &mut [(usize, usize)], arrival: &[Vec<Option<i64>>]) {
    run.sort_by_key(|&(ci, ei)| arrival.get(ci).and_then(|a| a.get(ei)).copied().flatten());
}

/// Emit one delivery receipt per conversation for the messages a pass newly
/// persisted (#857). Best effort: the message itself already landed.
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

/// The committer's sweep (#1041): after a staged commit has won its epoch and
/// before it is merged, decrypt every envelope sealed at the epoch it leaves,
/// which the committer's earlier catch-up could not have fetched.
///
/// Must run under the conversation's MLS lock with the won commit still staged.
/// Advances no watermark; the next ingest pass finds these already persisted.
/// See `.codesight/wiki/mls.md` → "Committer pre-merge sweep".
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
    // One run across all conversations, in sealing order (`into_sealing_order`).
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

/// Core of [`catch_up_mls_group_interleaved`]: drive the shared group's commit
/// replay once, decrypting envelopes from any conversation while the group is
/// at their epoch, and compute each conversation's watermark.
///
/// A watermark covers the contiguous prefix of handled envelopes (decrypted,
/// permanently undeliverable, or an epoch-independent tombstone) and stops
/// strictly before the first one sealed beyond the highest epoch this pass
/// reached, so that one is re-fetched rather than skipped
/// (`super::watermark::next_watermark`).
async fn ingest_group_envelopes_interleaved(
    state: &Arc<AppState>,
    user_id: &str,
    mls_group_id: &str,
    per_conv: &[(String, Vec<EnvelopeRow>)],
    // `arrival[ci][ei]`: the DS's group-wide position of `per_conv[ci].1[ei]`.
    arrival: &[Vec<Option<i64>>],
    // Receipts (#857) are DM-only.
    is_dm: bool,
    // The log head read before the envelope fetch (#1041); the replay stops
    // short of it. `None` replays to head.
    bound: Option<crate::commands::mls::ReplayBound>,
) -> Result<Vec<ConvIngest>> {
    // Index message/edit envelopes from every conversation by MLS position, so
    // the per-epoch hook decrypts exactly those sealed where the replay is.
    // The position is `(generation, epoch)`: a migrated conversation restarts at
    // epoch 0 in the next generation (#454).
    let mut by_epoch: HashMap<(i64, u64), Vec<(usize, usize)>> = HashMap::new();
    // epoch_of[ci][ei] mirrors per_conv[ci].1[ei]'s parsed position.
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
    // Decrypt each epoch's run in sealing order (`into_sealing_order`).
    for run in by_epoch.values_mut() {
        into_sealing_order(run, arrival);
    }

    // `max_fired_epoch`: the highest position the replay reached, the
    // watermark's caught-up boundary. Scoped so the hook's borrows end before
    // the watermark loop.
    let mut max_fired_epoch: Option<(i64, u64)> = None;
    // The replay's start and every position it reached tell an envelope never
    // attempted (keys gone before this pass) from one attempted and failed (#1312).
    let mut first_fired_epoch: Option<(i64, u64)> = None;
    let mut fired: HashSet<(i64, u64)> = HashSet::new();
    let mut failed: HashMap<(usize, usize), crate::commands::mls::DecryptFailure> =
        HashMap::new();
    // Messages newly persisted per conversation: the receipt batch (#857).
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
                // One decryptor for the whole run (#875): valid only because
                // every envelope here is sealed at the epoch just reached. It
                // is dropped before the replay applies the next commit.
                let Some(mut decryptor) =
                    crate::commands::mls::MlsDecryptor::open(conn, mls_group_id)
                else {
                    // No local group: the epoch counts as reached, so these
                    // are recorded as failed and the cursor passes them (#1312).
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

    // Apply delete tombstones after decryption, so one whose target was just
    // decrypted still lands. One whose redaction can still take effect later is
    // held as `DeletePending` so the watermark stops below it (#693); see
    // `resolve_delete`.
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
                // Only needed on the zero-row path.
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
                // Still in this fetch set at an epoch the replay has not
                // reached? Same `is_handled` the watermark uses.
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

    // Through the Kani-proved `super::watermark::next_watermark`, not a copy.
    // The key is `(seq, sent_at)`, ordered by `seq` alone (#1087).
    let mut out: Vec<ConvIngest> = Vec::with_capacity(per_conv.len());
    for (ci, (cid, envs)) in per_conv.iter().enumerate() {
        let items: Vec<super::watermark::EnvView<(i64, String), (i64, u64)>> = envs
            .iter()
            .enumerate()
            .map(|(ei, env)| {
                // An unapplied tombstone holds the cursor below it (#693).
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
            let floor = crate::commands::mls::first_join(conn, mls_group_id, sealed_at.0);
            let Some(reason) = super::loss::classify(sealed_at, lineage, floor, attempted) else {
                pre_join += 1;
                continue;
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
}

/// Decrypt one `message` or `edit` envelope at the group's current epoch and
/// persist it; the caller's per-epoch hook has already positioned the group.
/// A failed decrypt or DB error persists nothing, which the watermark accounts
/// for.
///
/// `Opened(Some(id))` only for a new inbound DM message, which is what a
/// delivery receipt (#857) acknowledges. Control frames never are, so a receipt
/// can never trigger a receipt.
fn decrypt_and_persist_one(
    conn: &rusqlite::Connection,
    // The epoch's already-loaded group (#875).
    decryptor: &mut crate::commands::mls::MlsDecryptor<'_>,
    conversation_id: &str,
    env: &EnvelopeRow,
    // Already hex-decoded by the caller (#875).
    bytes: &[u8],
    user_id: &str,
    is_dm: bool,
) -> Ingested {
    // Attribution comes from the MLS credential inside the ciphertext, never the
    // server-writable `sender_id` (sealed sender,
    // `docs/metadata-minimization-design.md` §2).
    let (id, _sender_id, _ciphertext, reply_to_id, target_id, sent_at, env_type, _seq) = env;
    match env_type.as_str() {
        "message" => {
            // A placeholder (#1312) does not count: if this ever opens after
            // all, the real message replaces it.
            if super::loss::has_real_row(conn, id) {
                return Ingested::Opened(None);
            }
            let (plain, cred_sender) = match decryptor.decrypt(bytes) {
                Ok(opened) => opened,
                // Whether this is a loss is decided in `record_losses`; the
                // reason is logged now (#1520).
                Err(why) => return note_decrypt_failure(conversation_id, env_type, id, &why),
            };
            // A control frame leaves no row: remember it opened, and drop any
            // placeholder standing in for it.
            if !matches!(super::framing::classify(&plain), super::framing::Frame::Text { .. }) {
                super::loss::record_opened_envelope(conn, id);
                let _ = conn.execute(
                    "DELETE FROM message WHERE id = ?1 AND undecryptable IS NOT NULL",
                    rusqlite::params![id],
                );
            }
            match super::framing::classify(&plain) {
                // "Delete for everyone": honoured only when the redaction's
                // MLS-authenticated author is the target's stored author, so
                // neither the server nor another member can redact someone
                // else's message. Never stored as a `message` row.
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
                // Text / attachment. `thread_id` (#825) rides inside the
                // ciphertext as a trailer, so the DS never sees it.
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
                        ).unwrap_or(0);
                        // Delivered (#857): fetched, decrypted and stored here
                        // for the first time, in a DM, from someone else.
                        if inserted > 0 && is_dm && cred_sender != user_id {
                            return Ingested::Opened(Some(id.clone()));
                        }
                    }
                }
                // A receipt (#857) is a control message: never stored and never
                // acknowledged, so two clients cannot ping-pong receipts.
                super::framing::Frame::Receipt { kind, at, message_ids } => {
                    // Our own receipt, echoed back.
                    if cred_sender == user_id {
                        return Ingested::Opened(None);
                    }
                    // `record_receipt` applies the DM-only gate itself
                    // (`receipts_permitted`). `cred_sender` is authenticated, so
                    // nobody can record a receipt in another member's name.
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
                // Honoured only when the edit's MLS-authenticated author is the
                // target's stored author: the authorship check the DS no longer
                // does (#607).
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
    // Membership and the peer list in one request. The envelopes are NOT
    // fetched here: the group-level catch-up below runs its own pass, and
    // pulling them twice would double the transfer for nothing.
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

    // Re-verify each peer's `account_id_pub` against the local TOFU pin on every
    // ingest, so a changed key raises the `KeyChanged` banner as soon as it's seen.
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
    // One query for the whole peer set (#875).
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
    //! #693: the redaction outcome → tombstone fate matrix. The watermark half
    //! is `watermark::delete_consumption_tests`.

    use super::{resolve_delete, DeleteResolution};

    /// The redaction matched a live row this pass — done.
    #[test]
    fn applied_now_is_handled() {
        assert_eq!(
            resolve_delete(Some(1), true, false),
            DeleteResolution::Handled
        );
    }

    /// Zero rows but already redacted: idempotent, not a wedge.
    #[test]
    fn already_redacted_is_handled() {
        assert_eq!(
            resolve_delete(Some(0), true, false),
            DeleteResolution::Handled
        );
    }

    /// Absent but still awaiting ingest: retry rather than consume.
    #[test]
    fn absent_but_awaiting_ingest_is_pending() {
        assert_eq!(
            resolve_delete(Some(0), false, true),
            DeleteResolution::Pending
        );
    }

    /// Absent and not awaiting ingest (pre-join): terminal, so it does not spin.
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

