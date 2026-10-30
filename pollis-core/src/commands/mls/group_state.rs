//! MLS group lifecycle: create / load / forget / encrypt / decrypt /
//! commit processing / external join / GroupInfo publishing.

use openmls::prelude::group_info::VerifiableGroupInfo;
use openmls::framing::errors::{MessageDecryptionError, SecretTreeError};
use openmls::prelude::*;
use openmls_basic_credential::SignatureKeyPair;
use openmls_traits::OpenMlsProvider;

use std::sync::Arc;
use tls_codec::{Deserialize as TlsDeserialize, Serialize as TlsSerialize};

use crate::error::Result;
use crate::state::AppState;

use super::device::{load_or_create_device_signer, AddedLeaf, IdentityDirectory, LeafVerdict};
use super::generation::{local_generation, mls_group_id, set_local_generation};
use super::provider::{
    current_suite, load_stored_group, load_stored_group_at, make_credential,
    parse_credential_device_id, parse_credential_user_id, signature_scheme, store_only,
    MlsProvider, PollisProvider,
};

/// Test-harness rendezvous points inside the join and commit paths.
///
/// Deterministic join-path interleavings are otherwise unreachable from a test.
#[cfg(feature = "test-harness")]
pub mod rendezvous {
    use std::sync::Mutex;
    use tokio::sync::{mpsc, oneshot};

    /// Handle a parked client hands the test: dropping or sending on it
    /// releases the client.
    pub type Release = oneshot::Sender<()>;

    /// The points a test can park a client at.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Point {
        /// External commit stored locally, CAS not yet submitted.
        ExternalJoinBeforeSubmit,
        /// `reconcile_group_mls_impl` entered, per-conversation lock not yet
        /// taken.
        ReconcileEntry,
        /// Self-update commit staged under the lock, CAS not yet submitted.
        SelfUpdateBeforeSubmit,
        /// An envelope sealed, not yet posted (#1041).
        EnvelopeBeforePost,
        /// An external join lost its CAS; the retry has not started (#1220).
        ExternalJoinAfterLostRace,
    }

    const POINTS: usize = 5;

    static ARMED: Mutex<[Option<mpsc::UnboundedSender<Release>>; POINTS]> =
        Mutex::new([None, None, None, None, None]);

    /// Arm `point`: clients reaching it park and send a [`Release`] on the returned receiver.
    pub fn arm(point: Point) -> mpsc::UnboundedReceiver<Release> {
        let (tx, rx) = mpsc::unbounded_channel();
        ARMED.lock().expect("rendezvous poisoned")[point as usize] = Some(tx);
        rx
    }

    /// Disarm `point` so later arrivals run straight through.
    pub fn disarm(point: Point) {
        ARMED.lock().expect("rendezvous poisoned")[point as usize] = None;
    }

    pub(crate) async fn park(point: Point) {
        let tx = ARMED.lock().expect("rendezvous poisoned")[point as usize].clone();
        if let Some(tx) = tx {
            let (release, parked) = oneshot::channel();
            if tx.send(release).is_ok() {
                let _ = parked.await;
            }
        }
    }
}

/// Export a fresh `GroupInfo` for the conversation and publish it via the DS.
///
/// Overwritten only by a STRICTLY greater epoch, so concurrent writers are idempotent.
///
/// No-op if:
///   - the device has no local MLS group for this conversation
///   - the device has no `account_id_key` (pre-enrollment)
pub async fn publish_group_info(
    state: &Arc<AppState>,
    conversation_id: &str,
) -> crate::error::Result<()> {
    // Sync scope: nothing !Send crosses await.
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

    // W4 seam: route through the DS (sole writer), like `submit_commit`.
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
/// `Ok(None)` when there is nothing usable to publish: this is a best-effort backstop.
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
    // A staged commit means the epoch is undecided; publishing GroupInfo for it is the #1079 defect.
    if group.pending_commit().is_some() {
        return Ok(None);
    }
    let epoch = group.epoch().as_u64();
    let msg = match group.export_group_info(provider.crypto(), &signer, true) {
        Ok(m) => m,
        Err(e) => {
            crate::diag!("[mls] publish_group_info: export failed for {conversation_id}: {e}");
            return Ok(None);
        }
    };
    let bytes = msg
        .tls_serialize_detached()
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("group_info serialize: {e}")))?;
    Ok(Some((epoch, bytes)))
}

/// Parse a stored `mls_group_info` payload, accepting BOTH wire forms.
///
/// Current writers wrap it in `MlsMessage`; older rows are a bare `GroupInfo`, told apart only by parse.
///
/// Misreading the bare form fails like this:
///
/// ```text
/// stored group_info envelope failed to deserialize: UnknownValue(82)
/// ```
///
/// That is the ciphersuite (`0x0052`) read where `wire_format` should be, not corruption.
///
/// Safe: the bare form is tried only after the strict envelope parse fails.
fn parse_stored_group_info(group_info_bytes: &[u8]) -> Result<VerifiableGroupInfo> {
    let mut env_reader: &[u8] = group_info_bytes;
    let envelope_err = match MlsMessageIn::tls_deserialize(&mut env_reader) {
        Ok(msg_in) => match msg_in.extract() {
            MlsMessageBodyIn::GroupInfo(gi) => return Ok(gi),
            other => {
                return Err(crate::error::Error::Other(anyhow::anyhow!(
                    "expected GroupInfo in mls_group_info, got {:?}",
                    std::mem::discriminant(&other)
                )));
            }
        },
        Err(e) => e,
    };

    // Legacy row: a bare GroupInfo with no MlsMessage envelope.
    let mut bare_reader: &[u8] = group_info_bytes;
    match VerifiableGroupInfo::tls_deserialize(&mut bare_reader) {
        Ok(gi) => {
            crate::diag!(
                "[mls] stored group_info is the legacy bare-GroupInfo form; accepted. \
                 It will be republished in the envelope form on the next publish."
            );
            Ok(gi)
        }
        Err(bare_err) => Err(crate::error::Error::Other(anyhow::anyhow!(
            "stored group_info failed to deserialize as an MlsMessage envelope \
             ({envelope_err}) and as a bare GroupInfo ({bare_err})"
        ))),
    }
}

/// Whether the durably-published GroupInfo is stale relative to our local epoch
/// and must be republished.
///
/// `published` is the log's `(generation, epoch)` GroupInfo head (`None` = no row); `local` is ours.
///
/// Republish iff absent or behind us.
///
/// Lexicographic on `(generation, epoch)` (#454 P4): a successor restarts at epoch 0.
fn group_info_is_stale(published: Option<(i64, u64)>, local: (i64, u64)) -> bool {
    match published {
        None => true,
        Some(p) => p < local,
    }
}

/// Head of the GroupInfo stored for this group; a failed read is `None`, biasing toward republishing.
pub(super) async fn published_group_info_head(
    state: &Arc<AppState>,
    conversation_id: &str,
) -> Option<(i64, u64)> {
    let snap = super::ds_reads::conversation_snapshot(
        state,
        super::ds_reads::state_query(conversation_id, None),
    )
    .await
    .ok()?;
    group_info_head_of(&snap)
}

/// The `(generation, epoch)` head out of a conversation snapshot, or `None` when
/// no GroupInfo is published — or the caller was not authorized to see one.
pub(super) fn group_info_head_of(
    snap: &pollis_api::reads::ConversationState,
) -> Option<(i64, u64)> {
    let gi = snap.group_info.as_ref()?;
    Some((gi.generation, gi.epoch as u64))
}

/// Durability backstop for MLS bootstrap: ensure the log DB holds a current-epoch
/// GroupInfo for this group, republishing if a past publish was dropped.
///
/// Create-time publishing is best-effort; a dropped one would leave the group unjoinable forever.
///
/// Called on every group-touching pass; one read for healthy groups.
async fn ensure_group_info_published(state: &Arc<AppState>, conversation_id: &str) {
    let local_head = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else { return };
        let generation = local_generation(db.conn(), conversation_id);
        match load_stored_group_at(db.conn(), conversation_id, generation) {
            Some(g) => (generation, g.epoch().as_u64()),
            None => return,
        }
    };

    // One snapshot answers both staleness and membership; a removed member's republish would 403.
    let Ok(snap) = super::ds_reads::conversation_snapshot(
        state,
        super::ds_reads::state_query(conversation_id, None),
    )
    .await
    else {
        // Unreadable: treat the GroupInfo as absent, so we republish (a missed heal is worse).
        if let Err(e) = publish_group_info(state, conversation_id).await {
            crate::diag!(
                "[mls] ensure_group_info_published: republish for {conversation_id} failed: {e}"
            );
        }
        return;
    };
    if !snap.authorized {
        return;
    }
    let published = group_info_head_of(&snap);
    if !group_info_is_stale(published, local_head) {
        return;
    }

    if let Err(e) = publish_group_info(state, conversation_id).await {
        crate::diag!(
            "[mls] ensure_group_info_published: republish for {conversation_id} \
             (local head {local_head:?}, published {published:?}) failed: {e}"
        );
    }
}

/// Join an existing MLS group via external commit, using the latest
/// `GroupInfo` blob stored server-side in `mls_group_info`. The new
/// device becomes a full member of the group at the epoch *after* the
/// one carried in the GroupInfo.
///
/// Used by Secret Key recovery when no sibling device is online to issue a Welcome.
///
/// Not cross-signed outbound; members' inbound cert check rejects a device whose cert does not chain to `account_id_pub`.
pub async fn external_join_group(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    let _guard = state.mls_group_lock(conversation_id).await?;
    external_join_group_inner(state, conversation_id, user_id).await
}

/// Result of a single external-join attempt — see [`external_join_attempt`].
enum ExternalJoinResult {
    /// Our external commit won its epoch and was persisted.
    Joined,
    /// Another member committed the target epoch; discard the branch built in `generation`.
    LostRace { generation: i64 },
    /// A Welcome for this device is available; stand down rather than race it.
    DeferredToWelcome,
    /// A CONFIRMED group is already held (#1220); building another branch would discard it.
    AlreadyMember,
}

/// Body of [`external_join_group`]. Assumes the caller already holds the
/// per-conversation MLS lock (`state.mls_group_lock`).
///
/// Submits as a CAS on the target epoch; on a lost race, discard the branch and retry (bounded).
pub(crate) async fn external_join_group_inner(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    const MAX_JOIN_ATTEMPTS: u32 = 5;
    for attempt in 0..MAX_JOIN_ATTEMPTS {
        match external_join_attempt(state, conversation_id, user_id).await? {
            ExternalJoinResult::Joined => return Ok(()),
            // A Welcome became available: defer to `apply_welcome`.
            ExternalJoinResult::DeferredToWelcome => return Ok(()),
            // Already in, by another path (#1220). Success: nothing to join.
            ExternalJoinResult::AlreadyMember => {
                crate::diag!(
                    "[mls] external_join_group: {conversation_id} already has a confirmed local \
                     group (attempt {}/{MAX_JOIN_ATTEMPTS}) — standing down instead of replacing it",
                    attempt + 1
                );
                return Ok(());
            }
            ExternalJoinResult::LostRace {
                generation: join_generation,
            } => {
                crate::diag!(
                    "[mls] external_join_group: lost epoch race for {conversation_id} \
                     (attempt {}/{MAX_JOIN_ATTEMPTS}) — discarding local branch and retrying",
                    attempt + 1
                );
                // Drop only the doomed branch; a Welcome-confirmed group stays (#1220).
                forget_unconfirmed_branch(state, conversation_id, join_generation).await;
                // Test-harness only: park between the lost race and the retry,
                // the window in which the device's own Welcome can land.
                #[cfg(feature = "test-harness")]
                rendezvous::park(rendezvous::Point::ExternalJoinAfterLostRace).await;
                // Let the winner publish GroupInfo at the new epoch.
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

/// One external-join attempt: build against the current GroupInfo and CAS its epoch.
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

    // 1. Fetch GroupInfo and its generation; the published one is always the newest lineage.
    let snap = super::ds_reads::conversation_snapshot(
        state,
        pollis_api::reads::ConversationStateQuery {
            conversation_id: conversation_id.to_string(),
            generation: None,
            since: None,
            want_group_info_blob: true,
            commit_at_epoch: None,
        },
    )
    .await?;
    let (group_info_bytes, stored_generation, stored_epoch): (Vec<u8>, i64, i64) = {
        match snap.group_info.as_ref().and_then(|gi| {
            gi.group_info
                .as_deref()
                .map(|b| (b, gi.generation, gi.epoch))
        }) {
            Some((blob, generation, epoch)) => (
                super::ds_reads::decode_b64("group_info", blob)?,
                generation,
                epoch,
            ),
            None => {
                return Err(crate::error::Error::Other(anyhow::anyhow!(
                    "no GroupInfo stored for {conversation_id} — cannot external-join"
                )))
            }
        }
    };

    // The DS writes commit, GroupInfo and Welcomes in one transaction, so a GroupInfo implies any Welcome for us exists: prefer it rather than race our own Welcome.
    // Both facts must come from ONE snapshot (#987); a second read would reopen the race and make the Kani proof vacuous.
    if snap.welcome_pending {
        crate::diag!(
            "[mls] external_join: a Welcome for {conversation_id} is available — deferring to it \
             instead of external-joining (avoids racing our own Welcome)"
        );
        return Ok(ExternalJoinResult::DeferredToWelcome);
    }

    // 2. Build the external commit; the GroupInfo names the suite, since there is no local group.
    let verifiable_group_info: VerifiableGroupInfo =
        parse_stored_group_info(&group_info_bytes)?;
    let suite = verifiable_group_info.ciphersuite();

    // 2a. Who may have built this lineage (#1161 M6). Read failures yield the weak answer and never refuse a join.
    let roster: Option<std::collections::HashSet<String>> =
        match crate::commands::ds_reads::catch_up_full(state, conversation_id, false, false, true)
            .await
        {
            Ok(resolved) if !resolved.roster.is_empty() => {
                Some(resolved.roster.into_iter().collect())
            }
            Ok(_) => None,
            Err(e) => {
                crate::diag!(
                    "[mls] external_join: roster read for {conversation_id} failed ({e}) — the \
                     GroupInfo signer's membership cannot be checked this pass"
                );
                None
            }
        };
    let roster_ids: Vec<String> = roster.iter().flatten().cloned().collect();
    let identities = match super::reconcile::load_pinned_identities(state, &roster_ids, user_id)
        .await
    {
        Ok(dir) => dir,
        Err(e) => {
            crate::diag!(
                "[mls] external_join: cross-signing material for {conversation_id} could not be \
                 loaded ({e}) — the GroupInfo signer stays unverifiable this pass"
            );
            IdentityDirectory::new(Vec::new())
        }
    };
    let provenance = LineageProvenance {
        identities: &identities,
        roster: roster.as_ref(),
    };

    let (commit_bytes, new_group_info_bytes): (Vec<u8>, Option<Vec<u8>>) = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        // Already a member by another path (#1220), checked under the build's guard.
        let stored = load_stored_group_at(db.conn(), conversation_id, stored_generation).is_some();
        let unconfirmed =
            super::join_branch::is_unconfirmed(db.conn(), conversation_id, stored_generation);
        if !super::join_branch::external_join_may_replace(stored, unconfirmed) {
            return Ok(ExternalJoinResult::AlreadyMember);
        }
        let provider = PollisProvider::new(db.conn());
        build_external_commit(
            &provider,
            conversation_id,
            stored_generation,
            user_id,
            &device_id,
            suite,
            verifiable_group_info,
            &provenance,
        )?
    };

    // Test-harness only: park with the branch stored and the CAS not submitted.
    #[cfg(feature = "test-harness")]
    rendezvous::park(rendezvous::Point::ExternalJoinBeforeSubmit).await;

    // 3. CAS on `stored_epoch`; a lost race means the branch is doomed.
    // Resolve against the log (#411): our exact bytes at `stored_epoch` prove a win despite a lost response.
    match super::delivery::submit_commit(
        state,
        conversation_id,
        stored_generation,
        stored_epoch,
        // An external join continues its lineage, so it closes nothing.
        None,
        user_id,
        &commit_bytes,
        Some(user_id),
        Some(&device_id),
        new_group_info_bytes.as_deref(),
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
                crate::diag!(
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
                crate::diag!(
                    "[mls] external_join: submit errored but our commit is canonical at epoch {stored_epoch} for {conversation_id} — adopting (lost response): {e}"
                );
            } else {
                // Genuine failure: discard the orphaned branch, as on LostRace (#1220).
                forget_unconfirmed_branch(state, conversation_id, stored_generation).await;
                return Err(e);
            }
        }
    }

    // 3b. Record the generation BEFORE anything reads the group back (monotone).
    {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            // The log accepted it: the branch is now canonical (#1220).
            super::join_branch::clear(db.conn(), conversation_id, stored_generation);
            super::join_branch::mark_fresh_if_first(db.conn(), conversation_id, stored_generation);
            // Only now: a losing branch never held the lineage (#1312).
            super::join_branch::record_first_join(
                db.conn(),
                conversation_id,
                stored_generation,
                stored_epoch as u64 + 1,
            );
            // #1571: a device registered before its user joined, joining late, records that gap as a loss.
            let fresh =
                super::join_branch::take_fresh(db.conn(), conversation_id, stored_generation);
            super::join_branch::record_entitlement(
                db.conn(),
                conversation_id,
                stored_generation,
                fresh,
                snap.device_registered_at.as_deref(),
                snap.member_since.as_deref(),
            );
            set_local_generation(db.conn(), conversation_id, stored_generation);
            // The rotation clock belongs to the lineage, not the conversation.
            super::self_update::record_self_update(
                db.conn(),
                conversation_id,
                stored_epoch as u64 + 1,
            );
        }
    }

    // 4. Refresh the GroupInfo at the new epoch for the next joiner.
    if let Err(e) = publish_group_info(state, conversation_id).await {
        crate::diag!(
            "[mls] external_join_group: publish_group_info failed (non-fatal): {e}"
        );
    }

    // Best-effort pin-key adopt (#99); usually a no-op until a member re-wraps.
    crate::commands::pinned_messages::try_adopt_pin_key(state, conversation_id).await;
    crate::commands::file_keys::try_adopt_file_keys(state, conversation_id).await;

    crate::diag!(
        "[mls] external_join_group: {user_id}:{device_id} joined {conversation_id} from epoch {stored_epoch}"
    );

    Ok(ExternalJoinResult::Joined)
}

/// The material an external join needs in order to decide WHO built the group
/// it is about to adopt (#1161 M6 residual).
///
/// Mandatory parameter so the provenance check cannot be skipped. Opposite biases:
///
/// * `identities` unloadable: every verdict is `Unverifiable`, which ADOPTS.
/// * `roster` is `None` only on a failed read; a signer outside `Some(set)` is refused.
pub(super) struct LineageProvenance<'a> {
    /// The roster's cross-signing material after `reconcile::pin_identities`.
    pub identities: &'a IdentityDirectory,
    /// The conversation's current member user ids, or `None` when the read that
    /// would have answered failed.
    pub roster: Option<&'a std::collections::HashSet<String>>,
}

impl LineageProvenance<'_> {
    /// Is `user_id` a current member? `None` when the roster is unknown — the
    /// distinction [`super::invariants::lineage_adoption`] turns on.
    fn signer_on_roster(&self, user_id: &str) -> Option<bool> {
        self.roster.map(|r| r.contains(user_id))
    }
}

/// The leaf of `tree` that actually signed `vgi`, with the identity it claims.
///
/// `VerifiableGroupInfo::signer()` is `pub(crate)`, so the signer is found by trial verification against every leaf's key.
///
/// `tree` came through `RatchetTreeIn::into_verified`, so each credential is bound to its key.
fn group_info_signer<'t, C>(
    provider: &MlsProvider<'_, C>,
    scheme: SignatureScheme,
    tree: &'t openmls::treesync::RatchetTree,
    vgi: &VerifiableGroupInfo,
) -> Option<&'t LeafNode>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    tree.leaves().find(|leaf| {
        let key = leaf.signature_key().as_slice().to_vec();
        let Ok(pk) = OpenMlsSignaturePublicKey::new(key.into(), scheme) else {
            return false;
        };
        // `verify_no_out` avoids cloning the ratchet tree per candidate leaf.
        vgi.verify_no_out(provider.crypto(), &pk).is_ok()
    })
}

/// Refuse a GroupInfo whose signer is not a certified device of a current
/// roster member (#1161 M6 residual).
///
/// Otherwise a hostile DS could publish a GroupInfo for its own group under the successor's GroupId and have us adopt it, deleting the predecessor's keys.
/// The GroupInfo's signer leaf is readable before adoption, and a DS cannot mint a leaf the roster certifies.
///
/// Runs before the stale-group delete below, so a refusal costs nothing.
fn check_lineage_provenance<C>(
    provider: &MlsProvider<'_, C>,
    conversation_id: &str,
    generation: i64,
    suite: Ciphersuite,
    group_id: &GroupId,
    vgi: &VerifiableGroupInfo,
    provenance: &LineageProvenance<'_>,
) -> Result<()>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let refuse = |why: String| {
        crate::error::Error::Other(anyhow::anyhow!(
            "external join for {conversation_id} generation {generation}: {why} — refusing to \
             adopt this lineage"
        ))
    };

    // Our groups always carry the ratchet tree; without it the signer is unknowable, so refuse before the delete.
    let Some(tree_ext) = vgi.extensions().ratchet_tree() else {
        return Err(refuse(
            "the published GroupInfo carries no ratchet-tree extension, so there is no way to \
             establish who built it"
            .to_string(),
        ));
    };

    // Binds each leaf's credential to its key, GroupId and position.
    let tree = tree_ext
        .ratchet_tree()
        .clone()
        .into_verified(suite, provider.crypto(), group_id)
        .map_err(|e| refuse(format!("the GroupInfo's ratchet tree does not verify: {e}")))?;

    let scheme = signature_scheme(suite);
    let Some(leaf) = group_info_signer(provider, scheme, &tree, vgi) else {
        return Err(refuse(
            "the published GroupInfo's signature does not verify under any leaf of its own tree"
                .to_string(),
        ));
    };

    let signer_user = parse_credential_user_id(leaf.credential());
    let signer_device = parse_credential_device_id(leaf.credential()).unwrap_or_default();
    let verdict = provenance.identities.leaf_verdict(
        &signer_user,
        &signer_device,
        leaf.signature_key().as_slice(),
        scheme,
    );
    let standing = match &verdict {
        LeafVerdict::Certified => super::invariants::SignerStanding::Certified,
        LeafVerdict::Unverifiable(_) => super::invariants::SignerStanding::Unverifiable,
        LeafVerdict::Uncertified(_) => super::invariants::SignerStanding::Uncertified,
    };
    let on_roster = provenance.signer_on_roster(&signer_user);

    match super::invariants::lineage_adoption(on_roster, standing) {
        super::invariants::LineageAdoption::Refuse => Err(refuse(format!(
            "the published GroupInfo is signed by {signer_user}:{signer_device}, which is not a \
             certified device of a current member (verdict {verdict:?}, on roster {on_roster:?})"
        ))),
        super::invariants::LineageAdoption::Adopt => {
            if !matches!(standing, super::invariants::SignerStanding::Certified) {
                // Adopted on the weak verdict (unreplicated certs must not strand a migration), but never silently.
                crate::diag!(
                    "[mls] external join for {conversation_id} generation {generation}: adopting a \
                     GroupInfo signed by {signer_user}:{signer_device} whose cross-signing could \
                     not be evaluated ({verdict:?}) — not positive evidence, so not a refusal"
                );
            }
            Ok(())
        }
    }
}

/// Build and locally stage an external commit into `verifiable_group_info`, returning the commit and the resulting GroupInfo.
///
/// `suite` is the group's; it decides the scheme the joining leaf signs under.
///
/// `provenance` answers the question the GroupId pin cannot — see
/// [`check_lineage_provenance`].
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
    provenance: &LineageProvenance<'_>,
) -> Result<(Vec<u8>, Option<Vec<u8>>)>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
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

    // The GroupInfo must describe the lineage we MEANT to join (#1161 M6), checked before the delete below.
    let group_id = mls_group_id(conversation_id, generation);
    if verifiable_group_info.group_id() != &group_id {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "external join for {conversation_id} generation {generation}: the published GroupInfo \
             is for a different group ({:?}) — refusing to join it",
            verifiable_group_info.group_id()
        )));
    }

    // …and must have been built by a member of this conversation (#1161 M6 residual).
    check_lineage_provenance(
        provider,
        conversation_id,
        generation,
        suite,
        &group_id,
        &verifiable_group_info,
        provenance,
    )?;

    // Drop only our own leftover branch for THIS lineage, NEVER a confirmed group (#1220) or a predecessor.
    if let Ok(Some(mut old)) = MlsGroup::load(provider.storage(), &group_id) {
        let unconfirmed =
            super::join_branch::is_unconfirmed(provider.raw_conn(), conversation_id, generation);
        if !super::join_branch::external_join_may_replace(true, unconfirmed) {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "external join for {conversation_id} generation {generation}: this device already \
                 holds a confirmed group at epoch {} — refusing to replace it with a new branch",
                old.epoch().as_u64()
            )));
        }
        let _ = old.delete(provider.storage());
        super::join_branch::clear(provider.raw_conn(), conversation_id, generation);
    }

    let join_config = super::provider::join_config();

    let (joined_group, commit_bundle) = MlsGroup::external_commit_builder()
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

    // Marked in the same critical section that stored the branch (#1220).
    super::join_branch::mark_unconfirmed(
        provider.raw_conn(),
        conversation_id,
        generation,
        joined_group.epoch().as_u64(),
    )?;

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
/// The one place a group's suite is decided; later operations inherit it.
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

    // Delete any stale group with this lineage's ID (repair); a predecessor is untouched.
    if let Ok(Some(mut old)) = MlsGroup::load(provider.storage(), &group_id) {
        let _ = old.delete(provider.storage());
    }

    let config = MlsGroupCreateConfig::builder()
        .ciphersuite(suite)
        .use_ratchet_tree_extension(true)
        .sender_ratchet_configuration(super::provider::sender_ratchet_configuration())
        .build();

    MlsGroup::new_with_group_id(provider, &sig_keys, &config, group_id, cred_with_key)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("create mls group: {e}")))?;
    // A repair keeps the original first-join floor (#1312).
    super::join_branch::record_first_join(provider.raw_conn(), conversation_id, generation, 0);

    Ok(())
}

/// Create a fresh MLS group for `conversation_id` with `creator_user_id` as the sole member.
pub async fn init_mls_group(
    state: &Arc<AppState>,
    conversation_id: &str,
    creator_user_id: &str,
) -> Result<()> {
    let device_id = state.device_id.lock().await.clone()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("device_id not set")))?;
    // Creating a group writes MLS state (and may delete a stale one) (#1599).
    let _cross_process = state.cross_process_lock().await?;

    // Drop the guard before `publish_group_info` re-acquires it.
    {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| {
            crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
        })?;
        let provider = PollisProvider::new(db.conn());
        create_mls_group_in_suite(
            &provider,
            conversation_id,
            0,
            creator_user_id,
            &device_id,
            // Every new group is born on the one suite this build ships (#669).
            current_suite(),
        )?;
    }

    // Publish the epoch-0 GroupInfo for future external joins.
    if let Err(e) = publish_group_info(state, conversation_id).await {
        crate::diag!("[mls] init_mls_group: publish_group_info failed (non-fatal): {e}");
    }

    // Mint the conversation's pin master key while the creator is the sole
    // member at epoch 0 (#99). Best-effort: on failure the first pin mints.
    crate::commands::pinned_messages::mint_pin_key_for_new_group(state, conversation_id).await;
    crate::commands::file_keys::mint_file_keys_for_new_group(state, conversation_id).await;

    Ok(())
}

/// Reload an existing MLS group from storage and recover the signer.
///
/// Returns `(MlsGroup, SignatureKeyPair)`; the signer is looked up under the group's own suite scheme.
///
/// **Does NOT merge a pending commit (#1079)**: only a site that knows the log's answer may; see `no_new_site_merges_a_pending_commit`.
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
    super::provider::upgrade_sender_ratchet(&mut group, provider.storage());

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

    Ok((group, signer))
}

/// Wipe all local MLS state for a group without publishing a commit.
///
/// MLS forbids committing your own removal, so we just delete the local group.
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
/// For a device holding two lineages: delete the one no longer needed, keep the other.
pub(super) async fn forget_local_mls_group_at(
    state: &Arc<AppState>,
    group_id: &str,
    generation: i64,
) -> crate::error::Result<()> {
    // Deleting a group is an MLS mutation another process must not interleave with (#1599).
    let _cross_process = state.cross_process_lock().await?;
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
    })?;
    if let Some(mut group) = load_stored_group_at(db.conn(), group_id, generation) {
        // Leaving a lineage leaves its epoch: keep the file KEK, as every merge does.
        crate::commands::file_keys::keep_epoch_kek(&PollisProvider::new(db.conn()), &group);
        group.delete(&store_only(db.conn()))
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("mls delete group: {e}")))?;
    }
    // Whatever was stored is gone, branch or not (#1220).
    super::join_branch::clear(db.conn(), group_id, generation);
    Ok(())
}

/// Delete this device's unconfirmed external-join branch for the lineage, and
/// nothing else (#1220).
///
/// A Welcome may already have replaced the branch with a confirmed group; check and delete share one critical section.
pub(super) async fn forget_unconfirmed_branch(
    state: &Arc<AppState>,
    conversation_id: &str,
    generation: i64,
) {
    let guard = state.local_db.lock().await;
    let Some(db) = guard.as_ref() else {
        return;
    };
    if !super::join_branch::is_unconfirmed(db.conn(), conversation_id, generation) {
        crate::diag!(
            "[mls] {conversation_id} generation {generation}: the stored group is no longer this \
             device's unconfirmed branch (a Welcome replaced it) — keeping it"
        );
        return;
    }
    if let Some(mut group) = load_stored_group_at(db.conn(), conversation_id, generation) {
        if let Err(e) = group.delete(&store_only(db.conn())) {
            crate::diag!(
                "[mls] could not delete the doomed external-join branch for {conversation_id}: {e}"
            );
            return;
        }
    }
    super::join_branch::clear(db.conn(), conversation_id, generation);
}

/// Ensure this device has a local MLS group at the latest epoch for
/// `mls_group_id`. Processes any pending commits from the commit log.
/// Falls back to external join if the local group is missing or unrecoverably behind.
///
/// Takes the per-conversation MLS lock; callers already holding it use [`process_pending_commits_locked`].
pub async fn process_pending_commits_inner(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    let _guard = state.mls_group_lock(mls_group_id).await?;
    process_pending_commits_locked(state, mls_group_id, user_id).await
}

/// Whether this device's `user_device` row still exists, so a revoked device cannot auto-rejoin.
///
/// Fails CLOSED: the DS does not gate commits on revocation, so this is the only guard.
fn local_device_registered(snap: Option<&pollis_api::reads::ConversationState>) -> bool {
    // Request failure and an unreadable `user_device` are both "couldn't check": false.
    snap.and_then(|s| s.device_registered).unwrap_or(false)
}

/// Whether `user_id` is a CURRENT member, so a removed-but-not-revoked member cannot rejoin (fuzzer finding #2).
///
/// Answered by `pollis_delivery::writes::is_member`, the DS's own predicate.
///
/// Fails CLOSED: this guards a membership leak.
fn local_user_is_member(snap: Option<&pollis_api::reads::ConversationState>) -> bool {
    snap.and_then(|s| s.is_member).unwrap_or(false)
}

/// Whether an undelivered MLS `Welcome` for THIS device targets `mls_group_id`.
///
/// When true, the Welcome is the canonical way in and external-join recovery must not race it.
///
/// Self-healing: the Welcome is acked even on a failed apply, so the gate lifts.
///
/// Fails OPEN: an unseen Welcome must never permanently block recovery.
///
/// **This is the OPPOSITE bias to [`local_user_is_member`], seven lines away,
/// and they must not be collapsed into one error path.** They read the same
/// snapshot; they do not share a default.
fn has_pending_welcome(snap: Option<&pollis_api::reads::ConversationState>) -> bool {
    snap.map(|s| s.welcome_pending).unwrap_or(false)
}

/// Both external-join recovery gates: device still registered AND user still a member; logs the reason.
fn may_rejoin_via_external_join(
    snap: Option<&pollis_api::reads::ConversationState>,
    mls_group_id: &str,
    user_id: &str,
) -> bool {
    // The conjunction is `invariants::may_rejoin`, Kani-proved (fuzzer finding #2).
    let registered = local_device_registered(snap);
    if !registered {
        crate::diag!(
            "[mls] external-join recovery for {mls_group_id}: device for {user_id} is no longer \
             registered (revoked) — staying out"
        );
        return super::invariants::may_rejoin(false, false);
    }
    let is_member = local_user_is_member(snap);
    if !is_member {
        crate::diag!(
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
    process_pending_commits_locked_impl(state, mls_group_id, user_id, None, None).await
}

/// Like [`process_pending_commits_inner`], but calls `on_epoch` at each epoch so the caller decrypts its envelopes before the next commit.
///
/// Required under `max_past_epochs = 0` (#418): an epoch's keys die when the group leaves it.
///
/// Not called for epochs skipped by a recovery jump; those are caught on the next ingest.
///
/// Receives `(generation, epoch)`: only the pair is monotone across a migration (#454 P4).
///
/// `bound` is the log head read BEFORE the envelope fetch (#1041); see [`ReplayBound`].
pub async fn process_pending_commits_inner_with_hook(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
    on_epoch: &mut (dyn FnMut(&rusqlite::Connection, i64, u64) + Send),
    bound: Option<ReplayBound>,
) -> crate::error::Result<()> {
    let _guard = state.mls_group_lock(mls_group_id).await?;
    process_pending_commits_locked_impl(state, mls_group_id, user_id, Some(on_epoch), bound).await
}

/// How far one ingesting replay may advance the local group (#1041).
///
/// A commit accepted after the envelope fetch could destroy keys for envelopes the fetch missed, so replay stops at the head read first.
///
/// Lexicographic on `(generation, epoch)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayBound {
    pub generation: i64,
    /// The head epoch in `generation` — the epoch the next accepted commit
    /// will be based on. A commit row at this epoch or beyond is not applied.
    pub epoch: u64,
}

impl ReplayBound {
    /// Whether a commit row at `(generation, epoch)` lies below the bound and
    /// may be applied on this pass.
    pub fn admits(self, generation: i64, epoch: u64) -> bool {
        (generation, epoch) < (self.generation, self.epoch)
    }

    /// Whether the bound lies in a generation beyond `generation`, i.e. the log
    /// had already opened a successor lineage when the bound was read, so
    /// adopting one on this pass cannot skip envelopes the pass did not fetch.
    pub fn allows_generation_beyond(self, generation: i64) -> bool {
        self.generation > generation
    }
}

/// Most suite generations a single pass will hop across (#454 P4).
///
/// Hops re-enter the replay with the hook live; bounded because a DS drives it.
const MAX_GENERATION_HOPS: usize = 4;

/// The per-epoch hook a replay pass may carry.
type EpochHook<'h> = dyn FnMut(&rusqlite::Connection, i64, u64) + Send + 'h;

async fn process_pending_commits_locked_impl(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
    mut on_epoch: Option<&mut EpochHook<'_>>,
    bound: Option<ReplayBound>,
) -> crate::error::Result<()> {
    for _hop in 0..MAX_GENERATION_HOPS {
        let advanced =
            process_one_generation(state, mls_group_id, user_id, on_epoch.as_deref_mut(), bound)
                .await?;
        if !advanced {
            break;
        }
    }
    Ok(())
}

/// One replay pass over a single lineage; returns whether it adopted a NEWER generation.
// `'h` is named so the hook may outlive each hop's fresh reborrow.
async fn process_one_generation<'h>(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
    mut on_epoch: Option<&mut EpochHook<'h>>,
    bound: Option<ReplayBound>,
) -> crate::error::Result<bool> {
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
            // No local group: arbitrate Welcome vs external join through the Kani-proved I6 gate (`invariants::join_recovery`); a revoked or removed member stays out.
            // All inputs come from ONE snapshot, or the proof turns vacuous.
            // Each gate keeps its OWN failure bias; do not fold them.
            let snap = super::ds_reads::conversation_snapshot(
                state,
                super::ds_reads::state_query(mls_group_id, Some(generation)),
            )
            .await
            .map_err(|e| {
                crate::diag!(
                    "[mls] process_pending_commits: conversation-state for {mls_group_id} failed: {e}"
                );
                e
            })
            .ok();
            let snap = snap.as_ref().filter(|s| s.authorized);
            let welcome_pending = has_pending_welcome(snap);
            // Fails CLOSED — a membership we cannot confirm must never license a
            // rebuild.
            let may_rejoin = may_rejoin_via_external_join(snap, mls_group_id, user_id);
            // #832: no GroupInfo means nothing to join onto.
            let group_info_available = snap.and_then(group_info_head_of).is_some();
            match super::invariants::join_recovery(
                welcome_pending,
                may_rejoin,
                group_info_available,
            ) {
                super::invariants::JoinRecovery::AwaitWelcome => {
                    if welcome_pending {
                        crate::diag!(
                            "[mls] process_pending_commits: no local group for {mls_group_id} but an \
                             undelivered Welcome targets this device — deferring to the Welcome path \
                             (not external-joining) to avoid racing our own Welcome"
                        );
                    } else {
                        // No GroupInfo yet: poll for the Welcome now (#832).
                        crate::diag!(
                            "[mls] process_pending_commits: no local group for {mls_group_id} and no \
                             published GroupInfo — awaiting a Welcome (external-join would have \
                             nothing to join onto); polling welcomes now"
                        );
                        // `Box::pin`: this can recurse via poll_mls_welcomes.
                        if let Err(e) =
                            Box::pin(super::poll_mls_welcomes(state, user_id.to_string())).await
                        {
                            crate::diag!(
                                "[mls] process_pending_commits: welcome poll for {mls_group_id} failed: {e}"
                            );
                        }
                    }
                }
                super::invariants::JoinRecovery::ExternalJoin => {
                    if let Err(e) = external_join_group_inner(state, mls_group_id, user_id).await {
                        crate::diag!("[mls] process_pending_commits: no local group for {mls_group_id}, external-join failed: {e}");
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

    // #418: decrypt the current epoch's envelopes before any commit.
    if let Some(hook) = on_epoch.as_deref_mut() {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            hook(db.conn(), generation, initial_epoch);
        }
    }

    // 2. Fetch this lineage's pending commits plus cross-signing rows for the users they hint at (#987).
    // The DS reads head and batch in ONE transaction and refuses a non-contiguous batch, which makes the gap handling below safe.
    let snapshot = super::ds_reads::conversation_snapshot(
        state,
        pollis_api::reads::ConversationStateQuery {
            conversation_id: mls_group_id.to_string(),
            generation: Some(generation),
            since: Some(initial_epoch as i64),
            want_group_info_blob: false,
            commit_at_epoch: None,
        },
    )
    .await?;
    if !snapshot.authorized {
        // Removed from the conversation: not an error; this pass simply ends.
        crate::diag!(
            "[mls] process_pending_commits: {user_id} is not a member of {mls_group_id} — nothing to replay"
        );
        return Ok(false);
    }

    #[derive(Debug)]
    struct PendingCommit {
        seq: i64,
        epoch: i64,
        commit_data: Vec<u8>,
    }

    // A short batch is a retention prune (#539) or a DS-reported `hole`; both recover by external join.
    if let Some(floor) = snapshot.pruned_below {
        if floor > initial_epoch as i64 {
            crate::diag!(
                "[mls] process_pending_commits: {mls_group_id} generation {generation} is pruned \
                 below epoch {floor} but we are at {initial_epoch} — the missing commits are \
                 retention-collected, not lost; recovering via external join"
            );
        }
    }
    if let Some(hole) = snapshot.hole {
        crate::diag!(
            "[mls] process_pending_commits: {mls_group_id} generation {generation} has a HOLE — \
             expected epoch {}, found {}. This is damage in the log, not a prune and not a torn \
             read (the DS checked contiguity in the same transaction as the head); recovering \
             via external join",
            hole.expected, hole.found
        );
    }

    // Captured before consuming the snapshot: only these permit destroying crypto state (#1161 M6).
    let hole_expected = snapshot.hole.and_then(|h| u64::try_from(h.expected).ok());
    let pruned_floor = snapshot.pruned_below.and_then(|f| u64::try_from(f).ok());

    let mut pending: Vec<PendingCommit> = Vec::with_capacity(snapshot.commits.len());
    for row in &snapshot.commits {
        pending.push(PendingCommit {
            seq: row.seq,
            epoch: row.epoch,
            commit_data: super::ds_reads::decode_b64("commit_data", &row.commit)?,
        });
    }

    // Cross-signing INPUTS only; a DS lie can only make a leaf look `Unverifiable`.
    // …and PINNED like the committing side (#1161 M3); an unpinnable directory degrades to empty.
    let directory = match super::reconcile::pin_identities(
        state,
        IdentityDirectory::new(snapshot.added_identities),
        user_id,
    )
    .await
    {
        Ok(dir) => dir,
        Err(e) => {
            crate::diag!(
                "[mls] process_pending_commits: cannot pin the identity directory for \
                 {mls_group_id} ({e}) — treating every added leaf as unverifiable this pass"
            );
            IdentityDirectory::new(Vec::new())
        }
    };

    // 3. Apply in epoch order; uncertified grafted leaves (#1161 H4) are flagged and evicted later, never refused.
    let mut flagged: Vec<(AddedLeaf, i64, String, FlagKind)> = Vec::new();
    let mut current_epoch = initial_epoch;
    let mut any_applied = false;
    // Explicit recovery reason (#680): a group that exists but cannot advance must still recover.
    let mut recover: Option<RecoverReason> = None;
    for commit in pending {
        // #1041: leave commits at or beyond the bound to the next pass.
        if let Some(b) = bound {
            if !b.admits(generation, commit.epoch as u64) {
                crate::diag!(
                    "[mls] process_pending_commits: {mls_group_id} holding at epoch {current_epoch} — \
                     commit at epoch {} landed after this pass fetched its envelopes (bound {b:?}); \
                     the next pass applies it",
                    commit.epoch
                );
                break;
            }
        }

        // Gap classification (I1), Kani-proved (`invariants::classify`); see `specs/tla/CommitLog.tla`.
        if super::invariants::classify(current_epoch, Some(commit.epoch as u64))
            != super::invariants::ReplayStep::Apply
        {
            // Gap: recovery drops the local group and external-joins, losing this lineage's keys.
            // So the gap alone does not license it (#1161 M6). The DS has to
            // have DECLARED, in the same read transaction that served this
            // batch, an explanation that covers the epoch we are missing: a
            // retention floor above us, or its own contiguity check naming this
            // exact epoch. Anything else holds position — nothing deleted,
            // nothing advanced, retried next pass — so one malformed or hostile
            // response cannot spend this device's crypto state.
            match super::invariants::gap_recovery(current_epoch, hole_expected, pruned_floor) {
                super::invariants::GapRecovery::Rebuild => {
                    crate::diag!(
                        "[mls] process_pending_commits: epoch gap for {mls_group_id}: expected \
                         {current_epoch}, got {} (hole={hole_expected:?} pruned_below={pruned_floor:?}) \
                         — dropping local group to recover via external join",
                        commit.epoch
                    );
                    let _ = forget_local_mls_group_at(state, mls_group_id, generation).await;
                }
                super::invariants::GapRecovery::HoldPosition => {
                    crate::diag!(
                        "[mls] process_pending_commits: epoch gap for {mls_group_id}: expected \
                         {current_epoch}, got {} — but the DS declared no prune or hole covering \
                         it (hole={hole_expected:?} pruned_below={pruned_floor:?}). Holding at \
                         {current_epoch} rather than destroying MLS state on an unexplained batch",
                        commit.epoch
                    );
                }
            }
            break;
        }

        let commit_data = commit.commit_data;

        // Scoped so nothing !Send crosses an await.
        let applied: Option<u64> = {
            let guard = state.local_db.lock().await;
            let db = match guard.as_ref() {
                Some(db) => db,
                None => break,
            };
            let provider = PollisProvider::new(db.conn());
            let outcome =
                apply_one_commit(&provider, mls_group_id, generation, commit.epoch, &commit_data);
            match outcome {
                CommitApply::Applied { adds, epoch_after } => {
                    // A canonical commit on top proves a stored branch is confirmed (#1220).
                    super::join_branch::clear(db.conn(), mls_group_id, generation);
                    // Verify after the merge, off the tree (#1161 H4): a StagedCommit cannot be re-processed.
                    for leaf in adds {
                        let verdict = directory.leaf_verdict(
                            &leaf.user_id,
                            &leaf.device_id,
                            &leaf.signature_key,
                            leaf.scheme,
                        );
                        let Some((reason, kind)) = flag_for(verdict) else {
                            continue;
                        };
                        // #1127: say which verdict this is.
                        let verdict_phrase = match kind {
                            FlagKind::Uncertified => "is NOT cross-signed by that account",
                            FlagKind::Unverifiable => "could not be verified against that account (yet)",
                        };
                        crate::diag!(
                            "[mls] process_pending_commits: commit seq {} epoch {} in {mls_group_id} \
                             added a leaf for {}:{} that {verdict_phrase} ({reason}) \
                             — applied to stay on the canonical branch; flagging for eviction",
                            commit.seq, commit.epoch, leaf.user_id, leaf.device_id
                        );
                        flagged.push((leaf, commit.epoch, reason, kind));
                    }
                    // #1161 C2: check the merged group's epoch rather than assume `+= 1`; a divergence mis-buckets every later envelope.
                    if epoch_after != commit.epoch as u64 + 1 {
                        crate::diag!(
                            "[mls] process_pending_commits: {mls_group_id} applied the commit at \
                             epoch {} but the group is at epoch {epoch_after}, not {} — refusing \
                             to replay against a desynchronised counter",
                            commit.epoch,
                            commit.epoch + 1
                        );
                        recover = Some(RecoverReason::EpochDesync);
                        None
                    } else {
                        Some(epoch_after)
                    }
                }
                CommitApply::Recover(reason) => {
                    crate::diag!(
                        "[mls] process_pending_commits: {mls_group_id} cannot advance past epoch {} ({reason:?}) — recovering rather than wedging (#680)",
                        commit.epoch
                    );
                    recover = Some(reason);
                    break;
                }
            }
        };

        // The desync guard set `recover`: stop before the hook runs on a wrong counter.
        if recover.is_some() {
            break;
        }

        if let Some(epoch_after) = applied {
            current_epoch = epoch_after;
            any_applied = true;
            // #418: decrypt this epoch's envelopes while its keys still exist.
            if let Some(hook) = on_epoch.as_deref_mut() {
                let guard = state.local_db.lock().await;
                if let Some(db) = guard.as_ref() {
                    hook(db.conn(), generation, current_epoch);
                }
            }
        }
    }

    if !flagged.is_empty() {
        report_uncertified_leaves(state, mls_group_id, user_id, flagged).await;
    }

    // Clear a commit left dangling by an interrupted submit: had it landed, the replay would have adopted it (#411).
    {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            if let Some(mut group) = load_stored_group_at(db.conn(), mls_group_id, generation) {
                if group.pending_commit().is_some() {
                    crate::diag!(
                        "[mls] process_pending_commits: clearing a dangling pending commit for {mls_group_id} (never landed) to avoid a phantom-epoch merge"
                    );
                    let _ = group.clear_pending_commit(&store_only(db.conn()));
                }
            }
        }
    }

    // Suite-generation backstop (#454 P4), after the replay and the dangling clear.
    // Only on an ingesting pass: adoption deletes the predecessor's keys.
    // And only when the bound (#1041) allows it.
    let bound_allows_hop = bound.is_none_or(|b| b.allows_generation_beyond(generation));
    if on_epoch.is_some() && bound_allows_hop {
        if let Some(new_generation) =
            maybe_advance_generation(state, mls_group_id, user_id, generation).await
        {
            crate::diag!(
                "[mls] process_pending_commits: {mls_group_id} advanced generation \
                 {generation} -> {new_generation} — replaying the new lineage"
            );
            // A different group means a new exporter secret for voice.
            crate::commands::voice_e2ee::on_mls_epoch_changed(state, mls_group_id).await;
            // And the pin key re-wraps under it (#99).
            crate::commands::pinned_messages::maybe_rewrap_pin_key(state, mls_group_id).await;
            crate::commands::file_keys::maybe_rewrap_file_keys(state, mls_group_id).await;
            return Ok(true);
        }
    }

    // GroupInfo durability backstop: also heals a dropped create-time publish.
    ensure_group_info_published(state, mls_group_id).await;

    // Pin-key durability backstop (#99): publish a fresh wrap if the epoch moved past it.
    crate::commands::pinned_messages::maybe_rewrap_pin_key(state, mls_group_id).await;
    crate::commands::file_keys::maybe_rewrap_file_keys(state, mls_group_id).await;

    if any_applied {
        // Voice E2EE: re-derive the room key on epoch advance.
        crate::commands::voice_e2ee::on_mls_epoch_changed(state, mls_group_id).await;
    }

    // Recover if the group is gone OR a commit asked us to (#680), so a wedge is never mistaken for caught-up.
    let group_exists = {
        let guard = state.local_db.lock().await;
        guard.as_ref().is_some_and(|db| {
            load_stored_group_at(db.conn(), mls_group_id, generation).is_some()
        })
    };
    if !group_exists || recover.is_some() {
        // Unless revoked or removed (fuzzer finding #2).
        // Re-read the gates: a commit may just have removed us.
        let gates = super::ds_reads::conversation_snapshot(
            state,
            super::ds_reads::state_query(mls_group_id, Some(generation)),
        )
        .await
        .ok();
        let gates = gates.as_ref().filter(|s| s.authorized);
        if may_rejoin_via_external_join(gates, mls_group_id, user_id) {
            // An explicit rebuild drops the group by name; an external join never replaces a confirmed group (#1220).
            if group_exists {
                let _ = forget_local_mls_group_at(state, mls_group_id, generation).await;
            }
            crate::diag!("[mls] process_pending_commits: group {mls_group_id} was deleted during processing — external-joining to recover");
            if let Err(e) = external_join_group_inner(state, mls_group_id, user_id).await {
                crate::diag!("[mls] process_pending_commits: recovery external-join failed for {mls_group_id}: {e}");
            }
        }
    }

    // Retention signal (#539, I4): report our applied epoch for the prune floor; best-effort.
    report_applied_epoch(state, mls_group_id, user_id).await;

    Ok(false)
}

/// Adopt a newer suite generation for `mls_group_id` if one exists.
///
/// Two ways a device learns it has been migrated, tried in order:
///
/// 1. **The successor is already here** from a Welcome: flip the lineage row, no network.
/// 2. **No Welcome for it**: a published head generation above ours is proof; external-join it behind the usual gates.
///
/// `None`: still on the newest lineage we can see.
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
        // Confirm against the log: a migration that lost its CAS leaves a group on a generation that never opened.
        match published_group_info_head(state, mls_group_id).await {
            Some((head_generation, _)) if head_generation >= successor => {
                adopt_generation_locked(state, mls_group_id, generation, successor).await;
                return Some(successor);
            }
            _ => {
                crate::diag!(
                    "[mls] process_pending_commits: {mls_group_id} holds a group at generation \
                     {successor} that the log does not know about — ignoring it (an abandoned \
                     migration) and staying on {generation}"
                );
            }
        }
    }

    // 2. One snapshot for "a successor exists" and "we may rejoin", so a removal cannot slip between them.
    let snap = super::ds_reads::conversation_snapshot(
        state,
        super::ds_reads::state_query(mls_group_id, None),
    )
    .await
    .ok();
    let snap = snap.as_ref().filter(|s| s.authorized);
    let (head_generation, _) = snap.and_then(group_info_head_of)?;
    if head_generation <= generation {
        return None;
    }
    if !may_rejoin_via_external_join(snap, mls_group_id, user_id) {
        return None;
    }
    crate::diag!(
        "[mls] process_pending_commits: {mls_group_id} was migrated to generation \
         {head_generation} but no Welcome reached this device — external-joining the successor"
    );
    // Lock held: use the inner variant; it records the lineage, so re-read.
    if let Err(e) = external_join_group_inner(state, mls_group_id, user_id).await {
        crate::diag!(
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
        // The drained predecessor's keys are now pure liability.
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
        // Entered by Welcome, so our leaf is UNMERGED: restart the rotation clock (#666).
        super::self_update::clear_self_update(db.conn(), conversation_id);
    }
    // Only now: a crash before this leaves a harmless orphan.
    let _ = forget_local_mls_group_at(state, conversation_id, predecessor).await;
}

/// Why the local group could not advance from a commit — see [`apply_one_commit`]
/// and [`classify_process_message_error`].
///
/// Every variant recovers by delete-and-rejoin; a distinct value so a wedge is never "done" (#680).
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
    /// A protocol message that is NOT a Commit (#1161 C2): damage, never a free advance.
    NotACommit,
    /// The row is a protocol message for a DIFFERENT group id than the lineage
    /// we are replaying (#1161 C2).
    WrongGroup,
    /// Merged, but not to `commit.epoch + 1` (#1161 C2).
    EpochDesync,
    /// Merging the staged commit failed (storage error / diverged tree).
    MergeFailed,
    /// Our keys cannot open the commit — we were evicted from the group.
    Evicted,
    /// The commit is at our epoch yet won't stage/validate: our local tree has
    /// diverged from the canonical branch (fork; #411, prod incident 01KQYX89…).
    ForkedTree,
    /// Our own commit was echoed back but merging the pending commit failed.
    OwnCommitMergeFailed,
    /// An unclassified openmls error: recover, NEVER wedge (#680).
    Unclassified,
}

/// Outcome of applying one commit from the log — see [`apply_one_commit`].
#[derive(Debug)]
pub(super) enum CommitApply {
    /// Merged. `adds` are the leaves the commit GRAFTED (by tree diff); `epoch_after` is read off the group.
    Applied {
        adds: Vec<AddedLeaf>,
        epoch_after: u64,
    },
    /// Must rebuild; `apply_one_commit` already deleted the local group.
    Recover(RecoverReason),
}

/// Map an openmls `process_message` error to the recovery it warrants.
///
/// Matches error ENUMS, not `Display` strings (#680); anything else is [`RecoverReason::Unclassified`].
pub(super) fn classify_process_message_error<S>(
    e: &ProcessMessageError<S>,
) -> RecoverReason {
    use ProcessMessageError as E;
    match e {
        // Evicted: rejoin only if still on the roster (`may_rejoin`).
        E::GroupStateError(MlsGroupStateError::UseAfterEviction) => RecoverReason::Evicted,
        // Our own commit came back mismatched: our pending state diverged.
        E::InvalidCommit(StageCommitError::OwnCommitMismatch) => RecoverReason::ForkedTree,
        // Passed the gap check yet fails to stage: a forked tree.
        E::InvalidCommit(_) | E::ValidationError(_) => RecoverReason::ForkedTree,
        _ => RecoverReason::Unclassified,
    }
}

/// Apply a single commit from the log to the local group.
///
/// Errors are handled in place, each with its own recovery; see [`CommitApply`].
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
        Ok(None) => {
            crate::diag!("[mls] process_pending_commits: no local group for {mls_group_id} gen {generation} at epoch {commit_epoch} — recovering via external-join");
            return CommitApply::Recover(RecoverReason::GroupMissing);
        }
        Err(e) => {
            crate::diag!("[mls] process_pending_commits: mls load failed for {mls_group_id}: {e} — recovering via external-join");
            return CommitApply::Recover(RecoverReason::GroupLoadFailed);
        }
    };

    let mut reader: &[u8] = commit_data;
    let msg_in = match MlsMessageIn::tls_deserialize(&mut reader) {
        Ok(m) => m,
        Err(e) => {
            crate::diag!("[mls] process_pending_commits: deserialize failed for {mls_group_id} at epoch {commit_epoch}: {e} — recovering via external-join");
            let _ = group.delete(provider.storage());
            return CommitApply::Recover(RecoverReason::MalformedCommit);
        }
    };
    let protocol_msg = match msg_in.try_into_protocol_message() {
        Ok(m) => m,
        Err(e) => {
            crate::diag!("[mls] process_pending_commits: protocol msg failed for {mls_group_id} at epoch {commit_epoch}: {e} — recovering via external-join");
            let _ = group.delete(provider.storage());
            return CommitApply::Recover(RecoverReason::NotAProtocolMessage);
        }
    };

    // #1161 C2/H5: a slot may carry only a COMMIT for THIS group, checked before openmls.
    // Both fields are authenticated, but any member can POST any message to `/v1/commits`.
    // A non-commit is log damage: advancing on it would mis-bucket every later envelope.
    if protocol_msg.group_id() != &group_id {
        crate::diag!(
            "[mls] process_pending_commits: commit-log row at epoch {commit_epoch} for \
             {mls_group_id} names a different group id ({:?}) — recovering via external-join",
            protocol_msg.group_id()
        );
        let _ = group.delete(provider.storage());
        return CommitApply::Recover(RecoverReason::WrongGroup);
    }
    if protocol_msg.content_type() != ContentType::Commit {
        crate::diag!(
            "[mls] process_pending_commits: commit-log row at epoch {commit_epoch} for \
             {mls_group_id} is a {:?}, not a Commit — recovering via external-join rather than \
             advancing the replay counter past a row that cannot advance the group",
            protocol_msg.content_type()
        );
        let _ = group.delete(provider.storage());
        return CommitApply::Recover(RecoverReason::NotACommit);
    }

    // The tree BEFORE the merge, for [`grafted_leaves`].
    let scheme = signature_scheme(group.ciphersuite());
    let before = leaf_fingerprints(&group);

    match group.process_message(provider, protocol_msg) {
        Ok(processed) => match processed.into_content() {
            ProcessedMessageContent::StagedCommitMessage(staged) => {
                crate::commands::file_keys::keep_epoch_kek(provider, &group);
                if let Err(e) = group.merge_staged_commit(provider, *staged) {
                    crate::diag!("[mls] process_pending_commits: merge failed for {mls_group_id} at epoch {commit_epoch}: {e} — recovering via external-join");
                    let _ = group.delete(provider.storage());
                    return CommitApply::Recover(RecoverReason::MergeFailed);
                }
            }
            // Our OWN commit echoed back (#411): openmls returns it as `OwnPrivateMessage` content, which we adopt by merging our pending commit.
            ProcessedMessageContent::OwnPendingCommit
            | ProcessedMessageContent::OwnPrivateMessage => {
                // The content-type gate makes this our own COMMIT, not any message from our leaf (#1161 H5).
                //
                // No pending commit to adopt: rebuild rather than wedge.
                if group.pending_commit().is_none() {
                    crate::diag!(
                        "[mls] process_pending_commits: own commit at epoch {commit_epoch} for {mls_group_id} but no pending commit to adopt — recovering via external-join"
                    );
                    let _ = group.delete(provider.storage());
                    return CommitApply::Recover(RecoverReason::OwnCommitMergeFailed);
                }
                crate::commands::file_keys::keep_epoch_kek(provider, &group);
                if let Err(merge_err) = group.merge_pending_commit(provider) {
                    crate::diag!(
                        "[mls] process_pending_commits: own commit at epoch {commit_epoch} for {mls_group_id} but merge_pending failed ({merge_err}) — recovering via external-join"
                    );
                    let _ = group.delete(provider.storage());
                    return CommitApply::Recover(RecoverReason::OwnCommitMergeFailed);
                }
                crate::diag!(
                    "[mls] process_pending_commits: adopted our own commit at epoch {commit_epoch} for {mls_group_id}"
                );
            }
            // Unreachable after the content-type check; still `Recover`, never a free advance (#1161 C2).
            _ => {
                crate::diag!(
                    "[mls] process_pending_commits: unexpected non-commit content at epoch {commit_epoch} for {mls_group_id} — recovering via external-join"
                );
                let _ = group.delete(provider.storage());
                return CommitApply::Recover(RecoverReason::NotACommit);
            }
        },
        Err(e) => {
            // Drop the local group so the caller rejoins at the latest epoch; message history is untouched and the commit cannot re-fail.
            let reason = classify_process_message_error(&e);
            crate::diag!(
                "[mls] process_pending_commits: commit at epoch {commit_epoch} for {mls_group_id} failed to apply ({e}) — classified {reason:?}, deleting local group to re-join"
            );
            let _ = group.delete(provider.storage());
            return CommitApply::Recover(reason);
        }
    }

    CommitApply::Applied {
        adds: grafted_leaves(&before, &group, scheme),
        epoch_after: group.epoch().as_u64(),
    }
}

/// One leaf of a ratchet tree, as the grafted-leaf diff identifies it:
/// `(position, user, device, signature key)`.
///
/// All four: position alone misses a swapped leaf; identity alone misses a duplicate.
type LeafFingerprint = (LeafNodeIndex, String, String, Vec<u8>);

/// Fingerprint every leaf currently in `group`'s tree.
fn leaf_fingerprints(group: &MlsGroup) -> std::collections::HashSet<LeafFingerprint> {
    group
        .members()
        .map(|m| {
            (
                m.index,
                parse_credential_user_id(&m.credential),
                parse_credential_device_id(&m.credential).unwrap_or_default(),
                m.signature_key.clone(),
            )
        })
        .collect()
}

/// Every leaf a just-merged commit GRAFTED: present in the tree now, absent
/// from `before`.
///
/// #1161 H4: diff the tree, not Add proposals, so an external join's UpdatePath leaf is verdicted too.
///
/// An ordinary UpdatePath rotates only the encryption key, so it does not read as grafted.
fn grafted_leaves(
    before: &std::collections::HashSet<LeafFingerprint>,
    group: &MlsGroup,
    scheme: SignatureScheme,
) -> Vec<AddedLeaf> {
    leaf_fingerprints(group)
        .into_iter()
        .filter(|leaf| !before.contains(leaf))
        .map(|(_, user_id, device_id, signature_key)| AddedLeaf {
            user_id,
            device_id,
            signature_key,
            scheme,
        })
        .collect()
}

/// Why a replayed add was flagged — and therefore whether the USER is told
/// (#1127).
///
/// `Uncertified` is evidence; `Unverifiable` is its absence. Both get an eviction reconcile; only the first alarms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlagKind {
    /// Positively not the claimed account's device.
    Uncertified,
    /// Could not be decided here.
    Unverifiable,
}

impl FlagKind {
    /// Whether this is worth a user-facing `security_event`.
    fn alarms(self) -> bool {
        matches!(self, FlagKind::Uncertified)
    }
}

/// Classify a leaf verdict for the replay loop: `None` when the leaf is fine,
/// otherwise the reason string and whether it is evidence (#1127).
fn flag_for(verdict: LeafVerdict) -> Option<(String, FlagKind)> {
    match verdict {
        LeafVerdict::Certified => None,
        LeafVerdict::Unverifiable(why) => Some((why.to_string(), FlagKind::Unverifiable)),
        LeafVerdict::Uncertified(why) => Some((why, FlagKind::Uncertified)),
    }
}

/// What a member does about leaves a replayed commit added that are NOT
/// cross-signed by the account they claim (`LeafVerdict` ≠ `Certified`).
///
/// The commit is already canonical, so: (1) a `security_event` per POSITIVELY uncertified leaf (#1127); (2) a detached, self-correcting eviction reconcile.
/// Residual: the rogue leaf holds the key schedule until the eviction commit lands.
async fn report_uncertified_leaves(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
    flagged: Vec<(AddedLeaf, i64, String, FlagKind)>,
) {
    for (leaf, epoch, reason, kind) in &flagged {
        // #1127: an `Unverifiable` leaf is not evidence; alarming would let a committer manufacture alarms.
        if !kind.alarms() {
            continue;
        }
        let ev = pollis_api::account::SecurityEventBody {
            kind: "uncertified_mls_leaf".to_string(),
            device_id: Some(leaf.device_id.clone()),
            metadata: Some(format!(
                "conversation={mls_group_id} user={} epoch={epoch} reason={reason}",
                leaf.user_id
            )),
            // No-auth fallback for the acting user.
            user_id: Some(user_id.to_string()),
        };
        if let Err(e) = super::ds_client::ds_post_ok(state, &ev).await {
            crate::diag!(
                "[mls] process_pending_commits: DS security-event for uncertified leaf failed (non-fatal): {e}"
            );
        }
    }

    // Detached: reconcile re-acquires the MLS lock we hold.
    tokio::spawn(eviction_reconcile(
        Arc::clone(state),
        mls_group_id.to_string(),
        user_id.to_string(),
    ));
}

/// The detached eviction reconcile [`report_uncertified_leaves`] spawns.
///
/// Type-erased to break the reconcile → replay → spawn cycle the compiler cannot prove `Send`.
fn eviction_reconcile(
    state: Arc<AppState>,
    conversation_id: String,
    user_id: String,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(async move {
        if let Err(e) =
            super::reconcile::reconcile_group_mls_impl(&state, &conversation_id, &user_id).await
        {
            crate::diag!(
                "[mls] process_pending_commits: eviction reconcile for {conversation_id} failed: {e}"
            );
        }
    })
}

/// Report this device's `(generation, epoch)` to the DS as the retention high-water (#539).
///
/// Detached so a slow DS never holds the MLS lock; best-effort.
async fn report_applied_epoch(state: &Arc<AppState>, mls_group_id: &str, user_id: &str) {
    // A PAIR, compared lexicographically (#454 P4), so a migrated device does not look regressed.
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
/// A user-facing catch-up entry point, so it must use the interleaved catch-up, never a bare replay (`max_past_epochs = 0`).
pub async fn process_pending_commits(
    state: &Arc<AppState>,
    conversation_id: String,
    user_id: String,
) -> crate::error::Result<()> {
    let mls_group_id = super::ds_reads::resolve_mls_group(state, &conversation_id).await?;
    crate::commands::messages::catch_up_mls_group_interleaved(state, &mls_group_id, &user_id).await
}

/// Check whether an MLS group exists in the local database.
///
/// Use this, not `try_mls_encrypt(..).is_none()`, which is also `None` while a commit is staged (#1079).
pub fn has_local_group(conn: &rusqlite::Connection, conversation_id: &str) -> bool {
    load_stored_group(conn, conversation_id).is_some()
}

/// Try to encrypt `plaintext` with the MLS group for `conversation_id`.
///
/// Returns `None`, without logging, if there is no local group or a commit is staged.
///
/// #1079: sealing with a staged commit would pick an epoch the log has not decided; user-initiated posts hold the lock, so only lock-free callers see this.
pub fn try_mls_encrypt(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    plaintext: &[u8],
) -> Option<Vec<u8>> {
    let generation = local_generation(conn, conversation_id);
    let provider = PollisProvider::new(conn);
    let (mut group, signer) =
        load_group_with_signer(&provider, conversation_id, generation).ok()?;
    if group.pending_commit().is_some() {
        return None;
    }
    let msg_out = group.create_message(&provider, &signer, plaintext).ok()?;
    msg_out.tls_serialize_detached().ok()
}

/// Parse the MLS epoch a `message` / `edit` envelope was sealed at, WITHOUT
/// decrypting it (no group state touched).
///
/// `None` when not a protocol message (e.g. a `delete` tombstone).
///
/// Drives the #418 ingest interleave; parsed, since `sent_at` is not a reliable proxy.
///
/// Returns `(generation, epoch)` (#454 P4), the generation read off the authenticated GroupId.
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
/// Takes TLS-serialised `MlsMessageOut`; `None` if not an application message or decryption fails.
///
/// The second element is the **MLS-authenticated** sender: sealed sender attributes from it, never from `message_envelope.sender_id`.
pub fn try_mls_decrypt(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    ciphertext: &[u8],
) -> Option<(Vec<u8>, String)> {
    try_mls_decrypt_why(conn, conversation_id, ciphertext).ok()
}

/// [`try_mls_decrypt`], keeping the reason a decrypt failed (#1520).
pub fn try_mls_decrypt_why(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    ciphertext: &[u8],
) -> std::result::Result<(Vec<u8>, String), DecryptFailure> {
    let Some(mut decryptor) = MlsDecryptor::open(conn, conversation_id) else {
        return Err(DecryptFailure::no_local_group());
    };
    decryptor.decrypt(ciphertext)
}

/// Which step of opening one envelope failed (#1520).
///
/// For the `[loss]` diagnostic; never anything derived from plaintext or keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecryptFailureKind {
    /// This device holds no group for the conversation (never joined, or forgotten).
    NoLocalGroup,
    /// The bytes do not TLS-deserialize as an MLS message.
    Malformed,
    /// An MLS message, but not a protocol message (a Welcome, GroupInfo or KeyPackage).
    NotAProtocolMessage,
    /// Sealed in another generation of this conversation's group.
    ForeignLineage,
    /// Sealed for a different group altogether.
    WrongGroup,
    /// openmls `SecretReuseError`: this generation's key was already consumed.
    SecretReuse,
    /// openmls `TooDistantInThePast`: the generation (or the epoch's secrets) fell
    /// out of the retained window.
    TooDistantInThePast,
    /// openmls `TooDistantInTheFuture` / `GenerationOutOfBound`: the generation is
    /// further ahead of the ratchet than the forward-distance limit.
    TooDistantInTheFuture,
    /// openmls `WrongEpoch`: sealed at an epoch above the group's.
    WrongEpoch,
    /// openmls `NoPastEpochData`: sealed at an epoch whose secrets are gone.
    NoPastEpochData,
    /// AEAD open of the sender data or the content failed.
    AeadFailure,
    /// The decrypted sender data or content did not parse.
    MalformedContent,
    /// The sender's leaf is not in this group's tree.
    UnknownSender,
    /// openmls `RatchetTypeError`: the sender leaf is this device's own.
    OwnRatchet,
    /// The sender's signature over the content did not verify.
    InvalidSignature,
    /// This device was evicted from the group.
    Evicted,
    /// The wire format is not a PrivateMessage, or is barred by the group's policy.
    WrongWireFormat,
    /// openmls could not read or write its storage.
    Storage,
    /// Opened, but the content is a proposal or commit, not an application message.
    NotAnApplicationMessage,
    /// An openmls error this code does not specifically classify.
    Other,
}

impl DecryptFailureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            DecryptFailureKind::NoLocalGroup => "no local group",
            DecryptFailureKind::Malformed => "malformed bytes",
            DecryptFailureKind::NotAProtocolMessage => "not a protocol message",
            DecryptFailureKind::ForeignLineage => "foreign lineage",
            DecryptFailureKind::WrongGroup => "group id mismatch",
            DecryptFailureKind::SecretReuse => "secret reuse",
            DecryptFailureKind::TooDistantInThePast => "too distant in the past",
            DecryptFailureKind::TooDistantInTheFuture => "generation too far in the future",
            DecryptFailureKind::WrongEpoch => "wrong epoch",
            DecryptFailureKind::NoPastEpochData => "no past epoch data",
            DecryptFailureKind::AeadFailure => "AEAD failure",
            DecryptFailureKind::MalformedContent => "malformed content",
            DecryptFailureKind::UnknownSender => "unknown sender",
            DecryptFailureKind::OwnRatchet => "own-sender ratchet",
            DecryptFailureKind::InvalidSignature => "invalid signature",
            DecryptFailureKind::Evicted => "evicted",
            DecryptFailureKind::WrongWireFormat => "wrong wire format",
            DecryptFailureKind::Storage => "storage error",
            DecryptFailureKind::NotAnApplicationMessage => "not an application message",
            DecryptFailureKind::Other => "unclassified openmls error",
        }
    }
}

/// Why [`MlsDecryptor::decrypt`] failed (#1520), with the clear-header epochs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecryptFailure {
    pub kind: DecryptFailureKind,
    /// The `(generation, epoch)` the envelope claims, when its header parsed.
    pub sealed_at: Option<(i64, u64)>,
    /// The `(generation, epoch)` the decrypting group sat at, when one was loaded.
    pub group_at: Option<(i64, u64)>,
}

impl DecryptFailure {
    pub fn no_local_group() -> Self {
        Self {
            kind: DecryptFailureKind::NoLocalGroup,
            sealed_at: None,
            group_at: None,
        }
    }
}

impl std::fmt::Display for DecryptFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.kind.as_str())?;
        match (self.sealed_at, self.group_at) {
            (Some(m), Some(g)) => {
                write!(f, " (message at ({}, {}), group at ({}, {}))", m.0, m.1, g.0, g.1)
            }
            (None, Some(g)) => write!(f, " (group at ({}, {}))", g.0, g.1),
            (Some(m), None) => write!(f, " (message at ({}, {}))", m.0, m.1),
            (None, None) => Ok(()),
        }
    }
}

/// Map an application-message `process_message` error to the failing step (#1520), by enum.
pub(super) fn classify_decrypt_error<S>(e: &ProcessMessageError<S>) -> DecryptFailureKind {
    use DecryptFailureKind as K;
    use ProcessMessageError as E;
    if classify_process_message_error(e) == RecoverReason::Evicted {
        return K::Evicted;
    }
    match e {
        E::StorageError(_) => K::Storage,
        E::IncompatibleWireFormat => K::WrongWireFormat,
        E::ValidationError(v) => match v {
            ValidationError::WrongGroupId => K::WrongGroup,
            ValidationError::WrongEpoch => K::WrongEpoch,
            ValidationError::NoPastEpochData => K::NoPastEpochData,
            ValidationError::UnknownMember => K::UnknownSender,
            ValidationError::InvalidSignature => K::InvalidSignature,
            ValidationError::WrongWireFormat | ValidationError::UnencryptedApplicationMessage => {
                K::WrongWireFormat
            }
            ValidationError::UnableToDecrypt(d) => match d {
                MessageDecryptionError::AeadError => K::AeadFailure,
                MessageDecryptionError::MalformedContent => K::MalformedContent,
                MessageDecryptionError::WrongWireFormat => K::WrongWireFormat,
                MessageDecryptionError::GenerationOutOfBound => K::TooDistantInTheFuture,
                MessageDecryptionError::SecretTreeError(t) => match t {
                    SecretTreeError::SecretReuseError => K::SecretReuse,
                    SecretTreeError::TooDistantInThePast => K::TooDistantInThePast,
                    SecretTreeError::TooDistantInTheFuture => K::TooDistantInTheFuture,
                    SecretTreeError::IndexOutOfBounds => K::UnknownSender,
                    SecretTreeError::RatchetTypeError => K::OwnRatchet,
                    _ => K::Other,
                },
                _ => K::Other,
            },
            _ => K::Other,
        },
        _ => K::Other,
    }
}

/// A group loaded **once** for a RUN of envelopes that are all sealed at the
/// epoch it currently sits at.
///
/// Before #875 every envelope reloaded the group from `mls_kv` (12 SELECTs + 1 UPDATE each).
///
/// **Safe:** decrypting mutates only the secret tree, which openmls persists inside `process_message`, so a held group equals a reloaded one.
///
/// **Must NOT span a commit**: one per `on_epoch` firing, dropped before the next commit.
pub struct MlsDecryptor<'a> {
    provider: PollisProvider<'a>,
    group: MlsGroup,
}

impl<'a> MlsDecryptor<'a> {
    /// Load the conversation's live group, or `None` when this device has none.
    pub fn open(conn: &'a rusqlite::Connection, conversation_id: &str) -> Option<Self> {
        let group = load_stored_group(conn, conversation_id)?;
        Some(Self {
            provider: PollisProvider::new(conn),
            group,
        })
    }

    /// The `(generation, epoch)` the loaded group sits at.
    pub fn lineage(&self) -> (i64, u64) {
        let raw = String::from_utf8_lossy(self.group.group_id().as_slice()).into_owned();
        let (_, generation) = super::generation::split_mls_group_id(&raw);
        (generation, self.group.epoch().as_u64())
    }

    /// Decrypt one envelope; see [`try_mls_decrypt`].
    ///
    /// Errors name the failing step (#1520), never plaintext or keys.
    pub fn decrypt(
        &mut self,
        ciphertext: &[u8],
    ) -> std::result::Result<(Vec<u8>, String), DecryptFailure> {
        let group_at = Some(self.lineage());
        let fail = |kind: DecryptFailureKind, sealed_at: Option<(i64, u64)>| DecryptFailure {
            kind,
            sealed_at,
            group_at,
        };
        let mut reader: &[u8] = ciphertext;
        let msg_in = MlsMessageIn::tls_deserialize(&mut reader)
            .map_err(|_| fail(DecryptFailureKind::Malformed, None))?;
        let protocol_msg = msg_in
            .try_into_protocol_message()
            .map_err(|_| fail(DecryptFailureKind::NotAProtocolMessage, None))?;
        let raw_group_id = String::from_utf8_lossy(protocol_msg.group_id().as_slice()).into_owned();
        let (message_base, message_generation) = super::generation::split_mls_group_id(&raw_group_id);
        let sealed_at = Some((message_generation, protocol_msg.epoch().as_u64()));
        // A retired lineage's envelope: fail fast rather than get an ambiguous openmls error.
        if protocol_msg.group_id() != self.group.group_id() {
            let own_raw = String::from_utf8_lossy(self.group.group_id().as_slice()).into_owned();
            let (own_base, _) = super::generation::split_mls_group_id(&own_raw);
            // Same conversation, other generation: a lineage this group is not.
            let kind = if message_base == own_base {
                DecryptFailureKind::ForeignLineage
            } else {
                DecryptFailureKind::WrongGroup
            };
            return Err(fail(kind, sealed_at));
        }
        let processed = self
            .group
            .process_message(&self.provider, protocol_msg)
            .map_err(|e| fail(classify_decrypt_error(&e), sealed_at))?;

        // Grab the sender credential BEFORE `into_content` consumes it.
        let sender_user_id = parse_credential_user_id(processed.credential());

        match processed.into_content() {
            ProcessedMessageContent::ApplicationMessage(app_msg) => {
                Ok((app_msg.into_bytes(), sender_user_id))
            }
            _ => Err(fail(DecryptFailureKind::NotAnApplicationMessage, sealed_at)),
        }
    }
}

#[cfg(test)]
mod stored_group_info_parse_tests {
    use super::parse_stored_group_info;
    use openmls::prelude::*;
    use tls_codec::Serialize as _;

    // The prefix that broke a shipped build: a bare GroupInfo's ciphersuite 0x0052 read as a wire format.
    const CS_PQ_CODE_POINT: [u8; 2] = [0x00, 0x52];

    // A bare GroupInfo is not mistaken for an envelope; the legacy code point stays this suite's.
    #[test]
    fn pq_ciphersuite_is_the_value_that_looked_like_a_wire_format() {
        let cs = u16::from(super::super::provider::CS_PQ);
        assert_eq!(
            cs.to_be_bytes(),
            CS_PQ_CODE_POINT,
            "CS_PQ moved; legacy stored GroupInfo rows still begin with the old \
             code point, so parse_stored_group_info's fallback must stay"
        );
        assert_eq!(
            cs, 82,
            "82 is the UnknownValue seen in the field report; it is the \
             ciphersuite read at the wire-format offset"
        );
    }

    // Neither form fails, naming both attempts.
    #[test]
    fn rejects_a_payload_that_is_neither_form() {
        let err = parse_stored_group_info(&[0xff, 0xff, 0xff, 0xff])
            .expect_err("nonsense must not parse as either form");
        let msg = err.to_string();
        assert!(
            msg.contains("MlsMessage envelope") && msg.contains("bare GroupInfo"),
            "error should report both parse attempts, got: {msg}"
        );
    }

    // An empty row errors, never panics.
    #[test]
    fn rejects_empty_payload() {
        assert!(parse_stored_group_info(&[]).is_err());
    }

    // The envelope-first parse cannot swallow a bare GroupInfo: 0x0052 is no WireFormat.
    #[test]
    fn no_wire_format_can_collide_with_the_ciphersuite() {
        let cs = u16::from(super::super::provider::CS_PQ);
        for wire_format in [
            WireFormat::PublicMessage,
            WireFormat::PrivateMessage,
            WireFormat::Welcome,
            WireFormat::GroupInfo,
            WireFormat::KeyPackage,
        ] {
            let encoded = wire_format.tls_serialize_detached().unwrap();
            assert_eq!(encoded.len(), 2, "wire format is a u16 on the wire");
            assert_ne!(
                u16::from_be_bytes([encoded[0], encoded[1]]),
                cs,
                "a bare GroupInfo would be indistinguishable from this wire \
                 format, making the envelope-then-bare probe ambiguous"
            );
        }
    }
}

#[cfg(test)]
mod group_info_heal_tests {
    use super::group_info_is_stale;

    // Republish iff the log's GroupInfo is missing or behind.
    #[test]
    fn republishes_when_groupinfo_never_landed() {
        // The stranded-creator case, even at epoch 0.
        assert!(group_info_is_stale(None, (0, 0)));
        assert!(group_info_is_stale(None, (0, 7)));
    }

    #[test]
    fn republishes_when_groupinfo_is_behind() {
        // A dropped epoch-advance publish.
        assert!(group_info_is_stale(Some((0, 0)), (0, 1)));
        assert!(group_info_is_stale(Some((0, 3)), (0, 7)));
    }

    #[test]
    fn skips_when_groupinfo_is_current() {
        assert!(!group_info_is_stale(Some((0, 0)), (0, 0)));
        assert!(!group_info_is_stale(Some((0, 5)), (0, 5)));
    }

    #[test]
    fn skips_when_groupinfo_is_ahead() {
        // Another member published past us.
        assert!(!group_info_is_stale(Some((0, 9)), (0, 4)));
    }

    /// #454 P4: lexicographic, since a successor restarts at epoch 0.
    #[test]
    fn a_successors_epoch_zero_is_ahead_of_the_retired_lineage() {
        assert!(group_info_is_stale(Some((0, 42)), (1, 0)));
        assert!(!group_info_is_stale(Some((1, 0)), (0, 42)));
    }
}

#[cfg(test)]
mod flag_kind_tests {
    use super::{flag_for, FlagKind};
    use crate::commands::mls::device::LeafVerdict;

    #[test]
    fn a_certified_leaf_is_not_flagged_at_all() {
        assert!(flag_for(LeafVerdict::Certified).is_none());
    }

    /// #1127: positively uncertified is evidence and alarms.
    #[test]
    fn an_uncertified_leaf_alarms() {
        let (reason, kind) =
            flag_for(LeafVerdict::Uncertified("leaf key is not the certified key".into()))
                .expect("must be flagged");
        assert_eq!(kind, FlagKind::Uncertified);
        assert!(kind.alarms(), "a positively uncertified leaf must reach the user");
        assert!(reason.contains("certified key"), "the reason is carried through");
    }

    /// Absence of evidence (#1080): flagged, but no alarm.
    #[test]
    fn an_unverifiable_leaf_is_flagged_but_does_not_alarm() {
        let (_, kind) = flag_for(LeafVerdict::Unverifiable("no device row"))
            .expect("still flagged, so the reconcile still runs");
        assert_eq!(kind, FlagKind::Unverifiable);
        assert!(
            !kind.alarms(),
            "\"I could not tell yet\" must not be reported as a compromise — that is what \
             trains people to ignore the real thing, and it would let a committer manufacture \
             alarms in other people's audit logs by omitting the hint"
        );
    }

    /// Exactly one verdict alarms. Stated so a third verdict added later has to
    /// make this decision deliberately rather than inherit one.
    #[test]
    fn only_uncertified_alarms() {
        assert!(FlagKind::Uncertified.alarms());
        assert!(!FlagKind::Unverifiable.alarms());
    }
}
