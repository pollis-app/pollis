//! MLS leaf self-update — the commit a member issues for ITSELF (issue #666).
//!
//! Without it non-committers never heal (PCS) and stay unmerged leaves, so commits grow linearly with group size; [`self_update_if_due`] decides when it fires.

use std::sync::Arc;

use openmls::prelude::*;
use openmls_traits::OpenMlsProvider;
use tls_codec::Serialize as TlsSerialize;

use crate::error::Result;
use crate::state::AppState;

use super::group_state::load_group_with_signer;
use super::provider::{stored_group_ciphersuite, MlsProvider, PollisProvider};
use super::reconcile::{publish_staged_commit, PublishOutcome};

/// How long a leaf may go unrotated; bounds how long a compromised device keeps decrypting.
const SELF_UPDATE_INTERVAL: chrono::Duration = chrono::Duration::days(7);

/// Per-(device, conversation) spread so a fleet does not rotate a shared group in the same hour.
pub(super) const SELF_UPDATE_JITTER: chrono::Duration = chrono::Duration::days(2);

/// Cap per sweep, so a long-absent device does not storm the log.
pub(super) const MAX_SELF_UPDATES_PER_SWEEP: usize = 3;

/// Rotate this device's leaf in `conversation_id` if it is overdue, and report
/// whether a commit actually landed.
///
/// Overdue: never rotated here (post-join), or older than [`SELF_UPDATE_INTERVAL`] plus jitter.
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
        // Never rotated (post-join, #666): the leaf is unmerged, so due now.
        None => return true,
    };
    chrono::Utc::now() - last >= SELF_UPDATE_INTERVAL + jitter_for(conversation_id)
}

/// Stable per-conversation offset in `[0, SELF_UPDATE_JITTER)`.
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
pub(super) fn record_self_update(conn: &rusqlite::Connection, conversation_id: &str, epoch: u64) {
    let now = chrono::Utc::now().to_rfc3339();
    if let Err(e) = conn.execute(
        "INSERT INTO mls_self_update (conversation_id, last_epoch, last_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT(conversation_id) DO UPDATE SET last_epoch = excluded.last_epoch, last_at = excluded.last_at",
        rusqlite::params![conversation_id, epoch as i64, now],
    ) {
        crate::diag!("[mls] self-update: recording rotation for {conversation_id} failed: {e}");
    }
}

/// Forget that this device ever rotated in `conversation_id`, making a
/// self-update due immediately.
///
/// Called on adopting a new generation (#454): the device is an unmerged leaf in the successor.
pub(super) fn clear_self_update(conn: &rusqlite::Connection, conversation_id: &str) {
    if let Err(e) = conn.execute(
        "DELETE FROM mls_self_update WHERE conversation_id = ?1",
        rusqlite::params![conversation_id],
    ) {
        crate::diag!("[mls] self-update: clearing rotation for {conversation_id} failed: {e}");
    }
}

/// Issue exactly one self-update commit for `conversation_id`, unconditionally.
///
/// Returns `false` if another member claimed the epoch first; the rotation stays due.
pub async fn self_update_group(
    state: &Arc<AppState>,
    conversation_id: &str,
    actor_user_id: &str,
) -> Result<bool> {
    // Reconcile's per-conversation lock: two commits from one epoch waste one.
    let mls_guard = state.mls_group_lock(conversation_id).await?;

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

    // Test-harness park point (#1041).
    #[cfg(feature = "test-harness")]
    super::group_state::rendezvous::park(super::group_state::rendezvous::Point::SelfUpdateBeforeSubmit)
        .await;

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
            crate::diag!(
                "[mls] self-update: rotated own leaf in {conversation_id} at epoch {epoch_before} → {}",
                epoch_before + 1
            );
            // Move voice frames to the new exporter key.
            crate::commands::voice_e2ee::on_mls_epoch_changed(state, conversation_id).await;
            // And the pin key's wrap moves to the new exporter too (#99).
            crate::commands::pinned_messages::maybe_rewrap_pin_key(state, conversation_id).await;
            crate::commands::file_keys::maybe_rewrap_file_keys(state, conversation_id).await;
            Ok(true)
        }
        PublishOutcome::LostRace => {
            // Converge via the interleaved ingesting catch-up, not a bare replay (`max_past_epochs = 0`); it takes the MLS lock, so drop ours.
            drop(mls_guard);
            if let Err(e) = crate::commands::messages::catch_up_mls_group_interleaved(
                state,
                conversation_id,
                actor_user_id,
            )
            .await
            {
                crate::diag!("[mls] self-update: converge-after-lost-race failed for {conversation_id}: {e}");
            }
            Ok(false)
        }
    }
}

/// What staging a self-update yields: `(epoch_before, commit_bytes,
/// group_info_bytes)`.
type StagedSelfUpdate = (u64, Vec<u8>, Option<Vec<u8>>);

/// Stage a path-bearing commit that carries no proposals: the MLS "update" that
/// replaces this member's leaf key and refreshes every node on its direct path.
///
/// `force_self_update(true)` is explicit: this commit exists for its path.
pub(super) fn stage_self_update<C>(
    provider: &MlsProvider<'_, C>,
    conversation_id: &str,
    generation: i64,
) -> Result<Option<StagedSelfUpdate>>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let (mut group, signer) = match load_group_with_signer(provider, conversation_id, generation) {
        Ok(pair) => pair,
        Err(e) => {
            crate::diag!("[mls] self-update: no usable local group for {conversation_id}: {e}");
            return Ok(None);
        }
    };

    let epoch_before = group.epoch().as_u64();

    // A dangling pending commit from an earlier aborted pass would make
    // `commit_builder` refuse; resolve it the same way reconcile does.
    crate::commands::file_keys::keep_epoch_kek(provider, &group);
    group
        .merge_pending_commit(provider)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("self-update merge pending: {e}")))?;
    if group.epoch().as_u64() != epoch_before {
        // Merging advanced us; re-read so the CAS targets the real epoch.
        crate::diag!(
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
