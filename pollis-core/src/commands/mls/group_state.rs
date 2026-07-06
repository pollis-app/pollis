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

    // W4 seam: route the GroupInfo republish through the Delivery Service (the
    // sole writer of the log DB). Mirrors `submit_commit`.
    use base64::Engine as _;
    let body = serde_json::json!({
        "conversation_id": conversation_id,
        "epoch": epoch as i64,
        "group_info": base64::engine::general_purpose::STANDARD.encode(&bytes),
        "updated_by_device_id": device_id,
    });
    let resp = super::ds_client::ds_post(state, "/v1/group-info", &body).await?;
    if !resp.status().is_success() {
        let s = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "publish_group_info DS {s}: {txt}"
        )));
    }

    Ok(())
}

/// Whether the durably-published GroupInfo is stale relative to our local epoch
/// and must be republished.
///
/// `published` is the epoch of the GroupInfo currently stored in the log DB
/// (`None` = no row at all — it was never published, or a create-time publish
/// was dropped). `local` is this device's current MLS group epoch.
///
/// Republish iff the log DB has no GroupInfo, or its GroupInfo is behind us. An
/// equal epoch is already durable, and a *higher* one means another member
/// advanced and published past us — neither needs our help. The DS `/v1/group-info`
/// upsert is epoch-monotone, so republishing an equal/stale epoch would be a
/// harmless no-op anyway; this check just avoids the needless round-trip.
fn group_info_is_stale(published: Option<u64>, local: u64) -> bool {
    match published {
        None => true,
        Some(p) => p < local,
    }
}

/// Highest epoch of the GroupInfo durably stored in the log DB for this group,
/// or `None` if there's no row — or the read fails, which we deliberately treat
/// as "absent" so the caller errs toward republishing rather than assuming
/// durability. (Mirrors `voice_e2ee::published_group_epoch`'s read idiom, but
/// with the opposite failure bias: this is a write-durability backstop, so a
/// redundant publish on a transient blip is safer than a missed heal.)
async fn published_group_info_epoch(state: &Arc<AppState>, mls_group_id: &str) -> Option<u64> {
    // Read-only GroupInfo epoch lookup → log_db (falls back to remote_db pre-cutover).
    let conn = state.log_db.conn().await.ok()?;
    let mut rows = conn
        .query(
            "SELECT epoch FROM mls_group_info WHERE conversation_id = ?1",
            libsql::params![mls_group_id.to_string()],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    row.get::<i64>(0).ok().map(|v| v as u64)
}

/// Durability backstop for MLS bootstrap: ensure the log DB holds a current-epoch
/// GroupInfo for this group, republishing if a past publish was dropped.
///
/// `publish_group_info` is best-effort at create time (`init_mls_group`) and on
/// epoch advance. If that DS post fails — e.g. a transient outage right as a group
/// is created — the group is otherwise stranded forever: with no GroupInfo in the
/// log DB, no member can external-join, and (if the add-member Welcome was dropped
/// in the same outage) no member can join at all. Nothing else heals it, because
/// the `any_applied` republish in `process_pending_commits_locked_impl` never fires
/// for a sole-member creator who applies no commits.
///
/// Called on every "group touched" pass (sweep / send / realtime ingest /
/// reconcile). Cheap for healthy groups — one indexed read, and a DS post only
/// when the stored GroupInfo is missing or behind. Idempotent (the upsert is
/// epoch-monotone), and it also rescues already-bricked groups on the next pass.
/// No-op when we have no local group (nothing to export).
async fn ensure_group_info_published(state: &Arc<AppState>, mls_group_id: &str) {
    let local_epoch = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else { return };
        let provider = PollisProvider::new(db.conn());
        let group_id = GroupId::from_slice(mls_group_id.as_bytes());
        match MlsGroup::load(provider.storage(), &group_id) {
            Ok(Some(g)) => g.epoch().as_u64(),
            _ => return,
        }
    };

    let published = published_group_info_epoch(state, mls_group_id).await;
    if !group_info_is_stale(published, local_epoch) {
        return;
    }

    if let Err(e) = publish_group_info(state, mls_group_id).await {
        eprintln!(
            "[mls] ensure_group_info_published: republish for {mls_group_id} \
             (local epoch {local_epoch}, published {published:?}) failed: {e}"
        );
    }
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
        // Read-only GroupInfo lookup → log_db (falls back to remote_db pre-cutover).
        let conn = state.log_db.conn().await?;
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

/// Whether `user_id` is a CURRENT member of the conversation backed by
/// `mls_group_id`. Gates the external-join *recovery* paths on group membership
/// so a member who was REMOVED from the group (their `group_member` /
/// `dm_channel_member` row deleted) can't rebuild/rejoin itself — even though the
/// device is still registered (not revoked). `local_device_registered` alone does
/// NOT catch this: a removed-but-not-revoked device passes that gate, and the DS
/// `/v1/commits` endpoint does not gate submissions on membership, so a removed
/// member's external-join would otherwise WIN its epoch on the CAS and climb the
/// removed member back into the tree — a membership leak (fuzzer finding #2).
///
/// Mirrors the DS-side `pollis_delivery::writes::is_member`: an MLS
/// `mls_group_id` is one of a group id (channels share one MLS group keyed by the
/// group id), a DM channel id, or a channel id, so all three membership shapes
/// are accepted.
///
/// Fails CLOSED (returns false) on a missing device context or any query error:
/// unlike the revoked-device check, this guards a membership *leak*, so when we
/// cannot confirm membership we must NOT rebuild. This is never a permanent
/// lockout — a legitimate current member simply recovers on the next catch-up
/// pass once the (transient) read succeeds — and by the time control reaches this
/// gate the same `remote_db` was already read successfully for the commit-log
/// fetch, so a failure here is vanishingly unlikely in practice.
async fn local_user_is_member(state: &Arc<AppState>, mls_group_id: &str, user_id: &str) -> bool {
    let conn = match state.remote_db.conn().await {
        Ok(c) => c,
        Err(_) => return false,
    };
    match conn
        .query(
            "SELECT 1 WHERE \
                EXISTS (SELECT 1 FROM dm_channel_member \
                        WHERE dm_channel_id = ?1 AND user_id = ?2) \
             OR EXISTS (SELECT 1 FROM group_member \
                        WHERE group_id = ?1 AND user_id = ?2) \
             OR EXISTS (SELECT 1 FROM channels c \
                        JOIN group_member gm ON gm.group_id = c.group_id \
                        WHERE c.id = ?1 AND gm.user_id = ?2) \
             LIMIT 1",
            libsql::params![mls_group_id, user_id],
        )
        .await
    {
        Ok(mut rows) => matches!(rows.next().await, Ok(Some(_))),
        Err(_) => false,
    }
}

/// Both cooperative gates on the external-join *recovery* paths: this device is
/// still registered (not revoked) AND its user is still a current member of the
/// group. A `false` from either means "do not rebuild/rejoin". Logs the specific
/// reason so a skipped recovery is never a silent no-op.
async fn may_rejoin_via_external_join(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
) -> bool {
    // The two gates as booleans, then the (proved) pure conjunction. Each `false`
    // still logs its specific reason so a skipped recovery is never a silent
    // no-op, and the membership query is short-circuited when the device is
    // already revoked (unchanged from the original). The AND itself is
    // `invariants::may_rejoin`, proved by Kani to admit a rejoin ONLY for
    // (registered && member) — a revoked or removed device can never climb back
    // in (fuzzer finding #2).
    let registered = local_device_registered(state, user_id).await;
    if !registered {
        eprintln!(
            "[mls] external-join recovery for {mls_group_id}: device for {user_id} is no longer \
             registered (revoked) — staying out"
        );
        return super::invariants::may_rejoin(false, false);
    }
    let is_member = local_user_is_member(state, mls_group_id, user_id).await;
    if !is_member {
        eprintln!(
            "[mls] external-join recovery for {mls_group_id}: {user_id} is no longer a group \
             member (removed) — staying out"
        );
    }
    super::invariants::may_rejoin(registered, is_member)
}

/// Body of [`process_pending_commits_inner`]. Assumes the caller already holds
/// the per-conversation MLS lock (`state.mls_group_lock`).
pub(crate) async fn process_pending_commits_locked(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    process_pending_commits_locked_impl(state, mls_group_id, user_id, None).await
}

/// Like [`process_pending_commits_inner`], but after the local group reaches
/// each epoch during the replay it invokes `on_epoch(conn, epoch)` so the caller
/// can decrypt the application-message envelopes sealed at that epoch BEFORE the
/// next commit advances past it.
///
/// This is the interleave that makes offline catch-up correct under
/// `max_past_epochs = 0` (issue #418). With the default `max_past_epochs = 0`,
/// the ratchet keys for an epoch are discarded the instant the group advances
/// past it. The old ingest path applied *every* pending commit first — jumping
/// straight to head — and only then tried to decrypt the backlog, so any message
/// sent at an intermediate epoch was sealed under keys that no longer existed and
/// decrypted as `WrongEpoch`, permanently lost. By decrypting each epoch's
/// messages while the group is still AT that epoch, every message a current
/// member was eligible to read survives a heavy offline-churn catch-up.
///
/// `on_epoch` fires once for the member's starting epoch (before any commit) and
/// once after each commit that successfully advances the group. It does NOT fire
/// for epochs skipped by a recovery jump (epoch-gap / fork / eviction →
/// external-join); messages at those epochs are caught on the NEXT ingest, when
/// the rejoined epoch becomes the starting epoch.
pub async fn process_pending_commits_inner_with_hook(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
    on_epoch: &mut (dyn FnMut(&rusqlite::Connection, u64) + Send),
) -> crate::error::Result<()> {
    let _guard = state.mls_group_lock(mls_group_id).await;
    process_pending_commits_locked_impl(state, mls_group_id, user_id, Some(on_epoch)).await
}

async fn process_pending_commits_locked_impl(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
    mut on_epoch: Option<&mut (dyn FnMut(&rusqlite::Connection, u64) + Send)>,
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
            // No local group — external-join to create one, UNLESS this device
            // has been revoked (its `user_device` row is gone) OR its user is no
            // longer a group member (removed from `group_member`). Either must
            // not climb back in: a revoked/removed member's external-join squats
            // an epoch and, under the UNIQUE(conversation_id, epoch) constraint,
            // would wedge the group — and, absent a DS membership gate on
            // `/v1/commits`, a removed member would otherwise rejoin the tree and
            // decrypt post-removal traffic (membership leak, fuzzer finding #2).
            // Lock already held by the wrapper, so call the unlocked inner variant.
            if may_rejoin_via_external_join(state, mls_group_id, user_id).await {
                if let Err(e) = external_join_group_inner(state, mls_group_id, user_id).await {
                    eprintln!("[mls] process_pending_commits: no local group for {mls_group_id}, external-join failed: {e}");
                }
            }
            return Ok(());
        }
    };

    // #418 interleave: decrypt the envelopes sealed at the member's CURRENT
    // epoch before any commit advances the group past it. (No-op for callers
    // that pass no hook — e.g. send / reconcile catch-up.)
    if let Some(hook) = on_epoch.as_deref_mut() {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            hook(db.conn(), initial_epoch);
        }
    }

    // 2. Fetch pending commits from remote, along with the add-metadata
    //    columns (`added_user_id`, `added_device_ids`) so we can verify
    //    cross-signing certs BEFORE calling `process_message`. Collected
    //    into an owned Vec so the `rows` cursor is dropped before any
    //    local-DB await below.
    // Read-only commit-log fetch → log_db (falls back to remote_db pre-cutover).
    let conn = state.log_db.conn().await?;
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

    // Cross-signing cert verification (`verify_added_devices` below) reads
    // `users` / `user_device` / `account_key_log` — all on the MAIN DB. `conn`
    // above is the read-only commit-log DB, which has none of those tables (a
    // verify against it fails with "no such table: users"). Open a main-DB
    // connection for verification. Falls back to the same DB pre-cutover.
    let verify_conn = state.remote_db.conn().await?;

    // 3. Apply each commit in epoch order. For any commit carrying add
    //    metadata, verify every added device's cross-signing cert
    //    against the user's account_id_pub BEFORE touching the group
    //    state.
    let mut current_epoch = initial_epoch;
    let mut any_applied = false;
    for commit in pending {
        // Gap classification (I1), proved by Kani (`invariants::classify`) never
        // to `Apply` across a gap: `Apply` iff this row's epoch is exactly
        // `current_epoch`, else `GapRecover`.
        if super::invariants::classify(current_epoch, Some(commit.epoch as u64))
            != super::invariants::ReplayStep::Apply
        {
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
                &verify_conn,
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
            // #418 interleave: the commit we just merged advanced the group to
            // `current_epoch`. Decrypt the envelopes sealed at this epoch NOW,
            // while the group still holds its ratchet keys — the next iteration's
            // commit will advance past it and (max_past_epochs = 0) discard them.
            if let Some(hook) = on_epoch.as_deref_mut() {
                let guard = state.local_db.lock().await;
                if let Some(db) = guard.as_ref() {
                    hook(db.conn(), current_epoch);
                }
            }
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

    // GroupInfo durability backstop. The `any_applied` path advanced our epoch,
    // so its freshly-exported GroupInfo must be republished — but we also heal the
    // case where a *past* publish (e.g. the create-time one in `init_mls_group`)
    // was dropped by a transient DS failure and never retried. `any_applied` is
    // false for a stranded sole-member creator, so the old gated republish never
    // ran and the group stayed unjoinable forever (no GroupInfo → no external-join).
    // `ensure_group_info_published` republishes only when the log DB's GroupInfo is
    // missing or behind us, so it subsumes the old `any_applied` republish too.
    ensure_group_info_published(state, mls_group_id).await;

    if any_applied {
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
        // `user_device` row is gone) OR its user was removed from the group
        // (`group_member` row gone). Either must stay out rather than squatting
        // an epoch to climb back in: a removed member that self-evicted here
        // (applied its own removal above) would otherwise rebuild and rejoin the
        // tree, decrypting post-removal traffic (membership leak, fuzzer finding
        // #2). A legitimately-forked or freshly-wiped CURRENT member keeps both
        // rows and recovers normally.
        if may_rejoin_via_external_join(state, mls_group_id, user_id).await {
            eprintln!("[mls] process_pending_commits: group {mls_group_id} was deleted during processing — external-joining to recover");
            // Lock already held by the wrapper, so call the unlocked inner variant.
            if let Err(e) = external_join_group_inner(state, mls_group_id, user_id).await {
                eprintln!("[mls] process_pending_commits: recovery external-join failed for {mls_group_id}: {e}");
            }
        }
    }

    Ok(())
}

/// Tauri command wrapper — resolves conversation_id to MLS group ID, then runs
/// the group-level interleaved catch-up.
///
/// This is a user-facing CATCH-UP entry point (the app's manual "sync" shortcut,
/// and the test harness's `process_commits_for`), so it must NOT be a bare
/// commit-only replay: advancing the shared group to head without decrypting
/// en route would strand any message sealed at an epoch it skips past
/// (`max_past_epochs = 0`). Route through `catch_up_mls_group_interleaved`, which
/// decrypts every bound conversation's messages at each epoch before advancing
/// past it, and still reaches head. (The send / edit / invite / remove commit-
/// INITIATION paths do NOT go through this command, but they run the SAME
/// interleaved catch-up before advancing their own epoch — see issue #440, the
/// committer strand — so a current-epoch inbound message is never stranded by a
/// self-initiated commit either.)
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
    crate::commands::messages::catch_up_mls_group_interleaved(state, &mls_group_id, &user_id).await
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

/// Parse the MLS epoch a `message` / `edit` envelope was sealed at, WITHOUT
/// decrypting it (no group state touched).
///
/// Returns `None` when the bytes are not a valid MLS `ProtocolMessage` — e.g. a
/// `delete` tombstone, whose ciphertext is empty — so the caller treats such
/// envelopes as epoch-independent.
///
/// This is the load-bearing primitive for the epoch-stepped ingest interleave
/// (issue #418): because `max_past_epochs` is 0, a message must be decrypted
/// while the local group is still AT its epoch, so the ingest pass routes each
/// envelope to the moment in the commit replay when the group reaches the
/// matching epoch. The epoch is read by PARSING the envelope rather than inferred
/// from `sent_at` — clock skew and same-epoch reordering make `sent_at` an
/// unreliable proxy for the cryptographic epoch.
pub fn envelope_epoch(ciphertext: &[u8]) -> Option<u64> {
    let mut reader: &[u8] = ciphertext;
    let msg_in = MlsMessageIn::tls_deserialize(&mut reader).ok()?;
    let protocol_msg = msg_in.try_into_protocol_message().ok()?;
    Some(protocol_msg.epoch().as_u64())
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

#[cfg(test)]
mod group_info_heal_tests {
    use super::group_info_is_stale;

    // The decision at the heart of the bootstrap-durability backstop: republish
    // the GroupInfo iff the log DB's copy is missing or behind our local epoch.
    // This is exactly what was missing before — the old republish only ran when a
    // commit was applied (`any_applied`), so a sole-member creator whose
    // create-time publish was dropped (`None`) never republished and the group
    // stayed permanently unjoinable.
    #[test]
    fn republishes_when_groupinfo_never_landed() {
        // No GroupInfo row at all — the stranded-creator bug. Must republish at
        // any epoch, including epoch 0 (a freshly created, sole-member group).
        assert!(group_info_is_stale(None, 0));
        assert!(group_info_is_stale(None, 7));
    }

    #[test]
    fn republishes_when_groupinfo_is_behind() {
        // A later epoch-advance publish was dropped; the stored GroupInfo lags our
        // local epoch, so the current epoch isn't externally joinable until we heal.
        assert!(group_info_is_stale(Some(0), 1));
        assert!(group_info_is_stale(Some(3), 7));
    }

    #[test]
    fn skips_when_groupinfo_is_current() {
        // Already durable at our epoch — no DS round-trip needed.
        assert!(!group_info_is_stale(Some(0), 0));
        assert!(!group_info_is_stale(Some(5), 5));
    }

    #[test]
    fn skips_when_groupinfo_is_ahead() {
        // Another member advanced and published past us; don't republish a stale
        // view of an epoch we no longer lead.
        assert!(!group_info_is_stale(Some(9), 4));
    }
}
