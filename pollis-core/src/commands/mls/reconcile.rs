//! Declarative roster reconciliation and self-repair for MLS groups.
//!
//! Diffs the desired roster against the MLS tree and issues one combined add/remove commit + Welcome.

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

/// Is the byte-for-byte commit we submitted already canonical at `epoch`?
///
/// Makes submission idempotent (#411): an ambiguous outcome adopts iff the log holds our exact bytes; a read failure is `false`.
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

/// The canonical commit bytes at `epoch`, or `None` on no row or a failed read; the I/O half of [`our_commit_is_canonical`].
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
    // `None` on failure maps to Rollback: an unreadable log must never read as "we won".
    let bytes = snap.commit_at_epoch.as_deref()?;
    super::ds_reads::decode_b64("commit_data", bytes).ok()
}

/// Apply a won commit's side effects (merge the still-staged pending commit), whether confirmed or discovered canonical.
async fn finalize_won_commit(
    state: &Arc<AppState>,
    conversation_id: &str,
    generation: i64,
) -> crate::error::Result<()> {
    // Welcomes and GroupInfo were written with the commit; scope the !Send provider so it never crosses an await.
    let guard = state.local_db.lock().await;
    if let Some(db) = guard.as_ref() {
        let provider = PollisProvider::new(db.conn());
        merge_pending_in_suite(&provider, conversation_id, generation)?;
    }
    Ok(())
}

/// Merge the still-staged pending commit for `conversation_id`, advancing the
/// local epoch. Suite-generic half of [`finalize_won_commit`].
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
/// leave local state consistent either way.
///
/// The single publish path for every commit, since the CAS resolution is subtle:
///
/// - `Committed` → we own the epoch.
/// - `LostRace` or a transport error → ambiguous; adopt iff [`our_commit_is_canonical`] (#411).
///
/// On a loss it is cleared; the caller converges after `LostRace` (it holds the MLS lock).
///
/// `generation` is the commit's lineage (#454 P4); `closes_epoch` is set only by a migration.
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

    // Claim the epoch; on a win the DS writes GroupInfo + Welcomes atomically with the commit.
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
            // #1041: no envelope can now be sealed at the closing epoch; read what was before merging.
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

/// The committer's sweep of the epoch a won commit is about to leave (#1041),
/// with the failure policy that keeps it from losing what it exists to save.
///
/// Returns the persisted envelopes (receipts owed after merge) or, on failure:
///
/// * an ordinary commit → `Err`, and the commit stays STAGED for the next catch-up to adopt.
/// * a migration → `Ok(None)`, merging anyway: abandoning an opened successor strands everyone.
///
/// One retry, for a flaky link.
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

/// Roll back a staged-but-unconfirmed pending commit (best effort; logs on
/// failure). Used only when our commit genuinely did not land.
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
    /// `(user_id, device_id)` pairs on the roster with no claimable KeyPackage in the group's suite (#1412, #669), or whose claim failed (#1571).
    pub skipped_no_suite_kp: Vec<(String, String)>,
    /// `(user_id, device_id, reason)` for each claimed KeyPackage refused because its leaf key is not (provably) the one the user certified; the package is burnt.
    pub refused_uncertified: Vec<(String, String, String)>,
}

/// Raw bytes produced by a reconcile commit, needed for posting to Turso.
pub struct ReconcileCommitData {
    pub commit_bytes: Vec<u8>,
    pub welcome_bytes: Option<Vec<u8>>,
    /// GroupInfo at the resulting epoch, written atomically with the commit + Welcome.
    pub group_info_bytes: Option<Vec<u8>>,
}

/// #679: [`desired_set`] gates both sources on `valid_devices`, so a revoked device's KeyPackage cannot re-admit it.
///
/// #1082: a `registered_devices` snapshot omitting the actor reads as "every device revoked"; refuse it rather than mass-evict.
///
/// An empty roster or unknown `actor_device_id` does not gate.
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

/// Sync core (staged): diff the desired roster against the tree and stage one combined commit, NOT merged; the caller merges or clears it.
///
/// `epoch_after` is the epoch the commit WILL produce.
///
/// `identities`: a tree leaf positively `Uncertified` is evicted; `Unverifiable` is kept; `None` disables the gate.
// Each argument is a distinct piece of MLS state the caller already holds
// separately; bundling them into a struct would only move the same list.
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

    // 1. Actual state: walk the MLS tree, ONE ENTRY PER LEAF.
    //
    //    Per leaf: openmls allows duplicate CREDENTIALS, so keying on them let a grafted leaf survive (#1161 M7).
    //
    //    An uncertified leaf is kept out of `actual` and queued for removal, so its genuine KeyPackage can be added in the same commit.
    //
    //    "Our own leaf" means our INDEX, never our credential, or a duplicate could never be evicted.
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

    // Can only disagree if the stored group is another device's; every self-exemption keys on the index.
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
        // Survivor among duplicate claims: our own leaf, else the leftmost (older, honest) one.
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

    // 2. Desired set; see `desired_set`.
    let kp_keys: Vec<(String, String)> = available_kps
        .iter()
        .map(|(uid, did, _)| (uid.clone(), did.clone()))
        .collect();
    let desired = desired_set(&kp_keys, actual.keys(), roster_user_ids, valid_devices);

    // 3. Diff.
    let actual_keys: HashSet<(String, String)> = actual.keys().cloned().collect();

    // Leaves in tree but not desired → remove, plus every leaf already queued for
    // eviction (uncertified, or a duplicate of a credential another leaf holds).
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

    // 4. Self-removal detection, on the INDEX (#1161 M7).
    let mut skipped_self_removal = false;
    if to_remove.iter().any(|(_, idx)| *idx == own_index) {
        to_remove.retain(|(_, idx)| *idx != own_index);
        skipped_self_removal = true;
    }

    // Collect validated KPs for the add set.
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

    // 6. Log the diff.
    let removed_desc: Vec<String> = to_remove.iter().map(|((u, d), _)| format!("{u}:{d}")).collect();
    let added_desc: Vec<String> = add_kps.iter().map(|(u, d, _)| format!("{u}:{d}")).collect();
    crate::diag!(
        "[mls] reconcile: removing [{}], adding [{}]",
        removed_desc.join(", "),
        added_desc.join(", "),
    );

    // 7. Stage one commit with both proposals; the epoch advances only on merge.
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

    // 8. Serialize commit, Welcome and resulting-epoch GroupInfo from the bundle.
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

/// Sync core: computes the diff between the desired roster and the actual MLS
/// tree, then issues a single combined commit. Testable without Turso or async.
///
/// Merges locally; for tests and local-only paths (async callers use the staged variant).
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


/// Load the group and signer, validate claimed KeyPackages, and stage the reconcile commit.
///
/// `Ok(None)` means the local group is gone; the caller reports a no-op.
///
/// `pub(super)` because migration (#454 P4) stages against a fresh successor with it.
///
/// Every claimed KeyPackage's leaf must be [`LeafVerdict::Certified`] against `identities` ([`load_pinned_identities`]): the DS is untrusted.
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
        // The DS is untrusted: re-check the KeyPackage's suite, or it could downgrade the group.
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
        // Cross-signing: anything short of `Certified` is refused.
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
            // Test-harness only: the flows suite disables this refusal to test the inbound check.
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

    // Stage, don't merge: the merge waits for the publish so local never runs ahead of the log.
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

/// The roster's cross-signing material, with the account keys pinned locally.
///
/// One DS read, then two local substitutions: our own account key from the keystore, and any peer key differing from its TOFU pin is DROPPED (so `Unverifiable`).
pub(super) async fn load_pinned_identities(
    state: &Arc<AppState>,
    roster_user_ids: &[String],
    actor_user_id: &str,
) -> crate::error::Result<IdentityDirectory> {
    let rows = crate::commands::ds_reads::roster_identities(state, roster_user_ids).await?;
    pin_identities(state, IdentityDirectory::new(rows), actor_user_id).await
}

/// The two local substitutions [`load_pinned_identities`] applies, over a
/// directory built from ANY source of DS rows.
///
/// Shared with the receive side, which lacked it (#1161 M3).
///
/// Both only ever weaken a verdict, never strengthen it.
pub(super) async fn pin_identities(
    state: &Arc<AppState>,
    mut dir: IdentityDirectory,
    actor_user_id: &str,
) -> crate::error::Result<IdentityDirectory> {
    // Our own root of trust is local; without an account identity the DS value stands.
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
            // Not on this roster, or the DS returned no key (already unverifiable).
            None => {}
        }
    }
    Ok(dir.into_pinned())
}

/// Test-harness only: when set, `stage_reconcile_commit` adds a KeyPackage whose
/// leaf FAILS the cross-signing check, playing a malicious or pre-fix committer so
/// the flows suite can prove the inbound side flags and evicts such a leaf.
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
/// whether it is holding a staged, unconfirmed commit.
///
/// `None` when there is no local group; lets a test catch a phantom epoch advance (#1079).
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
/// tree for `conversation_id`, so a test can assert what a reconcile admitted
/// or evicted rather than infer it.
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

/// How many passes [`reconcile_group_mls_impl`] makes when its commit keeps
/// losing the epoch race: the first, and two retries on the converged epoch.
const RECONCILE_ATTEMPTS: usize = 3;

/// One reconcile pass's end: done (with what it did), or its commit lost the
/// epoch race and the device has converged on the winner.
enum PassEnd {
    Done(ReconcileOutcome),
    LostRace,
}

/// Async entry point: reconcile the conversation's roster into its MLS group.
///
/// A pass that loses the epoch race is retried on the converged epoch (#1571), up to [`RECONCILE_ATTEMPTS`] passes; being declarative, it re-derives the add set.
pub async fn reconcile_group_mls_impl(
    state: &Arc<AppState>,
    conversation_id: &str,
    actor_user_id: &str,
) -> crate::error::Result<ReconcileOutcome> {
    for attempt in 1..=RECONCILE_ATTEMPTS {
        // Boxed: one pass is a large future; the retry loop must not hold it
        // inline in every caller's state machine.
        match Box::pin(reconcile_pass(state, conversation_id, actor_user_id)).await? {
            PassEnd::Done(outcome) => return Ok(outcome),
            PassEnd::LostRace => {
                crate::diag!(
                    "[mls] reconcile: {conversation_id}: pass {attempt}/{RECONCILE_ATTEMPTS} lost \
                     the epoch race{}",
                    if attempt < RECONCILE_ATTEMPTS {
                        " — retrying on the converged epoch"
                    } else {
                        " — giving up this time; the next reconcile or backstop retries"
                    }
                );
            }
        }
    }
    Ok(ReconcileOutcome::default())
}

async fn reconcile_pass(
    state: &Arc<AppState>,
    conversation_id: &str,
    actor_user_id: &str,
) -> crate::error::Result<PassEnd> {
    let conversation_id = conversation_id.to_owned();
    let actor_user_id = actor_user_id.to_owned();

    // Test-harness park point (#1041).
    #[cfg(feature = "test-harness")]
    super::group_state::rendezvous::park(super::group_state::rendezvous::Point::ReconcileEntry)
        .await;

    // Serialize MLS mutations per conversation, except the lost-race converge, which drops the lock for the interleaved catch-up.
    let _mls_guard = state.mls_group_lock(&conversation_id).await;

    // 1. Roster, from the one server-side definition `migrate` also uses (#987).
    let roster_user_ids: std::collections::HashSet<String> =
        crate::commands::ds_reads::catch_up_full(state, &conversation_id, false, false, true)
            .await?
            .roster
            .into_iter()
            .collect();

    // 1b. TOFU-pin peers' account keys before using server-reported keys; non-fatal.
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

    // 2. Find devices with unclaimed KPs for all roster users.
    //
    // A revoked device keeps its KeyPackages, so filter `revoked_at IS NULL` here too and never burn one.
    //
    // One request, and an ERROR rather than a short list: a truncated answer silently leaves devices out.
    let device_pairs: Vec<(String, String)> =
        crate::commands::ds_reads::claimable_devices(state, &ids, None).await?;

    // 2b. Still-registered devices, to evict revoked leaves of roster members.
    let valid_devices = crate::commands::ds_reads::registered_devices(state, &ids).await?;

    // 2c. Pinned cross-signing material; an error, never partial, since it decides admission.
    let identities = load_pinned_identities(state, &ids, &actor_user_id).await?;

    let actor_device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .unwrap_or_default();

    // 2d. #1082: refuse the pass if the snapshot that decides evictions does not
    //     even contain us. See `snapshot_includes_actor`.
    if !snapshot_includes_actor(&roster_user_ids, &valid_devices, &actor_user_id, &actor_device_id) {
        crate::diag!(
            "[reconcile] refusing pass for {conversation_id}: registered_devices snapshot \
             ({} device(s)) omits this device — not evicting a {}-member roster on it",
            valid_devices.len(),
            roster_user_ids.len()
        );
        return Ok(PassEnd::Done(ReconcileOutcome::default()));
    }

    // 3. Peek at the tree so we only claim KPs for devices not yet in it.
    //    The MLS lock also pins the suite generation for this pass (migration takes it too).
    let (already_in_tree, group_suite, generation): (
        std::collections::HashSet<(String, String)>,
        Ciphersuite,
        i64,
    ) = {
        let guard = state.local_db.lock().await;
        let db = match guard.as_ref() {
            Some(db) => db,
            None => {
                return Ok(PassEnd::Done(ReconcileOutcome::default()));
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
                return Ok(PassEnd::Done(ReconcileOutcome::default()));
            }
        }
    };

    // #1571: diff every registered roster device, not only claimable ones, so an unaddable one is reported in `skipped_no_suite_kp`.
    let claimable: std::collections::HashSet<&(String, String)> = device_pairs.iter().collect();
    let mut skipped_no_suite_kp: Vec<(String, String)> =
        unaddable_devices(&valid_devices, &already_in_tree, &claimable, &roster_user_ids);
    for (uid, did) in &skipped_no_suite_kp {
        crate::diag!(
            "[mls] reconcile: {conversation_id}: roster device {uid}:{did} has no leaf and no \
             KeyPackage to add it with — left out of this commit (#1571); it is added once it \
             publishes one, or joins itself on its next catch-up"
        );
    }

    // Only claim KPs for devices not already in the tree.
    let devices_to_claim: Vec<(String, String)> = device_pairs
        .into_iter()
        .filter(|pair| !already_in_tree.contains(pair))
        .collect();

    // 4. Claim one KP per device that needs to be added.
    //
    // A KP claim writes a peer's row, so it routes through the DS; none available → `Ok(None)`, skipped.
    //
    // Claim in the GROUP's suite, so a device lacking one is skipped, never downgraded in.
    //
    // A FAILED claim skips just that device (#1571); aborting would burn the packages already claimed.
    let mut kp_tuples: Vec<(String, String, Vec<u8>)> = Vec::new();
    for (uid, did) in &devices_to_claim {
        let claimed: Option<Vec<u8>> =
            match crate::commands::mls::ds_claim_key_package(state, uid, Some(did), group_suite)
                .await
            {
                Ok(claimed) => claimed,
                Err(e) => {
                    crate::diag!(
                        "[mls] reconcile: {conversation_id}: claiming a KeyPackage for \
                         {uid}:{did} failed ({e}) — skipping that device, not the pass (#1571)"
                    );
                    skipped_no_suite_kp.push((uid.clone(), did.clone()));
                    continue;
                }
            };
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

    // 5. Validate KPs and call the sync core under the local_db lock, in the
    //    group's own suite.
    let staged = {
        let guard = state.local_db.lock().await;
        let db = match guard.as_ref() {
            Some(db) => db,
            None => {
                return Ok(PassEnd::Done(ReconcileOutcome::default()));
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
        // Nothing to commit — but a device it could not add is still reported.
        None => {
            return Ok(PassEnd::Done(ReconcileOutcome {
                skipped_no_suite_kp,
                ..ReconcileOutcome::default()
            }));
        }
    };
    outcome.skipped_no_suite_kp = skipped_no_suite_kp;

    // 5. Publish FIRST, then merge locally: on failure the pending commit is rolled back, so we never run ahead of the log.
    if let Some(data) = commit_data_opt {
        // Added users/devices as a PREFETCH HINT; receivers verify the commit's own Add proposals, so a bad hint only looks unverifiable.
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

        // One Welcome blob per added recipient; the delivery seam writes them with the commit.
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

        // Claim the epoch; see [`publish_staged_commit`].
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
            // We own this epoch. Fall through to the roster hint / voice rotation.
            Ok(PublishOutcome::Won) => {}
            // Lost the race: our commit was rolled back; converge, and a later reconcile re-applies our change.
            Ok(PublishOutcome::LostRace) => {
                // Converge through the INTERLEAVED ingesting catch-up (`max_past_epochs = 0`, #440/#441); it takes the MLS lock, so drop ours.
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
                return Ok(PassEnd::LostRace);
            }
            // Genuinely did not land; the pending commit is already cleared.
            Err(e) => {
                return Err(e);
            }
        }
    }

    // Voice E2EE: the committer must rotate too, or it keeps sending under the old epoch's key.
    if outcome.epoch_after > outcome.epoch_before {
        crate::commands::voice_e2ee::on_mls_epoch_changed(state, &conversation_id).await;
        // Pin key too (#99), so a member added by this commit can unwrap at once.
        crate::commands::pinned_messages::maybe_rewrap_pin_key(state, &conversation_id).await;
        crate::commands::file_keys::maybe_rewrap_file_keys(state, &conversation_id).await;
        // #1382: a removal commit rotates the attachment key ring, so the
        // removed member cannot open files posted from this epoch on. The
        // owed-rotation mark it leaves survives a crash before the DS write.
        if !outcome.removed.is_empty() {
            crate::commands::file_keys::on_member_removed(state, &conversation_id).await;
        }
    }

    // Roster-change hint, only on an epoch bump; names no one (#1506).
    if outcome.epoch_after > outcome.epoch_before
        && (!outcome.added.is_empty() || !outcome.removed.is_empty())
    {
        // Local emit; no sink while signed out.
        let sink = state.livekit.lock().await.channel.clone();
        if let Some(ch) = sink {
            let _ = ch.send(crate::realtime::RealtimeEvent::RosterChanged {
                conversation_id: conversation_id.clone(),
                epoch_before: outcome.epoch_before,
                epoch_after: outcome.epoch_after,
            });
        }

        // Room broadcast: routing handle + epochs ONLY (§5.3, cleartext via LiveKit); non-fatal.
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

    Ok(PassEnd::Done(outcome))
}

/// Registered devices of roster members that hold no leaf and have no package
/// to claim (#1571) — the devices a reconcile cannot add this pass. Sorted, so
/// the log and the outcome read the same on every run.
fn unaddable_devices(
    valid_devices: &std::collections::HashSet<(String, String)>,
    tree: &std::collections::HashSet<(String, String)>,
    claimable: &std::collections::HashSet<&(String, String)>,
    roster: &std::collections::HashSet<String>,
) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = valid_devices
        .iter()
        .filter(|d| roster.contains(&d.0) && !tree.contains(*d) && !claimable.contains(d))
        .cloned()
        .collect();
    out.sort();
    out
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

    /// #679: a revoked device with an unclaimed KeyPackage must not re-enter via `available_kps`.
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

    /// The committer's own device appears ONLY as a tree member and must be retained.
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

    /// A device whose USER left the roster is undesired regardless of KeyPackages
    /// or validity — the plain removal case, which must keep working.
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

    /// A live roster device with a KeyPackage but no leaf is desired (the add path).
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

    /// #1571: a registered roster device with no leaf and nothing to claim is
    /// named, not dropped; a device in the tree, a claimable one and one whose
    /// user is off the roster are not.
    #[test]
    fn a_registered_device_with_nothing_to_claim_is_reported() {
        let valid = valid(&[("alice", "a1"), ("bob", "b1"), ("bob", "b2"), ("carol", "c1"), ("zed", "z1")]);
        let tree: HashSet<(String, String)> = [key("alice", "a1"), key("bob", "b1")].into_iter().collect();
        let c1 = key("carol", "c1");
        let claimable: HashSet<&(String, String)> = [&c1].into_iter().collect();
        let got = unaddable_devices(&valid, &tree, &claimable, &roster(&["alice", "bob", "carol"]));
        assert_eq!(got, vec![key("bob", "b2")]);
    }

    /// An empty `valid_devices` against a live roster would evict every leaf.
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
        // Partial is refused too: our own row is missing.
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
        // A genuine single-device revocation still passes.
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

    /// Revoking one device must keep its live sibling.
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

    /// `None` disables the gate; it must not act like an empty set.
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
