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

use super::group_state::process_pending_commits_locked;
use super::provider::{parse_credential_device_id, parse_credential_user_id, PollisProvider, CS};

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
    epoch: i64,
    our_commit: &[u8],
) -> bool {
    // Read-only commit-log lookup → log_db (falls back to remote_db pre-cutover).
    let conn = match state.log_db.conn().await {
        Ok(c) => c,
        Err(_) => return false,
    };
    let mut rows = match conn
        .query(
            "SELECT commit_data FROM mls_commit_log WHERE conversation_id = ?1 AND epoch = ?2",
            libsql::params![conversation_id.to_string(), epoch],
        )
        .await
    {
        Ok(r) => r,
        Err(_) => return false,
    };
    match rows.next().await {
        Ok(Some(row)) => match row.get::<Vec<u8>>(0) {
            Ok(stored) => stored == our_commit,
            Err(_) => false,
        },
        _ => false,
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
) -> crate::error::Result<()> {
    // Welcomes + the resulting-epoch GroupInfo were already written atomically
    // with the commit by `submit_commit` (Slice 1), so finalizing a won commit
    // only advances the local epoch by merging the still-staged pending commit.
    // Scope the !Send provider/group so neither crosses an await.
    let guard = state.local_db.lock().await;
    if let Some(db) = guard.as_ref() {
        let provider = PollisProvider::new(db.conn());
        let group_id = GroupId::from_slice(conversation_id.as_bytes());
        let mut group = MlsGroup::load(provider.storage(), &group_id)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("mls load for merge: {e}")))?
            .ok_or_else(|| {
                crate::error::Error::Other(anyhow::anyhow!("group missing at merge time"))
            })?;
        group
            .merge_pending_commit(&provider)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("reconcile merge: {e}")))?;
    }
    Ok(())
}

/// Roll back a staged-but-unconfirmed pending commit (best effort; logs on
/// failure). Used only when our commit genuinely did not land.
async fn clear_pending_best_effort(state: &Arc<AppState>, conversation_id: &str) {
    let guard = state.local_db.lock().await;
    if let Some(db) = guard.as_ref() {
        let provider = PollisProvider::new(db.conn());
        let group_id = GroupId::from_slice(conversation_id.as_bytes());
        match MlsGroup::load(provider.storage(), &group_id) {
            Ok(Some(mut group)) => {
                if let Err(e) = group.clear_pending_commit(provider.storage()) {
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
pub fn reconcile_group_mls_core_staged(
    provider: &PollisProvider<'_>,
    signer: &SignatureKeyPair,
    group: &mut MlsGroup,
    roster_user_ids: &std::collections::HashSet<String>,
    available_kps: &[(String, String, KeyPackage)],
    actor_user_id: &str,
    actor_device_id: &str,
    valid_devices: Option<&std::collections::HashSet<(String, String)>>,
) -> crate::error::Result<(ReconcileOutcome, Option<ReconcileCommitData>)> {
    use std::collections::{HashMap, HashSet};

    let epoch_before = group.epoch().as_u64();

    // 1. Actual state: walk the MLS tree.
    let mut actual: HashMap<(String, String), LeafNodeIndex> = HashMap::new();
    for m in group.members() {
        let uid = parse_credential_user_id(&m.credential);
        let did = parse_credential_device_id(&m.credential).unwrap_or_default();
        actual.insert((uid, did), m.index);
    }

    // 2. Build the desired set.
    //    Start with devices that have available KPs…
    let mut desired: HashSet<(String, String)> = available_kps
        .iter()
        .map(|(uid, did, _)| (uid.clone(), did.clone()))
        .collect();
    //    …UNION with existing tree members whose user is still in the roster
    //    AND whose device row still exists (when `valid_devices` is provided).
    //    This prevents removing the committer's own device (which consumed its
    //    KP on creation and has none left) or other devices that are already
    //    correctly in the tree, while still letting a `user_device` deletion
    //    drive a leaf removal (used by device revocation).
    for (uid, did) in actual.keys() {
        if !roster_user_ids.contains(uid) {
            continue;
        }
        if let Some(valid) = valid_devices {
            if !valid.contains(&(uid.clone(), did.clone())) {
                continue;
            }
        }
        desired.insert((uid.clone(), did.clone()));
    }

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
        .propose_adds(add_kps_only.into_iter())
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
pub fn reconcile_group_mls_core(
    provider: &PollisProvider<'_>,
    signer: &SignatureKeyPair,
    group: &mut MlsGroup,
    roster_user_ids: &std::collections::HashSet<String>,
    available_kps: &[(String, String, KeyPackage)],
    actor_user_id: &str,
    actor_device_id: &str,
    valid_devices: Option<&std::collections::HashSet<(String, String)>>,
) -> crate::error::Result<(ReconcileOutcome, Option<ReconcileCommitData>)> {
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
    // function; the lost-race recovery below calls the unlocked
    // `process_pending_commits_locked` rather than re-acquiring.
    let _mls_guard = state.mls_group_lock(&conversation_id).await;

    let conn = state.remote_db.conn().await?;

    // 1. Determine roster: group_member + pending invitees, or dm_channel_member.
    //    Pending invitees are included so their devices get a Welcome at invite
    //    time — the acceptor can join the MLS group without requiring any other
    //    member to be online simultaneously.
    let mut roster_user_ids = std::collections::HashSet::new();
    {
        let mut rows = conn
            .query(
                "SELECT user_id FROM group_member WHERE group_id = ?1",
                libsql::params![conversation_id.clone()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            roster_user_ids.insert(row.get::<String>(0)?);
        }
    }
    // Include pending invitees so they receive a Welcome pre-acceptance.
    {
        let mut rows = conn
            .query(
                "SELECT invitee_id FROM group_invite WHERE group_id = ?1",
                libsql::params![conversation_id.clone()],
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
                libsql::params![conversation_id.clone()],
            )
            .await?;
        while let Some(row) = rows.next().await? {
            roster_user_ids.insert(row.get::<String>(0)?);
        }
    }

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
        let safe_ids: Vec<String> = roster_user_ids
            .iter()
            .map(|id| id.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect::<String>())
            .collect();
        if !safe_ids.is_empty() {
            let in_clause = safe_ids.iter().map(|id| format!("'{id}'")).collect::<Vec<_>>().join(",");
            let query = format!(
                "SELECT d.user_id, d.device_id FROM user_device d \
                 WHERE d.user_id IN ({in_clause}) \
                 AND EXISTS ( \
                     SELECT 1 FROM mls_key_package kp \
                     WHERE kp.user_id = d.user_id AND kp.device_id = d.device_id AND kp.claimed = 0 \
                 )"
            );
            let mut rows = conn.query(&query, ()).await?;
            while let Some(row) = rows.next().await? {
                device_pairs.push((row.get::<String>(0)?, row.get::<String>(1)?));
            }
        }
    }

    // 2b. Snapshot of every (user_id, device_id) pair still registered in
    //     `user_device` for the current roster. Used by reconcile to drop
    //     leaves whose device row was revoked even though the user is still
    //     a roster member (single-device revoke flow).
    let mut valid_devices: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    {
        let safe_ids: Vec<String> = roster_user_ids
            .iter()
            .map(|id| id.chars().filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_').collect::<String>())
            .collect();
        if !safe_ids.is_empty() {
            let in_clause = safe_ids.iter().map(|id| format!("'{id}'")).collect::<Vec<_>>().join(",");
            let query = format!(
                "SELECT user_id, device_id FROM user_device WHERE user_id IN ({in_clause})"
            );
            let mut rows = conn.query(&query, ()).await?;
            while let Some(row) = rows.next().await? {
                valid_devices.insert((row.get::<String>(0)?, row.get::<String>(1)?));
            }
        }
    }

    let actor_device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .unwrap_or_default();

    // 3. Peek at the current tree to learn which devices are already members.
    //    This lets us skip claiming KPs for devices that don't need to be added,
    //    avoiding unnecessary KP exhaustion on repeated reconciles.
    let already_in_tree: std::collections::HashSet<(String, String)> = {
        let guard = state.local_db.lock().await;
        let db = match guard.as_ref() {
            Some(db) => db,
            None => {
                return Ok(ReconcileOutcome::default());
            }
        };
        let provider = PollisProvider::new(db.conn());
        let group_id = GroupId::from_slice(conversation_id.as_bytes());
        match MlsGroup::load(provider.storage(), &group_id) {
            Ok(Some(group)) => group
                .members()
                .map(|m| {
                    let uid = parse_credential_user_id(&m.credential);
                    let did = parse_credential_device_id(&m.credential).unwrap_or_default();
                    (uid, did)
                })
                .collect(),
            _ => {
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
    // DS seam: a KP claim is a `claimed = 1` write on a *peer's* row, so under a
    // read-only Turso token the direct `UPDATE … RETURNING` fails — route it
    // through the Delivery Service when configured. Either path yields the SAME
    // `key_package` bytes (the DS runs the identical SQL) and the SAME no-KP
    // control flow: a target device with no unclaimed package is simply skipped
    // (the DS replies 404 → `Ok(None)`, mirroring the direct path's empty
    // `RETURNING`).
    let mut kp_tuples: Vec<(String, String, Vec<u8>)> = Vec::new();
    for (uid, did) in &devices_to_claim {
        let claimed: Option<Vec<u8>> = match state.config.pollis_delivery_url.as_deref() {
            Some(_) => crate::commands::mls::ds_claim_key_package(state, uid, Some(did)).await?,
            None => {
                let mut rows = conn
                    .query(
                        "UPDATE mls_key_package \
                         SET claimed = 1 \
                         WHERE ref_hash = ( \
                             SELECT ref_hash FROM mls_key_package \
                             WHERE user_id = ?1 AND device_id = ?2 AND claimed = 0 \
                             ORDER BY created_at ASC LIMIT 1 \
                         ) \
                         RETURNING key_package",
                        libsql::params![uid.clone(), did.clone()],
                    )
                    .await?;
                match rows.next().await? {
                    Some(row) => Some(row.get::<Vec<u8>>(0)?),
                    None => None,
                }
            }
        };
        if let Some(kp) = claimed {
            kp_tuples.push((uid.clone(), did.clone(), kp));
        }
    }

    // 5. Validate KPs and call the sync core under the local_db lock.
    let (outcome, commit_data_opt) = {
        let guard = state.local_db.lock().await;
        let db = match guard.as_ref() {
            Some(db) => db,
            None => {
                return Ok(ReconcileOutcome::default());
            }
        };
        let provider = PollisProvider::new(db.conn());

        // Load group — early return if missing.
        let group_id = GroupId::from_slice(conversation_id.as_bytes());
        let group_opt = MlsGroup::load(provider.storage(), &group_id)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("mls load: {e}")))?;
        let mut group = match group_opt {
            Some(g) => g,
            None => {
                return Ok(ReconcileOutcome::default());
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
            CS.signature_algorithm(),
        )
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("signer not found in mls_kv")))?;

        // Resolve pending commit.
        group
            .merge_pending_commit(&provider)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("merge pending: {e}")))?;

        // Validate KPs.
        let mut available_kps: Vec<(String, String, KeyPackage)> = Vec::new();
        for (uid, did, kp_raw) in &kp_tuples {
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
            &provider,
            &signer,
            &mut group,
            &roster_user_ids,
            &available_kps,
            &actor_user_id,
            &actor_device_id,
            Some(&valid_devices),
        )?
    };

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

        let remote_result: crate::error::Result<SubmitOutcome> = async {
            // Claim this epoch through the delivery seam (Direct today, the
            // Delivery Service once POLLIS_DELIVERY_URL is set). On a win, the
            // seam also writes the resulting-epoch GroupInfo + the added
            // members' Welcomes — atomically with the commit. LostRace →
            // another member committed `epoch_before` first; report it (the
            // caller rolls back the local pending commit) instead of forking —
            // and nothing was written, so no Welcome points at a doomed branch.
            match super::delivery::submit_commit(
                state,
                &conversation_id,
                outcome.epoch_before as i64,
                &actor_user_id,
                &data.commit_bytes,
                added_uid.as_deref(),
                added_dids.as_deref(),
                data.group_info_bytes.as_deref(),
                &welcomes,
            )
            .await?
            {
                super::delivery::SubmitResult::LostRace => Ok(SubmitOutcome::LostRace),
                super::delivery::SubmitResult::Committed => Ok(SubmitOutcome::Committed),
            }
        }
        .await;

        // Decide commit-or-abort. The submit outcome can be ambiguous: a network
        // error may mean the commit landed and only the RESPONSE was lost, and a
        // `LostRace` can be a stale retry of our OWN already-accepted commit. The
        // canonical log is the arbiter — if our exact commit is at this epoch we
        // WON and must adopt it (merge + Welcomes + GroupInfo) rather than roll
        // back and wedge (issue #411). Roll back only when it truly didn't land.
        enum Resolution {
            Won,
            LostRace,
            Failed(crate::error::Error),
        }
        let epoch = outcome.epoch_before as i64;
        let resolution = match remote_result {
            Ok(SubmitOutcome::Committed) => Resolution::Won,
            Ok(SubmitOutcome::LostRace) => {
                if our_commit_is_canonical(state, &conversation_id, epoch, &data.commit_bytes).await {
                    eprintln!(
                        "[mls] reconcile: LostRace at epoch {epoch} for {conversation_id} but our commit is canonical — adopting (lost success-response)"
                    );
                    Resolution::Won
                } else {
                    Resolution::LostRace
                }
            }
            Err(e) => {
                if our_commit_is_canonical(state, &conversation_id, epoch, &data.commit_bytes).await {
                    eprintln!(
                        "[mls] reconcile: submit errored but our commit is canonical at epoch {epoch} for {conversation_id} — adopting (lost response): {e}"
                    );
                    Resolution::Won
                } else {
                    Resolution::Failed(e)
                }
            }
        };

        match resolution {
            // We own this epoch (confirmed, or discovered canonical after an
            // ambiguous failure). Write Welcomes, merge to advance, republish
            // GroupInfo — then fall through to roster banners / voice rotation.
            Resolution::Won => {
                finalize_won_commit(state, &conversation_id).await?;
            }
            // Another member committed this epoch first; our staged commit is on
            // a branch no one will adopt. Roll it back and converge on the
            // winner. The pending invite / membership row persists, so a later
            // reconcile re-applies our change at the new epoch.
            Resolution::LostRace => {
                eprintln!(
                    "[mls] reconcile: lost epoch {epoch} race for {conversation_id} — rolling back local pending commit and converging on the winner"
                );
                clear_pending_best_effort(state, &conversation_id).await;
                if let Err(e) =
                    process_pending_commits_locked(state, &conversation_id, &actor_user_id).await
                {
                    eprintln!(
                        "[mls] reconcile: converge-after-lost-race failed for {conversation_id}: {e}"
                    );
                }
                return Ok(ReconcileOutcome::default());
            }
            // The commit genuinely did not land. Clear the staged pending commit
            // so a later reconcile doesn't merge a commit the remote never saw,
            // then surface the error.
            Resolution::Failed(e) => {
                eprintln!(
                    "[mls] reconcile: remote persist failed for {conversation_id}, clearing local pending commit: {e}"
                );
                clear_pending_best_effort(state, &conversation_id).await;
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
        let sink = state.livekit.lock().await.channel.clone();
        if let Some(ch) = sink {
            let _ = ch.send(crate::realtime::RealtimeEvent::RosterChanged {
                conversation_id: conversation_id.clone(),
                epoch_before: outcome.epoch_before,
                epoch_after: outcome.epoch_after,
                joined_user_ids: joined_user_ids.clone(),
                left_user_ids: left_user_ids.clone(),
                devices_added: devices_added.clone(),
                devices_removed: devices_removed.clone(),
            });
        }

        // Room broadcast. Existing members already in the conversation
        // room receive this data packet, parse the diff client-side
        // (see `livekit/mod.rs` data-packet dispatch), and render the
        // banner. Non-fatal: a flaky LiveKit blip mustn't fail the
        // reconcile that already committed to Turso.
        if let Err(e) = crate::commands::livekit::publish_to_room_server(
            &state.config,
            &conversation_id,
            serde_json::json!({
                "type": "roster_changed",
                "conversation_id": conversation_id.clone(),
                "epoch_before": outcome.epoch_before,
                "epoch_after": outcome.epoch_after,
                "joined_user_ids": joined_user_ids,
                "left_user_ids": left_user_ids,
                "devices_added": devices_added,
                "devices_removed": devices_removed,
            }),
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

pub async fn reconcile_group_mls(
    state: &Arc<AppState>,
    conversation_id: String,
    actor_user_id: String,
) -> crate::error::Result<()> {
    reconcile_group_mls_impl(state, &conversation_id, &actor_user_id).await?;
    Ok(())
}
