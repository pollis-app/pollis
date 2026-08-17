//! Declarative roster reconciliation and self-repair for MLS groups.
//!
//! Compares the desired roster (from Turso) against the actual MLS tree
//! and issues a single combined add/remove commit + Welcome to bring them
//! into sync. Also houses the heavyweight repair path that re-creates
//! the entire MLS group when local state is unrecoverable.

use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::OpenMlsProvider;

use std::sync::Arc;
use tls_codec::{Deserialize as TlsDeserialize, Serialize as TlsSerialize};

use crate::state::AppState;

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
/// Makes submission idempotent (issue #411). A submit outcome can be ambiguous:
/// a network error may mean the commit LANDED and only the response was lost,
/// and a stale `LostRace` can be a retry of our OWN already-accepted commit. The
/// canonical log is the arbiter — if our exact commit bytes sit at this epoch,
/// we won and must adopt, not roll back and wedge. Any read failure returns
/// `false` (safe: fall back to the rollback/converge path).
pub(super) async fn our_commit_is_canonical(
    state: &Arc<AppState>,
    conversation_id: &str,
    generation: i64,
    epoch: i64,
    our_commit: &[u8],
) -> bool {
    // Fetch the canonical bytes at this epoch, then run the Kani-proved
    // adopt/rollback core. An ambiguous submit (LostRace or lost-response Failed)
    // adopts IFF the log holds OUR exact bytes; a missing row / read failure →
    // `None` → Rollback → `false` (fall back to the converge path), byte-for-byte
    // the original behavior.
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

/// The canonical commit bytes stored at `epoch` for `conversation_id`, or `None`
/// if there is no such row or the read fails. The read-only commit-log lookup
/// behind [`our_commit_is_canonical`]; separated so the adopt/rollback *decision*
/// is the pure, Kani-proved `invariants::resolve` and this async fn is only I/O.
async fn fetch_commit_at_epoch(
    state: &Arc<AppState>,
    conversation_id: &str,
    generation: i64,
    epoch: i64,
) -> Option<Vec<u8>> {
    // Read-only commit-log lookup → log_db (falls back to remote_db pre-cutover).
    let conn = state.log_db.conn().await.ok()?;
    let mut rows = conn
        .query(
            "SELECT commit_data FROM mls_commit_log \
             WHERE conversation_id = ?1 AND generation = ?2 AND epoch = ?3",
            libsql::params![conversation_id.to_string(), generation, epoch],
        )
        .await
        .ok()?;
    match rows.next().await {
        Ok(Some(row)) => row.get::<Vec<u8>>(0).ok(),
        _ => None,
    }
}

/// Apply the side effects of a commit that won its epoch — whether confirmed
/// synchronously (`Committed`) or discovered canonical after an ambiguous
/// failure (lost response). Writes Welcomes for added members, merges the
/// pending commit to advance the local epoch, and republishes GroupInfo. The
/// pending commit must still be staged (we never cleared it on a win).
async fn finalize_won_commit(
    state: &Arc<AppState>,
    conversation_id: &str,
    generation: i64,
) -> crate::error::Result<()> {
    // Welcomes + the resulting-epoch GroupInfo were already written atomically
    // with the commit by `submit_commit` (Slice 1), so finalizing a won commit
    // only advances the local epoch by merging the still-staged pending commit.
    // Scope the !Send provider/group so neither crosses an await.
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
/// The single publish path for every commit Pollis produces — roster reconcile
/// and self-update alike — because the CAS resolution here is subtle enough
/// that a second copy would drift:
///
/// - `Committed` → we own the epoch.
/// - `LostRace` or a transport error → **ambiguous**. A network error may mean
///   the commit LANDED and only the response was lost, and a stale `LostRace`
///   can be a retry of our OWN already-accepted commit. The canonical log is the
///   arbiter ([`our_commit_is_canonical`], whose decision core is Kani-proved):
///   if our exact bytes sit at this epoch we WON and must adopt rather than roll
///   back and wedge (issue #411).
///
/// On a win the pending commit is merged, advancing the local epoch. On a loss
/// or a genuine failure it is cleared, so a later pass recomputes from scratch
/// and the local group never sits ahead of the log. The caller owns convergence
/// after a `LostRace` (it, not this function, holds the MLS lock that the
/// catch-up needs to re-acquire).
///
/// `what` names the caller in logs ("reconcile", "self-update").
///
/// `generation` is the suite lineage the commit belongs to (#454 P4), and
/// `closes_epoch` is set only by a migration opening the next one. Both are
/// carried through every step of the resolution, because after a migration the
/// *same* conversation has two lineages and "is our commit canonical at epoch
/// 3?" has a different answer in each.
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

    // Claim this epoch through the delivery seam. On a win the seam also writes
    // the resulting-epoch GroupInfo + the added members' Welcomes — atomically
    // with the commit, so no Welcome can ever point at a doomed branch.
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
                eprintln!(
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
                eprintln!(
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
            finalize_won_commit(state, conversation_id, generation).await?;
            Ok(PublishOutcome::Won)
        }
        Resolution::LostRace => {
            eprintln!(
                "[mls] {what}: lost epoch {epoch} race for {conversation_id} — rolling back local pending commit and converging on the winner"
            );
            clear_pending_best_effort(state, conversation_id, generation).await;
            Ok(PublishOutcome::LostRace)
        }
        Resolution::Failed(e) => {
            eprintln!(
                "[mls] {what}: remote persist failed for {conversation_id}, clearing local pending commit: {e}"
            );
            clear_pending_best_effort(state, conversation_id, generation).await;
            Err(e)
        }
    }
}

/// Roll back a staged-but-unconfirmed pending commit (best effort; logs on
/// failure). Used only when our commit genuinely did not land.
async fn clear_pending_best_effort(state: &Arc<AppState>, conversation_id: &str, generation: i64) {
    let guard = state.local_db.lock().await;
    if let Some(db) = guard.as_ref() {
        // Storage-only: dropping a staged commit touches no crypto, so this
        // needs no suite dispatch.
        let store = store_only(db.conn());
        let group_id = mls_group_id(conversation_id, generation);
        match MlsGroup::load(&store, &group_id) {
            Ok(Some(mut group)) => {
                if let Err(e) = group.clear_pending_commit(&store) {
                    eprintln!("[mls] reconcile: clear_pending_commit failed: {e}");
                }
            }
            Ok(None) => eprintln!("[mls] reconcile: group vanished during rollback"),
            Err(e) => eprintln!("[mls] reconcile: group load failed during rollback: {e}"),
        }
    }
}

// NOTE: the former `repair_mls_group` (nuke-and-rebuild: re-create the group at
// epoch 0 and DELETE the conversation's commit log) was removed. Deleting the
// canonical commit log to repair a single device with missing local state
// destroyed every member's history and could fork the group — the exact
// append-only violation INV-1 forbids. The recovery for "my local MLS state is
// missing" is now `external_join_group` (rejoin THIS device from the published
// GroupInfo), wired at the `try_mls_encrypt` → None site in messages/edit_delete.

// ── Declarative reconcile ────────────────────────────────────────────────────

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
    /// `(user_id, device_id)` pairs that belong on the roster but could not be
    /// added because they have published no KeyPackage in the group's
    /// ciphersuite — since #669 that means a device that has not republished its
    /// pool since the group's suite stopped being the current one.
    ///
    /// Reported rather than silently dropped: these devices will not receive
    /// messages until they run a client that publishes a pool in that suite, and
    /// downgrading the group to admit them is never an option.
    pub skipped_no_suite_kp: Vec<(String, String)>,
}

/// Raw bytes produced by a reconcile commit, needed for posting to Turso.
pub struct ReconcileCommitData {
    pub commit_bytes: Vec<u8>,
    pub welcome_bytes: Option<Vec<u8>>,
    /// TLS-serialized GroupInfo at the *resulting* epoch, captured from the
    /// commit bundle. Travels with the commit + Welcome so the Delivery
    /// Service writes all three atomically (Slice 1). `Some` whenever the
    /// commit builder produced a GroupInfo — our groups always do
    /// (`use_ratchet_tree_extension(true)` + an explicit `create_group_info(true)`).
    pub group_info_bytes: Option<Vec<u8>>,
}

/// The devices that *should* hold a leaf: the single definition of "desired",
/// extracted from [`reconcile_group_mls_core_staged`] as pure set algebra so the
/// #679 invariant can be tested without a live MLS group, real KeyPackages, or a
/// crypto backend.
///
/// Two sources, and **both** are gated on `valid_devices`:
///
/// 1. Devices with an available (unclaimed) KeyPackage — these are the additions.
/// 2. Devices already holding a leaf whose user is still in the roster — these
///    are the retentions. Retaining them is what stops us evicting the
///    committer's own device, which consumed its KeyPackage when it created the
///    group and so has none left to appear in (1).
///
/// `valid_devices` is the `revoked_at IS NULL` snapshot from
/// [`registered_devices`]. Gating both sources on it is the whole point: a
/// revoked device keeps its published KeyPackage, so filtering only source (2)
/// leaves source (1) re-admitting the exact device (2) was trying to evict. It
/// would land in `desired`, therefore never in `to_remove`, and — if its leaf had
/// already been pruned — be *added back*. That was #679: revocation reduced to a
/// cosmetic flag while the device went on decrypting every message and deriving
/// the call media key.
///
/// `None` means "no revocation snapshot available" and disables the gate. That is
/// deliberately a distinct state from an empty set (which means "no device is
/// valid — remove everything") so a caller that cannot read `user_device` degrades
/// to the old roster-only behaviour instead of silently emptying the group.
fn desired_set<'a>(
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

/// Sync core (staged variant): computes the diff between the desired roster
/// and the actual MLS tree, then issues a single combined commit. Does NOT
/// merge the commit locally — the commit is left as a pending commit on the
/// group. The caller is responsible for either calling `merge_pending_commit`
/// (after successfully persisting the commit/welcome rows to Turso) or
/// `clear_pending_commit` (on remote failure) to avoid split-brain between
/// the local epoch and the remote commit log.
///
/// Returns the outcome plus optional commit/welcome bytes for the caller to
/// post to Turso. On the returned `ReconcileOutcome`, `epoch_after` reflects
/// the epoch the commit WILL produce when merged (i.e. `epoch_before + 1`
/// when a commit is staged, equal to `epoch_before` on no-op).
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
) -> crate::error::Result<(ReconcileOutcome, Option<ReconcileCommitData>)>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    use std::collections::{HashMap, HashSet};

    let epoch_before = group.epoch().as_u64();

    // 1. Actual state: walk the MLS tree.
    let mut actual: HashMap<(String, String), LeafNodeIndex> = HashMap::new();
    for m in group.members() {
        let uid = parse_credential_user_id(&m.credential);
        let did = parse_credential_device_id(&m.credential).unwrap_or_default();
        actual.insert((uid, did), m.index);
    }

    // 2. Build the desired set. Pure set algebra, extracted so it can be tested
    //    exhaustively without standing up a live MLS group — see `desired_set`.
    let kp_keys: Vec<(String, String)> = available_kps
        .iter()
        .map(|(uid, did, _)| (uid.clone(), did.clone()))
        .collect();
    let desired = desired_set(&kp_keys, actual.keys(), roster_user_ids, valid_devices);

    // 3. Diff.
    let actual_keys: HashSet<(String, String)> = actual.keys().cloned().collect();

    // Leaves in tree but not desired → remove
    let mut to_remove: Vec<((String, String), LeafNodeIndex)> = actual
        .iter()
        .filter(|(key, _)| !desired.contains(key))
        .map(|(key, &idx)| (key.clone(), idx))
        .collect();

    // Devices desired but not in tree → add
    let to_add_keys: HashSet<(String, String)> = desired
        .difference(&actual_keys)
        .cloned()
        .collect();

    // 4. Committer-in-remove-set detection.
    let mut skipped_self_removal = false;
    let actor_key = (actor_user_id.to_string(), actor_device_id.to_string());
    if to_remove.iter().any(|(key, _)| key == &actor_key) {
        to_remove.retain(|(key, _)| key != &actor_key);
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
    eprintln!(
        "[mls] reconcile: removing [{}], adding [{}]",
        removed_desc.join(", "),
        added_desc.join(", "),
    );

    // 7. Build a single commit with both proposals. `stage_commit` writes the
    //    commit as a pending commit on the group (persisted via the storage
    //    provider), but does NOT advance the group's epoch — that only
    //    happens on `merge_pending_commit`.
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

    // 8. Serialize commit + welcome + GroupInfo. All three bytes are available
    //    pre-merge directly from the commit bundle. The GroupInfo is the
    //    resulting-epoch tree snapshot a future joiner external-joins against;
    //    it travels with the commit so the Delivery Service writes commit +
    //    Welcome + GroupInfo atomically (Slice 1).
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

    // `epoch_after` is the epoch this commit will produce when merged. We
    // don't merge here, but we can report it deterministically: a staged
    // commit always advances the epoch by exactly one.
    let epoch_after = epoch_before + 1;

    let removed: Vec<(String, String)> = to_remove.into_iter().map(|(key, _)| key).collect();
    let added: Vec<(String, String)> = add_kps.into_iter().map(|(u, d, _)| (u, d)).collect();

    eprintln!(
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
/// Returns the outcome plus optional commit/welcome bytes for the caller to
/// post to Turso. The commit is merged locally before returning.
///
/// NOTE: async callers should prefer the staged variant
/// (`reconcile_group_mls_core_staged`) so remote persistence can happen
/// *before* the local merge, avoiding split-brain on remote failure. This
/// thin wrapper exists for test helpers and any path that deliberately wants
/// a local-only merge.
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
    )?;

    // If a commit was staged, merge it locally. No-op runs leave the group
    // in Operational state with no pending commit, so the merge is skipped.
    if commit_data_opt.is_some() {
        group
            .merge_pending_commit(provider)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("reconcile merge: {e}")))?;
        outcome.epoch_after = group.epoch().as_u64();
    }

    Ok((outcome, commit_data_opt))
}

/// Every `(user_id, device_id)` still registered in `user_device` for `roster`.
///
/// The counterpart to [`desired_roster_user_ids`] one level down: the roster says
/// which *users* belong, this says which of their *devices* still exist. Reconcile
/// diffs the tree against it to drop leaves whose device row was revoked while the
/// user remained a member, and migration (#454 P4) uses the same set as the list
/// of devices the successor lineage must be able to admit — the two must never
/// disagree about who belongs, which is why there is one query, not two.
///
/// A revoked device is NOT still registered. `revoke_device` tombstones the row
/// (`user_device.revoked_at`, migration 000004) rather than deleting it, so the
/// row's mere existence says nothing about whether the device still belongs —
/// only `revoked_at IS NULL` does. Without that predicate the tombstoned device
/// stays in `desired`, never reaches `to_remove`, and keeps its leaf: it goes on
/// decrypting every subsequent message and deriving the call media key. The
/// filter matches the DS, which gets this right in `current_member_devices`.
///
/// libsql has no array binding, so the `IN` clause is built with a generated
/// `?n` placeholder per id and the ids are BOUND, not interpolated. They used to
/// be interpolated behind a character filter; binding is what makes the
/// predicate above trustworthy rather than something a stray quote can reshape.
///
/// `pub` so the #679 revocation regression tests can reach it from
/// `tests/revoked_device_reconcile.rs`. They cannot live in the crate's `--lib`
/// test binary: libsql's local backend calls `sqlite3_config` on first use, which
/// returns `SQLITE_MISUSE` once rusqlite/SQLCipher (`LocalDb`) has already run
/// `sqlite3_initialize` in the same process — so any lib test that opens a local
/// libsql DB panics as soon as it shares a binary with a test that touches the
/// local DB. An integration test gets its own process and does not load rusqlite.
pub async fn registered_devices(
    conn: &libsql::Connection,
    roster: &std::collections::HashSet<String>,
) -> crate::error::Result<std::collections::HashSet<(String, String)>> {
    let mut devices = std::collections::HashSet::new();
    if roster.is_empty() {
        return Ok(devices);
    }
    let ids: Vec<String> = roster.iter().cloned().collect();
    // Chunked (#916): `roster` is a whole group's membership.
    for chunk in crate::db::chunk::bind_chunks(&ids, 0) {
        let query = format!(
            "SELECT user_id, device_id FROM user_device \
             WHERE revoked_at IS NULL AND user_id IN ({})",
            crate::db::chunk::placeholders(chunk.len(), 1)
        );
        let params: Vec<libsql::Value> =
            chunk.iter().cloned().map(libsql::Value::from).collect();
        let mut rows = conn.query(&query, params).await?;
        while let Some(row) = rows.next().await? {
            devices.insert((row.get::<String>(0)?, row.get::<String>(1)?));
        }
    }
    Ok(devices)
}

/// The set of user_ids that *should* have a leaf in `conversation_id`'s tree:
/// `group_member` plus pending `group_invite` rows, falling back to
/// `dm_channel_member` for DMs.
///
/// Pending invitees are included so their devices get a Welcome at invite time —
/// the acceptor can join the MLS group without requiring any other member to be
/// online simultaneously.
///
/// The single definition of "desired roster". `reconcile_group_mls_impl` diffs
/// the tree against it, and `migrate` moves exactly this set into a successor
/// group; the two must never disagree about who belongs, or a migration would
/// strand whoever the diff would have added.
pub(super) async fn desired_roster_user_ids(
    conn: &libsql::Connection,
    conversation_id: &str,
) -> crate::error::Result<std::collections::HashSet<String>> {
    let mut roster_user_ids = std::collections::HashSet::new();
    {
        let mut rows = conn
            .query(
                "SELECT user_id FROM group_member WHERE group_id = ?1",
                libsql::params![conversation_id.to_string()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            roster_user_ids.insert(row.get::<String>(0)?);
        }
    }
    {
        let mut rows = conn
            .query(
                "SELECT invitee_id FROM group_invite WHERE group_id = ?1",
                libsql::params![conversation_id.to_string()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            roster_user_ids.insert(row.get::<String>(0)?);
        }
    }
    if roster_user_ids.is_empty() {
        let mut rows = conn
            .query(
                "SELECT user_id FROM dm_channel_member WHERE dm_channel_id = ?1",
                libsql::params![conversation_id.to_string()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            roster_user_ids.insert(row.get::<String>(0)?);
        }
    }
    Ok(roster_user_ids)
}

/// Load the group, read its signer, validate the claimed KeyPackages and stage
/// the reconcile commit — the whole crypto-bearing half of
/// [`reconcile_group_mls_impl`], generic over the crypto backend so the caller
/// can dispatch on the group's ciphersuite.
///
/// `Ok(None)` means the local group is gone; the caller reports a no-op.
///
/// `generation` names which lineage's group to stage against. Migration (#454
/// P4) reuses this against the freshly-created successor at generation `N + 1`,
/// which is why it is `pub(super)`: standing up a successor is exactly "add the
/// whole roster to an empty group", and a second copy of the KeyPackage
/// validation — including the cross-suite refusal below — would drift.
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

    // Read signer.
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

    // Resolve pending commit.
    group
        .merge_pending_commit(provider)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("merge pending: {e}")))?;

    // Validate KPs.
    let mut available_kps: Vec<(String, String, KeyPackage)> = Vec::new();
    for (uid, did, kp_raw) in kp_tuples {
        let mut reader: &[u8] = kp_raw;
        let kp_in = match KeyPackageIn::tls_deserialize(&mut reader) {
            Ok(k) => k,
            Err(e) => {
                eprintln!("[mls] reconcile: kp deserialize failed for {uid}:{did}: {e}");
                continue;
            }
        };
        let kp = match kp_in.validate(provider.crypto(), ProtocolVersion::Mls10) {
            Ok(k) => k,
            Err(e) => {
                eprintln!("[mls] reconcile: kp validate failed for {uid}:{did}: {e}");
                continue;
            }
        };
        // Claiming is suite-scoped, but the DS is untrusted: re-check that the
        // KeyPackage it handed us really is in the group's suite before it can
        // reach `propose_adds`. A cross-suite KP would otherwise be a
        // server-driven downgrade of an entire group.
        if kp.ciphersuite() != group.ciphersuite() {
            eprintln!(
                "[mls] reconcile: kp suite {:?} != group suite {:?} for {uid}:{did} — refusing",
                kp.ciphersuite(),
                group.ciphersuite(),
            );
            continue;
        }
        let cred_user = parse_credential_user_id(kp.leaf_node().credential());
        if cred_user != *uid {
            eprintln!("[mls] reconcile: credential user '{cred_user}' != '{uid}' for device {did}");
            continue;
        }
        available_kps.push((uid.clone(), did.clone(), kp));
    }

    // Stage the commit: builds the commit locally and writes it as a
    // PENDING commit on the group (persisted to MLS storage) WITHOUT
    // advancing the local epoch. The merge is deferred until after the
    // remote INSERTs succeed so a remote failure cannot leave the local
    // group ahead of the remote commit log.
    reconcile_group_mls_core_staged(
        provider,
        &signer,
        &mut group,
        roster_user_ids,
        &available_kps,
        actor_user_id,
        actor_device_id,
        Some(valid_devices),
    )
    .map(Some)
}

/// Async entry point: reads desired state from Turso, loads local MLS group,
/// calls `reconcile_group_mls_core`, posts commit + welcome rows.
pub async fn reconcile_group_mls_impl(
    state: &Arc<AppState>,
    conversation_id: &str,
    actor_user_id: &str,
) -> crate::error::Result<ReconcileOutcome> {
    let conversation_id = conversation_id.to_owned();
    let actor_user_id = actor_user_id.to_owned();

    // Serialize all MLS mutations for this conversation on this device so two
    // concurrent reconciles (or a reconcile racing the external-join path)
    // can't both stage + commit from the same epoch. Held for the whole
    // function EXCEPT the lost-race converge below, which drops it and runs the
    // INTERLEAVED ingesting catch-up (which re-acquires the lock) so a converge
    // that advances/rebuilds can't strand an un-ingested current-epoch message.
    let _mls_guard = state.mls_group_lock(&conversation_id).await;

    let conn = state.remote_db.conn().await?;

    // 1. Determine roster: group_member + pending invitees, or dm_channel_member.
    let roster_user_ids = desired_roster_user_ids(&conn, &conversation_id).await?;

    // 1b. TOFU-pin every roster member's account_id_pub before we use
    //     server-reported keys to add devices to the MLS tree. Without
    //     this, a malicious Turso write could swap a member's key on the
    //     fly and the next reconcile would silently graft an attacker's
    //     device into the group with no inline signal to other members.
    //     The DM ingest path already does this per-message; groups
    //     piggyback on reconcile because that's the only choke point
    //     where roster changes get applied. Skip the actor's own id —
    //     contact_verification is for peers, not self. Non-fatal: a
    //     transient failure must not block a legitimate membership
    //     update. Caught + logged.
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
            eprintln!("[reconcile] batch_check_and_pin_account_keys failed: {e}");
        }
    }

    // 2. Find devices with unclaimed KPs for all roster users.
    let mut device_pairs: Vec<(String, String)> = Vec::new();
    {
        // A revoked device keeps its unclaimed KeyPackages, so this query needs
        // the same `revoked_at IS NULL` predicate as `registered_devices` — it is
        // the source of `available_kps`, and therefore of the KP half of
        // `desired`. Without it a revoked device is handed back a claimed KP and
        // re-admitted to the tree. `compute_diff` now filters this set against
        // `valid_devices` as a second line of defence; filtering here as well
        // means we never claim (and so never burn) a revoked device's KeyPackage.
        //
        // The ids are BOUND as `?n` placeholders rather than interpolated behind
        // a character filter, matching `registered_devices`. libsql has no array
        // binding, so the placeholder list is generated but the values are not.
        let ids: Vec<String> = roster_user_ids.iter().cloned().collect();
        // Chunked (#916), same reason as `registered_devices` above.
        for chunk in crate::db::chunk::bind_chunks(&ids, 0) {
            let query = format!(
                "SELECT d.user_id, d.device_id FROM user_device d \
                 WHERE d.revoked_at IS NULL AND d.user_id IN ({}) \
                 AND EXISTS ( \
                     SELECT 1 FROM mls_key_package kp \
                     WHERE kp.user_id = d.user_id AND kp.device_id = d.device_id AND kp.claimed = 0 \
                 )",
                crate::db::chunk::placeholders(chunk.len(), 1)
            );
            let params: Vec<libsql::Value> =
                chunk.iter().cloned().map(libsql::Value::from).collect();
            let mut rows = conn.query(&query, params).await?;
            while let Some(row) = rows.next().await? {
                device_pairs.push((row.get::<String>(0)?, row.get::<String>(1)?));
            }
        }
    }

    // 2b. Snapshot of every (user_id, device_id) pair still registered in
    //     `user_device` for the current roster. Used by reconcile to drop
    //     leaves whose device row was revoked even though the user is still
    //     a roster member (single-device revoke flow).
    let valid_devices = registered_devices(&conn, &roster_user_ids).await?;

    let actor_device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .unwrap_or_default();

    // 3. Peek at the current tree to learn which devices are already members.
    //    This lets us skip claiming KPs for devices that don't need to be added,
    //    avoiding unnecessary KP exhaustion on repeated reconciles.
    //
    //    The same peek yields the group's ciphersuite, which decides the suite
    //    of every KeyPackage claimed below. Reading the tree is storage-only —
    //    no crypto backend, hence no suite dispatch, is needed to learn it.
    //
    //    It also pins the suite GENERATION for the rest of this pass. Migration
    //    takes the same per-conversation MLS lock we hold here, so the lineage
    //    cannot move underneath us: every KeyPackage claimed, every commit
    //    staged, and the epoch claim itself all belong to this one generation.
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

    // Only claim KPs for devices not already in the tree.
    let devices_to_claim: Vec<(String, String)> = device_pairs
        .into_iter()
        .filter(|pair| !already_in_tree.contains(pair))
        .collect();

    // 4. Claim one KP per device that needs to be added.
    //
    // A KP claim is a `claimed = 1` write on a *peer's* row, so under a read-only
    // Turso token a direct `UPDATE … RETURNING` would fail — it routes through the
    // Delivery Service. A target device with no unclaimed package is simply
    // skipped (the DS replies 404 → `Ok(None)`).
    //
    // The claim is scoped to the GROUP's ciphersuite, never a fixed one. This is
    // what makes "a group can never contain a member without a KeyPackage in
    // that group's suite" hold: a device that has published no KP in the group's
    // suite has nothing to claim, so it is skipped rather than downgraded in.
    // #669 left exactly one suite in production, but a group awaiting migration
    // is still on its predecessor's, so the claim stays group-scoped.
    let mut kp_tuples: Vec<(String, String, Vec<u8>)> = Vec::new();
    let mut skipped_no_suite_kp: Vec<(String, String)> = Vec::new();
    for (uid, did) in &devices_to_claim {
        let claimed: Option<Vec<u8>> =
            crate::commands::mls::ds_claim_key_package(state, uid, Some(did), group_suite)
                .await?;
        match claimed {
            Some(kp) => kp_tuples.push((uid.clone(), did.clone(), kp)),
            None => {
                eprintln!(
                    "[mls] reconcile: no KeyPackage in suite 0x{:04x} for {uid}:{did} — \
                     skipping (never downgrading)",
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
        )?
    };
    let (mut outcome, commit_data_opt) = match staged {
        Some(v) => v,
        None => {
            return Ok(ReconcileOutcome::default());
        }
    };
    outcome.skipped_no_suite_kp = skipped_no_suite_kp;

    // 5. Post commit + welcome to Turso FIRST, then merge locally.
    //
    //    Ordering rationale: if any remote INSERT fails (e.g. libsql hrana
    //    "stream not found" after the slow MLS crypto work evicted the
    //    stream), we must NOT advance the local epoch — otherwise this
    //    client is at epoch N+1 while no other member can see the commit,
    //    producing permanent split-brain. On remote failure we roll back
    //    the local pending commit via `clear_pending_commit` so the next
    //    reconcile recomputes from scratch.
    if let Some(data) = commit_data_opt {
        // Collect metadata about added devices so receivers can verify
        // cross-signing certs before processing the commit.
        let (added_uid, added_dids): (Option<String>, Option<String>) = if outcome.added.is_empty() {
            (None, None)
        } else {
            // All adds in one reconcile commit target devices of different
            // users, so we record the first user and all device IDs. For
            // single-user adds (the common case) this is exact.
            let uid = outcome.added[0].0.clone();
            let dids = outcome
                .added
                .iter()
                .map(|(_, d)| d.as_str())
                .collect::<Vec<_>>()
                .join(",");
            (Some(uid), Some(dids))
        };

        // Try the remote INSERTs on a FRESH connection. The libsql hrana
        // stream captured at the top of this function may have been evicted
        // by the server during the slow MLS crypto work above; reusing it
        // would produce "stream not found" for the critical writes. A fresh
        // connection here is not a retry — it's preventing a stale stream
        // from being our only attempt. On failure, roll back the local
        // pending commit before returning.
        // Build the Welcomes for this commit: one blob per added recipient.
        // The delivery seam now OWNS Welcomes + GroupInfo — it writes all three
        // (commit + GroupInfo + Welcomes) as one unit on a win, atomically
        // through the DS, mirrored by the Direct path. We no longer write
        // Welcomes inline or republish GroupInfo after the merge.
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

        // Claim this epoch and resolve the (possibly ambiguous) outcome. On a
        // win the pending commit is merged; on a loss or a genuine failure it is
        // rolled back. See [`publish_staged_commit`].
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
            // We own this epoch. Fall through to roster banners / voice rotation.
            Ok(PublishOutcome::Won) => {}
            // Another member committed this epoch first; our staged commit is on
            // a branch no one will adopt. It has been rolled back — converge on
            // the winner. The pending invite / membership row persists, so a
            // later reconcile re-applies our change at the new epoch.
            Ok(PublishOutcome::LostRace) => {
                // Converge on the winner through the INTERLEAVED ingesting
                // catch-up rather than a bare commit-only replay. This is a
                // recovery-seam ADVANCE path: applying the winner's commit (or
                // rebuilding via external-join if we forked) moves us past our
                // current epoch, and with `max_past_epochs = 0` a current-epoch
                // inbound message we haven't ingested yet would have its keys
                // discarded — the last advance path not covered by the #440/#441
                // ingest-before-advance invariant. The interleaved catch-up
                // decrypts each epoch's messages BEFORE advancing past it and
                // still reaches head. It re-acquires the per-conversation MLS
                // lock, so we drop ours first; reconcile returns immediately
                // after, so this is equivalent to reconcile finishing and a
                // normal catch-up running.
                drop(_mls_guard);
                if let Err(e) = crate::commands::messages::catch_up_mls_group_interleaved(
                    state,
                    &conversation_id,
                    &actor_user_id,
                )
                .await
                {
                    eprintln!(
                        "[mls] reconcile: converge-after-lost-race failed for {conversation_id}: {e}"
                    );
                }
                return Ok(ReconcileOutcome::default());
            }
            // The commit genuinely did not land; the staged pending commit was
            // already cleared so a later reconcile doesn't merge a commit the
            // remote never saw. Surface the error.
            Err(e) => {
                return Err(e);
            }
        }
    }

    // Voice E2EE: the committer path also advances the local epoch (via
    // `merge_pending_commit` inside `reconcile_group_mls_core`), so the
    // rotation hook must fire here too — otherwise the user who invites or
    // removes someone keeps publishing voice frames under the previous
    // epoch's key while every other member has already rotated.
    if outcome.epoch_after > outcome.epoch_before {
        crate::commands::voice_e2ee::on_mls_epoch_changed(state, &conversation_id).await;
    }

    // Roster-change banners. Fire only when an actual epoch bump happened
    // — `outcome.added` / `outcome.removed` are populated only on the
    // committing branch, and the no-op early-returns above leave them
    // empty. Local emit drives this client's own UI; the room-server
    // publish reaches existing members so their inline timeline picks up
    // the banner without refetching. New joiners don't see banners for
    // themselves because the Welcome path doesn't go through this hook
    // (it lives in `process_pending_welcomes`).
    if outcome.epoch_after > outcome.epoch_before
        && (!outcome.added.is_empty() || !outcome.removed.is_empty())
    {
        use std::collections::HashSet;

        // Per-user device counts BEFORE this commit — derived from the
        // `already_in_tree` snapshot captured at the top of this function.
        let prior_user_ids: HashSet<&String> =
            already_in_tree.iter().map(|(uid, _)| uid).collect();

        // Per-user device counts AFTER this commit. Start from
        // `already_in_tree`, drop removed pairs, add added pairs.
        let mut post_tree: HashSet<(String, String)> = already_in_tree.clone();
        for pair in &outcome.removed {
            post_tree.remove(pair);
        }
        for pair in &outcome.added {
            post_tree.insert(pair.clone());
        }
        let post_user_ids: HashSet<&String> =
            post_tree.iter().map(|(uid, _)| uid).collect();

        let mut joined_user_ids: Vec<String> = Vec::new();
        let mut devices_added: Vec<(String, String)> = Vec::new();
        let mut seen_added_user: HashSet<&String> = HashSet::new();
        for pair in &outcome.added {
            if prior_user_ids.contains(&pair.0) {
                devices_added.push(pair.clone());
            } else if seen_added_user.insert(&pair.0) {
                joined_user_ids.push(pair.0.clone());
            }
        }

        let mut left_user_ids: Vec<String> = Vec::new();
        let mut devices_removed: Vec<(String, String)> = Vec::new();
        let mut seen_removed_user: HashSet<&String> = HashSet::new();
        for pair in &outcome.removed {
            if post_user_ids.contains(&pair.0) {
                devices_removed.push(pair.clone());
            } else if seen_removed_user.insert(&pair.0) {
                left_user_ids.push(pair.0.clone());
            }
        }

        // Local emit. The sink is None during early boot / signed-out;
        // dropping the send is the right behaviour there.
        // The LOCAL sink keeps the full per-user / per-device diff so the acting
        // client renders inline "X joined / X left" banners. Only the cleartext
        // LiveKit broadcast below is minimized (§5.3).
        let sink = state.livekit.lock().await.channel.clone();
        if let Some(ch) = sink {
            let _ = ch.send(crate::realtime::RealtimeEvent::RosterChanged {
                conversation_id: conversation_id.clone(),
                epoch_before: outcome.epoch_before,
                epoch_after: outcome.epoch_after,
                joined_user_ids,
                left_user_ids,
                devices_added,
                devices_removed,
            });
        }

        // Room broadcast. §5.3 metadata minimization: the LiveKit broadcast
        // carries the routing handle + epochs ONLY — the per-user join/left and
        // device id lists are deliberately omitted, since LiveKit forwards them
        // in cleartext and they are a direct social-graph leak. The full lists
        // stay on the LOCAL sink above (where the acting client renders inline
        // banners). Remote peers receive the bare nudge and refetch the member
        // list (`livekit/mod.rs` dispatch → member-list invalidation); they
        // re-derive the diff from the authenticated MLS commit they process, not
        // from this cleartext packet. Non-fatal: a flaky LiveKit blip mustn't
        // fail the reconcile that already committed to Turso.
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
            eprintln!(
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

    // ─── `desired_set` — the diff chokepoint (#679) ───────────────────────────

    fn key(uid: &str, did: &str) -> (String, String) {
        (uid.to_string(), did.to_string())
    }

    fn valid(pairs: &[(&str, &str)]) -> HashSet<(String, String)> {
        pairs.iter().map(|(u, d)| key(u, d)).collect()
    }

    /// The load-bearing #679 case, and the one a `registered_devices`-only fix
    /// does NOT cover: the revoked device still has an unclaimed KeyPackage, so
    /// it arrives via `available_kps`. If only the tree-member half were gated,
    /// the KP half would put it straight back into `desired` — out of
    /// `to_remove`, and re-added if its leaf were already gone.
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

    /// The committer's own device consumed its KeyPackage creating the group, so
    /// it appears ONLY as a tree member. Retaining it is why the tree-member half
    /// of the union exists; losing it would make every reconcile try to evict the
    /// actor.
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

    /// A live device that is in the roster and has a KeyPackage but no leaf yet
    /// is desired — this is the addition path, and it must not be collateral
    /// damage of the new filter.
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

    /// A user may hold several devices and have only one revoked. The live
    /// sibling must survive — otherwise revoking one device would silently log
    /// the user out everywhere.
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

    /// `None` (no snapshot readable) must disable the gate, NOT behave like an
    /// empty set. An empty set means "nothing is valid — remove everything"; if
    /// `None` collapsed to that, a transient `user_device` read failure would
    /// empty the group.
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
