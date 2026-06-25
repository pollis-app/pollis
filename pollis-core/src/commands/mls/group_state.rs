//! MLS group lifecycle: create / load / forget / encrypt / decrypt /
//! commit processing / external join / GroupInfo publishing.

use openmls::prelude::group_info::VerifiableGroupInfo;
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::OpenMlsProvider;

use std::sync::Arc;
use tls_codec::{Deserialize as TlsDeserialize, Serialize as TlsSerialize};

use crate::error::Result;
use crate::state::AppState;

use super::device::{load_or_create_device_signer, verify_added_devices, VerifyOutcome};
use super::provider::{make_credential, PollisProvider, CS};

// ── GroupInfo publishing ─────────────────────────────────────────────────────

/// Export a fresh `GroupInfo` for the given conversation and upsert it
/// into the remote `mls_group_info` table. Called by every device that
/// merges a commit (the originator right after `merge_pending_commit`,
/// receivers right after `merge_staged_commit`).
///
/// The row is conversation-scoped and only overwritten with a STRICTLY
/// greater epoch, so concurrent writers at the same epoch are idempotent
/// and receivers don't waste work once the committer has already
/// published.
///
/// No-op if:
///   - the device has no local MLS group for this conversation
///   - the device has no `account_id_key` (pre-enrollment)
///
/// This function is the prerequisite for the Secret Key recovery path:
/// a brand-new device uses the stored `GroupInfo` to construct an MLS
/// external commit joining the group, without needing a Welcome.
pub async fn publish_group_info(
    state: &Arc<AppState>,
    conversation_id: &str,
) -> crate::error::Result<()> {
    // Sync scope: load the local group, recover the signer, export a
    // GroupInfo, and TLS-serialize it. Nothing !Send crosses await.
    let device_id_opt = state.device_id.lock().await.clone();
    let Some(device_id) = device_id_opt else {
        return Ok(());
    };

    let exported: Option<(u64, Vec<u8>)> = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else {
            return Ok(());
        };
        let provider = PollisProvider::new(db.conn());
        let (group, signer) = match load_group_with_signer(&provider, conversation_id) {
            Ok(pair) => pair,
            Err(_) => return Ok(()),
        };
        let epoch = group.epoch().as_u64();
        let msg = match group.export_group_info(provider.crypto(), &signer, true) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("[mls] publish_group_info: export failed for {conversation_id}: {e}");
                return Ok(());
            }
        };
        let bytes = msg
            .tls_serialize_detached()
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("group_info serialize: {e}")))?;
        Some((epoch, bytes))
    };

    let Some((epoch, bytes)) = exported else {
        return Ok(());
    };

    let conn = state.remote_db.conn().await?;
    conn.execute(
        "INSERT INTO mls_group_info \
         (conversation_id, epoch, group_info, updated_at, updated_by_device_id) \
         VALUES (?1, ?2, ?3, datetime('now'), ?4) \
         ON CONFLICT(conversation_id) DO UPDATE SET \
             epoch = excluded.epoch, \
             group_info = excluded.group_info, \
             updated_at = datetime('now'), \
             updated_by_device_id = excluded.updated_by_device_id \
         WHERE excluded.epoch > mls_group_info.epoch",
        libsql::params![conversation_id, epoch as i64, bytes, device_id],
    )
    .await?;

    Ok(())
}

// ── External-commit joining ──────────────────────────────────────────────────

/// Join an existing MLS group via external commit, using the latest
/// `GroupInfo` blob stored server-side in `mls_group_info`. The new
/// device becomes a full member of the group at the epoch *after* the
/// one carried in the GroupInfo.
///
/// Used by the Secret Key recovery path: when a new device recovers
/// `account_id_key` without any sibling device online to issue a
/// Welcome, it fetches each of the user's groups' GroupInfo and
/// externally commits into them. The commit is posted to
/// `mls_commit_log` so existing members will merge it on their next
/// `process_pending_commits` pass.
///
/// Safety note: this path does NOT currently pass through the outbound
/// cross-signing cert check. Existing members that implement the
/// step-3b inbound cert verification will reject external-join commits
/// from devices whose cert doesn't chain to the user's
/// `account_id_pub` — which is exactly the desired behavior.
pub async fn external_join_group(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    let _guard = state.mls_group_lock(conversation_id).await;
    external_join_group_inner(state, conversation_id, user_id).await
}

/// Result of a single external-join attempt — see [`external_join_attempt`].
enum ExternalJoinResult {
    /// Our external commit won its epoch and was persisted.
    Joined,
    /// Another member already committed at the target epoch; our freshly
    /// built local branch is doomed and must be discarded before retrying.
    LostRace,
}

/// Body of [`external_join_group`]. Assumes the caller already holds the
/// per-conversation MLS lock (`state.mls_group_lock`).
///
/// Submits the external commit as a compare-and-swap on the target epoch (see
/// [`external_join_attempt`]). On a lost race we discard the doomed local
/// branch, let the winner republish GroupInfo at the advanced epoch, and retry
/// — bounded — from the new epoch. This is what stops two concurrent joins
/// from forking the group.
pub(crate) async fn external_join_group_inner(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    const MAX_JOIN_ATTEMPTS: u32 = 5;
    for attempt in 0..MAX_JOIN_ATTEMPTS {
        match external_join_attempt(state, conversation_id, user_id).await? {
            ExternalJoinResult::Joined => return Ok(()),
            ExternalJoinResult::LostRace => {
                eprintln!(
                    "[mls] external_join_group: lost epoch race for {conversation_id} \
                     (attempt {}/{MAX_JOIN_ATTEMPTS}) — discarding local branch and retrying",
                    attempt + 1
                );
                // Drop the doomed branch we just built so the next attempt
                // re-joins cleanly from the advanced GroupInfo.
                let _ = forget_local_mls_group(state, conversation_id).await;
                // Brief backoff so the winner can publish GroupInfo at the new
                // epoch before we re-read it.
                tokio::time::sleep(std::time::Duration::from_millis(
                    100 * (attempt as u64 + 1),
                ))
                .await;
            }
        }
    }
    Err(crate::error::Error::Other(anyhow::anyhow!(
        "external-join for {conversation_id}: epoch contention, exhausted {MAX_JOIN_ATTEMPTS} attempts"
    )))
}

/// One external-join attempt: read the current GroupInfo, build an external
/// commit against it, and try to claim its epoch in `mls_commit_log` via
/// `ON CONFLICT(conversation_id, epoch) DO NOTHING`. Returns [`ExternalJoinResult`].
async fn external_join_attempt(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
) -> crate::error::Result<ExternalJoinResult> {
    let device_id = state
        .device_id
        .lock()
        .await
        .clone()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("device_id not set")))?;

    // 1. Fetch the stored GroupInfo for this conversation.
    let (group_info_bytes, stored_epoch): (Vec<u8>, i64) = {
        let conn = state.remote_db.conn().await?;
        let mut rows = conn
            .query(
                "SELECT group_info, epoch FROM mls_group_info WHERE conversation_id = ?1",
                libsql::params![conversation_id],
            )
            .await?;
        match rows.next().await? {
            Some(row) => (row.get(0)?, row.get(1)?),
            None => {
                return Err(crate::error::Error::Other(anyhow::anyhow!(
                    "no GroupInfo stored for {conversation_id} — cannot external-join"
                )))
            }
        }
    };

    // 2. Run the external commit inside the local_db sync scope. Capture both
    //    the commit and its resulting-epoch GroupInfo so they land atomically
    //    through the delivery seam (Slice 1). Welcomes are empty — an external
    //    join only adds self.
    let (commit_bytes, new_group_info_bytes): (Vec<u8>, Option<Vec<u8>>) = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let provider = PollisProvider::new(db.conn());

        let mut env_reader: &[u8] = &group_info_bytes;
        let msg_in = MlsMessageIn::tls_deserialize(&mut env_reader).map_err(|e| {
            crate::error::Error::Other(anyhow::anyhow!(
                "stored group_info envelope failed to deserialize: {e}"
            ))
        })?;
        let verifiable_group_info: VerifiableGroupInfo = match msg_in.extract() {
            MlsMessageBodyIn::GroupInfo(gi) => gi,
            other => {
                return Err(crate::error::Error::Other(anyhow::anyhow!(
                    "expected GroupInfo in mls_group_info, got {:?}",
                    std::mem::discriminant(&other)
                )));
            }
        };

        // Load (or create) this device's stable MLS signing keypair.
        let (sig_keys, sig_pub_bytes) =
            load_or_create_device_signer(&provider, user_id, &device_id)?;

        let credential = make_credential(user_id, &device_id);
        let sig_pub = OpenMlsSignaturePublicKey::new(
            sig_pub_bytes.into(),
            CS.signature_algorithm(),
        )
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig pub key: {e}")))?;
        let cred_with_key = CredentialWithKey {
            credential,
            signature_key: sig_pub.into(),
        };

        // Drop any stale local group with the same ID so the external
        // commit builder doesn't collide.
        let group_id = GroupId::from_slice(conversation_id.as_bytes());
        if let Ok(Some(mut old)) = MlsGroup::load(provider.storage(), &group_id) {
            let _ = old.delete(provider.storage());
        }

        let join_config = MlsGroupJoinConfig::builder()
            .use_ratchet_tree_extension(true)
            .build();

        let (_joined_group, commit_bundle) = MlsGroup::external_commit_builder()
            .with_config(join_config)
            .build_group(&provider, verifiable_group_info, cred_with_key)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!(
                "external commit build_group: {e}"
            )))?
            .load_psks(provider.storage())
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!(
                "external commit load_psks: {e}"
            )))?
            .create_group_info(true)
            .build(provider.rand(), provider.crypto(), &sig_keys, |_| true)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!(
                "external commit build: {e}"
            )))?
            .finalize(&provider)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!(
                "external commit finalize: {e}"
            )))?;

        let (commit_msg, _welcome_msg, new_group_info) = commit_bundle.into_contents();
        let commit_bytes = commit_msg
            .tls_serialize_detached()
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("commit serialize: {e}")))?;
        let group_info_bytes = match new_group_info {
            Some(gi) => Some(
                gi.tls_serialize_detached().map_err(|e| {
                    crate::error::Error::Other(anyhow::anyhow!("external commit group_info serialize: {e}"))
                })?,
            ),
            None => None,
        };
        (commit_bytes, group_info_bytes)
    };

    // 3. Claim this epoch in mls_commit_log via compare-and-swap. If another
    //    member already committed `stored_epoch`, the conflict makes this a
    //    no-op (0 rows) and we report a lost race — the branch we just built
    //    locally is doomed and the caller will discard it and retry.
    // Submit through the delivery seam (Direct today, the Delivery Service once
    // POLLIS_DELIVERY_URL is set). LostRace → another member committed this
    // epoch first; discard our doomed local branch and retry.
    // Resolve the outcome against the canonical log (issue #411). A network
    // error may mean our external commit LANDED and only the response was lost,
    // and a stale `LostRace` can be a retry of our own accepted commit — in both
    // cases our exact commit bytes at `stored_epoch` prove we won and must keep
    // the locally-finalized join, not discard it and wedge.
    match super::delivery::submit_commit(
        state,
        conversation_id,
        stored_epoch,
        user_id,
        &commit_bytes,
        Some(user_id),
        Some(&device_id),
        new_group_info_bytes.as_deref(),
        // External join adds only self — no Welcomes to deliver.
        &[],
    )
    .await
    {
        Ok(super::delivery::SubmitResult::Committed) => {}
        Ok(super::delivery::SubmitResult::LostRace) => {
            if super::reconcile::our_commit_is_canonical(
                state,
                conversation_id,
                stored_epoch,
                &commit_bytes,
            )
            .await
            {
                eprintln!(
                    "[mls] external_join: LostRace at epoch {stored_epoch} for {conversation_id} but our commit is canonical — adopting (lost success-response)"
                );
            } else {
                return Ok(ExternalJoinResult::LostRace);
            }
        }
        Err(e) => {
            if super::reconcile::our_commit_is_canonical(
                state,
                conversation_id,
                stored_epoch,
                &commit_bytes,
            )
            .await
            {
                eprintln!(
                    "[mls] external_join: submit errored but our commit is canonical at epoch {stored_epoch} for {conversation_id} — adopting (lost response): {e}"
                );
            } else {
                // Genuine failure — discard the locally-finalized (orphaned)
                // join group for symmetry with the LostRace path (the caller
                // only forgets on LostRace), then surface the error.
                let _ = forget_local_mls_group(state, conversation_id).await;
                return Err(e);
            }
        }
    }

    // 4. Refresh the stored GroupInfo at the new epoch so any NEXT
    //    new device joining via this same path sees the up-to-date
    //    tree.
    if let Err(e) = publish_group_info(state, conversation_id).await {
        eprintln!(
            "[mls] external_join_group: publish_group_info failed (non-fatal): {e}"
        );
    }

    eprintln!(
        "[mls] external_join_group: {user_id}:{device_id} joined {conversation_id} from epoch {stored_epoch}"
    );

    Ok(ExternalJoinResult::Joined)
}

// ── Phase 3: Group / DM creation ─────────────────────────────────────────────

/// Internal: create a fresh MLS group for `conversation_id` with
/// `creator_user_id` as the sole initial member.  Group state is persisted in
/// the local `mls_kv` table via `MlsStore`.
///
/// `use_ratchet_tree_extension(true)` is set so that Welcome messages sent in
/// Phase 4 embed the full ratchet tree — recipients can join without a separate
/// out-of-band tree download.
pub async fn init_mls_group(
    state: &Arc<AppState>,
    conversation_id: &str,
    creator_user_id: &str,
) -> Result<()> {
    let device_id = state.device_id.lock().await.clone()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("device_id not set")))?;

    // Scope the local_db guard so it is dropped before the async
    // publish_group_info call below (which re-acquires it).
    {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let provider = PollisProvider::new(db.conn());

        let (sig_keys, sig_pub_bytes) =
            load_or_create_device_signer(&provider, creator_user_id, &device_id)?;

        let credential = make_credential(creator_user_id, &device_id);
        let sig_pub = OpenMlsSignaturePublicKey::new(
            sig_pub_bytes.into(),
            CS.signature_algorithm(),
        ).map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig pub key: {e}")))?;
        let cred_with_key = CredentialWithKey {
            credential,
            signature_key: sig_pub.into(),
        };

        let group_id = GroupId::from_slice(conversation_id.as_bytes());

        // Delete any stale group with the same ID so the create below never
        // collides.  This is a no-op on first creation and essential during
        // repair (where the old group still exists but is broken/outdated).
        if let Ok(Some(mut old)) = MlsGroup::load(provider.storage(), &group_id) {
            let _ = old.delete(provider.storage());
        }

        let config = MlsGroupCreateConfig::builder()
            .ciphersuite(CS)
            .use_ratchet_tree_extension(true)
            .build();

        MlsGroup::new_with_group_id(&provider, &sig_keys, &config, group_id, cred_with_key)
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("create mls group: {e}")))?;
    }

    // Publish the epoch-0 GroupInfo so a future device enrolling via the
    // Secret Key path can join this group via external commit.
    if let Err(e) = publish_group_info(state, conversation_id).await {
        eprintln!("[mls] init_mls_group: publish_group_info failed (non-fatal): {e}");
    }

    Ok(())
}

/// Create a fresh MLS group for `conversation_id` (a channel or DM ULID).
/// The creator becomes the sole initial member.  Other users are added via
/// `reconcile_group_mls`.
pub async fn create_mls_group(
    state: &Arc<AppState>,
    conversation_id: String,
    creator_user_id: String,
) -> Result<()> {
    init_mls_group(state, &conversation_id, &creator_user_id).await
}

// ── Phase 4: Member changes ───────────────────────────────────────────────────

/// Reload an existing MLS group from storage and recover the signer.
///
/// Returns `(MlsGroup, SignatureKeyPair)` ready for use with the provider whose
/// connection was passed to `PollisProvider::new`.
pub(super) fn load_group_with_signer(
    provider: &PollisProvider<'_>,
    conversation_id: &str,
) -> crate::error::Result<(MlsGroup, SignatureKeyPair)> {
    let group_id = GroupId::from_slice(conversation_id.as_bytes());

    let mut group = MlsGroup::load(provider.storage(), &group_id)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("mls load: {e}")))?
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!(
            "MLS group not found for conversation {conversation_id}"
        )))?;

    // Retrieve the signature public key stored in the group's leaf node, then
    // read back the full keypair from mls_kv.
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

    // Resolve any in-flight pending commit so the group is operational before
    // the caller performs new operations.
    group
        .merge_pending_commit(provider)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("merge pending: {e}")))?;

    Ok((group, signer))
}

/// Wipe all local MLS state for a group without publishing a commit.
///
/// Used when the local user leaves a group.  MLS does not allow a member to
/// commit their own removal (`remove_members` with self as target errors), so
/// instead we just delete the local group epoch.  The remaining members still
/// have this user in their group state until the next admin-issued commit, but
/// forward secrecy ensures the leaver cannot decrypt messages after the next
/// epoch advance.
pub async fn forget_local_mls_group(
    state: &Arc<AppState>,
    group_id: &str,
) -> crate::error::Result<()> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
    })?;
    let provider = PollisProvider::new(db.conn());
    let mls_group_id = GroupId::from_slice(group_id.as_bytes());

    if let Ok(Some(mut group)) = MlsGroup::load(provider.storage(), &mls_group_id) {
        group.delete(provider.storage())
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("mls delete group: {e}")))?;
    }
    // If the group wasn't found locally, nothing to clean up.
    Ok(())
}

/// Apply any commits from `mls_commit_log` that this member has not yet seen.
///
/// Reads rows where `epoch >= current_local_epoch` in ascending order, applies
/// each commit, and advances the local epoch.  An epoch gap (unexpected jump)
/// stops processing and logs an error — this indicates a missed or reordered
/// commit that would require manual intervention in a production system.
///
/// `mls_group_id` must already be resolved (group_id for channels,
/// conversation_id for DMs).
/// Ensure this device has a local MLS group at the latest epoch for
/// `mls_group_id`. Processes any pending commits from the commit log.
/// If the local group is missing, evicted, or unrecoverably behind,
/// falls back to external-join using the published GroupInfo.
///
/// `user_id` is needed for the external-join fallback.
///
/// Acquires the per-conversation MLS lock for the duration so concurrent
/// callers on this device (send, channel/DM ingest, the realtime inbox
/// handler) can't race into a commit fork. The actual work is in
/// [`process_pending_commits_locked`]; callers that already hold the lock
/// (e.g. reconcile's lost-race recovery) must call that directly.
pub async fn process_pending_commits_inner(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    let _guard = state.mls_group_lock(mls_group_id).await;
    process_pending_commits_locked(state, mls_group_id, user_id).await
}

/// Whether this device's `user_device` row still exists remotely. Gates the
/// external-join *recovery* paths so a revoked device (row deleted by
/// `revoke_device`) can't auto-rejoin a group it was removed from — which would
/// squat an epoch and wedge the group under the UNIQUE(conversation_id, epoch)
/// constraint.
///
/// Fails OPEN (returns true) on a missing device_id or any query error: a
/// transient inability to check must never lock a legitimate device out of
/// recovery. The authoritative, fail-closed control is the inbound self-add
/// rejection in `process_pending_commits_locked`; this is the cooperative
/// "don't even try to climb back in" half.
async fn local_device_registered(state: &Arc<AppState>, user_id: &str) -> bool {
    let device_id = match state.device_id.lock().await.clone() {
        Some(d) => d,
        None => return true,
    };
    let conn = match state.remote_db.conn().await {
        Ok(c) => c,
        Err(_) => return true,
    };
    match conn
        .query(
            "SELECT 1 FROM user_device \
             WHERE user_id = ?1 AND device_id = ?2 AND revoked_at IS NULL",
            libsql::params![user_id, device_id],
        )
        .await
    {
        Ok(mut rows) => matches!(rows.next().await, Ok(Some(_))),
        Err(_) => true,
    }
}

/// Body of [`process_pending_commits_inner`]. Assumes the caller already holds
/// the per-conversation MLS lock (`state.mls_group_lock`).
pub(crate) async fn process_pending_commits_locked(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    // 1. Get the current epoch from the local group.
    let has_group = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let provider = PollisProvider::new(db.conn());
        let group_id = GroupId::from_slice(mls_group_id.as_bytes());
        MlsGroup::load(provider.storage(), &group_id)
            .ok()
            .flatten()
            .map(|g| g.epoch().as_u64())
    };
    let initial_epoch = match has_group {
        Some(epoch) => epoch,
        None => {
            // No local group — external-join to create one, UNLESS this
            // device has been revoked (its `user_device` row is gone). A
            // revoked device must not climb back in: doing so squats an epoch
            // and, under the UNIQUE(conversation_id, epoch) constraint, would
            // wedge the group. Lock already held by the wrapper, so call the
            // unlocked inner variant.
            if local_device_registered(state, user_id).await {
                if let Err(e) = external_join_group_inner(state, mls_group_id, user_id).await {
                    eprintln!("[mls] process_pending_commits: no local group for {mls_group_id}, external-join failed: {e}");
                }
            } else {
                eprintln!("[mls] process_pending_commits: device for {user_id} is no longer registered — not external-joining {mls_group_id} (revoked)");
            }
            return Ok(());
        }
    };

    // 2. Fetch pending commits from remote, along with the add-metadata
    //    columns (`added_user_id`, `added_device_ids`) so we can verify
    //    cross-signing certs BEFORE calling `process_message`. Collected
    //    into an owned Vec so the `rows` cursor is dropped before any
    //    local-DB await below.
    let conn = state.remote_db.conn().await?;
    let mut rows = conn.query(
        "SELECT seq, epoch, commit_data, added_user_id, added_device_ids, sender_id \
         FROM mls_commit_log \
         WHERE conversation_id = ?1 AND epoch >= ?2 \
         ORDER BY epoch ASC, seq ASC",
        libsql::params![mls_group_id, initial_epoch as i64],
    ).await?;

    #[derive(Debug)]
    struct PendingCommit {
        seq: i64,
        epoch: i64,
        commit_data: Vec<u8>,
        added_user_id: Option<String>,
        added_device_ids: Vec<String>,
        sender_id: Option<String>,
    }

    let mut pending: Vec<PendingCommit> = Vec::new();
    while let Some(row) = rows.next().await? {
        let seq: i64 = row.get(0)?;
        let epoch: i64 = row.get(1)?;
        let data: Vec<u8> = row.get(2)?;
        let added_user_id: Option<String> = row.get::<Option<String>>(3).ok().flatten();
        let ids_csv: Option<String> = row.get::<Option<String>>(4).ok().flatten();
        let sender_id: Option<String> = row.get::<Option<String>>(5).ok().flatten();
        let added_device_ids: Vec<String> = ids_csv
            .as_deref()
            .map(|s| {
                s.split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        pending.push(PendingCommit {
            seq,
            epoch,
            commit_data: data,
            added_user_id,
            added_device_ids,
            sender_id,
        });
    }
    drop(rows);

    // 3. Apply each commit in epoch order. For any commit carrying add
    //    metadata, verify every added device's cross-signing cert
    //    against the user's account_id_pub BEFORE touching the group
    //    state.
    let mut current_epoch = initial_epoch;
    let mut any_applied = false;
    for commit in pending {
        if commit.epoch as u64 != current_epoch {
            // The commit that would bridge `current_epoch` -> next is missing
            // from the log while a HIGHER epoch is present. The commit log is
            // append-only and Turso reads are consistent, so a missing-but-
            // surpassed epoch means that commit is permanently gone (historic
            // bug that deleted a row, pruning, etc.) — there is nothing to
            // replay and we'd wedge here forever. Drop the stale local group so
            // the recovery block at the end external-joins us onto the current
            // published epoch instead. forget only drops MLS crypto state, not
            // decrypted message history (that lives in the local `message`
            // table).
            eprintln!(
                "[mls] process_pending_commits: epoch gap for {mls_group_id}: \
                 expected {current_epoch}, got {} — dropping local group to recover via external join",
                commit.epoch
            );
            let _ = forget_local_mls_group(state, mls_group_id).await;
            break;
        }

        // ── Inbound cert verification ────────────────────────────
        if let Some(ref added_user_id) = commit.added_user_id {
            let outcome = match verify_added_devices(
                &conn,
                added_user_id,
                &commit.added_device_ids,
            )
            .await
            {
                Ok(o) => o,
                Err(e) => {
                    eprintln!(
                        "[mls] process_pending_commits: cert verification error for {mls_group_id}: {e} — treating as AbsentRetry"
                    );
                    VerifyOutcome::AbsentRetry
                }
            };

            match outcome {
                VerifyOutcome::Verified => {}
                VerifyOutcome::Revoked => {
                    // The added device failed verification (revoked tombstone
                    // OR bad cert chain). We used to DELETE the commit row for a
                    // self-add to free the UNIQUE(conversation_id, epoch) slot —
                    // but that broke the append-only invariant the entire MLS
                    // replay depends on. A commit that won the epoch CAS is
                    // canonical and immutable: a member who had already applied
                    // it advanced past it, while a laggard reading the log AFTER
                    // the delete saw a permanent hole and wedged forever (prod
                    // incident: ELECTRON group, epoch 11 — dan applied it, ants
                    // wedged). Deleting a commit that any member may have applied
                    // forks the group; it is never safe.
                    //
                    // So we APPLY it (self-add and third-party alike) to stay on
                    // the one canonical branch. The revoked device is then
                    // evicted the MLS-native, append-only way: `reconcile`
                    // already drops leaves whose `user_device` row is revoked,
                    // via a normal remove commit on a later epoch. The device is
                    // present for at most one epoch before that eviction lands —
                    // the bounded, consistent trade for never wedging anyone.
                    eprintln!(
                        "[mls] process_pending_commits: revoked add for {added_user_id} at epoch {} in {mls_group_id} — applying to stay in sync (reconcile will evict the device)",
                        commit.epoch
                    );
                }
                VerifyOutcome::AbsentRetry => {
                    // Race / replication-lag: the added device's row hasn't
                    // reached our view of Turso yet (issue #372). Do NOT
                    // delete the commit row.
                    //
                    // Self-add: defer — the sender claims they're adding
                    // themselves, but we can't see their row to confirm.
                    // Stop processing here; next catch-up retries.
                    //
                    // Third-party add (admin/inviter adding someone else):
                    // the sender already merged this on their side and
                    // it's already in `mls_commit_log` past the epoch
                    // CAS. Stalling would diverge us from the rest of the
                    // group. Process the commit advisory — same fallback
                    // the pre-#372 code took for any unverified third-
                    // party add.
                    let is_self_add = commit
                        .sender_id
                        .as_deref()
                        .map_or(false, |s| s == added_user_id.as_str());
                    if is_self_add {
                        eprintln!(
                            "[mls] process_pending_commits: deferring self-add seq {} epoch {} for {mls_group_id} — added device {added_user_id} not yet visible (issue #372)",
                            commit.seq, commit.epoch
                        );
                        break;
                    }
                    eprintln!(
                        "[mls] process_pending_commits: WARN third-party add for {added_user_id} at epoch {} in {mls_group_id} not yet verifiable — processing anyway",
                        commit.epoch
                    );
                }
            }
        }

        let commit_data = commit.commit_data;

        // All MLS work is synchronous and scoped so nothing !Send crosses
        // the lock().await boundary.
        let applied = {
            let guard = state.local_db.lock().await;
            let db = match guard.as_ref() {
                Some(db) => db,
                None => break,
            };
            let provider = PollisProvider::new(db.conn());
            let group_id = GroupId::from_slice(mls_group_id.as_bytes());
            let mut group = match MlsGroup::load(provider.storage(), &group_id)
                .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("mls load: {e}")))?
            {
                Some(g) => g,
                None => break,
            };

            let mut reader: &[u8] = &commit_data;
            let msg_in = match MlsMessageIn::tls_deserialize(&mut reader) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("[mls] process_pending_commits: deserialize failed for {mls_group_id} at epoch {}: {e} — stopping", commit.epoch);
                    break;
                }
            };
            let protocol_msg = match msg_in.try_into_protocol_message() {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("[mls] process_pending_commits: protocol msg failed for {mls_group_id} at epoch {}: {e} — stopping", commit.epoch);
                    break;
                }
            };

            match group.process_message(&provider, protocol_msg) {
                Ok(processed) => {
                    if let ProcessedMessageContent::StagedCommitMessage(staged) = processed.into_content() {
                        if let Err(e) = group.merge_staged_commit(&provider, *staged) {
                            eprintln!("[mls] process_pending_commits: merge failed for {mls_group_id} at epoch {}: {e} — stopping", commit.epoch);
                            break;
                        }
                    }
                }
                Err(e) => {
                    let msg = format!("{e}");
                    // Our OWN commit is canonical at this epoch — e.g. we
                    // submitted it but a lost response made us converge instead
                    // of merging (issue #411). openmls refuses to process its own
                    // commit ("...created by this client"); the right move is to
                    // ADOPT it by merging our pending commit, not delete the group
                    // and try to re-join from a possibly-stale GroupInfo. This
                    // advances us to the same epoch as everyone else.
                    if msg.contains("created by this client") {
                        if let Err(merge_err) = group.merge_pending_commit(&provider) {
                            eprintln!(
                                "[mls] process_pending_commits: own commit at epoch {} for {mls_group_id} but merge_pending failed ({merge_err}) — deleting to recover",
                                commit.epoch
                            );
                            let _ = group.delete(provider.storage());
                            break;
                        }
                        eprintln!(
                            "[mls] process_pending_commits: adopted our own commit at epoch {} for {mls_group_id}",
                            commit.epoch
                        );
                        // Fall through (no break) so this counts as applied and
                        // the epoch advances.
                    } else {
                        // Two distinct recoverable failures, both handled by
                        // dropping the local group so the external-rejoin below
                        // rebuilds it from the latest published GroupInfo:
                        //
                        //   1. Eviction — we were removed; our keys can't open the
                        //      commit. Re-join only if we're still a roster member
                        //      (external_join no-ops cleanly if GroupInfo is gone).
                        //
                        //   2. Fork — the commit is at our CURRENT epoch (it passed
                        //      the epoch-gap check above) yet still won't apply, so
                        //      our local tree has diverged from the canonical
                        //      branch. This is the residue of a historical
                        //      concurrent-commit race (prod incident: group
                        //      `01KQYX89…`); the UNIQUE(conversation_id, epoch)
                        //      constraint stops new forks, but already-forked
                        //      devices only heal by re-joining the live branch.
                        //
                        // Deleting drops only this device's MLS crypto state, not
                        // its decrypted message history (that lives in the local
                        // `message` table). The rejoin lands at the latest epoch,
                        // so the next pass filters `epoch >= new_epoch` and can't
                        // re-fail on the same commit — no recovery loop.
                        if msg.contains("evicted") {
                            eprintln!("[mls] process_pending_commits: evicted from {mls_group_id} — deleting local group for recovery");
                        } else {
                            eprintln!(
                                "[mls] process_pending_commits: commit at epoch {} for {mls_group_id} failed to apply ({e}) — local state diverged from canonical branch; deleting local group to re-join",
                                commit.epoch
                            );
                        }
                        let _ = group.delete(provider.storage());
                        break;
                    }
                }
            }

            true
        };

        if applied {
            current_epoch += 1;
            any_applied = true;
        }
    }

    // Resolve any commit left DANGLING by an interrupted submit (a crash, or a
    // `clear_pending_commit` that itself failed). If our commit had actually
    // landed, the replay loop above would have adopted it (OwnCommit → merge),
    // so a commit still pending here never made it into the canonical log.
    // Clear it — otherwise a later blind `merge_pending_commit`
    // (`load_group_with_signer` / reconcile) would advance us to a phantom epoch
    // no other member can see, and our messages would become undecryptable to
    // the group (issue #411 item 2). Safe even if it *had* landed: a future
    // pass re-adopts it from the log.
    {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            let provider = PollisProvider::new(db.conn());
            let group_id = GroupId::from_slice(mls_group_id.as_bytes());
            if let Ok(Some(mut group)) = MlsGroup::load(provider.storage(), &group_id) {
                if group.pending_commit().is_some() {
                    eprintln!(
                        "[mls] process_pending_commits: clearing a dangling pending commit for {mls_group_id} (never landed) to avoid a phantom-epoch merge"
                    );
                    let _ = group.clear_pending_commit(provider.storage());
                }
            }
        }
    }

    if any_applied {
        if let Err(e) = publish_group_info(state, mls_group_id).await {
            eprintln!("[mls] process_pending_commits: publish_group_info failed (non-fatal): {e}");
        }
        // Voice E2EE: when the epoch advances for the MLS group currently
        // backing the active voice room, re-derive the per-room key and
        // rotate it on the live KeyProvider. No-op when voice is idle.
        crate::commands::voice_e2ee::on_mls_epoch_changed(state, mls_group_id).await;
    }

    // If the group was deleted during processing (e.g. eviction),
    // external-join to recover.
    let group_exists = {
        let guard = state.local_db.lock().await;
        guard.as_ref().map_or(false, |db| {
            has_local_group(db.conn(), mls_group_id)
        })
    };
    if !group_exists {
        // Recover by external-join — UNLESS this device was revoked (its
        // `user_device` row is gone), in which case it must stay out rather
        // than squatting an epoch to climb back in. A legitimately-forked or
        // freshly-wiped device keeps its row and recovers normally.
        if local_device_registered(state, user_id).await {
            eprintln!("[mls] process_pending_commits: group {mls_group_id} was deleted during processing — external-joining to recover");
            // Lock already held by the wrapper, so call the unlocked inner variant.
            if let Err(e) = external_join_group_inner(state, mls_group_id, user_id).await {
                eprintln!("[mls] process_pending_commits: recovery external-join failed for {mls_group_id}: {e}");
            }
        } else {
            eprintln!("[mls] process_pending_commits: group {mls_group_id} deleted and device for {user_id} is no longer registered — staying out (revoked)");
        }
    }

    Ok(())
}

/// Tauri command wrapper — resolves conversation_id to MLS group ID, then
/// delegates to `process_pending_commits_inner`.
pub async fn process_pending_commits(
    state: &Arc<AppState>,
    conversation_id: String,
    user_id: String,
) -> crate::error::Result<()> {
    let mls_group_id = {
        let conn = state.remote_db.conn().await?;
        let mut rows = conn.query(
            "SELECT group_id FROM channels WHERE id = ?1",
            libsql::params![conversation_id.clone()],
        ).await?;
        match rows.next().await? {
            Some(row) => row.get::<String>(0)?,
            None => conversation_id,
        }
    };
    process_pending_commits_inner(state, &mls_group_id, &user_id).await
}

// ── Phase 5 helpers: encrypt / decrypt ───────────────────────────────────────

/// Check whether an MLS group exists in the local database.
pub fn has_local_group(conn: &rusqlite::Connection, conversation_id: &str) -> bool {
    let provider = PollisProvider::new(conn);
    let group_id = GroupId::from_slice(conversation_id.as_bytes());
    matches!(MlsGroup::load(provider.storage(), &group_id), Ok(Some(_)))
}

/// Try to encrypt `plaintext` with the MLS group for `conversation_id`.
///
/// Returns `None` — without logging — if the group does not exist locally
/// (e.g. the channel was created before MLS was rolled out).  The caller
/// should fall back to the legacy Signal sender-key path in that case.
pub fn try_mls_encrypt(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    plaintext: &[u8],
) -> Option<Vec<u8>> {
    let provider = PollisProvider::new(conn);
    let (mut group, signer) = load_group_with_signer(&provider, conversation_id).ok()?;
    let msg_out = group.create_message(&provider, &signer, plaintext).ok()?;
    msg_out.tls_serialize_detached().ok()
}

/// Try to decrypt MLS ciphertext bytes for `conversation_id`.
///
/// The bytes must be TLS-serialised `MlsMessageOut` (i.e. what we stored in
/// `message_envelope.ciphertext` after `send_message` used MLS).  Returns
/// the raw plaintext bytes on success, or `None` if the bytes are not a
/// valid MLS `ApplicationMessage` or if decryption fails for any reason.
pub fn try_mls_decrypt(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    ciphertext: &[u8],
) -> Option<Vec<u8>> {
    let provider = PollisProvider::new(conn);
    let group_id = GroupId::from_slice(conversation_id.as_bytes());
    let mut group = MlsGroup::load(provider.storage(), &group_id).ok()??;

    let mut reader: &[u8] = ciphertext;
    let msg_in = MlsMessageIn::tls_deserialize(&mut reader).ok()?;
    let protocol_msg = msg_in.try_into_protocol_message().ok()?;
    let processed = group.process_message(&provider, protocol_msg).ok()?;

    match processed.into_content() {
        ProcessedMessageContent::ApplicationMessage(app_msg) => Some(app_msg.into_bytes()),
        _ => None,
    }
}
