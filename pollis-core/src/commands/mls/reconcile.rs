//! Declarative roster reconciliation for MLS groups: diff the desired roster
//! against the actual MLS tree and issue a single combined add/remove commit +
//! Welcome, plus the shared publish path every commit goes through.

use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::OpenMlsProvider;

use std::sync::Arc;
use tls_codec::{Deserialize as TlsDeserialize, Serialize as TlsSerialize};

use crate::state::AppState;

use super::device::{IdentityDirectory, LeafVerdict};
use super::generation::mls_group_id;
use super::provider::{
    load_stored_group, parse_credential_device_id, parse_credential_user_id, signature_scheme,
    store_only, MlsProvider, PollisProvider,
};

/// Result of the compare-and-swap commit submission in `reconcile_group_mls_impl`.
enum SubmitOutcome {
    /// Our commit claimed its epoch.
    Committed,
    /// Another member already committed this epoch; nothing was written.
    LostRace,
}

/// Is the byte-for-byte commit we submitted already canonical at `epoch` (#411)?
/// An ambiguous submit may have landed, so the log decides. Any read failure
/// returns `false` (roll back and converge).
pub(super) async fn our_commit_is_canonical(
    state: &Arc<AppState>,
    conversation_id: &str,
    generation: i64,
    epoch: i64,
    our_commit: &[u8],
) -> bool {
    let stored = fetch_commit_at_epoch(state, conversation_id, generation, epoch).await;
    matches!(
        crate::commands::mls::invariants::resolve(
            crate::commands::mls::invariants::SubmitOutcome::LostRace,
            our_commit,
            stored.as_deref(),
        ),
        crate::commands::mls::invariants::Resolution::Adopt
    )
}

/// The canonical commit bytes stored at `epoch`, or `None` if there is no such
/// row or the read fails. I/O only; the decision is `invariants::resolve`.
async fn fetch_commit_at_epoch(
    state: &Arc<AppState>,
    conversation_id: &str,
    generation: i64,
    epoch: i64,
) -> Option<Vec<u8>> {
    let snap = super::ds_reads::conversation_snapshot(
        state,
        pollis_api::reads::ConversationStateQuery {
            conversation_id: conversation_id.to_string(),
            generation: Some(generation),
            since: None,
            want_group_info_blob: false,
            commit_at_epoch: Some(epoch),
        },
    )
    .await
    .ok()?;
    // `None` on any failure maps to Rollback: adopting on a failed read would
    // leave the group at a phantom epoch.
    let bytes = snap.commit_at_epoch.as_deref()?;
    super::ds_reads::decode_b64("commit_data", bytes).ok()
}

/// Apply the side effects of a commit that won its epoch: merge the still-staged
/// pending commit to advance the local epoch. The DS already wrote the Welcomes
/// and GroupInfo with the commit.
async fn finalize_won_commit(
    state: &Arc<AppState>,
    conversation_id: &str,
    generation: i64,
) -> crate::error::Result<()> {
    // Scope the !Send provider/group so neither crosses an await.
    let guard = state.local_db.lock().await;
    if let Some(db) = guard.as_ref() {
        let provider = PollisProvider::new(db.conn());
        merge_pending_in_suite(&provider, conversation_id, generation)?;
    }
    Ok(())
}

fn merge_pending_in_suite<C>(
    provider: &MlsProvider<'_, C>,
    conversation_id: &str,
    generation: i64,
) -> crate::error::Result<()>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let group_id = mls_group_id(conversation_id, generation);
    let mut group = MlsGroup::load(provider.storage(), &group_id)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("mls load for merge: {e}")))?
        .ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("group missing at merge time"))
        })?;
    crate::commands::file_keys::keep_epoch_kek(provider, &group);
    group
        .merge_pending_commit(provider)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("reconcile merge: {e}")))?;
    Ok(())
}

/// What happened to a commit we staged locally and offered to the log.
pub(super) enum PublishOutcome {
    /// We own `epoch_before`; the pending commit has been merged locally.
    Won,
    /// Another member claimed `epoch_before` first. Nothing was written and our
    /// pending commit has been rolled back — the caller must converge.
    LostRace,
}

/// Offer a locally-staged commit to the canonical log, resolve the outcome, and
/// leave local state consistent either way. The single publish path for every
/// commit (reconcile, self-update, migration), so the CAS resolution cannot drift.
///
/// `LostRace` or a transport error is ambiguous and resolved against the log
/// ([`our_commit_is_canonical`], #411). On a win the pending commit is merged; on
/// a loss or failure it is cleared, so the local group never sits ahead of the
/// log. The caller owns convergence after a `LostRace`.
///
/// `generation` is the commit's suite lineage (#454 P4); `closes_epoch` is set
/// only by a migration opening the next one. `what` names the caller in logs.
#[allow(clippy::too_many_arguments)]
pub(super) async fn publish_staged_commit(
    state: &Arc<AppState>,
    conversation_id: &str,
    generation: i64,
    closes_epoch: Option<i64>,
    actor_user_id: &str,
    epoch_before: u64,
    commit_bytes: &[u8],
    group_info_bytes: Option<&[u8]>,
    added_uid: Option<&str>,
    added_dids: Option<&str>,
    welcomes: &[super::delivery::WelcomeOut],
    what: &str,
) -> crate::error::Result<PublishOutcome> {
    let epoch = epoch_before as i64;

    // On a win the seam also writes the GroupInfo + Welcomes atomically with the
    // commit, so no Welcome can ever point at a doomed branch.
    let remote_result: crate::error::Result<SubmitOutcome> = async {
        match super::delivery::submit_commit(
            state,
            conversation_id,
            generation,
            epoch,
            closes_epoch,
            actor_user_id,
            commit_bytes,
            added_uid,
            added_dids,
            group_info_bytes,
            welcomes,
        )
        .await?
        {
            super::delivery::SubmitResult::LostRace => Ok(SubmitOutcome::LostRace),
            super::delivery::SubmitResult::Committed => Ok(SubmitOutcome::Committed),
        }
    }
    .await;

    enum Resolution {
        Won,
        LostRace,
        Failed(crate::error::Error),
    }
    let resolution = match remote_result {
        Ok(SubmitOutcome::Committed) => Resolution::Won,
        Ok(SubmitOutcome::LostRace) => {
            if our_commit_is_canonical(state, conversation_id, generation, epoch, commit_bytes).await
            {
                crate::diag!(
                    "[mls] {what}: LostRace at epoch {epoch} for {conversation_id} but our commit is canonical — adopting (lost success-response)"
                );
                Resolution::Won
            } else {
                Resolution::LostRace
            }
        }
        Err(e) => {
            if our_commit_is_canonical(state, conversation_id, generation, epoch, commit_bytes).await
            {
                crate::diag!(
                    "[mls] {what}: submit errored but our commit is canonical at epoch {epoch} for {conversation_id} — adopting (lost response): {e}"
                );
                Resolution::Won
            } else {
                Resolution::Failed(e)
            }
        }
    };

    match resolution {
        Resolution::Won => {
            // #1041: the DS now refuses envelopes at the epoch this commit
            // closes; whatever was sealed there is readable only until we merge.
            let swept = sweep_before_merge(
                state,
                conversation_id,
                actor_user_id,
                closes_epoch.is_some(),
                what,
            )
            .await?;
            finalize_won_commit(state, conversation_id, generation).await?;
            if let Some(swept) = swept {
                swept.emit_receipts(state).await;
            }
            Ok(PublishOutcome::Won)
        }
        Resolution::LostRace => {
            crate::diag!(
                "[mls] {what}: lost epoch {epoch} race for {conversation_id} — rolling back local pending commit and converging on the winner"
            );
            clear_pending_best_effort(state, conversation_id, generation).await;
            Ok(PublishOutcome::LostRace)
        }
        Resolution::Failed(e) => {
            crate::diag!(
                "[mls] {what}: remote persist failed for {conversation_id}, clearing local pending commit: {e}"
            );
            clear_pending_best_effort(state, conversation_id, generation).await;
            Err(e)
        }
    }
}

/// The committer's sweep of the epoch a won commit is about to leave (#1041).
///
/// Returns the persisted envelopes (receipts owed after the merge). If the sweep
/// fails after one retry:
///
/// * an ordinary commit returns `Err` and stays STAGED (it has landed); the next
///   ingesting catch-up drains its epoch, then adopts it from the log.
/// * a migration returns `Ok(None)` and merges anyway: abandoning a successor the
///   log already opened would strand every member.
async fn sweep_before_merge(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
    is_migration: bool,
    what: &str,
) -> crate::error::Result<Option<crate::commands::messages::SweptEnvelopes>> {
    let mut last_err = None;
    for _attempt in 0..2 {
        match crate::commands::messages::sweep_current_epoch(state, conversation_id, user_id).await
        {
            Ok(swept) => return Ok(Some(swept)),
            Err(e) => {
                crate::diag!(
                    "[mls] {what}: pre-merge envelope sweep for {conversation_id} failed: {e}"
                );
                last_err = Some(e);
            }
        }
    }
    let e = last_err.expect("two attempts, both failed");
    if is_migration {
        crate::diag!(
            "[mls] {what}: merging the migration of {conversation_id} without its pre-merge \
             sweep — envelopes sealed at the closed epoch since the last catch-up may be lost \
             to this device"
        );
        return Ok(None);
    }
    crate::diag!(
        "[mls] {what}: leaving the won commit for {conversation_id} staged — the next catch-up \
         drains its epoch and adopts it from the log"
    );
    Err(e)
}

/// Roll back a staged pending commit that genuinely did not land (best effort).
async fn clear_pending_best_effort(state: &Arc<AppState>, conversation_id: &str, generation: i64) {
    let guard = state.local_db.lock().await;
    if let Some(db) = guard.as_ref() {
        let store = store_only(db.conn());
        let group_id = mls_group_id(conversation_id, generation);
        match MlsGroup::load(&store, &group_id) {
            Ok(Some(mut group)) => {
                if let Err(e) = group.clear_pending_commit(&store) {
                    crate::diag!("[mls] reconcile: clear_pending_commit failed: {e}");
                }
            }
            Ok(None) => crate::diag!("[mls] reconcile: group vanished during rollback"),
            Err(e) => crate::diag!("[mls] reconcile: group load failed during rollback: {e}"),
        }
    }
}

/// Outcome of a single reconcile pass.
#[derive(Debug, Default)]
pub struct ReconcileOutcome {
    /// `(user_id, device_id)` pairs added to the MLS tree.
    pub added: Vec<(String, String)>,
    /// `(user_id, device_id)` pairs removed from the MLS tree.
    pub removed: Vec<(String, String)>,
    pub epoch_before: u64,
    pub epoch_after: u64,
    /// True if the committer's own leaf was in `to_remove` and was skipped.
    pub skipped_self_removal: bool,
    /// `(user_id, device_id)` pairs on the roster with no unclaimed KeyPackage in
    /// the group's suite (#1412). Never downgraded in, and not recorded: the sweep
    /// re-derives them every pass.
    pub skipped_no_suite_kp: Vec<(String, String)>,
    /// `(user_id, device_id, reason)` for every claimed KeyPackage REFUSED because
    /// its leaf is not (or cannot be shown to be) the key the user's
    /// `account_id_pub` certified. The package is burnt; the device is
    /// reconsidered next pass.
    pub refused_uncertified: Vec<(String, String, String)>,
}

/// Raw bytes produced by a reconcile commit.
pub struct ReconcileCommitData {
    pub commit_bytes: Vec<u8>,
    pub welcome_bytes: Option<Vec<u8>>,
    /// TLS-serialized GroupInfo at the *resulting* epoch, written atomically with
    /// the commit + Welcome by the DS.
    pub group_info_bytes: Option<Vec<u8>>,
}

/// #1082: a `registered_devices` snapshot that omits the actor itself cannot be
/// trusted to decide who gets evicted.
///
/// The actor's own `(user_id, device_id)` MUST appear in any correct snapshot;
/// one without it is indistinguishable from "every device was revoked" and would
/// evict the whole tree. Refusing leaves the tree as it was.
///
/// Two states do NOT gate: an empty roster (legitimate teardown), and an unknown
/// `actor_device_id` ("cannot tell" must not become "abort").
pub(super) fn snapshot_includes_actor(
    roster_user_ids: &std::collections::HashSet<String>,
    valid_devices: &std::collections::HashSet<(String, String)>,
    actor_user_id: &str,
    actor_device_id: &str,
) -> bool {
    if roster_user_ids.is_empty() || actor_device_id.is_empty() {
        return true;
    }
    valid_devices.contains(&(actor_user_id.to_string(), actor_device_id.to_string()))
}

/// The devices that *should* hold a leaf, as pure set algebra: those with an
/// unclaimed KeyPackage (additions) plus tree members whose user is on the
/// roster (retentions, which keep the committer's own KeyPackage-less leaf).
///
/// BOTH sources are gated on `valid_devices`: a revoked device keeps its
/// published KeyPackage, so gating only retentions re-admits it (#679). `None`
/// disables the gate and is deliberately distinct from an empty set ("no device
/// is valid").
pub(super) fn desired_set<'a>(
    kp_keys: &[(String, String)],
    tree_members: impl Iterator<Item = &'a (String, String)>,
    roster_user_ids: &std::collections::HashSet<String>,
    valid_devices: Option<&std::collections::HashSet<(String, String)>>,
) -> std::collections::HashSet<(String, String)> {
    let is_valid = |key: &(String, String)| valid_devices.is_none_or(|v| v.contains(key));

    let mut desired: std::collections::HashSet<(String, String)> =
        kp_keys.iter().filter(|k| is_valid(k)).cloned().collect();

    for key in tree_members {
        if roster_user_ids.contains(&key.0) && is_valid(key) {
            desired.insert(key.clone());
        }
    }
    desired
}

/// Sync core (staged variant): computes the diff between the desired roster and
/// the actual MLS tree and stages a single combined commit, WITHOUT merging it.
/// The caller merges after the log accepts it, or clears it on failure.
/// `epoch_after` reports the epoch the commit will produce.
///
/// `identities`: a leaf already in the tree that is positively `Uncertified` is
/// evicted like a revoked device, which is how a grafted rogue leaf leaves the
/// tree. `Unverifiable` leaves are retained; `None` disables the gate.
#[allow(clippy::too_many_arguments)]
pub fn reconcile_group_mls_core_staged<C>(
    provider: &MlsProvider<'_, C>,
    signer: &SignatureKeyPair,
    group: &mut MlsGroup,
    roster_user_ids: &std::collections::HashSet<String>,
    available_kps: &[(String, String, KeyPackage)],
    actor_user_id: &str,
    actor_device_id: &str,
    valid_devices: Option<&std::collections::HashSet<(String, String)>>,
    identities: Option<&IdentityDirectory>,
) -> crate::error::Result<(ReconcileOutcome, Option<ReconcileCommitData>)>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    use std::collections::{HashMap, HashSet};

    let epoch_before = group.epoch().as_u64();
    let scheme = signature_scheme(group.ciphersuite());

    // 1. Actual state, ONE ENTRY PER LEAF: openmls allows duplicate credentials,
    //    and keying on the credential let a shadowed duplicate survive every
    //    reconcile (#1161 M7).
    //
    //    A positively uncertified leaf is kept OUT of `actual` and queued for
    //    removal, so the device's genuine KeyPackage can be added in the same
    //    commit. Our own leaf (by INDEX, never by credential, or a duplicate of
    //    our credential would be unevictable) is never judged.
    let own_index = group.own_leaf_index();
    let mut leaves: Vec<((String, String), LeafNodeIndex, Option<String>)> = Vec::new();
    for m in group.members() {
        let uid = parse_credential_user_id(&m.credential);
        let did = parse_credential_device_id(&m.credential).unwrap_or_default();
        let verdict = if m.index == own_index {
            None
        } else {
            match identities
                .map(|dir| dir.leaf_verdict(&uid, &did, m.signature_key.as_slice(), scheme))
            {
                Some(LeafVerdict::Uncertified(reason)) => Some(reason),
                _ => None,
            }
        };
        leaves.push(((uid, did), m.index, verdict));
    }

    // The actor's credential against the leaf it occupies; they disagree only
    // if the stored group belongs to another device. Every self-exemption below
    // keys on the index.
    if !actor_device_id.is_empty() {
        if let Some((key, _, _)) = leaves.iter().find(|(_, index, _)| *index == own_index) {
            if key.0 != actor_user_id || key.1 != actor_device_id {
                crate::diag!(
                    "[mls] reconcile: our own leaf {own_index:?} carries {}:{} but we are acting \
                     as {actor_user_id}:{actor_device_id}",
                    key.0, key.1
                );
            }
        }
    }

    let mut actual: HashMap<(String, String), LeafNodeIndex> = HashMap::new();
    let mut to_evict: Vec<((String, String), LeafNodeIndex, String)> = Vec::new();
    for (key, index, verdict) in leaves {
        if let Some(reason) = verdict {
            to_evict.push((key, index, reason));
            continue;
        }
        // The survivor for a `(user, device)` several leaves claim: our own leaf
        // always wins, otherwise the first in tree order (openmls fills the
        // leftmost free slot, so a later graft sits to the right).
        match actual.entry(key.clone()) {
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(index);
            }
            std::collections::hash_map::Entry::Occupied(mut slot) => {
                let (keep, drop) = if index == own_index {
                    (index, *slot.get())
                } else {
                    (*slot.get(), index)
                };
                slot.insert(keep);
                to_evict.push((
                    key,
                    drop,
                    format!("a second leaf claiming one credential; keeping {keep:?}"),
                ));
            }
        }
    }

    // 2. Build the desired set.
    let kp_keys: Vec<(String, String)> = available_kps
        .iter()
        .map(|(uid, did, _)| (uid.clone(), did.clone()))
        .collect();
    let desired = desired_set(&kp_keys, actual.keys(), roster_user_ids, valid_devices);

    // 3. Diff.
    let actual_keys: HashSet<(String, String)> = actual.keys().cloned().collect();

    // Leaves in tree but not desired → remove, plus every leaf queued for eviction.
    let mut to_remove: Vec<((String, String), LeafNodeIndex)> = actual
        .iter()
        .filter(|(key, _)| !desired.contains(key))
        .map(|(key, &idx)| (key.clone(), idx))
        .collect();
    for (key, idx, reason) in &to_evict {
        crate::diag!(
            "[mls] reconcile: evicting leaf {:?} for {}:{} — {reason}",
            idx, key.0, key.1
        );
        to_remove.push((key.clone(), *idx));
    }

    // Devices desired but not in tree → add
    let to_add_keys: HashSet<(String, String)> = desired
        .difference(&actual_keys)
        .cloned()
        .collect();

    // 4. Committer-in-remove-set detection, on the INDEX (#1161 M7): MLS forbids
    //    removing our own leaf.
    let mut skipped_self_removal = false;
    if to_remove.iter().any(|(_, idx)| *idx == own_index) {
        to_remove.retain(|(_, idx)| *idx != own_index);
        skipped_self_removal = true;
    }

    let add_kps: Vec<(String, String, KeyPackage)> = available_kps
        .iter()
        .filter(|(uid, did, _)| to_add_keys.contains(&(uid.clone(), did.clone())))
        .cloned()
        .collect();

    let remove_indices: Vec<LeafNodeIndex> = to_remove.iter().map(|(_, idx)| *idx).collect();

    // 5. No-op check.
    if remove_indices.is_empty() && add_kps.is_empty() {
        return Ok((
            ReconcileOutcome {
                epoch_before,
                epoch_after: epoch_before,
                skipped_self_removal,
                ..Default::default()
            },
            None,
        ));
    }

    let removed_desc: Vec<String> = to_remove.iter().map(|((u, d), _)| format!("{u}:{d}")).collect();
    let added_desc: Vec<String> = add_kps.iter().map(|(u, d, _)| format!("{u}:{d}")).collect();
    crate::diag!(
        "[mls] reconcile: removing [{}], adding [{}]",
        removed_desc.join(", "),
        added_desc.join(", "),
    );

    // 7. Build a single commit with both proposals. `stage_commit` writes it as a
    //    pending commit but does NOT advance the epoch.
    let add_kps_only: Vec<KeyPackage> = add_kps.iter().map(|(_, _, kp)| kp.clone()).collect();

    let bundle = group
        .commit_builder()
        .propose_removals(remove_indices.iter().copied())
        .propose_adds(add_kps_only)
        .load_psks(provider.storage())
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("reconcile load_psks: {e}")))?
        .create_group_info(true)
        .build(provider.rand(), provider.crypto(), signer, |_| true)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("reconcile build: {e}")))?
        .stage_commit(provider)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("reconcile stage: {e}")))?;

    // 8. Serialize commit + welcome + GroupInfo from the bundle, pre-merge.
    let (commit_out, welcome_opt, group_info_opt) = bundle.into_messages();

    let commit_bytes = commit_out
        .tls_serialize_detached()
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("reconcile commit serialize: {e}")))?;

    let welcome_bytes = match welcome_opt {
        Some(w) => Some(
            w.tls_serialize_detached()
                .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("reconcile welcome serialize: {e}")))?,
        ),
        None => None,
    };

    let group_info_bytes = match group_info_opt {
        Some(gi) => Some(
            gi.tls_serialize_detached()
                .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("reconcile group_info serialize: {e}")))?,
        ),
        None => None,
    };

    // A staged commit always advances the epoch by exactly one.
    let epoch_after = epoch_before + 1;

    let removed: Vec<(String, String)> = to_remove.into_iter().map(|(key, _)| key).collect();
    let added: Vec<(String, String)> = add_kps.into_iter().map(|(u, d, _)| (u, d)).collect();

    crate::diag!(
        "[mls] reconcile: staged epoch {epoch_before} → {epoch_after}, removed {}, added {} (pending merge)",
        removed.len(),
        added.len(),
    );

    Ok((
        ReconcileOutcome {
            added,
            removed,
            epoch_before,
            epoch_after,
            skipped_self_removal,
            ..Default::default()
        },
        Some(ReconcileCommitData {
            commit_bytes,
            welcome_bytes,
            group_info_bytes,
        }),
    ))
}

/// Sync core that also merges the staged commit locally. Async callers should
/// use [`reconcile_group_mls_core_staged`] so the log accepts the commit before
/// the local merge; this exists for tests and local-only paths.
#[allow(clippy::too_many_arguments)]
pub fn reconcile_group_mls_core<C>(
    provider: &MlsProvider<'_, C>,
    signer: &SignatureKeyPair,
    group: &mut MlsGroup,
    roster_user_ids: &std::collections::HashSet<String>,
    available_kps: &[(String, String, KeyPackage)],
    actor_user_id: &str,
    actor_device_id: &str,
    valid_devices: Option<&std::collections::HashSet<(String, String)>>,
    identities: Option<&IdentityDirectory>,
) -> crate::error::Result<(ReconcileOutcome, Option<ReconcileCommitData>)>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let (mut outcome, commit_data_opt) = reconcile_group_mls_core_staged(
        provider,
        signer,
        group,
        roster_user_ids,
        available_kps,
        actor_user_id,
        actor_device_id,
        valid_devices,
        identities,
    )?;

    if commit_data_opt.is_some() {
        crate::commands::file_keys::keep_epoch_kek(provider, group);
        group
            .merge_pending_commit(provider)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("reconcile merge: {e}")))?;
        outcome.epoch_after = group.epoch().as_u64();
    }

    Ok((outcome, commit_data_opt))
}

/// Load the group, read its signer, validate the claimed KeyPackages and stage
/// the reconcile commit: the crypto-bearing half of [`reconcile_group_mls_impl`].
/// `Ok(None)` means the local group is gone.
///
/// `pub(super)` because migration (#454 P4) reuses it against a fresh successor
/// at `generation`, so the KeyPackage validation exists once.
///
/// Every claimed KeyPackage must be [`LeafVerdict::Certified`] against
/// `identities` before it becomes an Add: the DS is untrusted, and
/// `KeyPackageIn::validate` proves only that *someone* holds the leaf key.
#[allow(clippy::too_many_arguments)]
pub(super) fn stage_reconcile_commit<C>(
    provider: &MlsProvider<'_, C>,
    conversation_id: &str,
    generation: i64,
    kp_tuples: &[(String, String, Vec<u8>)],
    roster_user_ids: &std::collections::HashSet<String>,
    actor_user_id: &str,
    actor_device_id: &str,
    valid_devices: &std::collections::HashSet<(String, String)>,
    identities: &IdentityDirectory,
) -> crate::error::Result<Option<(ReconcileOutcome, Option<ReconcileCommitData>)>>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let group_id = mls_group_id(conversation_id, generation);
    let group_opt = MlsGroup::load(provider.storage(), &group_id)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("mls load: {e}")))?;
    let mut group = match group_opt {
        Some(g) => g,
        None => {
            return Ok(None);
        }
    };

    let sig_pub_bytes = group
        .own_leaf_node()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("no own leaf node")))?
        .signature_key()
        .as_slice()
        .to_vec();
    let signer = SignatureKeyPair::read(
        provider.storage(),
        &sig_pub_bytes,
        signature_scheme(group.ciphersuite()),
    )
    .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("signer not found in mls_kv")))?;

    // Resolve a dangling pending commit.
    crate::commands::file_keys::keep_epoch_kek(provider, &group);
    group
        .merge_pending_commit(provider)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("merge pending: {e}")))?;

    let mut available_kps: Vec<(String, String, KeyPackage)> = Vec::new();
    let mut refused: Vec<(String, String, String)> = Vec::new();
    for (uid, did, kp_raw) in kp_tuples {
        let mut reader: &[u8] = kp_raw;
        let kp_in = match KeyPackageIn::tls_deserialize(&mut reader) {
            Ok(k) => k,
            Err(e) => {
                crate::diag!("[mls] reconcile: kp deserialize failed for {uid}:{did}: {e}");
                continue;
            }
        };
        let kp = match kp_in.validate(provider.crypto(), ProtocolVersion::Mls10) {
            Ok(k) => k,
            Err(e) => {
                crate::diag!("[mls] reconcile: kp validate failed for {uid}:{did}: {e}");
                continue;
            }
        };
        // The DS is untrusted: re-check the suite, or a cross-suite KeyPackage
        // would be a server-driven downgrade of the group.
        if kp.ciphersuite() != group.ciphersuite() {
            crate::diag!(
                "[mls] reconcile: kp suite {:?} != group suite {:?} for {uid}:{did} — refusing",
                kp.ciphersuite(),
                group.ciphersuite(),
            );
            continue;
        }
        let cred_user = parse_credential_user_id(kp.leaf_node().credential());
        if cred_user != *uid {
            crate::diag!("[mls] reconcile: credential user '{cred_user}' != '{uid}' for device {did}");
            continue;
        }
        let cred_device = parse_credential_device_id(kp.leaf_node().credential()).unwrap_or_default();
        if cred_device != *did {
            crate::diag!(
                "[mls] reconcile: credential device '{cred_device}' != '{did}' for user {uid} — refusing"
            );
            continue;
        }
        // Cross-signing, committer side: anything short of `Certified` is a
        // refusal; `Unverifiable` is no basis for handing out the key schedule.
        let verdict = identities.leaf_verdict(
            uid,
            did,
            kp.leaf_node().signature_key().as_slice(),
            signature_scheme(kp.ciphersuite()),
        );
        let reason = match verdict {
            LeafVerdict::Certified => None,
            LeafVerdict::Unverifiable(why) => Some(why.to_string()),
            LeafVerdict::Uncertified(why) => Some(why),
        };
        if let Some(reason) = reason {
            // Test-harness only: play a legacy / malicious committer so the
            // INBOUND check can be exercised against a real rogue leaf.
            #[cfg(feature = "test-harness")]
            if committer_leaf_check_skipped() {
                crate::diag!(
                    "[mls] reconcile: TEST HOOK — adding uncertified leaf {uid}:{did} ({reason})"
                );
                available_kps.push((uid.clone(), did.clone(), kp));
                continue;
            }
            crate::diag!(
                "[mls] reconcile: REFUSING to add {uid}:{did} — leaf not cross-signed by the \
                 claimed account: {reason}"
            );
            refused.push((uid.clone(), did.clone(), reason));
            continue;
        }
        available_kps.push((uid.clone(), did.clone(), kp));
    }

    // Stage as a PENDING commit; the merge waits for the log.
    let (mut outcome, data) = reconcile_group_mls_core_staged(
        provider,
        &signer,
        &mut group,
        roster_user_ids,
        &available_kps,
        actor_user_id,
        actor_device_id,
        Some(valid_devices),
        Some(identities),
    )?;
    outcome.refused_uncertified = refused;
    Ok(Some((outcome, data)))
}

/// The roster's cross-signing material, with the account keys pinned locally
/// (see [`pin_identities`]).
pub(super) async fn load_pinned_identities(
    state: &Arc<AppState>,
    roster_user_ids: &[String],
    actor_user_id: &str,
) -> crate::error::Result<IdentityDirectory> {
    let rows = crate::commands::ds_reads::roster_identities(state, roster_user_ids).await?;
    pin_identities(state, IdentityDirectory::new(rows), actor_user_id).await
}

/// The two local substitutions over a directory built from ANY source of DS rows
/// (#1161 M3): our own account key comes from the keystore, and a peer whose
/// reported key differs from the TOFU pin (`contact_verification`) has it
/// DROPPED. Both only ever weaken a verdict, so they are safe on live paths.
pub(super) async fn pin_identities(
    state: &Arc<AppState>,
    mut dir: IdentityDirectory,
    actor_user_id: &str,
) -> crate::error::Result<IdentityDirectory> {
    // Our own root of trust is local. On failure this device holds no account
    // identity, so it could not have certified a device either.
    if let Ok(key) = crate::commands::account_identity::load_account_id_key(state, actor_user_id).await {
        use ml_dsa::Keypair as _;
        dir.set_account_key(actor_user_id, Some(key.verifying_key().encode().to_vec()));
    }

    let pins: Vec<(String, Vec<u8>)> = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let mut stmt = db
            .conn()
            .prepare("SELECT peer_user_id, account_id_pub FROM contact_verification")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?)))?
            .collect::<std::result::Result<Vec<_>, rusqlite::Error>>()?;
        rows
    };
    for (peer, pinned) in pins {
        if peer == actor_user_id {
            continue;
        }
        match dir.account_key(&peer) {
            Some(reported) if reported == pinned => {}
            Some(_) => {
                crate::diag!(
                    "[mls] reconcile: account_id_pub for {peer} differs from the local pin — \
                     treating every leaf of theirs as unverifiable this pass"
                );
                dir.set_account_key(&peer, None);
            }
            None => {}
        }
    }
    Ok(dir.into_pinned())
}

/// Test-harness only: when set, `stage_reconcile_commit` adds a KeyPackage whose
/// leaf FAILS the cross-signing check, playing a malicious or pre-fix committer.
#[cfg(feature = "test-harness")]
static SKIP_COMMITTER_LEAF_CHECK: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(feature = "test-harness")]
fn committer_leaf_check_skipped() -> bool {
    SKIP_COMMITTER_LEAF_CHECK.load(std::sync::atomic::Ordering::Relaxed)
}

/// Test-harness only — see [`SKIP_COMMITTER_LEAF_CHECK`].
#[cfg(feature = "test-harness")]
pub fn set_skip_committer_leaf_check(skip: bool) {
    SKIP_COMMITTER_LEAF_CHECK.store(skip, std::sync::atomic::Ordering::Relaxed);
}

/// Test-harness only: this device's local epoch for `conversation_id`, and
/// whether it holds a staged commit, so a test can assert an operation did NOT
/// advance the epoch (#1079). `None` when there is no local group.
#[cfg(feature = "test-harness")]
pub async fn local_epoch_and_pending(
    state: &Arc<AppState>,
    conversation_id: &str,
) -> Option<(u64, bool)> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref()?;
    let group = load_stored_group(db.conn(), conversation_id)?;
    Some((group.epoch().as_u64(), group.pending_commit().is_some()))
}

/// Test-harness only: the `(user_id, device_id)` leaves of this device's local
/// tree for `conversation_id`.
#[cfg(feature = "test-harness")]
pub async fn local_tree_members(
    state: &Arc<AppState>,
    conversation_id: &str,
) -> Vec<(String, String)> {
    let guard = state.local_db.lock().await;
    let Some(db) = guard.as_ref() else {
        return Vec::new();
    };
    match load_stored_group(db.conn(), conversation_id) {
        Some(group) => group
            .members()
            .map(|m| {
                (
                    parse_credential_user_id(&m.credential),
                    parse_credential_device_id(&m.credential).unwrap_or_default(),
                )
            })
            .collect(),
        None => Vec::new(),
    }
}

/// Async entry point: diff the roster against the local MLS tree and publish the
/// reconcile commit.
pub async fn reconcile_group_mls_impl(
    state: &Arc<AppState>,
    conversation_id: &str,
    actor_user_id: &str,
) -> crate::error::Result<ReconcileOutcome> {
    let conversation_id = conversation_id.to_owned();
    let actor_user_id = actor_user_id.to_owned();

    // Test-harness only: park a reconcile to line it up against a join (#1041).
    #[cfg(feature = "test-harness")]
    super::group_state::rendezvous::park(super::group_state::rendezvous::Point::ReconcileEntry)
        .await;

    // Serialize all MLS mutations for this conversation on this device. Held for
    // the whole function EXCEPT the lost-race converge, which re-acquires it.
    let _mls_guard = state.mls_group_lock(&conversation_id).await;

    // 1. Roster: the server-side definition `migrate` also uses (#987).
    let roster_user_ids: std::collections::HashSet<String> =
        crate::commands::ds_reads::catch_up_full(state, &conversation_id, false, false, true)
            .await?
            .roster
            .into_iter()
            .collect();

    // 1b. TOFU-pin every peer's account_id_pub before server-reported keys can
    //     add devices to the tree. Non-fatal: a transient failure must not block
    //     a legitimate membership update.
    {
        let peers: Vec<String> = roster_user_ids
            .iter()
            .filter(|id| id.as_str() != actor_user_id.as_str())
            .cloned()
            .collect();
        if let Err(e) = crate::commands::safety::batch_check_and_pin_account_keys(
            state, &peers,
        )
        .await
        {
            crate::diag!("[reconcile] batch_check_and_pin_account_keys failed: {e}");
        }
    }

    let ids: Vec<String> = roster_user_ids.iter().cloned().collect();

    // 2. Devices with unclaimed KPs, excluding revoked devices (which keep their
    //    KeyPackages) so we never claim and burn one. All-or-nothing: a truncated
    //    answer would silently commit without those devices.
    let device_pairs: Vec<(String, String)> =
        crate::commands::ds_reads::claimable_devices(state, &ids, None).await?;

    // 2b. Every (user_id, device_id) still registered for the roster, so a
    //     revoked device of a present user is evicted.
    let valid_devices = crate::commands::ds_reads::registered_devices(state, &ids).await?;

    // 2c. The roster's pinned cross-signing material. An error, never a partial
    //     answer: this read decides who is admitted.
    let identities = load_pinned_identities(state, &ids, &actor_user_id).await?;

    let actor_device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .unwrap_or_default();

    // 2d. #1082: refuse the pass if the eviction snapshot omits us.
    if !snapshot_includes_actor(&roster_user_ids, &valid_devices, &actor_user_id, &actor_device_id) {
        crate::diag!(
            "[reconcile] refusing pass for {conversation_id}: registered_devices snapshot \
             ({} device(s)) omits this device — not evicting a {}-member roster on it",
            valid_devices.len(),
            roster_user_ids.len()
        );
        return Ok(ReconcileOutcome::default());
    }

    // 3. Peek at the tree: skip claiming KPs for existing members, and learn the
    //    group's suite. The generation is pinned for the pass, since migration
    //    takes the same MLS lock we hold.
    let (already_in_tree, group_suite, generation): (
        std::collections::HashSet<(String, String)>,
        Ciphersuite,
        i64,
    ) = {
        let guard = state.local_db.lock().await;
        let db = match guard.as_ref() {
            Some(db) => db,
            None => {
                return Ok(ReconcileOutcome::default());
            }
        };
        let generation = super::generation::local_generation(db.conn(), &conversation_id);
        match load_stored_group(db.conn(), &conversation_id) {
            Some(group) => {
                let suite = group.ciphersuite();
                let members = group
                    .members()
                    .map(|m| {
                        let uid = parse_credential_user_id(&m.credential);
                        let did = parse_credential_device_id(&m.credential).unwrap_or_default();
                        (uid, did)
                    })
                    .collect();
                (members, suite, generation)
            }
            None => {
                return Ok(ReconcileOutcome::default());
            }
        }
    };

    let devices_to_claim: Vec<(String, String)> = device_pairs
        .into_iter()
        .filter(|pair| !already_in_tree.contains(pair))
        .collect();

    // 4. Claim one KP per device to add, in the GROUP's suite, never a fixed one:
    //    a device with no KeyPackage there is skipped, never downgraded in.
    let mut kp_tuples: Vec<(String, String, Vec<u8>)> = Vec::new();
    let mut skipped_no_suite_kp: Vec<(String, String)> = Vec::new();
    for (uid, did) in &devices_to_claim {
        let claimed: Option<Vec<u8>> =
            crate::commands::mls::ds_claim_key_package(state, uid, Some(did), group_suite)
                .await?;
        match claimed {
            Some(kp) => kp_tuples.push((uid.clone(), did.clone(), kp)),
            None => {
                crate::diag!(
                    "[mls] reconcile: no KeyPackage in suite 0x{:04x} for {uid}:{did} — \
                     skipping (never downgrading); a later sweep re-adds it once its pool \
                     refills, or it external-joins on its next catch-up (#1412)",
                    u16::from(group_suite),
                );
                skipped_no_suite_kp.push((uid.clone(), did.clone()));
            }
        }
    }

    // 5. Validate KPs and stage the commit in the group's own suite.
    let staged = {
        let guard = state.local_db.lock().await;
        let db = match guard.as_ref() {
            Some(db) => db,
            None => {
                return Ok(ReconcileOutcome::default());
            }
        };
        let provider = PollisProvider::new(db.conn());
        stage_reconcile_commit(
            &provider,
            &conversation_id,
            generation,
            &kp_tuples,
            &roster_user_ids,
            &actor_user_id,
            &actor_device_id,
            &valid_devices,
            &identities,
        )?
    };
    let (mut outcome, commit_data_opt) = match staged {
        Some(v) => v,
        None => {
            return Ok(ReconcileOutcome::default());
        }
    };
    outcome.skipped_no_suite_kp = skipped_no_suite_kp;

    // 6. Publish to the log FIRST, merge locally only on a win.
    if let Some(data) = commit_data_opt {
        // A PREFETCH HINT naming every distinct added user and device (CSV);
        // receivers verify the commit itself, so an incomplete hint only makes an
        // honest add look unverifiable.
        let (added_uid, added_dids): (Option<String>, Option<String>) = if outcome.added.is_empty() {
            (None, None)
        } else {
            let mut uids: Vec<&str> = Vec::new();
            for (u, _) in &outcome.added {
                if !uids.contains(&u.as_str()) {
                    uids.push(u.as_str());
                }
            }
            let dids = outcome
                .added
                .iter()
                .map(|(_, d)| d.as_str())
                .collect::<Vec<_>>()
                .join(",");
            (Some(uids.join(",")), Some(dids))
        };

        // One Welcome blob per added recipient; the delivery seam writes them with
        // the commit and GroupInfo.
        let welcomes: Vec<super::delivery::WelcomeOut> = match data.welcome_bytes.as_ref() {
            Some(welcome_bytes) => outcome
                .added
                .iter()
                .map(|(uid, did)| super::delivery::WelcomeOut {
                    recipient_id: uid.clone(),
                    recipient_device_id: did.clone(),
                    welcome: welcome_bytes.clone(),
                })
                .collect(),
            None => Vec::new(),
        };

        let published = publish_staged_commit(
            state,
            &conversation_id,
            generation,
            None,
            &actor_user_id,
            outcome.epoch_before,
            &data.commit_bytes,
            data.group_info_bytes.as_deref(),
            added_uid.as_deref(),
            added_dids.as_deref(),
            &welcomes,
            "reconcile",
        )
        .await;

        match published {
            Ok(PublishOutcome::Won) => {}
            // Lost and rolled back. The membership row persists, so a later
            // reconcile re-applies our change at the new epoch.
            Ok(PublishOutcome::LostRace) => {
                // Converge through the INTERLEAVED ingesting catch-up, never a
                // bare replay (`max_past_epochs = 0`, #440/#441). It re-acquires
                // the MLS lock, so drop ours.
                drop(_mls_guard);
                if let Err(e) = crate::commands::messages::catch_up_mls_group_interleaved(
                    state,
                    &conversation_id,
                    &actor_user_id,
                )
                .await
                {
                    crate::diag!(
                        "[mls] reconcile: converge-after-lost-race failed for {conversation_id}: {e}"
                    );
                }
                return Ok(ReconcileOutcome::default());
            }
            // Genuinely not landed; the pending commit was already cleared.
            Err(e) => {
                return Err(e);
            }
        }
    }

    // The committer's epoch advanced too: voice, the pin key (#99) and file keys
    // move to the new exporter.
    if outcome.epoch_after > outcome.epoch_before {
        crate::commands::voice_e2ee::on_mls_epoch_changed(state, &conversation_id).await;
        crate::commands::pinned_messages::maybe_rewrap_pin_key(state, &conversation_id).await;
        crate::commands::file_keys::maybe_rewrap_file_keys(state, &conversation_id).await;
        // #1382: a removal rotates the attachment key ring; the owed-rotation mark
        // survives a crash before the DS write.
        if !outcome.removed.is_empty() {
            crate::commands::file_keys::on_member_removed(state, &conversation_id).await;
        }
    }

    // Roster-change refetch hint, only on a real add/remove. It names no one
    // (#1506).
    if outcome.epoch_after > outcome.epoch_before
        && (!outcome.added.is_empty() || !outcome.removed.is_empty())
    {
        // The sink is None during early boot / signed-out.
        let sink = state.livekit.lock().await.channel.clone();
        if let Some(ch) = sink {
            let _ = ch.send(crate::realtime::RealtimeEvent::RosterChanged {
                conversation_id: conversation_id.clone(),
                epoch_before: outcome.epoch_before,
                epoch_after: outcome.epoch_after,
            });
        }

        // Room broadcast: routing handle + epochs only (§5.3). Non-fatal.
        if let Err(e) = crate::commands::livekit::publish_to_room_server(
            state,
            &conversation_id,
            crate::commands::livekit_signalling::roster_changed_payload(
                &conversation_id,
                outcome.epoch_before,
                outcome.epoch_after,
            ),
        )
        .await
        {
            crate::diag!(
                "[realtime] reconcile: publish roster_changed for {conversation_id}: {e}"
            );
        }
    }

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn roster(ids: &[&str]) -> HashSet<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    fn key(uid: &str, did: &str) -> (String, String) {
        (uid.to_string(), did.to_string())
    }

    fn valid(pairs: &[(&str, &str)]) -> HashSet<(String, String)> {
        pairs.iter().map(|(u, d)| key(u, d)).collect()
    }

    /// #679: the revoked device still has an unclaimed KeyPackage, so gating only
    /// the tree-member half would re-admit it.
    #[test]
    fn a_revoked_device_with_a_key_package_is_not_desired() {
        let tree = [key("alice", "a1"), key("bob", "b1")];
        let kps = vec![key("bob", "b1")];

        let got = desired_set(
            &kps,
            tree.iter(),
            &roster(&["alice", "bob"]),
            Some(&valid(&[("alice", "a1")])),
        );

        assert!(
            !got.contains(&key("bob", "b1")),
            "REVOCATION LEAK (#679): a revoked device was re-admitted through its own \
             leftover KeyPackage, so it can never reach `to_remove`. got {got:?}"
        );
        assert_eq!(
            got,
            valid(&[("alice", "a1")]),
            "only alice is desired; got {got:?}"
        );
    }

    /// The committer's own device has no KeyPackage left and must stay desired.
    #[test]
    fn the_committers_own_leaf_is_retained_without_a_key_package() {
        let tree = [key("alice", "a1")];

        let got = desired_set(
            &[],
            tree.iter(),
            &roster(&["alice"]),
            Some(&valid(&[("alice", "a1")])),
        );

        assert_eq!(
            got,
            valid(&[("alice", "a1")]),
            "committer must stay desired; got {got:?}"
        );
    }

    #[test]
    fn a_leaf_whose_user_left_the_roster_is_not_desired() {
        let tree = [key("alice", "a1"), key("bob", "b1")];

        let got = desired_set(
            &[],
            tree.iter(),
            &roster(&["alice"]),
            Some(&valid(&[("alice", "a1"), ("bob", "b1")])),
        );

        assert!(
            !got.contains(&key("bob", "b1")),
            "a non-roster user's leaf must not be desired; got {got:?}"
        );
    }

    /// The addition path must not be collateral damage of the validity filter.
    #[test]
    fn a_live_device_awaiting_its_first_leaf_is_desired() {
        let tree: Vec<(String, String)> = vec![key("alice", "a1")];
        let kps = vec![key("bob", "b1")];

        let got = desired_set(
            &kps,
            tree.iter(),
            &roster(&["alice", "bob"]),
            Some(&valid(&[("alice", "a1"), ("bob", "b1")])),
        );

        assert!(
            got.contains(&key("bob", "b1")),
            "a live roster device with a KP must be desired (the add path); got {got:?}"
        );
    }

    /// An empty `valid_devices` against a live roster desires NOBODY: the mass
    /// eviction #1082 gates.
    #[test]
    fn an_empty_snapshot_would_evict_the_whole_roster() {
        let tree = [key("alice", "a1"), key("bob", "b1")];

        let got = desired_set(&[], tree.iter(), &roster(&["alice", "bob"]), Some(&valid(&[])));

        assert!(
            got.is_empty(),
            "an empty snapshot desires nobody — this is the mass eviction #1082 gates; got {got:?}"
        );
    }

    /// So that input is refused before it can be acted on.
    #[test]
    fn a_snapshot_that_omits_the_actor_is_refused() {
        assert!(
            !snapshot_includes_actor(
                &roster(&["alice", "bob"]),
                &valid(&[]),
                "alice",
                "a1",
            ),
            "an empty snapshot for a live roster must not be trusted"
        );
        assert!(
            !snapshot_includes_actor(
                &roster(&["alice", "bob"]),
                &valid(&[("bob", "b1")]),
                "alice",
                "a1",
            ),
            "a snapshot missing the actor is a short answer, not a revocation"
        );
    }

    #[test]
    fn a_snapshot_containing_the_actor_is_trusted() {
        assert!(snapshot_includes_actor(
            &roster(&["alice", "bob"]),
            &valid(&[("alice", "a1"), ("bob", "b1")]),
            "alice",
            "a1",
        ));
        assert!(snapshot_includes_actor(
            &roster(&["alice", "bob"]),
            &valid(&[("alice", "a1")]),
            "alice",
            "a1",
        ));
    }

    /// The two states that deliberately do not gate.
    #[test]
    fn the_gate_does_not_fire_when_it_cannot_judge() {
        // Empty roster: legitimate teardown, nothing left to protect.
        assert!(snapshot_includes_actor(&roster(&[]), &valid(&[]), "alice", "a1"));
        // Unknown own device id: "cannot tell" must not become "abort".
        assert!(snapshot_includes_actor(&roster(&["alice"]), &valid(&[]), "alice", ""));
    }

    /// Revoking one device must spare its live sibling.
    #[test]
    fn revoking_one_device_spares_its_live_sibling() {
        let tree = [key("alice", "a1"), key("alice", "a2")];

        let got = desired_set(
            &[],
            tree.iter(),
            &roster(&["alice"]),
            Some(&valid(&[("alice", "a1")])),
        );

        assert_eq!(
            got,
            valid(&[("alice", "a1")]),
            "exactly the live sibling stays desired; got {got:?}"
        );
    }

    /// `None` (no snapshot readable) disables the gate, NOT behaving like an
    /// empty set.
    #[test]
    fn a_missing_snapshot_disables_the_gate_rather_than_emptying_the_group() {
        let tree = [key("alice", "a1"), key("bob", "b1")];
        let kps = vec![key("bob", "b1")];

        let unguarded = desired_set(&kps, tree.iter(), &roster(&["alice", "bob"]), None);
        assert_eq!(
            unguarded,
            valid(&[("alice", "a1"), ("bob", "b1")]),
            "None must keep the roster-only behaviour; got {unguarded:?}"
        );

        let empty_snapshot = desired_set(
            &kps,
            tree.iter(),
            &roster(&["alice", "bob"]),
            Some(&HashSet::new()),
        );
        assert!(
            empty_snapshot.is_empty(),
            "an EMPTY snapshot genuinely means nothing is valid; got {empty_snapshot:?}"
        );
        assert_ne!(
            unguarded, empty_snapshot,
            "`None` and `Some(empty)` must not be the same state"
        );
    }
}
