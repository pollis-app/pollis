//! Migrating an existing conversation onto the post-quantum suite (#454 P4).
//!
//! MLS binds the ciphersuite into the group at creation and RFC 9420 provides no
//! way to change it afterwards — there is no "rekey to another suite" commit. So
//! a migration is not an operation on the group at all: it stands up a
//! **successor group** for the same conversation in [`CS_HYBRID`] and moves the
//! roster in by Welcome. The predecessor is retired, not converted.
//!
//! That is what suite *generations* are (see [`super::generation`]): the
//! conversation's commit log is keyed by `(generation, epoch)` rather than
//! `epoch` alone, the successor restarts at epoch 0 of generation `N + 1`, and
//! ordering is lexicographic so restarting the epoch counter never reads as going
//! backwards.
//!
//! ## What makes it safe
//!
//! The whole migration is one compare-and-swap, enforced server-side by the DS's
//! second accept branch (`pollis-delivery/src/commit.rs`): opening generation
//! `N + 1` at epoch 0 is accepted only when the submitter also names the head of
//! generation `N` in `closes_epoch`. If any commit lands in the old lineage
//! between our read and our write, `closes_epoch` is stale and the entire
//! migration is rejected — the alternative being a commit silently orphaned on a
//! lineage nobody will ever read again.
//!
//! Locally the same property comes from the ordering below: the successor is
//! built under its own `GroupId`, so up until the CAS resolves this device holds
//! *both* lineages and a failure is a plain delete of the successor. The
//! predecessor's key material — the only thing that can still open envelopes
//! sealed before the boundary, since `max_past_epochs = 0` — is dropped last, and
//! only after the predecessor has been drained to its head.
//!
//! ## No downgrades, no stranding
//!
//! #454's acceptance criterion is that *a hybrid group can never contain a member
//! without a hybrid KeyPackage*. Migration honours it by aborting before it
//! creates anything if a single roster device cannot supply one: a mixed fleet is
//! a no-op that retries, never a partial move that locks the un-upgraded device
//! out of its own conversation.
//!
//! The roster is not the whole question, though, because a conversation's roster
//! grows. A group that migrates while a classic-only device still exists anywhere
//! in the fleet is one invite away from stranding that device just as completely
//! — reconcile can only skip it, and rebuilding the group classic to admit it is
//! the downgrade the suite routing exists to prevent. So migration takes the same
//! second gate `group_state::suite_for_new_group` does: the whole live fleet must
//! be hybrid-capable (#454 P5). One switch, one meaning — until the fleet
//! completes, everything stays classic; after it completes, new groups are born
//! hybrid and existing ones migrate here.

use std::collections::HashSet;
use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

use super::delivery::WelcomeOut;
use super::group_state::{
    create_mls_group_in_suite, fleet_is_fully_pq_capable, forget_local_mls_group_at,
    roster_is_fully_pq_capable,
};
use super::provider::{load_stored_group_at, with_suite_provider, CS_CLASSIC, CS_HYBRID};
use super::reconcile::{
    desired_roster_user_ids, publish_staged_commit, registered_devices, stage_reconcile_commit,
    PublishOutcome,
};

/// Most suite migrations to attempt in one sweep. A migration claims a
/// KeyPackage per roster device and publishes a full add-everyone hybrid commit,
/// so it is by some distance the most expensive thing the sweep can do — and a
/// device that has just updated may be newly eligible in every conversation at
/// once. Spreading them costs nothing: eligibility does not expire.
pub(super) const MAX_MIGRATIONS_PER_SWEEP: usize = 2;

/// Migrate `conversation_id` onto the hybrid suite if it is due and possible,
/// reporting whether a successor lineage actually opened.
///
/// `Ok(false)` is the overwhelmingly common answer and is never an error: already
/// hybrid, no local group, a roster that isn't fully PQ-capable yet, a lineage we
/// haven't finished draining, a KeyPackage we couldn't claim, or a lost race. All
/// of them mean "not now" and are retried by the next sweep.
pub(super) async fn migrate_to_hybrid_if_due(
    state: &Arc<AppState>,
    conversation_id: &str,
    actor_user_id: &str,
) -> Result<bool> {
    // 1. Cheapest gate first, and entirely local: is there a classic group here
    //    at all? Skips every already-hybrid conversation (which is the steady
    //    state after the fleet upgrades) without a single remote read.
    let generation = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else {
            return Ok(false);
        };
        let generation = super::generation::local_generation(db.conn(), conversation_id);
        match load_stored_group_at(db.conn(), conversation_id, generation) {
            Some(group) if group.ciphersuite() == CS_CLASSIC => generation,
            _ => return Ok(false),
        }
    };

    // 2. Would every roster device survive the move, and would every device that
    //    may be invited *later*? `pq_capable` is derived server-side from each
    //    device's published KeyPackage pool, so both are real capability tests
    //    rather than advertised intent. One classic-only device on the roster
    //    means this conversation stays classic (it could not follow us across —
    //    see the module docs); one classic-only device anywhere else in the live
    //    fleet means the same, because a hybrid group can never admit it and
    //    inviting it after the migration would strand it exactly as surely
    //    (#454 P5, the same gate `suite_for_new_group` applies to new groups).
    if !roster_is_fully_pq_capable(state, conversation_id).await?
        || !fleet_is_fully_pq_capable(state).await?
    {
        return Ok(false);
    }

    // 3. Drain the predecessor to its head BEFORE taking the lock, through the
    //    interleaved catch-up so every message sealed on the old lineage is
    //    ingested on the way (`max_past_epochs = 0` — once we retire the group,
    //    nothing can decrypt what we skipped). It re-acquires the per-conversation
    //    MLS lock itself, hence "before".
    crate::commands::messages::catch_up_mls_group_interleaved(
        state,
        conversation_id,
        actor_user_id,
    )
    .await?;

    // 4. From here on the lineage must not move under us. The same lock reconcile
    //    and self-update take, so no other commit can be staged from the epoch we
    //    are about to close.
    let mls_guard = state.mls_group_lock(conversation_id).await;

    // Re-read under the lock: the catch-up above may have adopted a successor
    // somebody else published while we were draining, in which case there is
    // nothing left to do.
    let local_epoch = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else {
            return Ok(false);
        };
        if super::generation::local_generation(db.conn(), conversation_id) != generation {
            return Ok(false);
        }
        match load_stored_group_at(db.conn(), conversation_id, generation) {
            Some(group) if group.ciphersuite() == CS_CLASSIC => group.epoch().as_u64() as i64,
            _ => return Ok(false),
        }
    };

    // 5. The head of the lineage we are closing. Our drained local epoch must
    //    equal it — a commit is stored at the epoch it was built FROM, so a group
    //    that has applied 0..H-1 sits at H, which is exactly the next epoch the
    //    DS would hand out. Any mismatch means the drain didn't reach head, and
    //    closing a lineage we haven't finished reading would orphan the tail.
    let head = head_epoch_in(state, conversation_id, generation).await?;
    if head != local_epoch {
        eprintln!(
            "[mls] migrate: {conversation_id} is at epoch {local_epoch} but generation \
             {generation} heads at {head} — deferring until drained"
        );
        return Ok(false);
    }

    let actor_device_id = state.device_id.lock().await.clone().unwrap_or_default();

    // 6. Who has to come across, and can they? Every registered roster device
    //    except our own needs a hybrid KeyPackage; a single miss aborts the whole
    //    migration BEFORE anything is created. This is the no-downgrade criterion
    //    in its strongest form — not "add whoever we can", but "move everyone or
    //    nobody".
    let remote_conn = state.remote_db.conn().await?;
    let roster_user_ids = desired_roster_user_ids(&remote_conn, conversation_id).await?;
    let valid_devices = registered_devices(&remote_conn, &roster_user_ids).await?;
    let targets: Vec<(String, String)> = valid_devices
        .iter()
        .filter(|(uid, did)| !(uid == actor_user_id && did == &actor_device_id))
        .cloned()
        .collect();

    let mut kp_tuples: Vec<(String, String, Vec<u8>)> = Vec::new();
    for (uid, did) in &targets {
        match super::ds_claim_key_package(state, uid, Some(did), CS_HYBRID).await? {
            Some(kp) => kp_tuples.push((uid.clone(), did.clone(), kp)),
            None => {
                eprintln!(
                    "[mls] migrate: {conversation_id} — no hybrid KeyPackage for {uid}:{did}; \
                     aborting so nobody is left behind on the classic lineage"
                );
                return Ok(false);
            }
        }
    }

    // 7. Build the successor locally. It lives under its own `GroupId`
    //    (`{conversation_id}#g{successor}`), so the predecessor is untouched and
    //    still serving reads; nothing this device does before the CAS resolves is
    //    observable to the conversation.
    let successor = generation + 1;
    let staged = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else {
            return Ok(false);
        };
        with_suite_provider!(db.conn(), CS_HYBRID, |provider| {
            stage_successor_commit(
                &provider,
                conversation_id,
                successor,
                actor_user_id,
                &actor_device_id,
                &kp_tuples,
                &roster_user_ids,
                &valid_devices,
            )
        })
    };
    // Every failure past the create must tear the successor down. A stored group
    // on a lineage that never opened is not merely wasted space: the
    // generation backstop in `process_pending_commits` treats a stored successor
    // as evidence of a migration, so leaving one behind could walk this device
    // off a perfectly healthy lineage onto one that does not exist.
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

    // 8. The compare-and-swap. `closes_epoch` names the predecessor head we
    //    observed, which is what makes opening the lineage conditional on the old
    //    one not having moved. `epoch_before` is 0 because the successor is a
    //    brand-new group — the DS accepts epoch 0 only on this branch.
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
            // Someone committed on the old lineage (or migrated it first) between
            // our read and our write. Our staged commit was rolled back; the
            // successor group itself is ours alone and now meaningless.
            eprintln!(
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

    // 9. We own generation `successor` at epoch 1 (the opening commit merged
    //    inside `publish_staged_commit`). Record the lineage FIRST, then drop the
    //    predecessor: a crash in between leaves a harmless orphan group, whereas
    //    the reverse order would leave this device pointing at a lineage it no
    //    longer has. Same ordering as `adopt_generation_locked`.
    {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            super::generation::set_local_generation(db.conn(), conversation_id, successor);
            // Unlike a device that arrives by Welcome, the migrator FOUNDED this
            // group — its leaf is merged by construction, so the new lineage
            // starts already rotated rather than immediately due (#666).
            super::self_update::record_self_update(db.conn(), conversation_id, 1);
        }
    }
    let _ = forget_local_mls_group_at(state, conversation_id, generation).await;

    eprintln!(
        "[mls] migrate: {conversation_id} moved to the hybrid suite — generation \
         {generation} (head {head}) closed, generation {successor} open at epoch 1"
    );

    // The exporter secret is per-group and we are now on a different group, so a
    // live voice room must re-derive immediately.
    crate::commands::voice_e2ee::on_mls_epoch_changed(state, conversation_id).await;
    drop(mls_guard);

    // Report our position on the NEW lineage so the retention floor follows us
    // across the boundary instead of pinning the retired one forever.
    super::ds_client::ds_report_commit_since(state, conversation_id, actor_user_id, successor, 1)
        .await;

    Ok(true)
}

/// Create the successor group and stage its opening commit, returning
/// `(welcomes, commit_bytes, group_info_bytes)` — or `None` when there is
/// nothing to commit at all.
///
/// Synchronous and suite-pinned to [`CS_HYBRID`]: the caller dispatches, because
/// the classic provider panics rather than fails on a hybrid group.
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
) -> Result<Option<(Vec<WelcomeOut>, Vec<u8>, Option<Vec<u8>>)>>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    create_mls_group_in_suite(
        provider,
        conversation_id,
        successor,
        actor_user_id,
        actor_device_id,
        CS_HYBRID,
    )?;

    // Reuse reconcile's staging verbatim: the successor is a fresh group
    // containing only us, so the diff against the desired roster IS the
    // add-everyone commit — with the same revoked-device filtering and the same
    // server-untrusted re-check that every claimed KeyPackage really is in this
    // group's suite.
    let staged = stage_reconcile_commit(
        provider,
        conversation_id,
        successor,
        kp_tuples,
        roster_user_ids,
        actor_user_id,
        actor_device_id,
        valid_devices,
    )?;

    match staged {
        Some((outcome, Some(data))) => {
            let welcomes = welcomes_for(&outcome.added, data.welcome_bytes.as_deref());
            Ok(Some((welcomes, data.commit_bytes, data.group_info_bytes)))
        }
        // A conversation whose roster resolves to this device alone has nobody to
        // add, so reconcile stages no commit — but the lineage still needs one at
        // epoch 0 for the compare-and-swap to have anything to write. A
        // self-update is the smallest commit that qualifies.
        _ => Ok(super::self_update::stage_self_update(provider, conversation_id, successor)?
            .map(|(_, commit, group_info)| (Vec::new(), commit, group_info))),
    }
}

/// Tear down a successor group that never opened its lineage.
///
/// Safe precisely because nothing else can reference it yet: the CAS did not
/// land, so no Welcome was delivered and no commit exists at generation
/// `successor`. Best-effort — a leftover group is inert (nothing routes to it
/// until the lineage row moves) and the next attempt deletes it on create.
async fn abandon_successor(state: &Arc<AppState>, conversation_id: &str, successor: i64) {
    if let Err(e) = forget_local_mls_group_at(state, conversation_id, successor).await {
        eprintln!("[mls] migrate: discarding successor {successor} for {conversation_id}: {e}");
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
    let conn = state.log_db.conn().await?;
    let mut rows = conn
        .query(
            "SELECT COALESCE(MAX(epoch), -1) + 1 FROM mls_commit_log \
             WHERE conversation_id = ?1 AND generation = ?2",
            libsql::params![conversation_id, generation],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<i64>(0)?),
        None => Ok(0),
    }
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
/// receivers can verify cross-signing certs before processing it. Mirrors
/// reconcile: one user id (the first) plus every added device id.
fn added_metadata(welcomes: &[WelcomeOut]) -> (Option<String>, Option<String>) {
    if welcomes.is_empty() {
        return (None, None);
    }
    let uid = welcomes[0].recipient_id.clone();
    let dids = welcomes
        .iter()
        .map(|w| w.recipient_device_id.as_str())
        .collect::<Vec<_>>()
        .join(",");
    (Some(uid), Some(dids))
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

    #[test]
    fn added_metadata_lists_every_device() {
        let (uid, dids) = added_metadata(&[welcome("u1", "d1"), welcome("u2", "d2")]);
        assert_eq!(uid.as_deref(), Some("u1"));
        assert_eq!(dids.as_deref(), Some("d1,d2"));
    }

    #[test]
    fn added_metadata_is_absent_when_nobody_was_added() {
        // The sole-device case again: a self-update opening commit adds nobody, so
        // the row's verification columns must stay NULL rather than naming a
        // device the commit never touched.
        let (uid, dids) = added_metadata(&[]);
        assert!(uid.is_none());
        assert!(dids.is_none());
    }
}
