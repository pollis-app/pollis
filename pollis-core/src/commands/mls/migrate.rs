//! Migrating an existing conversation onto the current suite (#454 P4, #669).
//!
//! RFC 9420 cannot change a group's suite, so this stands up a **successor
//! group** in [`current_suite`](super::provider::current_suite) and moves the
//! roster in by Welcome. In the steady state it is one local read returning
//! `Ok(false)`; it stays because `CS_PQ`'s code point is provisional.
//!
//! Safety: the whole migration is one DS compare-and-swap on `closes_epoch`;
//! locally both lineages coexist until it resolves, and the predecessor is
//! dropped last. No stranding: a KeyPackage is claimed for every roster device
//! before anything is created, so everyone moves or nobody does. See
//! `.codesight/wiki/mls.md` → "Suite generations".

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

/// Most suite migrations to attempt in one sweep: each claims a KeyPackage per
/// roster device and publishes an add-everyone commit, and eligibility does not
/// expire.
pub(super) const MAX_MIGRATIONS_PER_SWEEP: usize = 2;

/// Migrate `conversation_id` onto the current suite if it is on some other one
/// and the move is possible, reporting whether a successor lineage opened.
///
/// `Ok(false)` is the common answer and never an error: it means "not now", and
/// the next sweep retries.
pub(super) async fn migrate_to_current_suite_if_due(
    state: &Arc<AppState>,
    conversation_id: &str,
    actor_user_id: &str,
) -> Result<bool> {
    // 1. Local gate: is this group on a suite that is no longer current?
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

    // 2. Drain the predecessor through the interleaved catch-up
    //    (`max_past_epochs = 0`: nothing decrypts what we skip after retiring it).
    //    It takes the MLS lock itself, hence before ours.
    crate::commands::messages::catch_up_mls_group_interleaved(
        state,
        conversation_id,
        actor_user_id,
    )
    .await?;

    // 3. From here the lineage must not move under us.
    let mls_guard = state.mls_group_lock(conversation_id).await;

    // Re-read under the lock: the catch-up may have adopted someone else's
    // successor.
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

    // 4. Our drained local epoch must equal the lineage head, or closing it would
    //    orphan the tail.
    let head = head_epoch_in(state, conversation_id, generation).await?;
    if head != local_epoch {
        crate::diag!(
            "[mls] migrate: {conversation_id} is at epoch {local_epoch} but generation \
             {generation} heads at {head} — deferring until drained"
        );
        return Ok(false);
    }

    let actor_device_id = state.device_id.lock().await.clone().unwrap_or_default();

    // 5. Every roster device but ours needs a target-suite KeyPackage; one miss
    //    aborts BEFORE anything is created. Roster and devices come from the same
    //    DS definitions reconcile uses (#987).
    let roster_user_ids: Vec<String> =
        crate::commands::ds_reads::catch_up_full(state, conversation_id, false, false, true)
            .await?
            .roster;
    let valid_devices =
        crate::commands::ds_reads::registered_devices(state, &roster_user_ids).await?;
    // The same cross-signing gate reconcile applies.
    let identities = load_pinned_identities(state, &roster_user_ids, actor_user_id).await?;
    let roster_user_ids: std::collections::HashSet<String> =
        roster_user_ids.into_iter().collect();

    // #1082: a snapshot that omits our own device would silently migrate a
    // subset of the roster.
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

    // 6. Build the successor under its own `GroupId`; the predecessor still
    //    serves reads.
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
    // Every failure past the create must tear the successor down: the generation
    // backstop in `process_pending_commits` treats a stored successor as
    // evidence of a migration.
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

    // 7. The compare-and-swap: `closes_epoch` names the predecessor head we
    //    observed; epoch 0 is accepted only on this branch.
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

    // 8. Record the lineage FIRST, then drop the predecessor: a crash in between
    //    leaves a harmless orphan, the reverse leaves a dangling pointer. Same
    //    ordering as `adopt_generation_locked`.
    {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            super::generation::set_local_generation(db.conn(), conversation_id, successor);
            // The migrator founded this group, so its leaf starts rotated (#666).
            super::self_update::record_self_update(db.conn(), conversation_id, 1);
        }
    }
    let _ = forget_local_mls_group_at(state, conversation_id, generation).await;

    crate::diag!(
        "[mls] migrate: {conversation_id} moved to the current suite — generation \
         {generation} (head {head}) closed, generation {successor} open at epoch 1"
    );

    // A new group means a new exporter: voice re-derives, and the migrator
    // re-wraps the pin key (#99) and file keys under the successor.
    crate::commands::voice_e2ee::on_mls_epoch_changed(state, conversation_id).await;
    crate::commands::pinned_messages::maybe_rewrap_pin_key(state, conversation_id).await;
    crate::commands::file_keys::maybe_rewrap_file_keys(state, conversation_id).await;
    drop(mls_guard);

    // Report our position on the NEW lineage so the retention floor follows us.
    super::ds_client::ds_report_commit_since(state, conversation_id, actor_user_id, successor, 1)
        .await;

    Ok(true)
}

type StagedSuccessor = (Vec<WelcomeOut>, Vec<u8>, Option<Vec<u8>>);

/// Create the successor group in the current suite and stage its opening
/// commit, or `None` when there is nothing to commit at all.
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

    // Reuse reconcile's staging: against a group containing only us, the diff IS
    // the add-everyone commit, with the same filtering and suite re-check.
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
        // A roster of only this device stages nothing, but the CAS needs a commit
        // at epoch 0: a self-update is the smallest one.
        _ => Ok(super::self_update::stage_self_update(provider, conversation_id, successor)?
            .map(|(_, commit, group_info)| (Vec::new(), commit, group_info))),
    }
}

/// Tear down a successor group that never opened its lineage. Best-effort: a
/// leftover group is inert and the next attempt deletes it on create.
async fn abandon_successor(state: &Arc<AppState>, conversation_id: &str, successor: i64) {
    if let Err(e) = forget_local_mls_group_at(state, conversation_id, successor).await {
        crate::diag!("[mls] migrate: discarding successor {successor} for {conversation_id}: {e}");
    }
}

/// The head epoch of `generation`: `MAX(epoch) + 1`, matching
/// `pollis_delivery::commit::head_epoch_in`. An empty lineage heads at 0.
async fn head_epoch_in(
    state: &Arc<AppState>,
    conversation_id: &str,
    generation: i64,
) -> Result<i64> {
    // Advisory only: the DS re-validates `closes_epoch` inside the CAS.
    let snap = super::ds_reads::conversation_snapshot(
        state,
        super::ds_reads::state_query(conversation_id, Some(generation)),
    )
    .await?;
    Ok(snap.head)
}

/// One Welcome per added recipient, all carrying the commit's single blob.
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

/// The `(added_user_id, added_device_ids)` prefetch hint for the commit row:
/// every distinct user in add order, plus every added device id, both CSV,
/// mirroring `reconcile` (#1080). Receivers verify the commit itself, so an
/// incomplete hint only costs a spurious eviction reconcile.
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

    /// The DS splits both CSV columns on `,`; one user carries no separator.
    #[test]
    fn added_metadata_of_one_user_carries_no_separator() {
        let (uid, dids) = added_metadata(&[welcome("u1", "d1")]);
        assert_eq!(uid.as_deref(), Some("u1"));
        assert_eq!(dids.as_deref(), Some("d1"));
    }

    #[test]
    fn added_metadata_is_absent_when_nobody_was_added() {
        let (uid, dids) = added_metadata(&[]);
        assert!(uid.is_none());
        assert!(dids.is_none());
    }
}
