//! Inbound MLS `Welcome` handling.

use openmls::prelude::*;
use openmls_traits::OpenMlsProvider;

use std::sync::Arc;
use tls_codec::Deserialize as TlsDeserialize;

use crate::error::Result;
use crate::state::AppState;

use super::key_packages::replenish_key_packages;
use super::provider::{welcome_ciphersuite, MlsProvider, PollisProvider};

/// Internal: deserialise a TLS-encoded `MlsMessageOut` (welcome wire format)
/// and persist the resulting MLS group state locally.
///
/// Returns the `(conversation_id, generation)` joined, for the post-join self-update (#666).
///
/// The generation comes from the GroupId, since a migrated group's Welcome admits us to the successor (#454).
pub async fn apply_welcome(
    state: &Arc<AppState>,
    welcome_bytes: &[u8],
    expected_conversation_id: Option<&str>,
) -> Result<(String, i64)> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
    })?;

    let mut reader: &[u8] = welcome_bytes;
    let msg_in = MlsMessageIn::tls_deserialize(&mut reader)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("welcome msg deserialize: {e}")))?;

    let welcome = match msg_in.extract() {
        MlsMessageBodyIn::Welcome(w) => w,
        _ => return Err(crate::error::Error::Other(anyhow::anyhow!(
            "expected Welcome message in mls_welcome"
        ))),
    };

    // Suite from the Welcome itself; an unknown code point is a hard failure, never a fallback.
    welcome_ciphersuite(&welcome).ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!(
            "welcome names a ciphersuite this build does not implement"
        ))
    })?;

    let provider = PollisProvider::new(db.conn());
    let raw_group_id = join_from_welcome(&provider, welcome, expected_conversation_id)?;
    Ok(super::generation::split_mls_group_id(&raw_group_id))
}

/// Materialise group state from an already-suite-matched `Welcome`, returning
/// the joined group's RAW GroupId string — the conversation id for generation 0,
/// `"{conversation_id}#g{generation}"` for a successor lineage (see
/// [`super::generation`]).
///
/// openmls rejects duplicate GroupIds in `into_staged_welcome`, so the stale group is deleted first, scoped to the incoming GroupId (a predecessor survives).
///
/// That delete is the dangerous half (#1161 C1): the GroupId is attacker-supplied. Two guards:
///
/// * `expected`, when known, must match the GroupId's conversation.
/// * A local group at or above the Welcome's epoch is never replaced: a genuine re-admit is always above it.
pub(super) fn join_from_welcome<C>(
    provider: &MlsProvider<'_, C>,
    welcome: Welcome,
    expected: Option<&str>,
) -> Result<String>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let join_config = super::provider::join_config();

    let processed = ProcessedWelcome::new_from_welcome(provider, &join_config, welcome)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("process welcome: {e}")))?;

    let new_group_id = processed.unverified_group_info().group_id().clone();
    let welcome_epoch = processed.unverified_group_info().epoch().as_u64();

    let raw_group_id = String::from_utf8(new_group_id.as_slice().to_vec()).map_err(|_| {
        crate::error::Error::Other(anyhow::anyhow!("welcome carries a non-UTF-8 group id"))
    })?;
    let (conversation_id, generation) = super::generation::split_mls_group_id(&raw_group_id);
    if let Some(expected) = expected {
        if conversation_id != expected {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "welcome filed under {expected} carries a group id for {conversation_id} — refusing"
            )));
        }
    }

    if let Ok(Some(mut old_group)) = MlsGroup::load(provider.storage(), &new_group_id) {
        let local_epoch = old_group.epoch().as_u64();
        // An unconfirmed branch at the Welcome's epoch is doomed, so the Welcome replaces it (#1220).
        let unconfirmed = super::join_branch::is_unconfirmed(
            provider.raw_conn(),
            &conversation_id,
            generation,
        );
        if !super::join_branch::welcome_may_replace(local_epoch, welcome_epoch, unconfirmed) {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "welcome for {raw_group_id} is at epoch {welcome_epoch} but this device already \
                 holds that group at epoch {local_epoch} — refusing to replace a group the commit \
                 log has already carried past it"
            )));
        }
        crate::diag!(
            "[mls] apply_welcome: replacing group {raw_group_id} at epoch {local_epoch} with the \
             Welcome's epoch {welcome_epoch}"
        );
        let _ = old_group.delete(provider.storage());
        super::join_branch::clear(provider.raw_conn(), &conversation_id, generation);
    }

    let staged = processed.into_staged_welcome(provider, None)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("stage welcome: {e}")))?;

    staged.into_group(provider)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("into group: {e}")))?;
    // Added to the tree here: nothing sealed before this epoch was sent to
    // this device (#1312).
    super::join_branch::mark_fresh_if_first(provider.raw_conn(), &conversation_id, generation);
    super::join_branch::record_added(
        provider.raw_conn(),
        &conversation_id,
        generation,
        welcome_epoch,
    );

    Ok(raw_group_id)
}

/// Poll the remote `mls_welcome` table for undelivered Welcome messages
/// addressed to `user_id`.  Each one is applied locally and then marked
/// `delivered = 1` so it is not processed again.
pub async fn poll_mls_welcomes_inner(state: &Arc<AppState>, user_id: &str, device_id: &str) -> Result<()> {
    // Fetch-to-ack under the cross-process lock (#1599): a second process draining the same
    // Welcome would join the group a second time over the first's state.
    let _cross_process = state.cross_process_lock().await?;
    // The DS scopes the drain to the verified signing device (#987).
    let super::ds_reads::FetchedWelcomes {
        items,
        unclaimed_key_packages,
        has_last_resort_key_package,
    } = super::ds_reads::fetch_welcomes(state, user_id, device_id).await?;

    let had_welcomes = !items.is_empty();
    // Ack even apply failures (they would loop forever), but re-join those conversations after the ack (#1571).
    let mut processed_ids: Vec<String> = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    // Joined lineages, deduped per conversation, highest generation wins (#666, #454).
    let mut joined: Vec<(String, i64)> = Vec::new();
    for super::ds_reads::FetchedWelcome {
        id,
        bytes,
        conversation_id: filed_under,
    } in items
    {
        // `None`: the corroboration below stands in for the filed-under check.
        match apply_welcome(state, &bytes, None).await {
            Ok((conversation_id, generation)) => {
                crate::diag!("[mls] poll_mls_welcomes: applied welcome {id}");
                if welcome_is_corroborated(state, &conversation_id, generation).await {
                    match joined.iter_mut().find(|(c, _)| c == &conversation_id) {
                        Some(entry) => entry.1 = entry.1.max(generation),
                        None => joined.push((conversation_id, generation)),
                    }
                }
            }
            Err(e) => {
                crate::diag!(
                    "[mls] poll_mls_welcomes: failed to apply welcome {id} (filed under \
                     {filed_under:?}): {e} — acknowledging it and re-joining that \
                     conversation now (#1571)"
                );
                if let Some(conversation_id) = filed_under {
                    if !failed.contains(&conversation_id) {
                        failed.push(conversation_id);
                    }
                }
            }
        }
        processed_ids.push(id);
    }

    // W5 seam: mark delivered=1 through the Delivery Service (sole writer).
    if !processed_ids.is_empty() {
        let body = pollis_api::writes::AckBody {
            welcome_ids: processed_ids,
            // No-auth fallback for the acting user; never widens what the caller may do.
            user_id: Some(user_id.to_string()),
        };
        match super::ds_client::ds_post(state, &body).await {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => {
                let s = resp.status();
                let txt = resp.text().await.unwrap_or_default();
                crate::diag!("[mls] poll_mls_welcomes: DS ack {s}: {txt}");
            }
            Err(e) => {
                crate::diag!("[mls] poll_mls_welcomes: DS ack failed: {e}");
            }
        }
    }

    // Top the pool up when low (#1412); the drain carries the pool size.
    if super::key_packages::drain_wants_refill(
        had_welcomes,
        unclaimed_key_packages,
        has_last_resort_key_package,
    ) {
        if let Err(e) = replenish_key_packages(state, user_id, device_id).await {
            crate::diag!("[mls] KP replenishment failed (non-fatal): {e}");
        }
    }

    adopt_undrainable_joins(state, &joined).await;

    // Adopt pin keys (#99) BEFORE the self-update moves the epoch past the adder's re-wrap.
    for (conversation_id, _) in &joined {
        crate::commands::pinned_messages::try_adopt_pin_key(state, conversation_id).await;
        crate::commands::file_keys::try_adopt_file_keys(state, conversation_id).await;
    }

    self_update_joined_groups(state, user_id, &joined).await;

    rejoin_after_failed_welcomes(state, user_id, &failed, &joined).await;

    Ok(())
}

/// Re-join each conversation whose Welcome this pass could not apply (#1571).
///
/// The ack lets the I6 gate admit an external join now, so the gap since the failed add is shown as a loss.
///
/// No loop: one attempt per failed Welcome; skipped if this pass joined it or a group is held.
async fn rejoin_after_failed_welcomes(
    state: &Arc<AppState>,
    user_id: &str,
    failed: &[String],
    joined: &[(String, i64)],
) {
    for conversation_id in failed {
        if joined.iter().any(|(c, _)| c == conversation_id) {
            continue;
        }
        let held = {
            let guard = state.local_db.lock().await;
            guard
                .as_ref()
                .is_some_and(|db| super::group_state::has_local_group(db.conn(), conversation_id))
        };
        if held {
            continue;
        }
        crate::diag!(
            "[mls] poll_mls_welcomes: {conversation_id}: Welcome unusable and no group held — \
             re-joining now"
        );
        // Boxed: the catch-up can poll Welcomes again (its no-GroupInfo branch),
        // which makes this future recursive.
        if let Err(e) = Box::pin(crate::commands::messages::catch_up_mls_group_interleaved(
            state,
            conversation_id,
            user_id,
        ))
        .await
        {
            crate::diag!(
                "[mls] poll_mls_welcomes: re-join of {conversation_id} after a failed Welcome \
                 failed: {e}"
            );
        }
    }
}

/// Does the conversation a just-applied Welcome claims actually list us as a
/// member? If not, drop the group it materialised (#1161 C1).
///
/// The Welcome's GroupId is chosen by whoever wrote the row, so it could point us at any conversation.
///
/// It also checks the lineage: the genuine commit is logged with its Welcome, so a log head below our epoch means a fabrication.
///
/// Fails OPEN on anything short of a definite "no".
async fn welcome_is_corroborated(
    state: &Arc<AppState>,
    conversation_id: &str,
    generation: i64,
) -> bool {
    // Consumed up front, whatever the verdict, so a marker can never outlive
    // the join that wrote it (#1571).
    let fresh = {
        let guard = state.local_db.lock().await;
        guard
            .as_ref()
            .is_some_and(|db| super::join_branch::take_fresh(db.conn(), conversation_id, generation))
    };
    let snap = match super::ds_reads::conversation_snapshot(
        state,
        super::ds_reads::state_query(conversation_id, Some(generation)),
    )
    .await
    {
        Ok(snap) => snap,
        Err(e) => {
            crate::diag!(
                "[mls] poll_mls_welcomes: cannot corroborate the welcome for {conversation_id} \
                 ({e}) — keeping the join"
            );
            return true;
        }
    };
    let joined_epoch = {
        let guard = state.local_db.lock().await;
        guard.as_ref().and_then(|db| {
            super::provider::load_stored_group_at(db.conn(), conversation_id, generation)
                .map(|g| g.epoch().as_u64())
        })
    };
    let epoch_is_in_the_log = match joined_epoch {
        Some(epoch) => u64::try_from(snap.head).is_ok_and(|head| head >= epoch),
        // Nothing stored — nothing to corroborate or discard.
        None => true,
    };
    if snap.authorized && snap.is_member != Some(false) && epoch_is_in_the_log {
        // #1571: a late Welcome records the same entitlement marker as an external join.
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            super::join_branch::record_entitlement(
                db.conn(),
                conversation_id,
                generation,
                fresh,
                snap.device_registered_at.as_deref(),
                snap.member_since.as_deref(),
            );
        }
        return true;
    }
    crate::diag!(
        "[mls] poll_mls_welcomes: a welcome admitted this device to {conversation_id} generation \
         {generation} at epoch {joined_epoch:?}, but the conversation does not corroborate it \
         (authorized={}, is_member={:?}, log head={}) — discarding the group it created",
        snap.authorized, snap.is_member, snap.head
    );
    if let Err(e) =
        super::group_state::forget_local_mls_group_at(state, conversation_id, generation).await
    {
        crate::diag!(
            "[mls] poll_mls_welcomes: could not discard the uncorroborated group for \
             {conversation_id}: {e}"
        );
    }
    false
}

/// Adopt a newly-joined lineage in the one case the catch-up backstop cannot
/// reach: we hold **no group at all** on our recorded generation (#454 P4).
///
/// Otherwise `process_pending_commits` drains the predecessor first, since adoption deletes its keys.
///
/// A member added after migration has no predecessor and would never hop, so it adopts here.
///
/// Not capped: a deferred adoption would never be fixed.
async fn adopt_undrainable_joins(state: &Arc<AppState>, joined: &[(String, i64)]) {
    for (conversation_id, generation) in joined {
        let predecessor = {
            let guard = state.local_db.lock().await;
            match guard.as_ref() {
                Some(db) => {
                    let local = super::generation::local_generation(db.conn(), conversation_id);
                    let drainable =
                        super::provider::load_stored_group_at(db.conn(), conversation_id, local)
                            .is_some();
                    if *generation > local && !drainable {
                        Some(local)
                    } else {
                        None
                    }
                }
                None => None,
            }
        };
        if let Some(predecessor) = predecessor {
            crate::diag!(
                "[mls] poll_mls_welcomes: {conversation_id} joined at generation {generation} \
                 with no group on {predecessor} — adopting immediately"
            );
            super::group_state::adopt_generation_locked(
                state,
                conversation_id,
                predecessor,
                *generation,
            )
            .await;
        }
    }
}

/// Post-join self-update (issue #666): rotate our own leaf in each group we just
/// joined, so it stops being an *unmerged* leaf.
///
/// Unmerged leaves make commits grow with N, not log N.
///
/// Best-effort, after the DS ack and KP replenish, so it cannot strand a Welcome.
async fn self_update_joined_groups(state: &Arc<AppState>, user_id: &str, joined: &[(String, i64)]) {
    // Cap the burst: a wiped device re-processes every Welcome.
    for (conversation_id, _) in joined.iter().take(super::self_update::MAX_SELF_UPDATES_PER_SWEEP) {
        // Catch up first (interleaved) so the commit is not from a stale epoch.
        if let Err(e) =
            crate::commands::messages::catch_up_mls_group_interleaved(state, conversation_id, user_id)
                .await
        {
            crate::diag!("[mls] post-join self-update: catch-up for {conversation_id} failed: {e}");
            continue;
        }
        match super::self_update::self_update_if_due(state, conversation_id, user_id).await {
            Ok(true) => {
                crate::diag!("[mls] post-join self-update: merged own leaf in {conversation_id}");
            }
            Ok(false) => {}
            Err(e) => {
                crate::diag!("[mls] post-join self-update for {conversation_id} failed: {e}");
            }
        }
    }
    if joined.len() > super::self_update::MAX_SELF_UPDATES_PER_SWEEP {
        crate::diag!(
            "[mls] post-join self-update: {} of {} groups deferred to the next sweep",
            joined.len() - super::self_update::MAX_SELF_UPDATES_PER_SWEEP,
            joined.len()
        );
    }
}

/// Re-arm welcome delivery so `poll_mls_welcomes` re-processes Welcomes and a
/// freshly-created/wiped local DB recovers all MLS group memberships.
///
/// Called on an empty `mls_kv`, AFTER `ensure_device_cert`, so the signed reset authenticates.
///
/// Scoped to THIS device when one exists, so siblings keep their MLS state.
pub async fn reset_welcome_delivery(state: &Arc<AppState>, user_id: &str) -> Result<()> {
    let maybe_device_id = state
        .keystore
        .load_for_user("device_id", user_id)
        .await
        .ok()
        .flatten()
        .and_then(|b| String::from_utf8(b).ok());

    // The DS derives the recipient from the authenticated user; the body
    // only carries the device scope (null ⇒ all of this user's welcomes).
    let body = pollis_api::writes::ResetBody {
        device_id: maybe_device_id,
        // No-auth fallback for the acting user; never widens what the caller may do.
        user_id: Some(user_id.to_string()),
    };
    match super::ds_client::ds_post(state, &body).await {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => {
            let s = resp.status();
            let txt = resp.text().await.unwrap_or_default();
            crate::diag!("[mls] reset_welcome_delivery: DS reset {s}: {txt}");
        }
        Err(e) => {
            crate::diag!("[mls] reset_welcome_delivery: DS reset failed: {e}");
        }
    }

    Ok(())
}

pub async fn poll_mls_welcomes(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<()> {
    let device_id = state.device_id.lock().await.clone()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("device_id not set")))?;
    poll_mls_welcomes_inner(state, &user_id, &device_id).await
}
