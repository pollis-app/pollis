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

/// Test-harness rendezvous points inside the join and commit paths: a test arms
/// one, a client parks there, and the test interleaves against it before
/// releasing. Join-path interleavings are otherwise unreachable from a test.
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
        /// External commit built and stored locally, CAS not yet submitted.
        ExternalJoinBeforeSubmit,
        /// `reconcile_group_mls_impl` entered, MLS lock not yet taken.
        ReconcileEntry,
        /// Self-update commit staged under the MLS lock, CAS not yet submitted.
        SelfUpdateBeforeSubmit,
        /// An envelope sealed and about to be posted: a commit landing now makes
        /// it epoch-behind (#1041).
        EnvelopeBeforePost,
        /// An external join lost its CAS and dropped its branch, retry not yet
        /// started: the window in which the device's own Welcome lands (#1220).
        ExternalJoinAfterLostRace,
    }

    const POINTS: usize = 5;

    static ARMED: Mutex<[Option<mpsc::UnboundedSender<Release>>; POINTS]> =
        Mutex::new([None, None, None, None, None]);

    /// Arm `point`: every client reaching it then parks and sends a [`Release`]
    /// on the returned receiver. Dropping the receiver also disarms it.
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

/// Export a fresh `GroupInfo` for the conversation and upsert it through the DS.
/// The row only moves to a STRICTLY greater epoch, so concurrent writers are
/// idempotent. No-op without a local group or a device id.
///
/// The prerequisite for Secret Key recovery: a new device external-joins from
/// the stored `GroupInfo` without a Welcome.
pub async fn publish_group_info(
    state: &Arc<AppState>,
    conversation_id: &str,
) -> crate::error::Result<()> {
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

/// Export the current `GroupInfo` for `conversation_id` as `(epoch, tls_bytes)`,
/// or `Ok(None)` when there is nothing usable to publish (best-effort backstop).
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
    // A staged commit means the epoch is still being decided; publishing
    // GroupInfo for it is the same defect as sealing at it (#1079).
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

/// Parse a stored `mls_group_info` payload, accepting BOTH wire forms: the
/// current `MlsMessage`-wrapped form and the legacy **bare `GroupInfo`**.
///
/// Misreading a bare `GroupInfo` as an envelope fails as:
///
/// ```text
/// stored group_info envelope failed to deserialize: UnknownValue(82)
/// ```
///
/// That is this build's own ciphersuite (`0x0052`) read where the envelope's
/// `wire_format` would be. The bare form is tried ONLY after the strict envelope
/// parse fails, so a well-formed envelope is never reinterpreted.
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

/// Whether the published GroupInfo (`None` = no row) is behind our local head and
/// must be republished. Lexicographic on `(generation, epoch)` (#454 P4): a
/// successor restarts at epoch 0, and an epoch-only test would fight to overwrite
/// it with the retired lineage's.
fn group_info_is_stale(published: Option<(i64, u64)>, local: (i64, u64)) -> bool {
    match published {
        None => true,
        Some(p) => p < local,
    }
}

/// The `(generation, epoch)` head of the published GroupInfo, or `None` if there
/// is no row or the read fails: biased toward republishing, since a redundant
/// publish is safer than a missed heal.
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
/// no GroupInfo is published or the caller may not see one (the same decision
/// everywhere this feeds).
pub(super) fn group_info_head_of(
    snap: &pollis_api::reads::ConversationState,
) -> Option<(i64, u64)> {
    let gi = snap.group_info.as_ref()?;
    Some((gi.generation, gi.epoch as u64))
}

/// Durability backstop for MLS bootstrap: republish GroupInfo if a past publish
/// was dropped. Without one, no member can external-join, and nothing else heals
/// a sole-member creator who applies no commits.
///
/// Called on every "group touched" pass; one read for healthy groups. No-op
/// without a local group.
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

    // One snapshot answers both whether the head is behind and whether we are
    // still a member (republishing for a group we left is a 403 every pass).
    let Ok(snap) = super::ds_reads::conversation_snapshot(
        state,
        super::ds_reads::state_query(conversation_id, None),
    )
    .await
    else {
        // Unreadable → treat the GroupInfo as ABSENT and republish.
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

/// Join an existing MLS group via external commit onto the latest published
/// `GroupInfo`; the device becomes a member at the next epoch. Used by Secret
/// Key recovery, where no sibling is online to issue a Welcome.
pub async fn external_join_group(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    let _guard = state.mls_group_lock(conversation_id).await;
    external_join_group_inner(state, conversation_id, user_id).await
}

enum ExternalJoinResult {
    /// Our external commit won its epoch and was persisted.
    Joined,
    /// Another member committed at the target epoch first; our local branch is
    /// doomed. Carries the branch's generation, the lineage to delete.
    LostRace { generation: i64 },
    /// A Welcome for this device became available: stand down rather than race
    /// our own Welcome.
    DeferredToWelcome,
    /// This device already holds a CONFIRMED group for the lineage (#1220): it is
    /// a member, and another branch would discard that group.
    AlreadyMember,
}

/// Body of [`external_join_group`]; the caller holds the MLS lock. On a lost
/// CAS it discards the doomed branch and retries, bounded, from the new epoch,
/// so two concurrent joins cannot fork the group.
pub(crate) async fn external_join_group_inner(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    const MAX_JOIN_ATTEMPTS: u32 = 5;
    for attempt in 0..MAX_JOIN_ATTEMPTS {
        match external_join_attempt(state, conversation_id, user_id).await? {
            ExternalJoinResult::Joined => return Ok(()),
            // A Welcome became available mid-recovery: the group will be joined
            // through `apply_welcome`.
            ExternalJoinResult::DeferredToWelcome => return Ok(()),
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
                // Drop the doomed branch ONLY: a Welcome that replaced it while the
                // CAS was in flight is confirmed and stays (#1220).
                forget_unconfirmed_branch(state, conversation_id, join_generation).await;
                // Test-harness only: the window in which our own Welcome can land.
                #[cfg(feature = "test-harness")]
                rendezvous::park(rendezvous::Point::ExternalJoinAfterLostRace).await;
                // Let the winner publish GroupInfo at the new epoch first.
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
/// commit against it, and try to claim its epoch through the commit-log CAS.
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

    // 1. The published GroupInfo is always the newest lineage, so a recovering
    //    device joins the successor directly. The Welcome check below reads the
    //    SAME snapshot, which is load-bearing.
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

    // The DS writes the commit, its GroupInfo and the add's Welcomes in ONE
    // transaction, so a present GroupInfo means any Welcome for this device is
    // present too: stand down rather than race it (the DM-accept convergence
    // race). Both facts come from ONE snapshot (#987); reading the Welcome in a
    // second request would reopen the race and make the Kani property vacuous.
    if snap.welcome_pending {
        crate::diag!(
            "[mls] external_join: a Welcome for {conversation_id} is available — deferring to it \
             instead of external-joining (avoids racing our own Welcome)"
        );
        return Ok(ExternalJoinResult::DeferredToWelcome);
    }

    // 2. Parsing the GroupInfo touches no crypto, and it is what tells a
    //    recovering device the group's suite.
    let verifiable_group_info: VerifiableGroupInfo =
        parse_stored_group_info(&group_info_bytes)?;
    let suite = verifiable_group_info.ciphersuite();

    // 2a. Who may have built this lineage (#1161 M6 residual). Both reads fail
    //     to the weak answer (`None` roster, unpinned directory), and neither
    //     failure refuses a join; see `invariants::lineage_adoption`.
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
        // Already a member of this lineage by another path (#1220), most often our
        // own Welcome. Checked under the same guard as the build.
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

    // Test-harness only: the branch is stored and the CAS not yet submitted.
    #[cfg(feature = "test-harness")]
    rendezvous::park(rendezvous::Point::ExternalJoinBeforeSubmit).await;

    // 3. Claim the epoch via the commit-log CAS. An ambiguous outcome is resolved
    //    against the log (#411): our exact bytes there mean we won.
    match super::delivery::submit_commit(
        state,
        conversation_id,
        stored_generation,
        stored_epoch,
        // An external join continues the lineage it read; it closes nothing.
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
                // Genuine failure: discard the orphaned branch (only), as on
                // LostRace (#1220).
                forget_unconfirmed_branch(state, conversation_id, stored_generation).await;
                return Err(e);
            }
        }
    }

    // 3b. Record the lineage BEFORE anything reads the group back. Monotone, so a
    //     stale-generation join cannot walk the device backwards.
    {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            // The log accepted the commit: the branch is now confirmed (#1220).
            super::join_branch::clear(db.conn(), conversation_id, stored_generation);
            // Recorded only now: a branch that loses never held the lineage (#1312).
            super::join_branch::record_first_join(
                db.conn(),
                conversation_id,
                stored_generation,
                stored_epoch as u64 + 1,
            );
            set_local_generation(db.conn(), conversation_id, stored_generation);
            // Our new leaf is already merged; the rotation clock is per lineage.
            super::self_update::record_self_update(
                db.conn(),
                conversation_id,
                stored_epoch as u64 + 1,
            );
        }
    }

    // 4. Refresh the stored GroupInfo at the new epoch.
    if let Err(e) = publish_group_info(state, conversation_id).await {
        crate::diag!(
            "[mls] external_join_group: publish_group_info failed (non-fatal): {e}"
        );
    }

    // Best-effort pin-key adopt (#99): usually a no-op, since our commit just
    // advanced past the stored wrap, but it catches a re-join into a quiet group.
    crate::commands::pinned_messages::try_adopt_pin_key(state, conversation_id).await;
    crate::commands::file_keys::try_adopt_file_keys(state, conversation_id).await;

    crate::diag!(
        "[mls] external_join_group: {user_id}:{device_id} joined {conversation_id} from epoch {stored_epoch}"
    );

    Ok(ExternalJoinResult::Joined)
}

/// The material an external join needs to decide WHO built the group it is about
/// to adopt (#1161 M6 residual). A mandatory parameter, so no call site can skip
/// the provenance check.
///
/// * unpinned or empty `identities` → every verdict `Unverifiable`, which ADOPTS.
/// * `roster` is `None` only when it could not be READ; a signer outside
///   `Some(set)` is refused.
pub(super) struct LineageProvenance<'a> {
    /// The roster's cross-signing material, already pinned
    /// (`reconcile::pin_identities`).
    pub identities: &'a IdentityDirectory,
    /// The conversation's current member user ids, or `None` when unreadable.
    pub roster: Option<&'a std::collections::HashSet<String>>,
}

impl LineageProvenance<'_> {
    /// Is `user_id` a current member? `None` when the roster is unknown.
    fn signer_on_roster(&self, user_id: &str) -> Option<bool> {
        self.roster.map(|r| r.contains(user_id))
    }
}

/// The leaf of `tree` that actually signed `vgi`, with the identity it claims.
///
/// `VerifiableGroupInfo::signer()` is `pub(crate)` at the pinned openmls rev, so
/// the signer is recovered by trial verification against each leaf's key, which
/// is the same check an index would need. `tree` has been through
/// `RatchetTreeIn::into_verified`, so each credential is bound to its key.
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
        // `verify_no_out`: `verify` consumes the GroupInfo, forcing a clone of the
        // ratchet tree per candidate leaf.
        vgi.verify_no_out(provider.crypto(), &pk).is_ok()
    })
}

/// Refuse a GroupInfo whose signer is not a certified device of a current
/// roster member (#1161 M6 residual).
///
/// A GroupId is not a capability, so pinning the lineage's name does not stop a
/// hostile DS standing up its own successor and having us adopt it (deleting
/// the predecessor's keys). A DS holds no account identity key, so it cannot
/// mint a certified signer. Runs before the stale-group delete, so a refusal
/// costs nothing. See `.codesight/wiki/mls.md` → "Adopting a lineage is gated
/// twice".
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

    // Every group we build carries the ratchet-tree extension, and without one
    // there is no way to see who signed it.
    let Some(tree_ext) = vgi.extensions().ratchet_tree() else {
        return Err(refuse(
            "the published GroupInfo carries no ratchet-tree extension, so there is no way to \
             establish who built it"
            .to_string(),
        ));
    };

    // Binds every leaf's credential to its key, this GroupId and leaf position.
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
                // Adopted on the weak verdict (the deliberate bias), never silently.
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

/// Build (and locally stage) an external commit joining the group described by
/// `verifiable_group_info`, returning the serialised commit and the GroupInfo at
/// the resulting epoch. `suite` is the group's and decides the joining leaf's
/// scheme. Sync: the caller owns the local-DB guard.
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

    // The GroupInfo must describe the lineage we MEANT to join (#1161 M6):
    // everything downstream is keyed on `(conversation_id, generation)`, but the
    // blob comes from the DS. Checked before the delete, so a refusal costs
    // nothing.
    let group_id = mls_group_id(conversation_id, generation);
    if verifiable_group_info.group_id() != &group_id {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "external join for {conversation_id} generation {generation}: the published GroupInfo \
             is for a different group ({:?}) — refusing to join it",
            verifiable_group_info.group_id()
        )));
    }

    // …and it must have been built by a member of this conversation, not merely
    // named after it (#1161 M6 residual).
    check_lineage_provenance(
        provider,
        conversation_id,
        generation,
        suite,
        &group_id,
        &verifiable_group_info,
        provenance,
    )?;

    // Drop this device's own leftover branch for the lineage, but NEVER a
    // confirmed group (#1220): it holds the only keys for its epoch. A caller
    // that means to rebuild deletes it first. Scoped to THIS lineage, so a
    // predecessor still being drained survives.
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

    // The stored group is now a branch the log has not accepted (#1220), marked
    // in the same critical section that stored it.
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
/// with `creator_user_id`:`device_id` as the sole initial member. The one place
/// a group's suite is decided; a parameter so `super::migrate` can choose it.
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

    // Delete any stale group with the same id (a repair). Lineage-scoped, so a
    // successor (#454 P4) leaves the predecessor serving reads.
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
    // A repair recreates a lineage this device already held; the first-join
    // record keeps the original floor (#1312).
    super::join_branch::record_first_join(provider.raw_conn(), conversation_id, generation, 0);

    Ok(())
}

/// Create a fresh MLS group for `conversation_id` with `creator_user_id` as the
/// sole member, on generation 0 and the current suite, and publish its GroupInfo.
pub async fn init_mls_group(
    state: &Arc<AppState>,
    conversation_id: &str,
    creator_user_id: &str,
) -> Result<()> {
    let device_id = state.device_id.lock().await.clone()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("device_id not set")))?;

    // Dropped before `publish_group_info`, which re-acquires it.
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
            current_suite(),
        )?;
    }

    // Publish the epoch-0 GroupInfo so Secret Key recovery can external-join.
    if let Err(e) = publish_group_info(state, conversation_id).await {
        crate::diag!("[mls] init_mls_group: publish_group_info failed (non-fatal): {e}");
    }

    // Mint the pin master key while the creator is the sole member (#99).
    // Best-effort: on failure the first pin mints.
    crate::commands::pinned_messages::mint_pin_key_for_new_group(state, conversation_id).await;
    crate::commands::file_keys::mint_file_keys_for_new_group(state, conversation_id).await;

    Ok(())
}

/// Reload an existing MLS group from storage and recover the signer under the
/// stored group's own suite scheme.
///
/// **Does NOT merge a pending commit (#1079).** Merging is a decision only the
/// commit log may make; callers with the log's answer merge themselves (see
/// `no_new_site_merges_a_pending_commit`).
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

/// Wipe all local MLS state for a group without publishing a commit: MLS does
/// not let a member commit its own removal, so a leaver just deletes its group.
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

/// [`forget_local_mls_group`] scoped to one suite generation (#454 P4), for a
/// device holding two groups for one conversation, where deleting the wrong one
/// destroys the one still needed.
pub(super) async fn forget_local_mls_group_at(
    state: &Arc<AppState>,
    group_id: &str,
    generation: i64,
) -> crate::error::Result<()> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
    })?;
    if let Some(mut group) = load_stored_group_at(db.conn(), group_id, generation) {
        // Leaving a lineage leaves its epoch too: keep the file KEK, as every
        // merge does.
        crate::commands::file_keys::keep_epoch_kek(&PollisProvider::new(db.conn()), &group);
        group.delete(&store_only(db.conn()))
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("mls delete group: {e}")))?;
    }
    // Whatever was stored is gone, branch or not (#1220).
    super::join_branch::clear(db.conn(), group_id, generation);
    Ok(())
}

/// Delete this device's unconfirmed external-join branch for the lineage, and
/// nothing else (#1220). By the time a lost CAS is known, our own Welcome may
/// have replaced the branch; the check and the delete share one critical
/// section so it cannot land between them.
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
/// `mls_group_id` (already resolved: group_id for channels, conversation_id for
/// DMs), applying pending commits and falling back to external join when the
/// group is missing or unrecoverably behind.
///
/// Holds the per-conversation MLS lock throughout; callers that already hold it
/// call [`process_pending_commits_locked`].
pub async fn process_pending_commits_inner(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    let _guard = state.mls_group_lock(mls_group_id).await;
    process_pending_commits_locked(state, mls_group_id, user_id).await
}

/// Whether this device's `user_device` row still exists remotely, gating the
/// external-join recovery paths so a revoked device cannot rejoin.
///
/// Fails CLOSED: the DS `/v1/commits` endpoint does not gate on revocation, so
/// this check is the only thing between a just-revoked device and a climb-back.
/// A legitimate device recovers on the next pass.
fn local_device_registered(snap: Option<&pollis_api::reads::ConversationState>) -> bool {
    // A failed request (`None`) and an unreadable `user_device`
    // (`device_registered: None`) are both "couldn't check": false.
    snap.and_then(|s| s.device_registered).unwrap_or(false)
}

/// Whether `user_id` is a CURRENT member of the conversation backed by
/// `mls_group_id`, gating recovery so a removed-but-not-revoked member cannot
/// external-join back into the tree (fuzzer finding #2). Answered by the DS's
/// own `writes::is_member`.
///
/// Fails CLOSED: it guards a membership leak. A legitimate member recovers on
/// the next pass.
fn local_user_is_member(snap: Option<&pollis_api::reads::ConversationState>) -> bool {
    snap.and_then(|s| s.is_member).unwrap_or(false)
}

/// Whether an undelivered MLS `Welcome` for THIS device targets `mls_group_id`;
/// when true, external-join recovery stands down rather than race it.
///
/// Fails OPEN: a Welcome we cannot see must never block recovery forever, and
/// `apply_welcome` marks the row delivered even on failure, so the gate lifts.
///
/// **The OPPOSITE bias to [`local_user_is_member`]; do not collapse their error
/// paths.**
fn has_pending_welcome(snap: Option<&pollis_api::reads::ConversationState>) -> bool {
    snap.map(|s| s.welcome_pending).unwrap_or(false)
}

/// Both cooperative gates on the external-join recovery paths (registered AND
/// member), through the Kani-proved `invariants::may_rejoin`. Logs the specific
/// reason so a skipped recovery is never silent.
fn may_rejoin_via_external_join(
    snap: Option<&pollis_api::reads::ConversationState>,
    mls_group_id: &str,
    user_id: &str,
) -> bool {
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

/// Body of [`process_pending_commits_inner`]; the caller holds the MLS lock.
pub(crate) async fn process_pending_commits_locked(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    process_pending_commits_locked_impl(state, mls_group_id, user_id, None, None).await
}

/// Like [`process_pending_commits_inner`], but invokes `on_epoch(conn,
/// generation, epoch)` each time the group reaches an epoch, so the caller
/// decrypts that epoch's envelopes BEFORE the next commit discards its keys
/// (`max_past_epochs = 0`, #418).
///
/// Fires for the starting epoch and after each applied commit, not for epochs a
/// recovery jump skips (caught on the next ingest). `bound` (#1041) limits the
/// replay; see [`ReplayBound`]. `None` replays to head.
pub async fn process_pending_commits_inner_with_hook(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
    on_epoch: &mut (dyn FnMut(&rusqlite::Connection, i64, u64) + Send),
    bound: Option<ReplayBound>,
) -> crate::error::Result<()> {
    let _guard = state.mls_group_lock(mls_group_id).await;
    process_pending_commits_locked_impl(state, mls_group_id, user_id, Some(on_epoch), bound).await
}

/// How far one ingesting replay may advance the local group (#1041).
///
/// The pass reads the log head, then envelopes, then commits. A commit accepted
/// after the envelope fetch could destroy the keys of an envelope that fetch did
/// not carry, so the replay stops at the head read FIRST; the rest applies next
/// pass. Lexicographic on `(generation, epoch)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayBound {
    pub generation: i64,
    /// The head epoch in `generation`; a commit row at or beyond it is not applied.
    pub epoch: u64,
}

impl ReplayBound {
    /// Whether a commit row at `(generation, epoch)` lies below the bound and
    /// may be applied on this pass.
    pub fn admits(self, generation: i64, epoch: u64) -> bool {
        (generation, epoch) < (self.generation, self.epoch)
    }

    /// Whether the log had already opened a successor beyond `generation` when the
    /// bound was read, so adopting one cannot skip unfetched envelopes.
    pub fn allows_generation_beyond(self, generation: i64) -> bool {
        self.generation > generation
    }
}

/// Most suite generations a single pass will hop across (#454 P4), so a
/// conversation that migrated while offline is ingested on the discovering
/// pass. Bounded because a misbehaving DS drives it.
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

/// One replay pass over a single suite lineage. Returns whether it ended by
/// adopting a NEWER generation, in which case the caller replays again.
// `'h` is named so the hook's lifetime is decoupled from the per-hop reborrow.
async fn process_one_generation<'h>(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
    mut on_epoch: Option<&mut EpochHook<'h>>,
    bound: Option<ReplayBound>,
) -> crate::error::Result<bool> {
    // 1. The current lineage and epoch.
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
            // No local group: arbitrate Welcome vs external-join recovery through
            // the Kani-proved I6 gate (`invariants::join_recovery`). All inputs come
            // from ONE snapshot (split, a device could see GroupInfo without its
            // Welcome and strand itself), and each gate keeps its OWN failure bias,
            // so the three reads below must not be folded together.
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
            // Fails OPEN.
            let welcome_pending = has_pending_welcome(snap);
            // Fails CLOSED.
            let may_rejoin = may_rejoin_via_external_join(snap, mls_group_id, user_id);
            // #832: with no published GroupInfo there is nothing to join onto.
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
                        // No GroupInfo yet: a Welcome is the only way in, so poll
                        // for it now (#832).
                        crate::diag!(
                            "[mls] process_pending_commits: no local group for {mls_group_id} and no \
                             published GroupInfo — awaiting a Welcome (external-join would have \
                             nothing to join onto); polling welcomes now"
                        );
                        // `Box::pin`: the poll can re-enter this path, so the future
                        // is recursive.
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
                    // `may_rejoin_via_external_join` already logged the reason.
                }
            }
            return Ok(false);
        }
    };

    // #418 interleave: decrypt the envelopes at the CURRENT epoch before any
    // commit advances past it.
    if let Some(hook) = on_epoch.as_deref_mut() {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            hook(db.conn(), generation, initial_epoch);
        }
    }

    // 2. Fetch this lineage's pending commits, with the cross-signing rows for
    //    every user the add-metadata hints at (#987). The DS reads head and batch
    //    in ONE transaction and refuses a non-contiguous batch, which is what
    //    makes the gap handling below (which can delete MLS state) safe.
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
        // Removed from the conversation: not a recovery trigger
        // (`may_rejoin_via_external_join` decides that), so the pass ends.
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

    // A short batch is a retention prune (#539, `pruned_below > since`) or a
    // `hole` the DS found inside the same read transaction. Both recover the
    // same way; the log tells them apart.
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

    // The server's own account of a short batch: the gap gate may destroy crypto
    // state only when one of these covers our missing epoch (#1161 M6).
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

    // Cross-signing rows for every user the batch's hints name (#987). INPUTS
    // only: a user the hint omits verdicts `Unverifiable`. PINNED through the
    // committing side's helper (#1161 M3); an unpinnable directory degrades to
    // an EMPTY one, never a DS-authored one.
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

    // 3. Apply each commit in epoch order. Every leaf a commit GRAFTS (#1161 H4)
    //    is verdicted; an uncertified one is flagged and evicted afterwards
    //    (`report_uncertified_leaves`). The commit is still merged: it is
    //    canonical, and refusing it would strand this device. Eviction is the
    //    append-only answer.
    let mut flagged: Vec<(AddedLeaf, i64, String, FlagKind)> = Vec::new();
    let mut current_epoch = initial_epoch;
    let mut any_applied = false;
    // Set when a commit could not be applied and the lineage must be rebuilt
    // (#680): recovery is driven by an EXPLICIT reason, never inferred from a
    // group's absence, so a group that cannot advance never wedges.
    let mut recover: Option<RecoverReason> = None;
    for commit in pending {
        // #1041: a commit at or beyond the bound was accepted after this pass's
        // envelope fetch; leave it, and everything after, to the next pass.
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

        // Gap classification (I1, `invariants::classify`, Kani-proved never to
        // `Apply` across a gap; modelled in `specs/tla/CommitLog.tla`).
        if super::invariants::classify(current_epoch, Some(commit.epoch as u64))
            != super::invariants::ReplayStep::Apply
        {
            // A missing bridge. Rebuilding deletes the only keys for this
            // lineage's un-ingested envelopes, so only a DS-declared explanation
            // covering our epoch licenses it (#1161 M6,
            // `invariants::gap_recovery`); otherwise hold position.
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

        // All MLS work is synchronous and scoped so nothing !Send crosses an await.
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
                    // A canonical commit applied on top of the stored group
                    // confirms a branch whose acceptance was never recorded
                    // (#1220).
                    super::join_branch::clear(db.conn(), mls_group_id, generation);
                    // Verify AFTER the merge, OFF THE TREE: a StagedCommit cannot
                    // be re-processed later, so the remedy is flag-and-evict.
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
                        // #1127: say which of the two this is.
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
                    // #1161 C2: every envelope this pass buckets is keyed on
                    // `current_epoch`, so CHECK the merged group's epoch rather
                    // than assuming `+= 1`; a silent divergence mis-buckets and
                    // permanently marks every later envelope handled.
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

        // Stop rather than run the epoch hook against a counter proved wrong.
        if recover.is_some() {
            break;
        }

        if let Some(epoch_after) = applied {
            current_epoch = epoch_after;
            any_applied = true;
            // #418 interleave: decrypt this epoch's envelopes while the group
            // still holds its keys.
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

    // Resolve a commit left DANGLING by an interrupted submit. Had it landed, the
    // replay above would have adopted it, so clear it rather than risk a phantom
    // epoch (#411). Safe even if it did land: a future pass re-adopts it.
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

    // Suite-generation backstop (#454 P4): the lineage we just replayed may have
    // been retired. After the replay (predecessor drained) and the pending clear.
    // Only on an INGESTING pass, since adoption deletes the predecessor's keys,
    // and only when the bound (#1041) allows it.
    let bound_allows_hop = bound.is_none_or(|b| b.allows_generation_beyond(generation));
    if on_epoch.is_some() && bound_allows_hop {
        if let Some(new_generation) =
            maybe_advance_generation(state, mls_group_id, user_id, generation).await
        {
            crate::diag!(
                "[mls] process_pending_commits: {mls_group_id} advanced generation \
                 {generation} -> {new_generation} — replaying the new lineage"
            );
            // A new group means a new exporter: voice, pin key (#99), file keys.
            crate::commands::voice_e2ee::on_mls_epoch_changed(state, mls_group_id).await;
            crate::commands::pinned_messages::maybe_rewrap_pin_key(state, mls_group_id).await;
            crate::commands::file_keys::maybe_rewrap_file_keys(state, mls_group_id).await;
            return Ok(true);
        }
    }

    // GroupInfo durability backstop: republishes when the published GroupInfo is
    // missing or behind, which also heals a dropped create-time publish.
    ensure_group_info_published(state, mls_group_id).await;

    // Pin-key durability backstop (#99): republish the wrap if the epoch moved
    // past it. One local SELECT in the steady state.
    crate::commands::pinned_messages::maybe_rewrap_pin_key(state, mls_group_id).await;
    crate::commands::file_keys::maybe_rewrap_file_keys(state, mls_group_id).await;

    if any_applied {
        crate::commands::voice_e2ee::on_mls_epoch_changed(state, mls_group_id).await;
    }

    // Recover by external join if the group is gone or a commit asked for
    // recovery (#680). Keyed off the explicit reason, so a wedge is never
    // mistaken for "caught up".
    let group_exists = {
        let guard = state.local_db.lock().await;
        guard.as_ref().is_some_and(|db| {
            load_stored_group_at(db.conn(), mls_group_id, generation).is_some()
        })
    };
    if !group_exists || recover.is_some() {
        // Behind the revoked/removed gates (fuzzer finding #2), re-read rather
        // than reused: applying a commit may have removed THIS member. Fails
        // CLOSED.
        let gates = super::ds_reads::conversation_snapshot(
            state,
            super::ds_reads::state_query(mls_group_id, Some(generation)),
        )
        .await
        .ok();
        let gates = gates.as_ref().filter(|s| s.authorized);
        if may_rejoin_via_external_join(gates, mls_group_id, user_id) {
            // An explicit rebuild: an external join never replaces a confirmed
            // group on its own (#1220), and the epoch-desync path leaves it stored.
            if group_exists {
                let _ = forget_local_mls_group_at(state, mls_group_id, generation).await;
            }
            crate::diag!("[mls] process_pending_commits: group {mls_group_id} was deleted during processing — external-joining to recover");
            if let Err(e) = external_join_group_inner(state, mls_group_id, user_id).await {
                crate::diag!("[mls] process_pending_commits: recovery external-join failed for {mls_group_id}: {e}");
            }
        }
    }

    // Retention signal (#539, I4): report our applied epoch so the DS prune floor
    // follows the slowest current member. Best-effort.
    report_applied_epoch(state, mls_group_id, user_id).await;

    Ok(false)
}

/// Adopt a newer suite generation for `mls_group_id` if one exists, returning the
/// generation adopted:
///
/// 1. **The successor is already here** (its Welcome stored it, inert until now):
///    flip the lineage row and delete the predecessor.
/// 2. **The successor exists but we have no Welcome**: a published head
///    generation above ours is proof, so external-join it behind the same
///    revoked/removed gates.
///
/// `None` means "still on the newest lineage we can see", the common case.
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
        // The log is the authority on which lineages exist: a stored successor
        // may be a migration that lost its CAS. A Welcome we could have applied
        // implies a visible GroupInfo (one DS transaction), so this check has no
        // false negatives.
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

    // 2. No successor locally. Ask the log in ONE answer whether one exists and
    //    whether we may still join it, so a removal cannot straddle the two.
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
    // The caller holds the MLS lock. On success the join records the lineage
    // itself; re-read rather than assume.
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
        // The predecessor is drained and superseded.
        let _ = forget_local_mls_group_at(state, mls_group_id, generation).await;
        Some(adopted)
    } else {
        None
    }
}

/// Move this device onto `successor` for `mls_group_id`: record the lineage,
/// drop the drained predecessor, and re-arm the post-join self-update. The
/// caller holds the MLS lock and has drained the predecessor.
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
        // Our Welcome-joined leaf in the successor is UNMERGED, so the rotation
        // clock restarts with the lineage (#666).
        super::self_update::clear_self_update(db.conn(), conversation_id);
    }
    // Only now is the predecessor safe to destroy: a crash in between leaves a
    // harmless orphan, not a device with no group.
    let _ = forget_local_mls_group_at(state, conversation_id, predecessor).await;
}

/// Why the local group could not advance from a commit. Every variant is
/// recovered by delete-and-rejoin, and is a value distinct from `Applied`, so a
/// wedge is never mistaken for "caught up" (#680).
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
    /// The row is a protocol message but NOT a Commit (#1161 C2): damage, never
    /// a free advance.
    NotACommit,
    /// The row is a protocol message for a DIFFERENT group id than the lineage
    /// we are replaying (#1161 C2).
    WrongGroup,
    /// The commit merged but left the group at an epoch other than
    /// `commit.epoch + 1` (#1161 C2).
    EpochDesync,
    /// Merging the staged commit failed (storage error / diverged tree).
    MergeFailed,
    /// Our keys cannot open the commit — we were evicted from the group.
    Evicted,
    /// The commit is at our epoch yet won't stage/validate: our local tree has
    /// diverged from the canonical branch (#411).
    ForkedTree,
    /// Our own commit was echoed back but merging the pending commit failed.
    OwnCommitMergeFailed,
    /// An openmls error this code does not classify: recover, NEVER wedge (#680).
    Unclassified,
}

/// Outcome of applying one commit from the log — see [`apply_one_commit`].
#[derive(Debug)]
pub(super) enum CommitApply {
    /// Merged; the local epoch advanced. `adds` are the leaves the commit
    /// GRAFTED (see [`grafted_leaves`]); `epoch_after` is read back off the
    /// merged group.
    Applied {
        adds: Vec<AddedLeaf>,
        epoch_after: u64,
    },
    /// The group cannot advance on this lineage and must be rebuilt;
    /// `apply_one_commit` has already deleted it where one was loaded.
    Recover(RecoverReason),
}

/// Map an openmls `process_message` error to the recovery it warrants, matching
/// the error ENUMS, never `Display` strings, so an openmls rewording cannot
/// silently wedge a group (#680).
pub(super) fn classify_process_message_error<S>(
    e: &ProcessMessageError<S>,
) -> RecoverReason {
    use ProcessMessageError as E;
    match e {
        // Evicted: our keys can't open this or any later commit. The `may_rejoin`
        // gate downstream decides whether to rejoin.
        E::GroupStateError(MlsGroupStateError::UseAfterEviction) => RecoverReason::Evicted,
        // Our OWN commit came back but does NOT match our pending commit: rebuild
        // rather than diverge further.
        E::InvalidCommit(StageCommitError::OwnCommitMismatch) => RecoverReason::ForkedTree,
        // Any other stage/validation failure at our epoch is the same fork shape.
        E::InvalidCommit(_) | E::ValidationError(_) => RecoverReason::ForkedTree,
        _ => RecoverReason::Unclassified,
    }
}

/// Apply a single commit from the log to the local group. Errors are handled
/// in place, each with its own recovery, so none aborts the caller's replay
/// loop (see [`CommitApply`]). Sync: the caller owns the local-DB guard.
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

    // #1161 C2/H5: a commit-log slot may carry a COMMIT for THIS group and
    // nothing else, checked HERE before openmls sees it. The DS does not parse
    // commit bytes, so any member can POST an application message to
    // `/v1/commits`; treating one as a no-op would desynchronise the replay
    // counter and lose every later envelope (a fourth loss mode).
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

    // The tree BEFORE the merge, so grafted leaves can be diffed out afterwards.
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
            // Our OWN commit fanned back (e.g. a lost response, #411). At the
            // pinned openmls rev this is CONTENT, not an error, and since Pollis
            // frames handshakes as PrivateMessage, `OwnPrivateMessage` is the path
            // we hit. Adopt it by merging our pending commit.
            ProcessedMessageContent::OwnPendingCommit
            | ProcessedMessageContent::OwnPrivateMessage => {
                // The content-type gate above is what makes "our own message"
                // mean "our own commit" (#1161 H5). With no pending commit to
                // adopt, rebuild rather than wedge.
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
            // Unreachable now that the content type is checked up front, and
            // still a `Recover`, never a free advance (#1161 C2).
            _ => {
                crate::diag!(
                    "[mls] process_pending_commits: unexpected non-commit content at epoch {commit_epoch} for {mls_group_id} — recovering via external-join"
                );
                let _ = group.delete(provider.storage());
                return CommitApply::Recover(RecoverReason::NotACommit);
            }
        },
        Err(e) => {
            // Drop the local group so the external rejoin rebuilds it at the latest
            // epoch (MLS crypto state only; decrypted history is untouched). The
            // next pass starts past this commit, so it cannot loop.
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

/// One leaf of a ratchet tree as the grafted-leaf diff identifies it:
/// `(position, user, device, signature key)`. All four, since each alone misses
/// a swap into an occupied index or a duplicate credential.
type LeafFingerprint = (LeafNodeIndex, String, String, Vec<u8>);

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
/// Diffing the tree rather than reading Add proposals catches a leaf planted by
/// an external join's UpdatePath (#1161 H4). An ordinary UpdatePath rotates only
/// the ENCRYPTION key, so it does not read as grafted.
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

/// Why a replayed add was flagged, and therefore whether the USER is told
/// (#1127). Both kinds earn a self-correcting eviction reconcile; only
/// `Uncertified` (evidence, not the absence of it) earns a security alarm, or a
/// teammate enrolling a device would read as a compromise.
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
/// otherwise the reason and whether it is evidence (#1127).
fn flag_for(verdict: LeafVerdict) -> Option<(String, FlagKind)> {
    match verdict {
        LeafVerdict::Certified => None,
        LeafVerdict::Unverifiable(why) => Some((why.to_string(), FlagKind::Unverifiable)),
        LeafVerdict::Uncertified(why) => Some((why, FlagKind::Uncertified)),
    }
}

/// What a member does about leaves a replayed (already merged) commit added that
/// are not `Certified`: (1) a `security_event` (`uncertified_mls_leaf`) in this
/// user's own audit log for each POSITIVELY uncertified leaf (#1127), and (2) a
/// detached reconcile that evicts them, self-correcting for leaves that turn out
/// certified.
///
/// Residual: the rogue leaf holds the key schedule until one honest member's
/// eviction commit lands; nothing sooner preserves the append-only log.
async fn report_uncertified_leaves(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
    flagged: Vec<(AddedLeaf, i64, String, FlagKind)>,
) {
    for (leaf, epoch, reason, kind) in &flagged {
        // #1127: no user-facing alarm for an `Unverifiable` leaf, or a committer
        // could manufacture alarms by omitting the hint.
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
            // The DS's no-auth fallback for the acting user (`resolve_actor`).
            user_id: Some(user_id.to_string()),
        };
        if let Err(e) = super::ds_client::ds_post_ok(state, &ev).await {
            crate::diag!(
                "[mls] process_pending_commits: DS security-event for uncertified leaf failed (non-fatal): {e}"
            );
        }
    }

    // Detached: we hold the MLS lock, which reconcile re-acquires.
    tokio::spawn(eviction_reconcile(
        Arc::clone(state),
        mls_group_id.to_string(),
        user_id.to_string(),
    ));
}

/// The detached eviction reconcile [`report_uncertified_leaves`] spawns. Boxed
/// and type-erased to break the reconcile → catch-up → replay → spawn cycle the
/// compiler cannot prove `Send` through.
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

/// Report this device's current local `(generation, epoch)` for `mls_group_id`
/// as the retention high-water (#539). Detached so the MLS lock is never held
/// across the round trip; best-effort.
async fn report_applied_epoch(state: &Arc<AppState>, mls_group_id: &str, user_id: &str) {
    // The PAIR, compared lexicographically by the DS (#454 P4): epoch alone would
    // make every migrated device look like it regressed.
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

/// Tauri command wrapper: resolve conversation_id to its MLS group, then run the
/// group-level INTERLEAVED catch-up, never a bare commit replay, so no message
/// sealed at a skipped epoch is stranded (`max_past_epochs = 0`).
pub async fn process_pending_commits(
    state: &Arc<AppState>,
    conversation_id: String,
    user_id: String,
) -> crate::error::Result<()> {
    let mls_group_id = super::ds_reads::resolve_mls_group(state, &conversation_id).await?;
    crate::commands::messages::catch_up_mls_group_interleaved(state, &mls_group_id, &user_id).await
}

/// Check whether an MLS group exists in the local database. Ask this, not
/// `try_mls_encrypt(..).is_none()`, which since #1079 also means "a commit is
/// staged".
pub fn has_local_group(conn: &rusqlite::Connection, conversation_id: &str) -> bool {
    load_stored_group(conn, conversation_id).is_some()
}

/// Try to encrypt `plaintext` with the MLS group for `conversation_id`.
///
/// Returns `None`, without logging, if the group does not exist locally or holds
/// a **staged, unconfirmed commit** (#1079): sealing then would mean choosing an
/// epoch the log has not decided. Locked callers never see a pending commit; the
/// lock-free ones (`receipts::emit_receipt`, the `edit_delete` probes) skip.
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

/// Parse the `(generation, epoch)` a `message` / `edit` envelope was sealed at,
/// WITHOUT decrypting it. `None` when the bytes are not an MLS `ProtocolMessage`
/// (e.g. a `delete` tombstone), i.e. epoch-independent.
///
/// The ingest interleave (#418) routes each envelope to the replay moment its
/// epoch is reached; parsing beats `sent_at`, which skews. The generation comes
/// off the envelope's own `GroupId` (#454 P4), so a retired lineage's epoch 7
/// never reads as ahead of a successor's epoch 1.
pub fn envelope_lineage(ciphertext: &[u8]) -> Option<(i64, u64)> {
    let mut reader: &[u8] = ciphertext;
    let msg_in = MlsMessageIn::tls_deserialize(&mut reader).ok()?;
    let protocol_msg = msg_in.try_into_protocol_message().ok()?;
    let raw_group_id = String::from_utf8_lossy(protocol_msg.group_id().as_slice()).into_owned();
    let (_, generation) = super::generation::split_mls_group_id(&raw_group_id);
    Some((generation, protocol_msg.epoch().as_u64()))
}

/// Try to decrypt MLS ciphertext bytes for `conversation_id`, returning
/// `Some((plaintext, credential_user_id))`, or `None` on any failure.
///
/// The second element is the **MLS-authenticated** sender from the ciphertext's
/// credential, never the server-writable `message_envelope.sender_id` (sealed
/// sender, `docs/metadata-minimization-design.md` §2). Returning both together
/// means no caller can get plaintext without the authenticated sender.
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

/// Which step of opening one envelope failed (#1520), for the `[loss]`
/// diagnostic. Every variant names a step or an openmls error variant, never
/// anything derived from the plaintext or a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecryptFailureKind {
    /// This device holds no group for the conversation (never joined, or forgotten).
    NoLocalGroup,
    /// The bytes do not TLS-deserialize as an MLS message.
    Malformed,
    /// An MLS message, but not a protocol message (a Welcome, GroupInfo or KeyPackage).
    NotAProtocolMessage,
    /// Sealed in another generation of this conversation's group (a retired or
    /// successor lineage), which this decryptor does not hold.
    ForeignLineage,
    /// Sealed for a different group altogether.
    WrongGroup,
    /// openmls `SecretReuseError`: the key for this sender's generation was
    /// already consumed — either this device opened the same generation before,
    /// or the sender sealed two messages under one generation.
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

/// Why [`MlsDecryptor::decrypt`] could not open an envelope (#1520): the failing
/// step plus the clear-header epochs. The sender's leaf index and generation are
/// encrypted and not surfaced by openmls, so they are not here.
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

/// Map an openmls `process_message` error on an APPLICATION message to the step
/// that failed (#1520). Only the commit classifier's eviction case carries over;
/// enums, never `Display` strings (#680).
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

/// A group loaded **once** for a RUN of envelopes all sealed at its current
/// epoch (#875).
///
/// Holding it is safe: decrypting mutates only the secret tree, which openmls
/// persists inside `process_message` before returning, so a held group is
/// byte-identical to a reloaded one, even across a crash. It must NOT outlive
/// its epoch: the ingest interleave drops it before the replay applies the next
/// commit. See `.codesight/wiki/mls.md` → "One group load per epoch".
pub struct MlsDecryptor<'a> {
    provider: PollisProvider<'a>,
    group: MlsGroup,
}

impl<'a> MlsDecryptor<'a> {
    /// Load the conversation's live group, or `None` when this device has none,
    /// the same answer [`try_mls_decrypt`] gives.
    pub fn open(conn: &'a rusqlite::Connection, conversation_id: &str) -> Option<Self> {
        let group = load_stored_group(conn, conversation_id)?;
        Some(Self {
            provider: PollisProvider::new(conn),
            group,
        })
    }

    /// The `(generation, epoch)` the loaded group sits at. A staged-but-unmerged
    /// own commit does not move it.
    pub fn lineage(&self) -> (i64, u64) {
        let raw = String::from_utf8_lossy(self.group.group_id().as_slice()).into_owned();
        let (_, generation) = super::generation::split_mls_group_id(&raw);
        (generation, self.group.epoch().as_u64())
    }

    /// Decrypt one envelope; the contract is [`try_mls_decrypt`]'s. On failure
    /// the error names the failing step (#1520), never plaintext or key material.
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
        // A foreign-lineage envelope fails fast, rather than producing a
        // `process_message` error indistinguishable from a real decrypt failure.
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

    // A bare GroupInfo's prefix is protocol_version then ciphersuite 0x0052; the
    // envelope reader takes that as the wire format and reports `UnknownValue(82)`.
    const CS_PQ_CODE_POINT: [u8; 2] = [0x00, 0x52];

    // If the suite is ever renumbered, legacy rows still carry the old code point
    // and the fallback is what keeps them readable.
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

    // Neither form must fail, naming both attempts.
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

    // An empty row (a dropped publish) is an error, never a panic.
    #[test]
    fn rejects_empty_payload() {
        assert!(parse_stored_group_info(&[]).is_err());
    }

    // The premise of envelope-first parsing: no wire format collides with the
    // ciphersuite value at that offset.
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

    #[test]
    fn republishes_when_groupinfo_never_landed() {
        // No GroupInfo row at all, the stranded-creator case: republish at any epoch.
        assert!(group_info_is_stale(None, (0, 0)));
        assert!(group_info_is_stale(None, (0, 7)));
    }

    #[test]
    fn republishes_when_groupinfo_is_behind() {
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
        assert!(!group_info_is_stale(Some((0, 9)), (0, 4)));
    }

    /// #454 P4: a successor's epoch 0 is ahead of the retired lineage's last
    /// epoch under the lexicographic `(generation, epoch)` comparison.
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

    /// #1127: a positively uncertified leaf is evidence and alarms.
    #[test]
    fn an_uncertified_leaf_alarms() {
        let (reason, kind) =
            flag_for(LeafVerdict::Uncertified("leaf key is not the certified key".into()))
                .expect("must be flagged");
        assert_eq!(kind, FlagKind::Uncertified);
        assert!(kind.alarms(), "a positively uncertified leaf must reach the user");
        assert!(reason.contains("certified key"), "the reason is carried through");
    }

    /// The absence of evidence (#1080): still flagged so the reconcile runs, but
    /// the user is not told their membership is compromised.
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

    /// Exactly one verdict alarms, so a new verdict must decide deliberately.
    #[test]
    fn only_uncertified_alarms() {
        assert!(FlagKind::Uncertified.alarms());
        assert!(!FlagKind::Unverifiable.alarms());
    }
}
