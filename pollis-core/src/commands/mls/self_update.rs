//! MLS leaf self-update — the commit a member issues for ITSELF (issue #666).
//!
//! Every other commit Pollis produces is *about someone else*: reconcile adds
//! and removes leaves, external join re-attaches a wiped device. None of them
//! rotate the committer-less members' own key material, and until #666 Pollis
//! had no self-update at all. That cost two things:
//!
//! 1. **No post-compromise security for non-committing members.** MLS forward
//!    secrecy comes from the ratchet and holds regardless. PCS does not: healing
//!    after a device compromise requires *that device* to publish a fresh leaf
//!    secret along its direct path, which only its own commit can do. A member
//!    who never commits never heals, no matter how many epochs pass.
//!
//! 2. **Commit payloads that grow linearly with group size.** A member added by
//!    someone else stays an *unmerged leaf* in every ancestor of the adder's
//!    direct path until that member itself commits. Each unmerged leaf adds one
//!    HPKE ciphertext to a copath resolution, so with a single commit-producing
//!    member the per-commit cost never collapses to the logarithm TreeKEM
//!    promises. Measured on #454's hybrid suite by
//!    `self_update_turns_linear_commit_growth_into_logarithmic`: a commit is
//!    13.4 KB at 8 members and 24.0 KB at 16 with every leaf unmerged — about
//!    1.3 KB per member, so ~135 KB at N=100. With every leaf merged the same
//!    commits are 8.7 KB and 11.1 KB: ~2.4 KB per *doubling*, so ~17 KB at
//!    N=100. Post-quantum encapsulations are ~35× larger than X25519's, which
//!    is what turned a latent inefficiency into a real ceiling on group size.
//!
//! One self-update per member per join collapses (2); a periodic one gives (1).
//! Both are the same commit — the difference is only when it fires, which is
//! what [`self_update_if_due`] decides.

use std::sync::Arc;

use openmls::prelude::*;
use openmls_traits::OpenMlsProvider;
use tls_codec::Serialize as TlsSerialize;

use crate::error::Result;
use crate::state::AppState;

use super::group_state::load_group_with_signer;
use super::provider::{stored_group_ciphersuite, MlsProvider, PollisProvider};
use super::reconcile::{publish_staged_commit, PublishOutcome};

/// How long a device's leaf may go without rotating before [`self_update_if_due`]
/// rotates it. Bounds the window in which a compromised device keeps decrypting:
/// after this long, every group it is in has healed past the stolen key material.
///
/// Seven days is a deliberate trade: MLS's own guidance is "as often as you can
/// afford", and the cost here is one commit per member per week per group — a
/// rounding error next to message traffic, but not free on the hybrid suite
/// where a commit is several KB.
const SELF_UPDATE_INTERVAL: chrono::Duration = chrono::Duration::days(7);

/// Extra delay, spread deterministically per (device, conversation), so a fleet
/// that all installed the same release does not rotate every leaf of a shared
/// group in the same hour. Losers of the resulting CAS races are correct but
/// wasteful, and the waste is what this avoids.
pub(super) const SELF_UPDATE_JITTER: chrono::Duration = chrono::Duration::days(2);

/// Most self-updates to attempt in one sweep. A device returning after a long
/// absence can be overdue in every group at once; without a cap, one cold launch
/// becomes a commit storm against the log. The rest are picked up on later
/// sweeps — rotation is a deadline, not a heartbeat, so spreading it is free.
pub(super) const MAX_SELF_UPDATES_PER_SWEEP: usize = 3;

/// Rotate this device's leaf in `conversation_id` if it is overdue, and report
/// whether a commit actually landed.
///
/// "Overdue" is: never rotated in this group (the post-join case — the leaf is
/// still unmerged, so this is also what collapses commit size), or last rotated
/// more than [`SELF_UPDATE_INTERVAL`] plus a per-conversation jitter ago.
pub async fn self_update_if_due(
    state: &Arc<AppState>,
    conversation_id: &str,
    actor_user_id: &str,
) -> Result<bool> {
    if !is_due(state, conversation_id).await {
        return Ok(false);
    }
    self_update_group(state, conversation_id, actor_user_id).await
}

/// Is a self-update overdue for `conversation_id`? Never rotated ⇒ yes.
async fn is_due(state: &Arc<AppState>, conversation_id: &str) -> bool {
    let guard = state.local_db.lock().await;
    let db = match guard.as_ref() {
        Some(db) => db,
        None => return false,
    };
    // A group we have no local state for cannot be self-updated.
    if stored_group_ciphersuite(db.conn(), conversation_id).is_none() {
        return false;
    }
    let last = match last_self_update(db.conn(), conversation_id) {
        Some(ts) => ts,
        // Never rotated: this is the post-join case (#666 M2). Due immediately —
        // the leaf is unmerged right now and every commit in the group is paying
        // for it.
        None => return true,
    };
    chrono::Utc::now() - last >= SELF_UPDATE_INTERVAL + jitter_for(conversation_id)
}

/// Deterministic per-conversation offset in `[0, SELF_UPDATE_JITTER)`. Derived
/// from the conversation id so it is stable across launches (a re-rolled jitter
/// would let a device drift indefinitely) and differs per group.
pub(super) fn jitter_for(conversation_id: &str) -> chrono::Duration {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in conversation_id.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let span = SELF_UPDATE_JITTER.num_seconds().max(1) as u64;
    chrono::Duration::seconds((hash % span) as i64)
}

/// The last time this device committed a self-update in `conversation_id`.
fn last_self_update(
    conn: &rusqlite::Connection,
    conversation_id: &str,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let raw: String = conn
        .query_row(
            "SELECT last_at FROM mls_self_update WHERE conversation_id = ?1",
            rusqlite::params![conversation_id],
            |row| row.get(0),
        )
        .ok()?;
    chrono::DateTime::parse_from_rfc3339(&raw)
        .ok()
        .map(|dt| dt.with_timezone(&chrono::Utc))
}

/// Record a landed self-update so the next one is a week out, not immediate.
/// Written only on a confirmed win: a lost race did not rotate anything.
///
/// `pub(super)` for migration (#454 P4): the device that creates a successor
/// group is its founder, so its leaf is merged by construction and it starts the
/// new lineage already rotated.
pub(super) fn record_self_update(conn: &rusqlite::Connection, conversation_id: &str, epoch: u64) {
    let now = chrono::Utc::now().to_rfc3339();
    if let Err(e) = conn.execute(
        "INSERT INTO mls_self_update (conversation_id, last_epoch, last_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT(conversation_id) DO UPDATE SET last_epoch = excluded.last_epoch, last_at = excluded.last_at",
        rusqlite::params![conversation_id, epoch as i64, now],
    ) {
        eprintln!("[mls] self-update: recording rotation for {conversation_id} failed: {e}");
    }
}

/// Forget that this device ever rotated in `conversation_id`, making a
/// self-update due immediately.
///
/// Called when adopting a new suite generation (#454 P4). A device that joins
/// the successor group by Welcome is an unmerged leaf *there* regardless of how
/// recently it rotated in the predecessor, and every commit in the new lineage
/// pays for that leaf until it commits once. Keeping the old timestamp would
/// suppress the post-join rotation for up to a week — exactly the linear
/// commit-growth #666 exists to kill.
pub(super) fn clear_self_update(conn: &rusqlite::Connection, conversation_id: &str) {
    if let Err(e) = conn.execute(
        "DELETE FROM mls_self_update WHERE conversation_id = ?1",
        rusqlite::params![conversation_id],
    ) {
        eprintln!("[mls] self-update: clearing rotation for {conversation_id} failed: {e}");
    }
}

/// Issue exactly one self-update commit for `conversation_id`, unconditionally.
///
/// Returns `true` if the commit landed, `false` if another member claimed the
/// epoch first (in which case we converge on the winner and leave the rotation
/// due, so the next sweep retries).
///
/// Shares reconcile's publish path verbatim — same compare-and-swap on the head
/// epoch, same Kani-proved adopt/rollback for an ambiguous submit — because a
/// self-update is an ordinary commit as far as the log is concerned. It carries
/// no Welcome and adds nobody.
pub async fn self_update_group(
    state: &Arc<AppState>,
    conversation_id: &str,
    actor_user_id: &str,
) -> Result<bool> {
    // Same per-conversation serialisation reconcile takes: two commits staged
    // from one epoch on the same device would guarantee one of them is wasted.
    let mls_guard = state.mls_group_lock(conversation_id).await;

    let staged = {
        let guard = state.local_db.lock().await;
        let db = match guard.as_ref() {
            Some(db) => db,
            None => return Ok(false),
        };
        let generation = super::generation::local_generation(db.conn(), conversation_id);
        let provider = PollisProvider::new(db.conn());
        stage_self_update(&provider, conversation_id, generation)?
            .map(|staged| (generation, staged))
    };
    let (generation, (epoch_before, commit_bytes, group_info_bytes)) = match staged {
        Some(v) => v,
        None => return Ok(false),
    };

    let published = publish_staged_commit(
        state,
        conversation_id,
        generation,
        None,
        actor_user_id,
        epoch_before,
        &commit_bytes,
        group_info_bytes.as_deref(),
        None,
        None,
        &[],
        "self-update",
    )
    .await?;

    match published {
        PublishOutcome::Won => {
            {
                let guard = state.local_db.lock().await;
                if let Some(db) = guard.as_ref() {
                    record_self_update(db.conn(), conversation_id, epoch_before + 1);
                }
            }
            eprintln!(
                "[mls] self-update: rotated own leaf in {conversation_id} at epoch {epoch_before} → {}",
                epoch_before + 1
            );
            // The local epoch advanced, so this device's voice frames must move
            // to the new exporter key — same hook the committer path in
            // reconcile fires for exactly the same reason.
            crate::commands::voice_e2ee::on_mls_epoch_changed(state, conversation_id).await;
            Ok(true)
        }
        PublishOutcome::LostRace => {
            // Our commit was rolled back. Converge through the INTERLEAVED
            // ingesting catch-up, never a bare commit replay: advancing past an
            // epoch whose messages we have not ingested discards their keys
            // (`max_past_epochs = 0`). It re-acquires the per-conversation MLS
            // lock, so drop ours first. The rotation stays due and the next
            // sweep retries it at the new epoch.
            drop(mls_guard);
            if let Err(e) = crate::commands::messages::catch_up_mls_group_interleaved(
                state,
                conversation_id,
                actor_user_id,
            )
            .await
            {
                eprintln!("[mls] self-update: converge-after-lost-race failed for {conversation_id}: {e}");
            }
            Ok(false)
        }
    }
}

/// Stage a path-bearing commit that carries no proposals: the MLS "update" that
/// replaces this member's leaf key and refreshes every node on its direct path.
///
/// Returns `(epoch_before, commit_bytes, group_info_bytes)`, or `None` when
/// there is no local group to update.
///
/// `force_self_update(true)` is explicit rather than relying on "a commit with
/// no proposals needs a path anyway" — the whole point of this commit is the
/// path, so a future openmls that optimised the empty commit into a pathless one
/// must break here loudly, not silently stop rotating keys.
pub(super) fn stage_self_update<C>(
    provider: &MlsProvider<'_, C>,
    conversation_id: &str,
    generation: i64,
) -> Result<Option<(u64, Vec<u8>, Option<Vec<u8>>)>>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let (mut group, signer) = match load_group_with_signer(provider, conversation_id, generation) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("[mls] self-update: no usable local group for {conversation_id}: {e}");
            return Ok(None);
        }
    };

    let epoch_before = group.epoch().as_u64();

    // A dangling pending commit from an earlier aborted pass would make
    // `commit_builder` refuse; resolve it the same way reconcile does.
    group
        .merge_pending_commit(provider)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("self-update merge pending: {e}")))?;
    if group.epoch().as_u64() != epoch_before {
        // Merging advanced us; re-read so the CAS targets the real epoch.
        eprintln!(
            "[mls] self-update: merged a dangling pending commit in {conversation_id} ({epoch_before} → {})",
            group.epoch().as_u64()
        );
    }
    let epoch_before = group.epoch().as_u64();

    let bundle = group
        .commit_builder()
        .force_self_update(true)
        .load_psks(provider.storage())
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("self-update load_psks: {e}")))?
        .create_group_info(true)
        .build(provider.rand(), provider.crypto(), &signer, |_| true)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("self-update build: {e}")))?
        .stage_commit(provider)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("self-update stage: {e}")))?;

    let (commit_out, _welcome, group_info_opt) = bundle.into_messages();
    let commit_bytes = commit_out
        .tls_serialize_detached()
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("self-update serialize: {e}")))?;
    let group_info_bytes = match group_info_opt {
        Some(gi) => Some(
            gi.tls_serialize_detached().map_err(|e| {
                crate::error::Error::Other(anyhow::anyhow!("self-update group_info serialize: {e}"))
            })?,
        ),
        None => None,
    };

    Ok(Some((epoch_before, commit_bytes, group_info_bytes)))
}
