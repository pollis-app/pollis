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
use super::generation::{local_generation, mls_group_id, set_local_generation};
use super::provider::{
    current_suite, load_stored_group, load_stored_group_at, make_credential,
    parse_credential_user_id, signature_scheme, store_only, MlsProvider, PollisProvider,
};

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

    let exported: Option<(i64, u64, Vec<u8>)> = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else {
            return Ok(());
        };
        let generation = local_generation(db.conn(), conversation_id);
        let provider = PollisProvider::new(db.conn());
        export_group_info_blob(&provider, conversation_id, generation)?
            .map(|(epoch, bytes)| (generation, epoch, bytes))
    };

    let Some((generation, epoch, bytes)) = exported else {
        return Ok(());
    };

    // W4 seam: route the GroupInfo republish through the Delivery Service (the
    // sole writer of the log DB). Mirrors `submit_commit`.
    use base64::Engine as _;
    let body = pollis_api::writes::GroupInfoBody {
        conversation_id: conversation_id.to_string(),
        generation,
        epoch: epoch as i64,
        group_info: base64::engine::general_purpose::STANDARD.encode(&bytes),
        updated_by_device_id: device_id,
    };
    let resp = super::ds_client::ds_post(state, &body).await?;
    if !resp.status().is_success() {
        let s = resp.status();
        let txt = resp.text().await.unwrap_or_default();
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "publish_group_info DS {s}: {txt}"
        )));
    }

    Ok(())
}

/// Export the current `GroupInfo` for `conversation_id` as `(epoch, tls_bytes)`.
///
/// `Ok(None)` — not an error — when there is no usable local group or the export
/// itself fails: `publish_group_info` is a best-effort durability backstop, and
/// a device with nothing to publish has nothing to report.
pub(super) fn export_group_info_blob<C>(
    provider: &MlsProvider<'_, C>,
    conversation_id: &str,
    generation: i64,
) -> Result<Option<(u64, Vec<u8>)>>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let Ok((group, signer)) = load_group_with_signer(provider, conversation_id, generation) else {
        return Ok(None);
    };
    let epoch = group.epoch().as_u64();
    let msg = match group.export_group_info(provider.crypto(), &signer, true) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[mls] publish_group_info: export failed for {conversation_id}: {e}");
            return Ok(None);
        }
    };
    let bytes = msg
        .tls_serialize_detached()
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("group_info serialize: {e}")))?;
    Ok(Some((epoch, bytes)))
}

/// Whether the durably-published GroupInfo is stale relative to our local epoch
/// and must be republished.
///
/// `published` is the `(generation, epoch)` head of the GroupInfo currently
/// stored in the log DB (`None` = no row at all — it was never published, or a
/// create-time publish was dropped). `local` is this device's own head.
///
/// Republish iff the log DB has no GroupInfo, or its GroupInfo is behind us. An
/// equal head is already durable, and a *higher* one means another member
/// advanced and published past us — neither needs our help. The DS `/v1/group-info`
/// upsert is monotone on the same key, so republishing an equal/stale head would
/// be a harmless no-op anyway; this check just avoids the needless round-trip.
///
/// The comparison is lexicographic on `(generation, epoch)` and NOT on epoch
/// alone (#454 P4): a successor lineage restarts at epoch 0, numerically below
/// the retired lineage's last epoch, so an epoch-only test would read the
/// migrated group's GroupInfo as "stale" forever and have every member fight to
/// overwrite it with the retired suite's.
fn group_info_is_stale(published: Option<(i64, u64)>, local: (i64, u64)) -> bool {
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
pub(super) async fn published_group_info_head(
    state: &Arc<AppState>,
    conversation_id: &str,
) -> Option<(i64, u64)> {
    // Read-only GroupInfo head lookup → log_db (falls back to remote_db pre-cutover).
    let conn = state.log_db.conn().await.ok()?;
    let mut rows = conn
        .query(
            "SELECT generation, epoch FROM mls_group_info WHERE conversation_id = ?1",
            libsql::params![conversation_id.to_string()],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    Some((row.get::<i64>(0).ok()?, row.get::<i64>(1).ok()? as u64))
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
async fn ensure_group_info_published(state: &Arc<AppState>, conversation_id: &str) {
    let local_head = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else { return };
        // Reading an epoch is storage-only — no crypto backend, so no dispatch.
        let generation = local_generation(db.conn(), conversation_id);
        match load_stored_group_at(db.conn(), conversation_id, generation) {
            Some(g) => (generation, g.epoch().as_u64()),
            None => return,
        }
    };

    let published = published_group_info_head(state, conversation_id).await;
    if !group_info_is_stale(published, local_head) {
        return;
    }

    if let Err(e) = publish_group_info(state, conversation_id).await {
        eprintln!(
            "[mls] ensure_group_info_published: republish for {conversation_id} \
             (local head {local_head:?}, published {published:?}) failed: {e}"
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
    /// Carries the generation the branch was built in, because that is the
    /// lineage whose local group must be deleted — after a migration this
    /// device may hold two.
    LostRace { generation: i64 },
    /// A Welcome for this device became available for this conversation — the
    /// canonical join path. We stood down without building/submitting anything so
    /// we don't race our own inbound Welcome (both do "delete stale group →
    /// rejoin"). See the DM-accept convergence note in [`external_join_attempt`].
    DeferredToWelcome,
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
            // A Welcome became available mid-recovery — defer to it (the join will
            // happen via `apply_welcome`). Success, not an error: the group WILL be
            // joined, just via the canonical Welcome path.
            ExternalJoinResult::DeferredToWelcome => return Ok(()),
            ExternalJoinResult::LostRace {
                generation: join_generation,
            } => {
                eprintln!(
                    "[mls] external_join_group: lost epoch race for {conversation_id} \
                     (attempt {}/{MAX_JOIN_ATTEMPTS}) — discarding local branch and retrying",
                    attempt + 1
                );
                // Drop the doomed branch we just built so the next attempt
                // re-joins cleanly from the advanced GroupInfo.
                let _ = forget_local_mls_group_at(state, conversation_id, join_generation).await;
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

    // 1. Fetch the stored GroupInfo for this conversation, and with it the suite
    //    GENERATION it belongs to. The published GroupInfo is always the newest
    //    lineage (the upsert is monotone on `(generation, epoch)`), so a device
    //    recovering into a migrated conversation joins the successor directly —
    //    which is the only lineage still accepting commits.
    let (group_info_bytes, stored_generation, stored_epoch): (Vec<u8>, i64, i64) = {
        // Read-only GroupInfo lookup → log_db (falls back to remote_db pre-cutover).
        let conn = state.log_db.conn().await?;
        let mut rows = conn
            .query(
                "SELECT group_info, generation, epoch FROM mls_group_info \
                 WHERE conversation_id = ?1",
                libsql::params![conversation_id],
            )
            .await?;
        match rows.next().await? {
            Some(row) => (row.get(0)?, row.get(1)?, row.get(2)?),
            None => {
                return Err(crate::error::Error::Other(anyhow::anyhow!(
                    "no GroupInfo stored for {conversation_id} — cannot external-join"
                )))
            }
        }
    };

    // GroupInfo is present — so, because the DS writes the commit, its GroupInfo,
    // AND the add's Welcomes in ONE transaction (`pollis_delivery::commit::
    // submit_commit`, an IMMEDIATE tx), any Welcome reconcile produced for THIS
    // device is durably present too. Prefer that Welcome and stand down: external-
    // joining now would race our own inbound Welcome — both do "delete stale local
    // group → (re)join" — and can clobber the freshly-joined group, stranding us
    // (`no GroupInfo stored — cannot external-join`, the DM-accept convergence race).
    //
    // This is the load-bearing close of that race. The `create_dm` sequence makes a
    // recipient's membership row (and thus this recovery path) visible BEFORE
    // reconcile produces the Welcome+GroupInfo, so the caller's pre-check can run
    // in that gap and miss the Welcome; but external-join can only ever SUCCEED
    // once GroupInfo exists, and by the atomic write that is exactly when the
    // Welcome exists — so re-checking HERE catches it deterministically. A genuine
    // Secret-Key recovery / dropped-Welcome case has no such row and proceeds.
    if has_pending_welcome(state, conversation_id, user_id).await {
        eprintln!(
            "[mls] external_join: a Welcome for {conversation_id} is available — deferring to it \
             instead of external-joining (avoids racing our own Welcome)"
        );
        return Ok(ExternalJoinResult::DeferredToWelcome);
    }

    // 2. Run the external commit inside the local_db sync scope. Capture both
    //    the commit and its resulting-epoch GroupInfo so they land atomically
    //    through the delivery seam (Slice 1). Welcomes are empty — an external
    //    join only adds self.
    // Deserialising the stored GroupInfo touches no crypto, so it happens before
    // (and outside) provider selection — and it is what tells us which suite the
    // group runs, since a recovering device has no local group left to ask.
    let verifiable_group_info: VerifiableGroupInfo = {
        let mut env_reader: &[u8] = &group_info_bytes;
        let msg_in = MlsMessageIn::tls_deserialize(&mut env_reader).map_err(|e| {
            crate::error::Error::Other(anyhow::anyhow!(
                "stored group_info envelope failed to deserialize: {e}"
            ))
        })?;
        match msg_in.extract() {
            MlsMessageBodyIn::GroupInfo(gi) => gi,
            other => {
                return Err(crate::error::Error::Other(anyhow::anyhow!(
                    "expected GroupInfo in mls_group_info, got {:?}",
                    std::mem::discriminant(&other)
                )));
            }
        }
    };
    let suite = verifiable_group_info.ciphersuite();

    let (commit_bytes, new_group_info_bytes): (Vec<u8>, Option<Vec<u8>>) = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let provider = PollisProvider::new(db.conn());
        build_external_commit(
            &provider,
            conversation_id,
            stored_generation,
            user_id,
            &device_id,
            suite,
            verifiable_group_info,
        )?
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
        stored_generation,
        stored_epoch,
        // An external join always CONTINUES the lineage it read the GroupInfo
        // from; it never opens a new one, so it closes nothing.
        None,
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
                stored_generation,
                stored_epoch,
                &commit_bytes,
            )
            .await
            {
                eprintln!(
                    "[mls] external_join: LostRace at epoch {stored_epoch} for {conversation_id} but our commit is canonical — adopting (lost success-response)"
                );
            } else {
                return Ok(ExternalJoinResult::LostRace {
                    generation: stored_generation,
                });
            }
        }
        Err(e) => {
            if super::reconcile::our_commit_is_canonical(
                state,
                conversation_id,
                stored_generation,
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
                let _ = forget_local_mls_group_at(state, conversation_id, stored_generation).await;
                return Err(e);
            }
        }
    }

    // 3b. We are now a member of `stored_generation`. Record it BEFORE anything
    //     reads the group back, so every subsequent load resolves to the lineage
    //     we actually joined. Monotone, so a device that external-joined a stale
    //     generation while a newer one exists cannot walk itself backwards.
    {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            set_local_generation(db.conn(), conversation_id, stored_generation);
            // Our leaf in this lineage is brand new and, being an external
            // commit, already merged — but the rotation clock belongs to the
            // lineage, not the conversation.
            super::self_update::record_self_update(
                db.conn(),
                conversation_id,
                stored_epoch as u64 + 1,
            );
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

/// Build (and locally stage) an external commit joining the group described by
/// `verifiable_group_info`, returning the serialised commit and the GroupInfo at
/// the resulting epoch.
///
/// `suite` is the group's, read off the GroupInfo by the caller. It decides the
/// scheme the joining leaf signs under, so passing the wrong one produces a
/// commit the group rejects rather than a silently weaker signature.
///
/// Sync: the caller owns the local-DB guard.
#[allow(clippy::too_many_arguments)]
pub(super) fn build_external_commit<C>(
    provider: &MlsProvider<'_, C>,
    conversation_id: &str,
    generation: i64,
    user_id: &str,
    device_id: &str,
    suite: Ciphersuite,
    verifiable_group_info: VerifiableGroupInfo,
) -> Result<(Vec<u8>, Option<Vec<u8>>)>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    // Load (or create) this device's stable MLS signing keypair for the suite.
    let scheme = signature_scheme(suite);
    let (sig_keys, sig_pub_bytes) =
        load_or_create_device_signer(provider, user_id, device_id, scheme)?;

    let credential = make_credential(user_id, device_id);
    let sig_pub = OpenMlsSignaturePublicKey::new(sig_pub_bytes.into(), scheme)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig pub key: {e}")))?;
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    // Drop any stale local group with the same ID so the external commit builder
    // doesn't collide. Scoped to THIS lineage: a predecessor group under a
    // different id is still needed to drain messages sealed before the migration.
    let group_id = mls_group_id(conversation_id, generation);
    if let Ok(Some(mut old)) = MlsGroup::load(provider.storage(), &group_id) {
        let _ = old.delete(provider.storage());
    }

    let join_config = MlsGroupJoinConfig::builder()
        .use_ratchet_tree_extension(true)
        .build();

    let (_joined_group, commit_bundle) = MlsGroup::external_commit_builder()
        .with_config(join_config)
        .build_group(provider, verifiable_group_info, cred_with_key)
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
        .finalize(provider)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!(
            "external commit finalize: {e}"
        )))?;

    let (commit_msg, _welcome_msg, new_group_info) = commit_bundle.into_contents();
    let commit_bytes = commit_msg
        .tls_serialize_detached()
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("commit serialize: {e}")))?;
    let group_info_bytes = match new_group_info {
        Some(gi) => Some(gi.tls_serialize_detached().map_err(|e| {
            crate::error::Error::Other(anyhow::anyhow!(
                "external commit group_info serialize: {e}"
            ))
        })?),
        None => None,
    };
    Ok((commit_bytes, group_info_bytes))
}

/// Create a fresh MLS group for `conversation_id` in an explicit ciphersuite,
/// with `creator_user_id`:`device_id` as the sole initial member.
///
/// This is the one place a group's suite is decided; every later operation on
/// the group inherits it from the stored group state, so nothing downstream
/// needs a suite argument.
///
/// The suite is a parameter rather than a constant so it is visible at the call
/// site, and so `super::migrate` can build a successor group on a suite that is
/// not the one `init_mls_group` births on.
///
/// Sync: the caller owns the local-DB guard.
pub(super) fn create_mls_group_in_suite<C>(
    provider: &MlsProvider<'_, C>,
    conversation_id: &str,
    generation: i64,
    creator_user_id: &str,
    device_id: &str,
    suite: Ciphersuite,
) -> Result<()>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let scheme = signature_scheme(suite);
    let (sig_keys, sig_pub_bytes) =
        load_or_create_device_signer(provider, creator_user_id, device_id, scheme)?;

    let credential = make_credential(creator_user_id, device_id);
    let sig_pub = OpenMlsSignaturePublicKey::new(sig_pub_bytes.into(), scheme)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("sig pub key: {e}")))?;
    let cred_with_key = CredentialWithKey {
        credential,
        signature_key: sig_pub.into(),
    };

    let group_id = mls_group_id(conversation_id, generation);

    // Delete any stale group with the same ID so the create below never
    // collides.  This is a no-op on first creation and essential during
    // repair (where the old group still exists but is broken/outdated).
    // Lineage-scoped: standing up a successor (#454 P4) must not touch the
    // predecessor, which is still serving reads until the migration lands.
    if let Ok(Some(mut old)) = MlsGroup::load(provider.storage(), &group_id) {
        let _ = old.delete(provider.storage());
    }

    let config = MlsGroupCreateConfig::builder()
        .ciphersuite(suite)
        .use_ratchet_tree_extension(true)
        .build();

    MlsGroup::new_with_group_id(provider, &sig_keys, &config, group_id, cred_with_key)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("create mls group: {e}")))?;

    Ok(())
}

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
        create_mls_group_in_suite(
            &provider,
            conversation_id,
            // A brand-new conversation starts on generation 0: there is no
            // history to migrate.
            0,
            creator_user_id,
            &device_id,
            // Every new group is born on the one suite this build ships (#669).
            // #454 chose per group, behind a fleet-capability gate, because a
            // classic-only device invited afterwards could never join a hybrid
            // group — reconcile refuses to downgrade and MLS offers no other
            // way in. With the classic suite retired there is no such device to
            // lock out, so the gate and its two capability reads are gone
            // rather than pinned open.
            current_suite(),
        )?;
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
///
/// Takes no ciphersuite: a group's suite is part of the stored group state, and
/// since #668 the signature scheme follows from it — so the signer is looked up
/// under `group.ciphersuite()`'s scheme, not a constant. A leaf key of the
/// wrong scheme is simply absent from storage, which surfaces as "signer not
/// found" rather than as a signature nobody can verify.
pub(super) fn load_group_with_signer<C>(
    provider: &MlsProvider<'_, C>,
    conversation_id: &str,
    generation: i64,
) -> crate::error::Result<(MlsGroup, SignatureKeyPair)>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let group_id = mls_group_id(conversation_id, generation);

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
        signature_scheme(group.ciphersuite()),
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
    let generation = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        local_generation(db.conn(), group_id)
    };
    forget_local_mls_group_at(state, group_id, generation).await
}

/// [`forget_local_mls_group`] scoped to one suite generation (#454 P4).
///
/// The lineage matters whenever a device holds two groups for one conversation:
/// a migrator that lost its compare-and-swap must delete the *successor* it
/// speculatively built and keep the predecessor it is still reading from, and
/// a device adopting a successor deletes the *predecessor* once it is drained.
/// Deleting "the group" without saying which would, in both cases, destroy the
/// one that is still needed.
pub(super) async fn forget_local_mls_group_at(
    state: &Arc<AppState>,
    group_id: &str,
    generation: i64,
) -> crate::error::Result<()> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
    })?;
    // Deleting is storage-only — no crypto backend, so no suite dispatch.
    if let Some(mut group) = load_stored_group_at(db.conn(), group_id, generation) {
        group.delete(&store_only(db.conn()))
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("mls delete group: {e}")))?;
    }
    // If the group wasn't found locally, nothing to clean up.
    Ok(())
}

/// Apply any commits from `mls_commit_log` that this member has not yet seen.
///
/// Reads rows where `epoch >= current_local_epoch` in ascending order, applies
/// each commit, and advances the local epoch.  An epoch gap (a missing interior
/// commit while a higher epoch is present) does NOT wedge and needs no manual
/// intervention: the member drops its stale local group and auto-recovers onto
/// the current published epoch via external-join (the recovery block below /
/// `may_rejoin_via_external_join`). Messages sealed at the jumped-over epochs are
/// the only accepted loss.
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
/// Fails CLOSED (returns false) on any conn/query error: a transient inability
/// to confirm the device is still registered must NOT permit a rejoin. The DS
/// `/v1/commits` endpoint does not itself gate submissions on device-revocation,
/// so an errored check here is the only thing standing between a just-revoked
/// device and a climb-back — treating "couldn't check" as "registered" is the
/// same fail-OPEN hole `local_user_is_member` closes for membership. This is
/// never a permanent lockout: a legitimate device recovers on the next catch-up
/// pass once the (transient) read succeeds, and by the time control reaches this
/// gate the same `remote_db` was already read successfully for the commit-log
/// fetch, so an error here is vanishingly unlikely in practice.
///
/// The one fail-OPEN case is a missing local `device_id` (returns true): that is
/// not a failed check but the absence of a device context (pre-enrollment), and
/// a revoked device always HAS an id — its `user_device` row is what gets
/// tombstoned — so it can never reach the tree through this branch.
async fn local_device_registered(state: &Arc<AppState>, user_id: &str) -> bool {
    let device_id = match state.device_id.lock().await.clone() {
        Some(d) => d,
        None => return true,
    };
    let conn = match state.remote_db.conn().await {
        Ok(c) => c,
        Err(_) => return false,
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
        Err(_) => false,
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

/// Whether an undelivered MLS `Welcome` for THIS device targets `mls_group_id`.
///
/// When true, the Welcome is the canonical join path and the external-join
/// *recovery* must stand down: firing external-join in parallel makes this device
/// race its own inbound Welcome — both do "delete stale local group → (re)join" —
/// which can clobber the freshly-joined group and strand the member
/// (`no GroupInfo stored — cannot external-join`, the DM-accept convergence race).
///
/// Self-healing: `apply_welcome` (via `poll_mls_welcomes_inner`) marks the row
/// `delivered = 1` even when apply FAILS, so this gate lifts on a later pass once
/// the Welcome is genuinely consumed — a truly orphaned Welcome then hands off to
/// external-join rather than deadlocking recovery forever.
///
/// Reads the log DB (where `mls_welcome` lives post-#420), mirroring
/// `poll_mls_welcomes_inner`'s selection exactly (recipient + this device, or a
/// legacy device-agnostic row). Fails OPEN (returns `false` ⇒ "no Welcome, don't
/// suppress") on a missing device context or any read error: a transient inability
/// to see the Welcome must never permanently block recovery — external-join, which
/// reads the same log DB for GroupInfo, is the pre-existing fallback and simply
/// no-ops itself if the GroupInfo isn't visible either.
async fn has_pending_welcome(state: &Arc<AppState>, mls_group_id: &str, user_id: &str) -> bool {
    let device_id = match state.device_id.lock().await.clone() {
        Some(d) => d,
        None => return false,
    };
    let conn = match state.log_db.conn().await {
        Ok(c) => c,
        Err(_) => return false,
    };
    match conn
        .query(
            "SELECT 1 FROM mls_welcome \
             WHERE conversation_id = ?1 AND recipient_id = ?2 AND delivered = 0 \
             AND (recipient_device_id = ?3 OR recipient_device_id IS NULL) \
             LIMIT 1",
            libsql::params![mls_group_id, user_id, device_id],
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
///
/// It receives the `(generation, epoch)` pair, not the epoch alone (#454 P4).
/// After a suite migration a conversation has two lineages and the successor
/// restarts at epoch 0, so "epoch 1" is ambiguous across a migration; only the
/// pair is monotone, and it is the pair the ingest watermark advances on.
pub async fn process_pending_commits_inner_with_hook(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
    on_epoch: &mut (dyn FnMut(&rusqlite::Connection, i64, u64) + Send),
) -> crate::error::Result<()> {
    let _guard = state.mls_group_lock(mls_group_id).await;
    process_pending_commits_locked_impl(state, mls_group_id, user_id, Some(on_epoch)).await
}

/// Most suite generations a single pass will hop across (#454 P4).
///
/// Crossing a boundary re-enters the replay so the successor's commits are
/// applied with the interleave hook still live, rather than waiting for the next
/// sweep — a conversation that migrated while this device was offline would
/// otherwise ingest nothing from the new lineage on the pass that discovers it.
/// Bounded because "adopt a newer generation" is driven by server-reported state:
/// an unbounded loop would let a misbehaving DS spin this pass forever. Four is
/// far beyond any real fleet — a conversation migrates once per suite era.
const MAX_GENERATION_HOPS: usize = 4;

/// The per-epoch hook a replay pass may carry: called with the open connection,
/// the generation and the epoch each time a commit is applied. Named because the
/// bare `dyn FnMut(..) + Send + 'h` was spelled out at every level of the replay.
type EpochHook<'h> = dyn FnMut(&rusqlite::Connection, i64, u64) + Send + 'h;

async fn process_pending_commits_locked_impl(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
    mut on_epoch: Option<&mut EpochHook<'_>>,
) -> crate::error::Result<()> {
    for _hop in 0..MAX_GENERATION_HOPS {
        let advanced =
            process_one_generation(state, mls_group_id, user_id, on_epoch.as_deref_mut()).await?;
        if !advanced {
            break;
        }
    }
    Ok(())
}

/// One replay pass over a single suite lineage. Returns whether it ended by
/// adopting a NEWER generation, in which case the caller replays again — the
/// successor's commits have not been applied yet.
// `'h` is named rather than elided so the hook's own lifetime is decoupled from
// the reborrow that reaches it: the caller loops, handing this fn a fresh short
// `&mut` to the same `Option` on every hop, which only typechecks if the trait
// object may outlive that borrow.
async fn process_one_generation<'h>(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
    mut on_epoch: Option<&mut EpochHook<'h>>,
) -> crate::error::Result<bool> {
    // 1. Get the current lineage + epoch from the local group.
    let (generation, has_group) = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let generation = local_generation(db.conn(), mls_group_id);
        (
            generation,
            load_stored_group_at(db.conn(), mls_group_id, generation).map(|g| g.epoch().as_u64()),
        )
    };
    let initial_epoch = match has_group {
        Some(epoch) => epoch,
        None => {
            // No local group. Two ways in — the inbound Welcome (apply_welcome)
            // and external-join *recovery* — and they must not both run: both do
            // "delete stale local group → (re)join", so in parallel the device
            // races its own Welcome and can clobber the freshly-joined group,
            // stranding it (`no GroupInfo stored — cannot external-join`, the
            // DM-accept convergence race). Arbitrate through the pure I6 gate
            // (`invariants::join_recovery`, Kani-proved to admit ExternalJoin ONLY
            // when no Welcome is pending):
            //   * a queued Welcome wins — defer to it (it self-heals into
            //     external-join on a later pass if it never applies);
            //   * otherwise external-join, UNLESS this device was revoked (its
            //     `user_device` row is gone) OR its user was removed from the group
            //     — either must not climb back in (a revoked/removed member's
            //     external-join squats an epoch and, under
            //     UNIQUE(conversation_id, epoch), wedges the group; absent a DS
            //     membership gate on `/v1/commits` a removed member would also
            //     rejoin the tree and decrypt post-removal traffic — membership
            //     leak, fuzzer finding #2).
            let welcome_pending = has_pending_welcome(state, mls_group_id, user_id).await;
            let may_rejoin = may_rejoin_via_external_join(state, mls_group_id, user_id).await;
            // #832: external-join builds a commit ON the published GroupInfo, so
            // with no `mls_group_info` row there is nothing to join onto. Feeding
            // that to the gate is what stops it selecting an impossible join and
            // retrying it once per pass through the pre-establishment window.
            let group_info_available =
                published_group_info_head(state, mls_group_id).await.is_some();
            match super::invariants::join_recovery(
                welcome_pending,
                may_rejoin,
                group_info_available,
            ) {
                super::invariants::JoinRecovery::AwaitWelcome => {
                    if welcome_pending {
                        eprintln!(
                            "[mls] process_pending_commits: no local group for {mls_group_id} but an \
                             undelivered Welcome targets this device — deferring to the Welcome path \
                             (not external-joining) to avoid racing our own Welcome"
                        );
                    } else {
                        // No GroupInfo yet: the group is not established from our
                        // view, so a Welcome is the only way in. Poll for it NOW
                        // rather than waiting for the next sweep — idempotent, and
                        // it is the step that actually converges this state (#832).
                        eprintln!(
                            "[mls] process_pending_commits: no local group for {mls_group_id} and no \
                             published GroupInfo — awaiting a Welcome (external-join would have \
                             nothing to join onto); polling welcomes now"
                        );
                        // `Box::pin`: poll_mls_welcomes can re-enter this path
                        // (welcome → apply → process commits), so the future is
                        // recursive and needs indirection to have a finite size.
                        if let Err(e) =
                            Box::pin(super::poll_mls_welcomes(state, user_id.to_string())).await
                        {
                            eprintln!(
                                "[mls] process_pending_commits: welcome poll for {mls_group_id} failed: {e}"
                            );
                        }
                    }
                }
                super::invariants::JoinRecovery::ExternalJoin => {
                    // Lock already held by the wrapper, so call the unlocked inner variant.
                    if let Err(e) = external_join_group_inner(state, mls_group_id, user_id).await {
                        eprintln!("[mls] process_pending_commits: no local group for {mls_group_id}, external-join failed: {e}");
                    }
                }
                super::invariants::JoinRecovery::StayOut => {
                    // `may_rejoin_via_external_join` already logged the specific
                    // revoked/removed reason.
                }
            }
            return Ok(false);
        }
    };

    // #418 interleave: decrypt the envelopes sealed at the member's CURRENT
    // epoch before any commit advances the group past it. (No-op for callers
    // that pass no hook — e.g. send / reconcile catch-up.)
    if let Some(hook) = on_epoch.as_deref_mut() {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            hook(db.conn(), generation, initial_epoch);
        }
    }

    // 2. Fetch pending commits from remote, along with the add-metadata
    //    columns (`added_user_id`, `added_device_ids`) so we can verify
    //    cross-signing certs BEFORE calling `process_message`. Collected
    //    into an owned Vec so the `rows` cursor is dropped before any
    //    local-DB await below.
    // Read-only commit-log fetch → log_db (falls back to remote_db pre-cutover).
    let conn = state.log_db.conn().await?;
    // Scoped to ONE lineage: commits of a different generation are encrypted
    // under a different suite's key schedule and are not replayable here.
    let mut rows = conn.query(
        "SELECT seq, epoch, commit_data, added_user_id, added_device_ids, sender_id \
         FROM mls_commit_log \
         WHERE conversation_id = ?1 AND generation = ?2 AND epoch >= ?3 \
         ORDER BY epoch ASC, seq ASC",
        libsql::params![mls_group_id, generation, initial_epoch as i64],
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
    // Set when a commit could not be applied and the lineage must be rebuilt
    // (#680). Carried out of the loop so recovery is driven by an EXPLICIT reason,
    // never inferred from a group's mere absence — a group that exists locally yet
    // cannot advance must still recover, not wedge.
    let mut recover: Option<RecoverReason> = None;
    for commit in pending {
        // Gap classification (I1), proved by Kani (`invariants::classify`) never
        // to `Apply` across a gap: `Apply` iff this row's epoch is exactly
        // `current_epoch`, else `GapRecover`. Abstracted by the `Apply` /
        // `ExternalJoin` actions in `specs/tla/CommitLog.tla` (Spec A): TLC
        // checks this replay+recovery loop never adopts a foreign commit
        // (NoForeignAdopt, I2) over the log the DS `submit_commit` maintains.
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
            let _ = forget_local_mls_group_at(state, mls_group_id, generation).await;
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
                        .as_deref() == Some(added_user_id.as_str());
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
            let outcome =
                apply_one_commit(&provider, mls_group_id, generation, commit.epoch, &commit_data);
            match outcome {
                CommitApply::Applied => true,
                CommitApply::Recover(reason) => {
                    eprintln!(
                        "[mls] process_pending_commits: {mls_group_id} cannot advance past epoch {} ({reason:?}) — recovering rather than wedging (#680)",
                        commit.epoch
                    );
                    recover = Some(reason);
                    break;
                }
            }
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
                    hook(db.conn(), generation, current_epoch);
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
            // Clearing a pending commit is storage-only — no suite dispatch.
            if let Some(mut group) = load_stored_group_at(db.conn(), mls_group_id, generation) {
                if group.pending_commit().is_some() {
                    eprintln!(
                        "[mls] process_pending_commits: clearing a dangling pending commit for {mls_group_id} (never landed) to avoid a phantom-epoch merge"
                    );
                    let _ = group.clear_pending_commit(&store_only(db.conn()));
                }
            }
        }
    }

    // Suite-generation backstop (#454 P4). The lineage we just replayed may have
    // been RETIRED by a migration, in which case no future commit or message will
    // ever appear on it again and staying put is a permanent stall. Runs here —
    // after the replay, so the predecessor is drained to its final epoch, and
    // after the dangling-pending clear, so we never carry a doomed commit across
    // a boundary.
    //
    // Only on an INGESTING pass (`on_epoch.is_some()`). Adoption deletes the
    // predecessor's key material, and under `max_past_epochs = 0` that is the
    // only thing that can still decrypt envelopes sealed on the old lineage. A
    // pass with no hook has fetched no envelopes, so adopting there would discard
    // messages this device was eligible to read. Every production path into this
    // function runs through `catch_up_mls_group_interleaved`, which always passes
    // a hook, so this costs no liveness — it only refuses to advance on a pass
    // that could not have drained first.
    if on_epoch.is_some() {
        if let Some(new_generation) =
            maybe_advance_generation(state, mls_group_id, user_id, generation).await
        {
            eprintln!(
                "[mls] process_pending_commits: {mls_group_id} advanced generation \
                 {generation} -> {new_generation} — replaying the new lineage"
            );
            // The exporter secret is per-group, and we are now on a different
            // group entirely, so any live voice room must re-derive immediately.
            crate::commands::voice_e2ee::on_mls_epoch_changed(state, mls_group_id).await;
            return Ok(true);
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

    // If the group was deleted during processing (e.g. eviction), OR a commit
    // could not be applied and asked us to recover (#680 — the four former `Stop`
    // wedge sites), external-join to rebuild. The explicit `recover` reason is the
    // load-bearing addition: previously recovery was gated ONLY on `!group_exists`,
    // so a group that still existed but could never advance (corrupt commit bytes,
    // a merge failure, a diverged tree) wedged permanently. `apply_one_commit`
    // deletes the group before returning `Recover`, so in practice `group_exists`
    // is already false here — but we key off the reason so a wedge can never again
    // be silently mistaken for "caught up".
    let group_exists = {
        let guard = state.local_db.lock().await;
        guard.as_ref().is_some_and(|db| {
            load_stored_group_at(db.conn(), mls_group_id, generation).is_some()
        })
    };
    if !group_exists || recover.is_some() {
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

    // Retention signal (#539, I4): report this device's now-current applied MLS
    // epoch so the DS can compute the commit-log prune floor from the SLOWEST
    // current member (Tier 1, zero loss — Spec B `NoLossForCurrentMember`).
    // Best-effort + event-driven (fires on catch-up, never polls); a failed or
    // skipped report only leaves the floor conservative (Tier 2's cap still bounds
    // storage). Read the final epoch from the local group so it reflects both a
    // normal replay and an external-join recovery above.
    report_applied_epoch(state, mls_group_id, user_id).await;

    Ok(false)
}

/// Adopt a newer suite generation for `mls_group_id` if one exists, returning the
/// generation adopted.
///
/// Two ways a device learns it has been migrated, tried in order:
///
/// 1. **The successor is already here.** Its Welcome landed and
///    [`super::welcomes::apply_welcome`] stored it under the successor's own
///    `GroupId`, deliberately inert until now. This is the ordinary path, and it
///    needs no network: flip the lineage row and delete the predecessor.
/// 2. **The successor exists but we have no Welcome for it** — dropped, or this
///    device was added to the new lineage without one. The published GroupInfo is
///    always the newest lineage (its upsert is monotone on `(generation, epoch)`),
///    so a head generation above ours is proof. Recover by external-joining it,
///    behind the same revoked/removed gates as every other rebuild: a device that
///    was legitimately removed must not climb into the successor either.
///
/// `None` means "still on the newest lineage we can see" — the overwhelmingly
/// common case, and one indexed local read plus one indexed remote read.
async fn maybe_advance_generation(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
    generation: i64,
) -> Option<i64> {
    // 1. A stored-but-unadopted successor, applied from a Welcome.
    let stored_successor = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref()?;
        (1..=MAX_GENERATION_HOPS as i64)
            .map(|hop| generation + hop)
            .find(|g| load_stored_group_at(db.conn(), mls_group_id, *g).is_some())
    };
    if let Some(successor) = stored_successor {
        // The log is the authority on which lineages exist; a stored group proves
        // only that THIS device built or received one. A migration that lost its
        // compare-and-swap leaves behind exactly that — a group on a generation
        // the conversation never opened — and adopting it would walk us off a
        // healthy lineage onto a dead one. The DS writes the opening commit, its
        // GroupInfo and its Welcomes in one transaction, so a Welcome we could
        // have applied implies a GroupInfo we can see: this confirmation has no
        // false negatives, and it costs a read only in the rare case that a
        // successor is sitting here at all.
        match published_group_info_head(state, mls_group_id).await {
            Some((head_generation, _)) if head_generation >= successor => {
                adopt_generation_locked(state, mls_group_id, generation, successor).await;
                return Some(successor);
            }
            _ => {
                eprintln!(
                    "[mls] process_pending_commits: {mls_group_id} holds a group at generation \
                     {successor} that the log does not know about — ignoring it (an abandoned \
                     migration) and staying on {generation}"
                );
            }
        }
    }

    // 2. No successor locally. Ask the log whether one exists at all.
    let (head_generation, _) = published_group_info_head(state, mls_group_id).await?;
    if head_generation <= generation {
        return None;
    }
    if !may_rejoin_via_external_join(state, mls_group_id, user_id).await {
        return None;
    }
    eprintln!(
        "[mls] process_pending_commits: {mls_group_id} was migrated to generation \
         {head_generation} but no Welcome reached this device — external-joining the successor"
    );
    // The caller holds the MLS lock, so use the unlocked inner variant. On
    // success it records the new lineage itself; re-read rather than assume.
    if let Err(e) = external_join_group_inner(state, mls_group_id, user_id).await {
        eprintln!(
            "[mls] process_pending_commits: successor external-join failed for {mls_group_id}: {e}"
        );
        return None;
    }
    let adopted = {
        let guard = state.local_db.lock().await;
        guard
            .as_ref()
            .map_or(generation, |db| local_generation(db.conn(), mls_group_id))
    };
    if adopted > generation {
        // The predecessor is drained and superseded; its key material is now pure
        // liability (it can still decrypt pre-migration traffic).
        let _ = forget_local_mls_group_at(state, mls_group_id, generation).await;
        Some(adopted)
    } else {
        None
    }
}

/// Move this device onto `successor` for `mls_group_id`: record the lineage,
/// drop the drained predecessor, and re-arm the post-join self-update.
///
/// Assumes the caller holds the per-conversation MLS lock and that the
/// predecessor has been drained — see [`maybe_advance_generation`].
pub(super) async fn adopt_generation_locked(
    state: &Arc<AppState>,
    conversation_id: &str,
    predecessor: i64,
    successor: i64,
) {
    {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else { return };
        set_local_generation(db.conn(), conversation_id, successor);
        // We entered the successor by Welcome, so our leaf there is UNMERGED —
        // regardless of how recently we rotated in the predecessor. Every commit
        // in the new lineage pays for it until we commit once (#666), so the
        // rotation clock restarts with the lineage.
        super::self_update::clear_self_update(db.conn(), conversation_id);
    }
    // Only now is the predecessor safe to destroy: the lineage row already points
    // past it, so a crash between these two steps leaves a harmless orphan rather
    // than a device with no group at all.
    let _ = forget_local_mls_group_at(state, conversation_id, predecessor).await;
}

/// Why the local group could not advance from a commit — see [`apply_one_commit`]
/// and [`classify_process_message_error`].
///
/// Every variant is RECOVERABLE by delete-and-rejoin (external join). The reason
/// is carried so (a) the caller acts on the specific failure, and (b) a wedge is
/// a *distinct* return value from `Applied`/"caught up" and can never be silently
/// mistaken for "done" — that conflation is the #680 bug, where four `Stop` sites
/// left a group that exists locally yet can never advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecoverReason {
    /// The stored group is gone (never built, or already deleted).
    GroupMissing,
    /// The stored group failed to load from storage.
    GroupLoadFailed,
    /// The commit bytes did not TLS-deserialize (corrupt / truncated row).
    MalformedCommit,
    /// The deserialized message was not a protocol message.
    NotAProtocolMessage,
    /// Merging the staged commit failed (storage error / diverged tree).
    MergeFailed,
    /// Our keys cannot open the commit — we were evicted from the group.
    Evicted,
    /// The commit is at our epoch yet won't stage/validate: our local tree has
    /// diverged from the canonical branch (fork; #411, prod incident 01KQYX89…).
    ForkedTree,
    /// Our own commit was echoed back but merging the pending commit failed.
    OwnCommitMergeFailed,
    /// An openmls error this code does not specifically classify. Recover
    /// conservatively — NEVER wedge. A future openmls bump that reshapes the
    /// error surface lands HERE (loudly, and caught by the classification unit
    /// tests) instead of silently wedging on a stale string match (#680).
    Unclassified,
}

/// Outcome of applying one commit from the log — see [`apply_one_commit`].
#[derive(Debug)]
pub(super) enum CommitApply {
    /// Merged; the local epoch advanced.
    Applied,
    /// The local group cannot advance on this lineage and must be rebuilt. The
    /// reason is carried so the caller recovers (external-join) rather than
    /// treating the stop as "caught up". `apply_one_commit` has already deleted
    /// the local group where one was loaded, so the caller's rejoin path has a
    /// clean slate.
    Recover(RecoverReason),
}

/// Map an openmls `process_message` error to the recovery it warrants.
///
/// Matching the real error ENUMS — not their `Display` strings (`format!("{e}")`)
/// — is the load-bearing change of #680: a string match silently reclassifies the
/// moment openmls rewords a message or moves a case (as the pinned rev already did
/// for the own-commit path), and the group wedges with no signal. An enum match is
/// a compile-checked, test-pinned contract; anything unrecognised falls to
/// [`RecoverReason::Unclassified`], which still recovers rather than wedging.
pub(super) fn classify_process_message_error<S>(
    e: &ProcessMessageError<S>,
) -> RecoverReason {
    use ProcessMessageError as E;
    match e {
        // We tried to use a group we've been evicted from: our leaf is gone and
        // our keys can't open this or any later commit. Re-join only if still a
        // roster member — the `may_rejoin` gate downstream enforces that.
        E::GroupStateError(MlsGroupStateError::UseAfterEviction) => RecoverReason::Evicted,
        // Our OWN commit came back but does NOT match our pending commit — our
        // pending state diverged from what actually landed. This was the old
        // "created by this client" string; now a typed StageCommitError. Merging
        // our stale pending here would diverge us further, so rebuild instead.
        E::InvalidCommit(StageCommitError::OwnCommitMismatch) => RecoverReason::ForkedTree,
        // The commit passed the log's epoch-gap check yet still fails to stage or
        // validate: our local tree has diverged from the canonical branch. Every
        // remaining StageCommit / Validation failure is this same fork shape.
        E::InvalidCommit(_) | E::ValidationError(_) => RecoverReason::ForkedTree,
        // Anything else (LibraryError, StorageError, wire-format policy, and any
        // future variant) — recover conservatively rather than wedge.
        _ => RecoverReason::Unclassified,
    }
}

/// Apply a single commit from the log to the local group.
///
/// `provider` must serve the group's suite (the caller dispatches on the stored
/// group's ciphersuite). Errors are handled in-place rather than returned
/// because each has its own recovery, and none of them should abort the caller's
/// replay loop with an `Err` — see [`CommitApply`].
///
/// Sync: the caller owns the local-DB guard.
pub(super) fn apply_one_commit<C>(
    provider: &MlsProvider<'_, C>,
    mls_group_id: &str,
    generation: i64,
    commit_epoch: i64,
    commit_data: &[u8],
) -> CommitApply
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let group_id = super::generation::mls_group_id(mls_group_id, generation);
    let mut group = match MlsGroup::load(provider.storage(), &group_id) {
        Ok(Some(g)) => g,
        // The group is simply absent (never built, or already deleted by an
        // earlier recovery). Nothing to delete; the caller's rejoin path rebuilds.
        Ok(None) => {
            eprintln!("[mls] process_pending_commits: no local group for {mls_group_id} gen {generation} at epoch {commit_epoch} — recovering via external-join");
            return CommitApply::Recover(RecoverReason::GroupMissing);
        }
        Err(e) => {
            eprintln!("[mls] process_pending_commits: mls load failed for {mls_group_id}: {e} — recovering via external-join");
            return CommitApply::Recover(RecoverReason::GroupLoadFailed);
        }
    };

    let mut reader: &[u8] = commit_data;
    let msg_in = match MlsMessageIn::tls_deserialize(&mut reader) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[mls] process_pending_commits: deserialize failed for {mls_group_id} at epoch {commit_epoch}: {e} — recovering via external-join");
            let _ = group.delete(provider.storage());
            return CommitApply::Recover(RecoverReason::MalformedCommit);
        }
    };
    let protocol_msg = match msg_in.try_into_protocol_message() {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[mls] process_pending_commits: protocol msg failed for {mls_group_id} at epoch {commit_epoch}: {e} — recovering via external-join");
            let _ = group.delete(provider.storage());
            return CommitApply::Recover(RecoverReason::NotAProtocolMessage);
        }
    };

    match group.process_message(provider, protocol_msg) {
        Ok(processed) => match processed.into_content() {
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                if let Err(e) = group.merge_staged_commit(provider, *staged) {
                    eprintln!("[mls] process_pending_commits: merge failed for {mls_group_id} at epoch {commit_epoch}: {e} — recovering via external-join");
                    let _ = group.delete(provider.storage());
                    return CommitApply::Recover(RecoverReason::MergeFailed);
                }
            }
            // Our OWN commit, fanned back to us at this epoch — e.g. we submitted
            // it but a lost response made us converge instead of merging (issue
            // #411). At the pinned openmls rev this is surfaced as CONTENT (an
            // `Ok`), NOT an error: `OwnPendingCommit` when the commit is framed as
            // a PublicMessage, or `OwnPrivateMessage` when framed as a
            // PrivateMessage. Pollis frames handshake messages as PrivateMessage,
            // so `OwnPrivateMessage` is the path we ACTUALLY hit — its ciphertext
            // cannot be decrypted by its own author, so openmls can't stage it and
            // hands it back for us to adopt.
            //
            // The pre-#680 code matched only the OLD "created by this client"
            // error STRING in the `Err` arm, which at this rev catches NEITHER
            // shape. So the own-commit replay fell through unmerged: it was
            // miscounted as `Applied`, the pending commit was left dangling, and
            // the dangling-clear backstop then DISCARDED our landed commit —
            // stranding us at a phantom epoch no other member shares. Adopt it by
            // merging our pending commit so we land at the same epoch as everyone
            // else.
            ProcessedMessageContent::OwnPendingCommit
            | ProcessedMessageContent::OwnPrivateMessage => {
                // A commit-log slot always carries a Commit, so an own message
                // here is always our own commit. If we hold no pending commit to
                // adopt (e.g. a crash cleared it) we cannot advance from it
                // locally — rebuild from the published GroupInfo rather than wedge.
                if group.pending_commit().is_none() {
                    eprintln!(
                        "[mls] process_pending_commits: own commit at epoch {commit_epoch} for {mls_group_id} but no pending commit to adopt — recovering via external-join"
                    );
                    let _ = group.delete(provider.storage());
                    return CommitApply::Recover(RecoverReason::OwnCommitMergeFailed);
                }
                if let Err(merge_err) = group.merge_pending_commit(provider) {
                    eprintln!(
                        "[mls] process_pending_commits: own commit at epoch {commit_epoch} for {mls_group_id} but merge_pending failed ({merge_err}) — recovering via external-join"
                    );
                    let _ = group.delete(provider.storage());
                    return CommitApply::Recover(RecoverReason::OwnCommitMergeFailed);
                }
                eprintln!(
                    "[mls] process_pending_commits: adopted our own commit at epoch {commit_epoch} for {mls_group_id}"
                );
                // Fall through so this counts as applied and the epoch advances.
            }
            // A commit-log slot only ever carries a Commit, so proposal /
            // application content here is impossible in practice. Treat it as a
            // no-op advance rather than wedging on an unreachable shape.
            _ => {
                eprintln!(
                    "[mls] process_pending_commits: unexpected non-commit content at epoch {commit_epoch} for {mls_group_id} — skipping"
                );
            }
        },
        Err(e) => {
            // Classify by the real error enum, then recover by dropping the local
            // group so the caller's external-rejoin rebuilds it from the latest
            // published GroupInfo. Deleting drops only this device's MLS crypto
            // state, not its decrypted message history (that lives in the local
            // `message` table). The rejoin lands at the latest epoch, so the next
            // pass filters `epoch >= new_epoch` and can't re-fail on the same
            // commit — no recovery loop.
            let reason = classify_process_message_error(&e);
            eprintln!(
                "[mls] process_pending_commits: commit at epoch {commit_epoch} for {mls_group_id} failed to apply ({e}) — classified {reason:?}, deleting local group to re-join"
            );
            let _ = group.delete(provider.storage());
            return CommitApply::Recover(reason);
        }
    }

    CommitApply::Applied
}

/// Read this device's current local MLS epoch for `mls_group_id` and report it to
/// the DS as the retention high-water (`ds_client::ds_report_commit_since`). No-op
/// when there is no local group (nothing applied to report). See #539.
///
/// The report is DETACHED (`tokio::spawn`): it must never hold the per-conversation
/// MLS lock across a network round-trip, so a slow/unreachable DS can't stall
/// catch-up. Fully best-effort — the spawned task swallows every error.
async fn report_applied_epoch(state: &Arc<AppState>, mls_group_id: &str, user_id: &str) {
    // The high-water is the PAIR `(generation, epoch)`, compared
    // lexicographically by the DS (#454 P4): epoch 3 of a successor lineage is
    // ahead of epoch 12 of the retired one, not behind it. Reporting the epoch
    // alone would make every migrated device look like it had regressed, and the
    // retention floor is computed from the slowest member.
    let head = {
        let guard = state.local_db.lock().await;
        guard.as_ref().and_then(|db| {
            let generation = local_generation(db.conn(), mls_group_id);
            load_stored_group_at(db.conn(), mls_group_id, generation)
                .map(|g| (generation, g.epoch().as_u64()))
        })
    };
    if let Some((generation, epoch)) = head {
        let state = Arc::clone(state);
        let conversation_id = mls_group_id.to_string();
        let user_id = user_id.to_string();
        tokio::spawn(async move {
            super::ds_client::ds_report_commit_since(
                &state,
                &conversation_id,
                &user_id,
                generation,
                epoch as i64,
            )
            .await;
        });
    }
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
    load_stored_group(conn, conversation_id).is_some()
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
    let generation = local_generation(conn, conversation_id);
    let provider = PollisProvider::new(conn);
    let (mut group, signer) =
        load_group_with_signer(&provider, conversation_id, generation).ok()?;
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
///
/// Returns the `(generation, epoch)` PAIR, not the epoch alone (#454 P4). The
/// generation is read off the envelope's MLS `GroupId`, which our own
/// [`super::generation::mls_group_id`] suffixed at group-creation time — so it is
/// authenticated by the sender's own group state, not asserted by the server.
/// Without it, an un-ingested envelope from a retired lineage sealed at epoch 7
/// would read as "ahead of" the successor's epoch 1 and stall the ingest
/// watermark permanently.
pub fn envelope_lineage(ciphertext: &[u8]) -> Option<(i64, u64)> {
    let mut reader: &[u8] = ciphertext;
    let msg_in = MlsMessageIn::tls_deserialize(&mut reader).ok()?;
    let protocol_msg = msg_in.try_into_protocol_message().ok()?;
    let raw_group_id = String::from_utf8_lossy(protocol_msg.group_id().as_slice()).into_owned();
    let (_, generation) = super::generation::split_mls_group_id(&raw_group_id);
    Some((generation, protocol_msg.epoch().as_u64()))
}

/// Try to decrypt MLS ciphertext bytes for `conversation_id`.
///
/// The bytes must be TLS-serialised `MlsMessageOut` (i.e. what we stored in
/// `message_envelope.ciphertext` after `send_message` used MLS). Returns
/// `Some((plaintext, credential_user_id))` on success, or `None` if the bytes
/// are not a valid MLS `ApplicationMessage` or if decryption fails for any
/// reason.
///
/// The second tuple element is the **MLS-authenticated** sender — the `user_id`
/// parsed from the sender's `BasicCredential` inside the ciphertext
/// (`{user_id}:{device_id}`). This is the load-bearing primitive of sealed
/// sender (`docs/metadata-minimization-design.md` §2): attribution comes from
/// this crypto-authenticated credential, NOT from the server-writable
/// `message_envelope.sender_id` column. Returning both together makes it
/// impossible to obtain plaintext without also obtaining the authenticated
/// sender, so no caller can accidentally fall back to trusting the envelope
/// column.
pub fn try_mls_decrypt(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    ciphertext: &[u8],
) -> Option<(Vec<u8>, String)> {
    MlsDecryptor::open(conn, conversation_id)?.decrypt(ciphertext)
}

/// A group loaded **once** for a RUN of envelopes that are all sealed at the
/// epoch it currently sits at.
///
/// [`try_mls_decrypt`] is this type used for a run of length one, and that was
/// the whole decrypt path until #875: every envelope re-read the join config,
/// group context, ratchet tree, transcript hash, confirmation tag, leaf index,
/// group state, message secrets, resumption PSKs and proposal queue out of
/// `mls_kv` and re-parsed them, to decrypt one message and throw the lot away.
/// Measured at 12 `mls_kv` SELECTs + 1 UPDATE per message, so a 200-message
/// catch-up was 2400 statements where 12 would do.
///
/// **Why holding the group is not a shortcut.** Decrypting an application
/// message mutates exactly one piece of group state — the secret tree inside
/// `message_secrets_store`, which ratchets forward so a consumed key cannot be
/// reused — and openmls persists that itself, inside `process_message`, before
/// it returns (`will_modify_secret_tree` → `write_message_secrets`), precisely
/// so forward secrecy survives a crash mid-batch. Nothing else about the group
/// changes. So the reload this replaces was reading back the bytes the previous
/// decrypt had just written: an in-memory group carried across a run is
/// byte-identical to a group reloaded before each envelope, and a crash
/// half-way through a run leaves exactly the state a crash half-way through the
/// old loop left.
///
/// **What it must NOT span.** A commit changes the group out from under a held
/// copy, so a decryptor must not outlive the epoch it was opened at. The ingest
/// interleave (`messages::ingest`) opens one per `on_epoch` firing and drops it
/// before the replay applies the next commit, which is the only correct
/// lifetime — anything longer would decrypt against a superseded key schedule.
pub struct MlsDecryptor<'a> {
    provider: PollisProvider<'a>,
    group: MlsGroup,
}

impl<'a> MlsDecryptor<'a> {
    /// Load the conversation's live group, or `None` when this device has none
    /// (never joined, or forgotten) — the same "cannot decrypt" answer
    /// [`try_mls_decrypt`] gives, so callers need no new branch.
    pub fn open(conn: &'a rusqlite::Connection, conversation_id: &str) -> Option<Self> {
        let group = load_stored_group(conn, conversation_id)?;
        Some(Self {
            provider: PollisProvider::new(conn),
            group,
        })
    }

    /// Decrypt one envelope. See [`try_mls_decrypt`] for the contract — this is
    /// that function's body, and the `(plaintext, MLS-authenticated sender)`
    /// pair it returns is load-bearing for sealed sender.
    pub fn decrypt(&mut self, ciphertext: &[u8]) -> Option<(Vec<u8>, String)> {
        let mut reader: &[u8] = ciphertext;
        let msg_in = MlsMessageIn::tls_deserialize(&mut reader).ok()?;
        let protocol_msg = msg_in.try_into_protocol_message().ok()?;
        // Envelopes from a RETIRED lineage cannot be decrypted by the successor,
        // and their predecessor group is gone. Fail fast rather than handing a
        // foreign-group message to `process_message`, whose error would be
        // indistinguishable from a genuine decrypt failure.
        if protocol_msg.group_id() != self.group.group_id() {
            return None;
        }
        let processed = self.group.process_message(&self.provider, protocol_msg).ok()?;

        // Grab the authenticated sender credential BEFORE `into_content` consumes it.
        let sender_user_id = parse_credential_user_id(processed.credential());

        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app_msg) => {
                Some((app_msg.into_bytes(), sender_user_id))
            }
            _ => None,
        }
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
        assert!(group_info_is_stale(None, (0, 0)));
        assert!(group_info_is_stale(None, (0, 7)));
    }

    #[test]
    fn republishes_when_groupinfo_is_behind() {
        // A later epoch-advance publish was dropped; the stored GroupInfo lags our
        // local epoch, so the current epoch isn't externally joinable until we heal.
        assert!(group_info_is_stale(Some((0, 0)), (0, 1)));
        assert!(group_info_is_stale(Some((0, 3)), (0, 7)));
    }

    #[test]
    fn skips_when_groupinfo_is_current() {
        // Already durable at our epoch — no DS round-trip needed.
        assert!(!group_info_is_stale(Some((0, 0)), (0, 0)));
        assert!(!group_info_is_stale(Some((0, 5)), (0, 5)));
    }

    #[test]
    fn skips_when_groupinfo_is_ahead() {
        // Another member advanced and published past us; don't republish a stale
        // view of an epoch we no longer lead.
        assert!(!group_info_is_stale(Some((0, 9)), (0, 4)));
    }

    /// The #454 P4 property the scalar comparison could not express: a successor
    /// lineage restarts at MLS epoch 0, numerically *below* the retired lineage's
    /// last epoch. Compared lexicographically on `(generation, epoch)`, the
    /// published generation-0 GroupInfo is correctly stale against our
    /// generation-1 head — a scalar `published >= local` would have read the
    /// migrated group as "already durable" and left the successor unjoinable.
    #[test]
    fn a_successors_epoch_zero_is_ahead_of_the_retired_lineage() {
        assert!(group_info_is_stale(Some((0, 42)), (1, 0)));
        assert!(!group_info_is_stale(Some((1, 0)), (0, 42)));
    }
}
