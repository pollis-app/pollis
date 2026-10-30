//! Migrating an existing conversation onto the current suite (#454 P4, #669).
//!
//! MLS fixes the suite at creation, so a migration stands up a successor group in [`current_suite`](super::provider::current_suite) and moves the roster in by Welcome.
//!
//! Kept for when `CS_PQ`'s provisional code point is renumbered (a wire break).
//!
//! ## What makes it safe
//!
//! One compare-and-swap: opening generation N+1 is accepted only when `closes_epoch` names the head of N, so no commit is orphaned.
//!
//! Locally the successor has its own `GroupId`, so failure just deletes it; the predecessor is dropped last, after it is drained.
//!
//! ## No stranding
//!
//! Step 5 claims a target-suite KeyPackage for every roster device first and aborts on any miss: move everyone or nobody.

use std::collections::HashSet;
use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

use super::delivery::WelcomeOut;
use super::group_state::{create_mls_group_in_suite, forget_local_mls_group_at};
use super::provider::{current_suite, load_stored_group_at, PollisProvider};
use super::device::IdentityDirectory;
use super::reconcile::{
    load_pinned_identities, publish_staged_commit, stage_reconcile_commit,
    PublishOutcome,
};

/// Most migrations per sweep: each claims a KeyPackage per device and adds everyone.
pub(super) const MAX_MIGRATIONS_PER_SWEEP: usize = 2;

/// Migrate `conversation_id` onto the current suite if it is on some other one
/// and the move is possible, reporting whether a successor lineage actually
/// opened.
///
/// `Ok(false)` means "not now" and is retried by the next sweep.
pub(super) async fn migrate_to_current_suite_if_due(
    state: &Arc<AppState>,
    conversation_id: &str,
    actor_user_id: &str,
) -> Result<bool> {
    // 1. Cheap local gate: is this group on a non-current suite?
    let generation = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else {
            return Ok(false);
        };
        let generation = super::generation::local_generation(db.conn(), conversation_id);
        match load_stored_group_at(db.conn(), conversation_id, generation) {
            Some(group) if group.ciphersuite() != current_suite() => generation,
            _ => return Ok(false),
        }
    };

    // 2. Drain the predecessor first (`max_past_epochs = 0`); the catch-up takes the MLS lock itself.
    crate::commands::messages::catch_up_mls_group_interleaved(
        state,
        conversation_id,
        actor_user_id,
    )
    .await?;

    // 3. Hold the lineage still: the lock reconcile and self-update take.
    let mls_guard = state.mls_group_lock(conversation_id).await?;

    // Re-read under the lock: the catch-up may have adopted someone else's successor.
    let local_epoch = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else {
            return Ok(false);
        };
        if super::generation::local_generation(db.conn(), conversation_id) != generation {
            return Ok(false);
        }
        match load_stored_group_at(db.conn(), conversation_id, generation) {
            Some(group) if group.ciphersuite() != current_suite() => group.epoch().as_u64() as i64,
            _ => return Ok(false),
        }
    };

    // 4. Our drained epoch must equal the head, or closing would orphan the tail.
    let head = head_epoch_in(state, conversation_id, generation).await?;
    if head != local_epoch {
        crate::diag!(
            "[mls] migrate: {conversation_id} is at epoch {local_epoch} but generation \
             {generation} heads at {head} — deferring until drained"
        );
        return Ok(false);
    }

    let actor_device_id = state.device_id.lock().await.clone().unwrap_or_default();

    // 5. Every roster device but ours needs a target-suite KeyPackage; one miss aborts before anything is created.
    // Roster from the same server-side definitions reconcile uses (#987).
    let roster_user_ids: Vec<String> =
        crate::commands::ds_reads::catch_up_full(state, conversation_id, false, false, true)
            .await?
            .roster;
    let valid_devices =
        crate::commands::ds_reads::registered_devices(state, &roster_user_ids).await?;
    // The same cross-signing gate reconcile applies: a device whose claimed
    // KeyPackage is not the user's certified leaf is refused, not moved.
    let identities = load_pinned_identities(state, &roster_user_ids, actor_user_id).await?;
    let roster_user_ids: std::collections::HashSet<String> =
        roster_user_ids.into_iter().collect();

    // #1082: a snapshot omitting us would silently migrate a subset; refuse it as reconcile does.
    if !super::reconcile::snapshot_includes_actor(
        &roster_user_ids,
        &valid_devices,
        actor_user_id,
        &actor_device_id,
    ) {
        crate::diag!(
            "[mls] migrate: refusing {conversation_id}: registered_devices snapshot \
             ({} device(s)) omits this device — deferring rather than migrating a subset",
            valid_devices.len()
        );
        return Ok(false);
    }

    let targets: Vec<(String, String)> = valid_devices
        .iter()
        .filter(|(uid, did)| !(uid == actor_user_id && did == &actor_device_id))
        .cloned()
        .collect();

    let mut kp_tuples: Vec<(String, String, Vec<u8>)> = Vec::new();
    for (uid, did) in &targets {
        match super::ds_claim_key_package(state, uid, Some(did), current_suite()).await? {
            Some(kp) => kp_tuples.push((uid.clone(), did.clone(), kp)),
            None => {
                crate::diag!(
                    "[mls] migrate: {conversation_id} — no current-suite KeyPackage for {uid}:{did}; \
                     aborting so nobody is left behind on the old lineage"
                );
                return Ok(false);
            }
        }
    }

    // 6. Build the successor under its own `GroupId`; the predecessor is untouched.
    let successor = generation + 1;
    let staged = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else {
            return Ok(false);
        };
        let provider = PollisProvider::new(db.conn());
        stage_successor_commit(
            &provider,
            conversation_id,
            successor,
            actor_user_id,
            &actor_device_id,
            &kp_tuples,
            &roster_user_ids,
            &valid_devices,
            &identities,
        )
    };
    // Every failure past here must tear the successor down, or the generation backstop may adopt a lineage that never opened.
    let (welcomes, commit_bytes, group_info_bytes) = match staged {
        Ok(Some(v)) => v,
        Ok(None) => {
            abandon_successor(state, conversation_id, successor).await;
            return Ok(false);
        }
        Err(e) => {
            abandon_successor(state, conversation_id, successor).await;
            return Err(e);
        }
    };

    // 7. The CAS: `closes_epoch` names the predecessor head we saw; `epoch_before` is 0.
    let (added_uid, added_dids) = added_metadata(&welcomes);
    let published = publish_staged_commit(
        state,
        conversation_id,
        successor,
        Some(head),
        actor_user_id,
        0,
        &commit_bytes,
        group_info_bytes.as_deref(),
        added_uid.as_deref(),
        added_dids.as_deref(),
        &welcomes,
        "migrate",
    )
    .await;

    match published {
        Ok(PublishOutcome::Won) => {}
        Ok(PublishOutcome::LostRace) => {
            // Someone committed on the old lineage first; our successor is now meaningless.
            crate::diag!(
                "[mls] migrate: {conversation_id} lost the generation-{successor} race — \
                 discarding the successor and staying on {generation}"
            );
            abandon_successor(state, conversation_id, successor).await;
            return Ok(false);
        }
        Err(e) => {
            abandon_successor(state, conversation_id, successor).await;
            return Err(e);
        }
    }

    // 8. Record the lineage FIRST, then drop the predecessor: a crash between leaves only a harmless orphan.
    {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            super::generation::set_local_generation(db.conn(), conversation_id, successor);
            // The founder's leaf is merged by construction (#666).
            super::self_update::record_self_update(db.conn(), conversation_id, 1);
        }
    }
    let _ = forget_local_mls_group_at(state, conversation_id, generation).await;

    crate::diag!(
        "[mls] migrate: {conversation_id} moved to the current suite — generation \
         {generation} (head {head}) closed, generation {successor} open at epoch 1"
    );

    // A new group means a new exporter secret for voice.
    crate::commands::voice_e2ee::on_mls_epoch_changed(state, conversation_id).await;
    // Re-wrap the pin key under the successor's exporter (#99).
    crate::commands::pinned_messages::maybe_rewrap_pin_key(state, conversation_id).await;
    crate::commands::file_keys::maybe_rewrap_file_keys(state, conversation_id).await;
    drop(mls_guard);

    // Report our position on the NEW lineage so the retention floor follows us
    // across the boundary instead of pinning the retired one forever.
    super::ds_client::ds_report_commit_since(state, conversation_id, actor_user_id, successor, 1)
        .await;

    Ok(true)
}

/// What staging a successor's opening commit yields: `(welcomes, commit_bytes,
/// group_info_bytes)`.
type StagedSuccessor = (Vec<WelcomeOut>, Vec<u8>, Option<Vec<u8>>);

/// Create the successor group and stage its opening commit, returning
/// `(welcomes, commit_bytes, group_info_bytes)` — or `None` when there is
/// nothing to commit at all.
#[allow(clippy::too_many_arguments)]
fn stage_successor_commit<C>(
    provider: &super::provider::MlsProvider<'_, C>,
    conversation_id: &str,
    successor: i64,
    actor_user_id: &str,
    actor_device_id: &str,
    kp_tuples: &[(String, String, Vec<u8>)],
    roster_user_ids: &HashSet<String>,
    valid_devices: &HashSet<(String, String)>,
    identities: &IdentityDirectory,
) -> Result<Option<StagedSuccessor>>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    create_mls_group_in_suite(
        provider,
        conversation_id,
        successor,
        actor_user_id,
        actor_device_id,
        current_suite(),
    )?;

    // Reuse reconcile's staging: against a group of only us, the diff IS the add-everyone commit.
    let staged = stage_reconcile_commit(
        provider,
        conversation_id,
        successor,
        kp_tuples,
        roster_user_ids,
        actor_user_id,
        actor_device_id,
        valid_devices,
        identities,
    )?;

    match staged {
        Some((outcome, Some(data))) => {
            let welcomes = welcomes_for(&outcome.added, data.welcome_bytes.as_deref());
            Ok(Some((welcomes, data.commit_bytes, data.group_info_bytes)))
        }
        // A sole-device roster stages nothing, but epoch 0 still needs a commit; a self-update is the smallest.
        _ => Ok(super::self_update::stage_self_update(provider, conversation_id, successor)?
            .map(|(_, commit, group_info)| (Vec::new(), commit, group_info))),
    }
}

/// Tear down a successor group that never opened its lineage.
async fn abandon_successor(state: &Arc<AppState>, conversation_id: &str, successor: i64) {
    if let Err(e) = forget_local_mls_group_at(state, conversation_id, successor).await {
        crate::diag!("[mls] migrate: discarding successor {successor} for {conversation_id}: {e}");
    }
}

/// The head epoch of `generation` — `MAX(epoch) + 1`, i.e. the next epoch the DS
/// would accept, matching `pollis_delivery::commit::head_epoch_in` exactly. An
/// empty lineage heads at 0.
async fn head_epoch_in(
    state: &Arc<AppState>,
    conversation_id: &str,
    generation: i64,
) -> Result<i64> {
    // Advisory: the DS re-validates `closes_epoch` in the CAS, so no lock is needed.
    let snap = super::ds_reads::conversation_snapshot(
        state,
        super::ds_reads::state_query(conversation_id, Some(generation)),
    )
    .await?;
    Ok(snap.head)
}

/// One Welcome per added recipient, all carrying the same blob (the commit's
/// single Welcome message targets every added KeyPackage).
fn welcomes_for(added: &[(String, String)], welcome_bytes: Option<&[u8]>) -> Vec<WelcomeOut> {
    let Some(bytes) = welcome_bytes else {
        return Vec::new();
    };
    added
        .iter()
        .map(|(uid, did)| WelcomeOut {
            recipient_id: uid.clone(),
            recipient_device_id: did.clone(),
            welcome: bytes.to_vec(),
        })
        .collect()
}

/// The `(added_user_id, added_device_ids)` pair the commit row carries so
/// receivers can verify cross-signing certs before processing it.
///
/// **Every distinct user**, in add order, plus every added device id, both CSV (#1080).
///
/// A PREFETCH HINT: receivers verify the commit's own Add proposals, so a bad hint only looks unverifiable.
fn added_metadata(welcomes: &[WelcomeOut]) -> (Option<String>, Option<String>) {
    if welcomes.is_empty() {
        return (None, None);
    }
    let mut uids: Vec<&str> = Vec::new();
    for w in welcomes {
        if !uids.contains(&w.recipient_id.as_str()) {
            uids.push(w.recipient_id.as_str());
        }
    }
    let dids = welcomes
        .iter()
        .map(|w| w.recipient_device_id.as_str())
        .collect::<Vec<_>>()
        .join(",");
    (Some(uids.join(",")), Some(dids))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn welcome(uid: &str, did: &str) -> WelcomeOut {
        WelcomeOut {
            recipient_id: uid.to_string(),
            recipient_device_id: did.to_string(),
            welcome: vec![1, 2, 3],
        }
    }

    #[test]
    fn no_welcome_blob_means_no_recipients() {
        // A commit that adds nobody (the sole-device migration) carries no
        // Welcome, and must not fabricate rows the DS would then try to deliver.
        let added = vec![("u1".to_string(), "d1".to_string())];
        assert!(welcomes_for(&added, None).is_empty());
    }

    #[test]
    fn every_added_device_gets_the_same_blob() {
        let added = vec![
            ("u1".to_string(), "d1".to_string()),
            ("u1".to_string(), "d2".to_string()),
            ("u2".to_string(), "d3".to_string()),
        ];
        let out = welcomes_for(&added, Some(&[9, 9]));
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|w| w.welcome == vec![9, 9]));
        assert_eq!(out[2].recipient_id, "u2");
    }

    /// #1080: a migration re-adds the whole roster in ONE commit.
    #[test]
    fn added_metadata_lists_every_user_and_every_device() {
        let (uid, dids) = added_metadata(&[welcome("u1", "d1"), welcome("u2", "d2")]);
        assert_eq!(uid.as_deref(), Some("u1,u2"));
        assert_eq!(dids.as_deref(), Some("d1,d2"));
    }

    /// A user with several devices appears ONCE in the user list.
    #[test]
    fn added_metadata_does_not_repeat_a_multi_device_user() {
        let (uid, dids) = added_metadata(&[
            welcome("u1", "d1"),
            welcome("u1", "d2"),
            welcome("u2", "d3"),
        ]);
        assert_eq!(uid.as_deref(), Some("u1,u2"));
        assert_eq!(dids.as_deref(), Some("d1,d2,d3"));
    }

    /// Both columns are CSV; a single user is a bare id.
    #[test]
    fn added_metadata_of_one_user_carries_no_separator() {
        let (uid, dids) = added_metadata(&[welcome("u1", "d1")]);
        assert_eq!(uid.as_deref(), Some("u1"));
        assert_eq!(dids.as_deref(), Some("d1"));
    }

    #[test]
    fn added_metadata_is_absent_when_nobody_was_added() {
        // A self-update opening commit adds nobody: verification columns stay NULL.
        let (uid, dids) = added_metadata(&[]);
        assert!(uid.is_none());
        assert!(dids.is_none());
    }
}
