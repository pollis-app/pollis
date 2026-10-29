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
/// A test arms a point, a client parks there and hands back a release handle,
/// and the test interleaves against it — otherwise these orderings depend on
/// which of two clients' HTTP round trips lands first.
#[cfg(feature = "test-harness")]
pub mod rendezvous {
    use std::sync::Mutex;
    use tokio::sync::{mpsc, oneshot};

    /// Handle a parked client hands the test: dropping or sending on it
    /// releases the client.
    pub type Release = oneshot::Sender<()>;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Point {
        /// External commit built and stored locally, CAS not yet submitted —
        /// the window in which another member's commit turns it into a lost
        /// race.
        ExternalJoinBeforeSubmit,
        /// `reconcile_group_mls_impl` entered, per-conversation lock not yet
        /// taken.
        ReconcileEntry,
        /// Self-update commit staged under the per-conversation lock, CAS not
        /// yet submitted — the window between the pre-op catch-up and the
        /// epoch advance.
        SelfUpdateBeforeSubmit,
        /// An application envelope sealed and about to be posted — the window
        /// between the sender's catch-up and its post, in which a commit that
        /// lands makes the envelope epoch-behind (#1041).
        EnvelopeBeforePost,
        /// An external join lost its compare-and-swap and dropped its branch;
        /// the retry has not started — the window in which the device's own
        /// Welcome lands (#1220).
        ExternalJoinAfterLostRace,
    }

    const POINTS: usize = 5;

    static ARMED: Mutex<[Option<mpsc::UnboundedSender<Release>>; POINTS]> =
        Mutex::new([None, None, None, None, None]);

    /// Every client reaching `point` in this process parks and sends a
    /// [`Release`] on the returned receiver. Dropping the receiver also disarms.
    pub fn arm(point: Point) -> mpsc::UnboundedReceiver<Release> {
        let (tx, rx) = mpsc::unbounded_channel();
        ARMED.lock().expect("rendezvous poisoned")[point as usize] = Some(tx);
        rx
    }

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

/// Export a fresh `GroupInfo` and upsert it via the DS; every device that
/// merges a commit calls this. The DS only overwrites with a strictly greater
/// head, so concurrent same-epoch writers are idempotent. No-op without a
/// device id or a local group. External-join recovery bootstraps from this row.
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

/// Export the current `GroupInfo` for `conversation_id` as `(epoch, tls_bytes)`.
///
/// `Ok(None)`, not an error, when there is no usable local group or the export
/// fails: publishing is a best-effort durability backstop.
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
    // A staged commit means the epoch is undecided; publishing GroupInfo for an
    // epoch the log may never hold is the same defect as sealing at it (#1079).
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
/// Current writers publish the `MlsMessage`-wrapped form; older rows are a bare
/// `GroupInfo`, which read as an envelope fails like this:
///
/// ```text
/// stored group_info envelope failed to deserialize: UnknownValue(82)
/// ```
///
/// Its ciphersuite (`CS_PQ` = 0x0052) sits where the wire format should be.
/// Both parses are strict and the bare form is tried only after the envelope
/// fails, so a well-formed envelope is never reinterpreted.
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

/// Republish iff no GroupInfo is published or its `(generation, epoch)` head is
/// behind ours. Lexicographic, not epoch-only (#454): a successor lineage
/// restarts at epoch 0, below the retired lineage's last epoch.
fn group_info_is_stale(published: Option<(i64, u64)>, local: (i64, u64)) -> bool {
    match published {
        None => true,
        Some(p) => p < local,
    }
}

/// The published GroupInfo head, or `None` if there is none. A failed read is
/// also `None` on purpose: the caller errs toward a redundant republish rather
/// than a missed heal.
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

/// The `(generation, epoch)` head out of a snapshot, or `None` when no GroupInfo
/// is published or the caller may not see it; every caller treats those alike.
pub(super) fn group_info_head_of(
    snap: &pollis_api::reads::ConversationState,
) -> Option<(i64, u64)> {
    let gi = snap.group_info.as_ref()?;
    Some((gi.generation, gi.epoch as u64))
}

/// Durability backstop: republish GroupInfo when the published head is missing
/// or behind ours. Without it a group whose create-time publish was dropped is
/// unjoinable forever — nothing else republishes for a sole member who applies
/// no commits. Runs on every "group touched" pass; one read when healthy.
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

    // The same snapshot says whether we are still a member: a removed member's
    // lingering local group would otherwise 403 on every pass.
    let Ok(snap) = super::ds_reads::conversation_snapshot(
        state,
        super::ds_reads::state_query(conversation_id, None),
    )
    .await
    else {
        // Unreadable counts as absent: a redundant publish beats a missed heal.
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

/// Join an existing group by external commit onto its published GroupInfo,
/// becoming a member at the next epoch. Used when no Welcome can arrive (e.g.
/// Secret Key recovery with no sibling device online).
///
/// Security: there is no outbound cert check here; existing members' inbound
/// leaf verification rejects a joiner whose cert doesn't chain to the user's
/// `account_id_pub`.
pub async fn external_join_group(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    let _guard = state.mls_group_lock(conversation_id).await;
    external_join_group_inner(state, conversation_id, user_id).await
}

enum ExternalJoinResult {
    Joined,
    /// Another member committed at the target epoch; the local branch must be
    /// discarded before retrying. Carries the branch's generation, since after a
    /// migration the device may hold two lineages.
    LostRace { generation: i64 },
    /// A Welcome for this device is pending; stood down so the device doesn't
    /// race its own Welcome (see [`external_join_attempt`]).
    DeferredToWelcome,
    /// A confirmed group already exists for the lineage, typically because the
    /// Welcome landed during a lost race (#1220). Another branch would discard
    /// it and every envelope sealed at its epoch.
    AlreadyMember,
}

/// Body of [`external_join_group`]; the caller holds `state.mls_group_lock`.
/// Each attempt is a compare-and-swap on the target epoch, so two concurrent
/// joins cannot fork the group; a loser discards its branch and retries.
pub(crate) async fn external_join_group_inner(
    state: &Arc<AppState>,
    conversation_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    const MAX_JOIN_ATTEMPTS: u32 = 5;
    for attempt in 0..MAX_JOIN_ATTEMPTS {
        match external_join_attempt(state, conversation_id, user_id).await? {
            ExternalJoinResult::Joined => return Ok(()),
            // Success: `apply_welcome` will do the join.
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
                // The branch only: a Welcome that replaced it mid-CAS left a
                // confirmed group that must stay (#1220).
                forget_unconfirmed_branch(state, conversation_id, join_generation).await;
                #[cfg(feature = "test-harness")]
                rendezvous::park(rendezvous::Point::ExternalJoinAfterLostRace).await;
                // Give the winner time to publish GroupInfo at the new epoch.
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
/// commit against it, and try to claim its epoch in the commit log.
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

    // The published GroupInfo is always the newest lineage, so a recovering
    // device joins a migrated conversation's successor directly. The Welcome
    // check below must read this same snapshot.
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

    // The DS writes a commit, its GroupInfo and its Welcomes in one transaction,
    // so a visible GroupInfo means any Welcome for this device is visible in the
    // same snapshot; defer to it rather than race it. Must stay one snapshot —
    // see "Welcome-first arbitration" in .codesight/wiki/mls.md.
    if snap.welcome_pending {
        crate::diag!(
            "[mls] external_join: a Welcome for {conversation_id} is available — deferring to it \
             instead of external-joining (avoids racing our own Welcome)"
        );
        return Ok(ExternalJoinResult::DeferredToWelcome);
    }

    // A recovering device has no local group, so the GroupInfo is what names
    // the suite.
    let verifiable_group_info: VerifiableGroupInfo =
        parse_stored_group_info(&group_info_bytes)?;
    let suite = verifiable_group_info.ciphersuite();

    // Who may have built this lineage (#1161). Both reads fail to the weak
    // answer (unknown roster, unpinned directory), which never refuses a join —
    // see `invariants::lineage_adoption`.
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
        // Already a member by another path, usually our own Welcome (#1220).
        // Checked under the same guard as the build so nothing slips between.
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

    #[cfg(feature = "test-harness")]
    rendezvous::park(rendezvous::Point::ExternalJoinBeforeSubmit).await;

    // Compare-and-swap on `stored_epoch`. A network error or a `LostRace` may
    // still mean our commit landed (lost response, retried submit), so resolve
    // against the canonical log before discarding the branch (#411).
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
                // The caller only forgets on LostRace, so drop the orphaned
                // branch here (branch only, #1220).
                forget_unconfirmed_branch(state, conversation_id, stored_generation).await;
                return Err(e);
            }
        }
    }

    // Record the generation before anything reads the group back, so every load
    // resolves to the lineage we joined. Monotone: a stale join cannot walk the
    // device backwards.
    {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            super::join_branch::clear(db.conn(), conversation_id, stored_generation);
            super::join_branch::mark_fresh_if_first(db.conn(), conversation_id, stored_generation);
            // Only once the log accepted the join: a losing branch never held
            // the lineage (#1312).
            super::join_branch::record_first_join(
                db.conn(),
                conversation_id,
                stored_generation,
                stored_epoch as u64 + 1,
            );
            // A device registered before its user joined, joining only now,
            // missed the gap through no accepted loss; record it so the ingest
            // shows it rather than "sent before you joined" (#1571).
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

    if let Err(e) = publish_group_info(state, conversation_id).await {
        crate::diag!(
            "[mls] external_join_group: publish_group_info failed (non-fatal): {e}"
        );
    }

    // Usually a no-op (our commit advanced past the stored wrap; a key holder
    // re-wraps later), but catches a wrap that already matches (#99).
    crate::commands::pinned_messages::try_adopt_pin_key(state, conversation_id).await;
    crate::commands::file_keys::try_adopt_file_keys(state, conversation_id).await;

    crate::diag!(
        "[mls] external_join_group: {user_id}:{device_id} joined {conversation_id} from epoch {stored_epoch}"
    );

    Ok(ExternalJoinResult::Joined)
}

/// What an external join needs to decide who built the group it adopts
/// (#1161). A mandatory parameter of [`build_external_commit`], so no call
/// site can skip the provenance check.
pub(super) struct LineageProvenance<'a> {
    /// Already through `reconcile::pin_identities`. Unpinned or empty means
    /// every verdict is `Unverifiable`, which adopts.
    pub identities: &'a IdentityDirectory,
    /// `None` only when the roster read failed; `Some` asserts who the members
    /// are, and a signer outside it is refused.
    pub roster: Option<&'a std::collections::HashSet<String>>,
}

impl LineageProvenance<'_> {
    /// `None` when the roster is unknown.
    fn signer_on_roster(&self, user_id: &str) -> Option<bool> {
        self.roster.map(|r| r.contains(user_id))
    }
}

/// The leaf of `tree` that actually signed `vgi`, found by trial verification
/// because `VerifiableGroupInfo::signer()` is `pub(crate)` in openmls. `tree`
/// must come from `RatchetTreeIn::into_verified`, which binds each credential
/// to its signature key.
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
        // `verify` consumes the GroupInfo, forcing a tree clone per leaf.
        vgi.verify_no_out(provider.crypto(), &pk).is_ok()
    })
}

/// Refuse a GroupInfo whose signer is not a certified device of a current
/// roster member (#1161 M6). A GroupId is not a capability, so a hostile DS
/// could otherwise publish its own group under the successor's name; it holds
/// no account identity key, so it cannot mint a certified signer. See
/// "Adopting a lineage is gated twice" in .codesight/wiki/mls.md.
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

    // Every group we build carries the tree, and an external commit needs it
    // anyway; refusing here keeps "signer not visible" from bypassing the gate.
    let Some(tree_ext) = vgi.extensions().ratchet_tree() else {
        return Err(refuse(
            "the published GroupInfo carries no ratchet-tree extension, so there is no way to \
             establish who built it"
            .to_string(),
        ));
    };

    // Binds every leaf's credential to its signature key, GroupId and position.
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
                // Deliberate bias (unreplicated cert rows must not strand a
                // migration), but never silent.
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

/// Build and locally stage an external commit onto `verifiable_group_info`,
/// returning the serialised commit and the GroupInfo at the resulting epoch.
/// `suite` must be the group's own (read off the GroupInfo). The caller owns
/// the local-DB guard.
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

    // The DS-supplied blob must name the lineage we meant to join (#1161 M6);
    // checked before the delete below, so a rejected blob costs nothing.
    let group_id = mls_group_id(conversation_id, generation);
    if verifiable_group_info.group_id() != &group_id {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "external join for {conversation_id} generation {generation}: the published GroupInfo \
             is for a different group ({:?}) — refusing to join it",
            verifiable_group_info.group_id()
        )));
    }

    // …and have been built by a member, not merely named after the lineage.
    check_lineage_provenance(
        provider,
        conversation_id,
        generation,
        suite,
        &group_id,
        &verifiable_group_info,
        provenance,
    )?;

    // Drop our own leftover branch for this lineage, never a confirmed group: it
    // holds the only keys for its epoch (#1220). A caller rebuilding a broken
    // group deletes it first. Predecessor lineages are untouched.
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

    // Marked in the same critical section that stored the branch, so nothing
    // sees it unmarked (#1220).
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

/// Create a fresh group with `creator_user_id`:`device_id` as sole member. The
/// one place a group's suite is decided; everything later inherits it from the
/// stored state. The caller owns the local-DB guard.
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

    // A repair recreates over a broken group with this id. Lineage-scoped: the
    // predecessor keeps serving reads until a migration lands (#454).
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

/// Create a new conversation's group at generation 0 with `creator_user_id` as
/// sole member, then publish its GroupInfo and mint its pin and file keys.
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
            // The one suite this build ships (#669).
            current_suite(),
        )?;
    }

    if let Err(e) = publish_group_info(state, conversation_id).await {
        crate::diag!("[mls] init_mls_group: publish_group_info failed (non-fatal): {e}");
    }

    // Best-effort while the creator is sole member (#99): on failure the first
    // pin mints.
    crate::commands::pinned_messages::mint_pin_key_for_new_group(state, conversation_id).await;
    crate::commands::file_keys::mint_file_keys_for_new_group(state, conversation_id).await;

    Ok(())
}

/// Reload a stored group and its signer, looked up under the group's own
/// suite's scheme.
///
/// Never merges a pending commit: only the commit log decides an epoch (#1079);
/// the allowed merge sites are enforced by `no_new_site_merges_a_pending_commit`.
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

/// Wipe all local MLS state for a group without publishing a commit. Used on
/// leave: MLS forbids committing your own removal, so the others drop this
/// leaf on their next commit.
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

/// [`forget_local_mls_group`] scoped to one generation (#454): a device may
/// hold two lineages for one conversation and must delete only the one meant.
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
        // Leaving a lineage leaves its epoch: keep the file KEK, as every merge
        // does, so a ring still wrapped there stays openable.
        crate::commands::file_keys::keep_epoch_kek(&PollisProvider::new(db.conn()), &group);
        group.delete(&store_only(db.conn()))
            .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("mls delete group: {e}")))?;
    }
    super::join_branch::clear(db.conn(), group_id, generation);
    Ok(())
}

/// Delete this device's unconfirmed external-join branch for the lineage, and
/// nothing else (#1220): by now our own Welcome may have replaced it with a
/// confirmed group. Check and delete share one critical section.
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

/// Bring the local group for `mls_group_id` (already resolved) to the log's
/// head, applying pending commits; a missing, evicted or gapped group recovers
/// by external join, losing only the jumped-over epochs.
///
/// Takes the per-conversation MLS lock; callers already holding it use
/// [`process_pending_commits_locked`].
pub async fn process_pending_commits_inner(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    let _guard = state.mls_group_lock(mls_group_id).await;
    process_pending_commits_locked(state, mls_group_id, user_id).await
}

/// Whether this device is still registered, so a revoked device cannot
/// external-join back in. Fails closed: the DS does not gate commits on
/// revocation, so "couldn't check" must not permit a rejoin; a legitimate
/// device recovers on the next pass.
fn local_device_registered(snap: Option<&pollis_api::reads::ConversationState>) -> bool {
    // A failed request and a DS that couldn't read `user_device` are distinct
    // on the wire, but both are "couldn't check" here.
    snap.and_then(|s| s.device_registered).unwrap_or(false)
}

/// Whether `user_id` is a current member, so a removed-but-not-revoked device
/// cannot external-join back in (fuzzer finding #2). Fails closed: this guards a
/// membership leak. Answered by the DS's own `is_member` predicate.
fn local_user_is_member(snap: Option<&pollis_api::reads::ConversationState>) -> bool {
    snap.and_then(|s| s.is_member).unwrap_or(false)
}

/// Whether an undelivered Welcome for this device targets the group; if so,
/// external-join recovery stands down rather than race it. A failed apply
/// still marks the row delivered, so an orphaned Welcome hands off to
/// external join on a later pass.
///
/// Fails open, the opposite bias to [`local_user_is_member`] on the same
/// snapshot; don't merge their error paths.
fn has_pending_welcome(snap: Option<&pollis_api::reads::ConversationState>) -> bool {
    snap.map(|s| s.welcome_pending).unwrap_or(false)
}

/// Registered AND a current member, via the Kani-proved
/// `invariants::may_rejoin`. Logs which gate refused, so a skipped recovery is
/// never silent.
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

/// Body of [`process_pending_commits_inner`]; the caller holds
/// `state.mls_group_lock`.
pub(crate) async fn process_pending_commits_locked(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
) -> crate::error::Result<()> {
    process_pending_commits_locked_impl(state, mls_group_id, user_id, None, None).await
}

/// Like [`process_pending_commits_inner`], but calls `on_epoch(conn,
/// generation, epoch)` at the starting epoch and after each applied commit, so
/// the caller decrypts each epoch's envelopes before `max_past_epochs = 0`
/// destroys its keys (#418). It does not fire for epochs a recovery jump skips.
///
/// `bound` is the log head read before the envelope fetch; see [`ReplayBound`].
/// `None` replays to head.
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
/// The pass reads the log head before fetching envelopes; applying a commit
/// accepted after that fetch could destroy the keys for an envelope the fetch
/// missed. Held-back commits apply next pass. Lexicographic on
/// `(generation, epoch)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplayBound {
    pub generation: i64,
    /// The head epoch in `generation`; a commit row at or beyond it is not
    /// applied.
    pub epoch: u64,
}

impl ReplayBound {
    pub fn admits(self, generation: i64, epoch: u64) -> bool {
        (generation, epoch) < (self.generation, self.epoch)
    }

    /// The log had already opened a successor lineage when the bound was read,
    /// so adopting one this pass cannot skip unfetched envelopes.
    pub fn allows_generation_beyond(self, generation: i64) -> bool {
        self.generation > generation
    }
}

/// Most generations one pass will hop across, so the successor's commits are
/// replayed with the hook still live (#454). Bounded because the hop is driven
/// by server-reported state; a migration happens once per suite era.
const MAX_GENERATION_HOPS: usize = 4;

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

/// One replay pass over a single lineage. Returns whether it adopted a newer
/// generation, whose commits the caller must then replay.
// `'h` is named so the trait object may outlive the fresh `&mut` reborrow the
// caller hands in on every hop.
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
            // No local group: Welcome vs external-join recovery, arbitrated by
            // the Kani-proved `invariants::join_recovery` (I6). All three inputs
            // must come from ONE snapshot, and each keeps its own failure bias —
            // see "Welcome-first arbitration" in .codesight/wiki/mls.md.
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
            let may_rejoin = may_rejoin_via_external_join(snap, mls_group_id, user_id);
            // Without GroupInfo there is nothing to join onto (#832).
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
                        // A Welcome is the only way in: poll for it now rather
                        // than wait for the next sweep (#832).
                        crate::diag!(
                            "[mls] process_pending_commits: no local group for {mls_group_id} and no \
                             published GroupInfo — awaiting a Welcome (external-join would have \
                             nothing to join onto); polling welcomes now"
                        );
                        // `Box::pin`: the welcome poll re-enters this path, so the
                        // future is recursive.
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
                    // `may_rejoin_via_external_join` already logged why.
                }
            }
            return Ok(false);
        }
    };

    // Decrypt the current epoch's envelopes before any commit leaves it (#418).
    if let Some(hook) = on_epoch.as_deref_mut() {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            hook(db.conn(), generation, initial_epoch);
        }
    }

    // Pending commits of this lineage only, plus cross-signing rows for every
    // user the batch's add hints name (#987). The DS reads head and batch in one
    // transaction and checks contiguity, which is what makes the gap handling
    // below safe to act on.
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
        // Removed: neither an error nor a recovery trigger.
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

    // A short batch is a retention prune (#539) or, rarer, real damage
    // (`hole`); both recover the same way, but only damage is a bug, so log them
    // apart.
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

    // Captured before the snapshot is consumed; the gap gate needs them.
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

    // DS-supplied rows are inputs only; pin them as the committing side does
    // (#1161 M3). An unpinnable directory degrades to an empty one (every leaf
    // `Unverifiable`), never to a DS-authored one.
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

    // Uncertified grafted leaves are flagged and evicted afterwards, but the
    // commit is still merged: it is canonical, and refusing it would strand this
    // device (#1161 H4).
    let mut flagged: Vec<(AddedLeaf, i64, String, FlagKind)> = Vec::new();
    let mut current_epoch = initial_epoch;
    let mut any_applied = false;
    // Recovery is driven by an explicit reason, never inferred from a group's
    // absence: a group that exists yet cannot advance must recover too (#680).
    let mut recover: Option<RecoverReason> = None;
    for commit in pending {
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

        // I1, Kani-proved never to `Apply` across a gap; modelled by
        // `specs/tla/CommitLog.tla` (NoForeignAdopt, I2).
        if super::invariants::classify(current_epoch, Some(commit.epoch as u64))
            != super::invariants::ReplayStep::Apply
        {
            // Rebuilding deletes this lineage's keys, so only a declared prune or
            // hole covering our epoch licenses it (#1161 M6).
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
                    // A canonical commit on top of the stored group confirms a
                    // branch whose acceptance a crash left unrecorded (#1220).
                    super::join_branch::clear(db.conn(), mls_group_id, generation);
                    // A StagedCommit cannot be re-processed later, so there is
                    // no defer: verify after the merge, flag, evict.
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
                    // Envelopes and watermarks are bucketed on `current_epoch`,
                    // so check it against the merged group rather than assume
                    // `+= 1`: one divergence mis-buckets every later envelope
                    // (#1161 C2).
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

        // Never run the hook against a counter just proved wrong.
        if recover.is_some() {
            break;
        }

        if let Some(epoch_after) = applied {
            current_epoch = epoch_after;
            any_applied = true;
            // #418: decrypt this epoch now; the next commit discards its keys.
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

    // A commit still pending after the replay never landed (the loop would have
    // adopted it), so clear it before anything merges it into a phantom epoch
    // (#411). Safe even if wrong: a later pass re-adopts it from the log.
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

    // A retired lineage never moves again, so hop to its successor (#454) —
    // after the replay has drained it. Adoption deletes the predecessor's keys,
    // so only on an ingesting pass (one with a hook, which has fetched the
    // envelopes) and only within the bound (#1041).
    let bound_allows_hop = bound.is_none_or(|b| b.allows_generation_beyond(generation));
    if on_epoch.is_some() && bound_allows_hop {
        if let Some(new_generation) =
            maybe_advance_generation(state, mls_group_id, user_id, generation).await
        {
            crate::diag!(
                "[mls] process_pending_commits: {mls_group_id} advanced generation \
                 {generation} -> {new_generation} — replaying the new lineage"
            );
            // A new group means a new exporter: voice, pin and file keys re-derive.
            crate::commands::voice_e2ee::on_mls_epoch_changed(state, mls_group_id).await;
            crate::commands::pinned_messages::maybe_rewrap_pin_key(state, mls_group_id).await;
            crate::commands::file_keys::maybe_rewrap_file_keys(state, mls_group_id).await;
            return Ok(true);
        }
    }

    // Also heals a dropped past publish, not just this pass's advance.
    ensure_group_info_published(state, mls_group_id).await;

    // Pin/file-key durability backstop (#99): re-wrap if the epoch moved past
    // the last published wrap, so a keyless device can unwrap.
    crate::commands::pinned_messages::maybe_rewrap_pin_key(state, mls_group_id).await;
    crate::commands::file_keys::maybe_rewrap_file_keys(state, mls_group_id).await;

    if any_applied {
        crate::commands::voice_e2ee::on_mls_epoch_changed(state, mls_group_id).await;
    }

    // Rebuild if the group is gone (e.g. eviction) or a commit asked to
    // recover; keyed on the reason so a wedge is never mistaken for caught up
    // (#680).
    let group_exists = {
        let guard = state.local_db.lock().await;
        guard.as_ref().is_some_and(|db| {
            load_stored_group_at(db.conn(), mls_group_id, generation).is_some()
        })
    };
    if !group_exists || recover.is_some() {
        // Fresh snapshot: the replay may just have applied our own removal,
        // which the gate must see before rebuilding.
        let gates = super::ds_reads::conversation_snapshot(
            state,
            super::ds_reads::state_query(mls_group_id, Some(generation)),
        )
        .await
        .ok();
        let gates = gates.as_ref().filter(|s| s.authorized);
        if may_rejoin_via_external_join(gates, mls_group_id, user_id) {
            // External join never replaces a confirmed group (#1220), so drop
            // it explicitly; only the epoch-desync path leaves one in place.
            if group_exists {
                let _ = forget_local_mls_group_at(state, mls_group_id, generation).await;
            }
            crate::diag!("[mls] process_pending_commits: group {mls_group_id} was deleted during processing — external-joining to recover");
            if let Err(e) = external_join_group_inner(state, mls_group_id, user_id).await {
                crate::diag!("[mls] process_pending_commits: recovery external-join failed for {mls_group_id}: {e}");
            }
        }
    }

    // Retention signal (#539, I4); best-effort, a missed report only keeps the
    // prune floor conservative.
    report_applied_epoch(state, mls_group_id, user_id).await;

    Ok(false)
}

/// Adopt a newer generation if one exists, returning it: a successor already
/// stored from its Welcome, else an external join onto a published head above
/// ours (behind the usual rejoin gates).
async fn maybe_advance_generation(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
    generation: i64,
) -> Option<i64> {
    let stored_successor = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref()?;
        (1..=MAX_GENERATION_HOPS as i64)
            .map(|hop| generation + hop)
            .find(|g| load_stored_group_at(db.conn(), mls_group_id, *g).is_some())
    };
    if let Some(successor) = stored_successor {
        // A migration that lost its CAS leaves a group the log never opened, so
        // confirm against the published head. No false negatives: the DS writes
        // the opening commit, GroupInfo and Welcomes in one transaction.
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

    // One snapshot answers both "does a successor exist" and "may we join it",
    // so a removal cannot land between them.
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
    // On success it records the new lineage itself; re-read rather than assume.
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
        let _ = forget_local_mls_group_at(state, mls_group_id, generation).await;
        Some(adopted)
    } else {
        None
    }
}

/// Move onto `successor`: record the lineage, re-arm the post-join self-update
/// and drop the predecessor. The caller holds the MLS lock and has drained the
/// predecessor.
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
        // Our leaf entered by Welcome is unmerged until we commit once (#666),
        // so the rotation clock restarts with the lineage.
        super::self_update::clear_self_update(db.conn(), conversation_id);
    }
    // After the lineage row moves: a crash in between leaves a harmless orphan,
    // not a device with no group.
    let _ = forget_local_mls_group_at(state, conversation_id, predecessor).await;
}

/// Why the local group could not advance from a commit. Every variant recovers
/// by delete-and-rejoin; carrying it keeps a wedge distinct from "caught up"
/// (#680).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RecoverReason {
    GroupMissing,
    GroupLoadFailed,
    MalformedCommit,
    NotAProtocolMessage,
    /// An application message or bare proposal in a commit-log slot (#1161 C2):
    /// damage, never a free advance.
    NotACommit,
    WrongGroup,
    /// The merge left the group at an epoch other than `commit.epoch + 1`
    /// (#1161 C2).
    EpochDesync,
    MergeFailed,
    Evicted,
    /// The commit is at our epoch yet won't stage: our tree diverged (#411).
    ForkedTree,
    OwnCommitMergeFailed,
    /// An openmls error not specifically classified; still recovers, never
    /// wedges (#680).
    Unclassified,
}

#[derive(Debug)]
pub(super) enum CommitApply {
    /// `adds` are the leaves the commit grafted (see [`grafted_leaves`]), for
    /// the caller to verify; `epoch_after` is read back off the merged group.
    Applied {
        adds: Vec<AddedLeaf>,
        epoch_after: u64,
    },
    /// Must be rebuilt; any loaded local group has already been deleted.
    Recover(RecoverReason),
}

/// Map an openmls `process_message` error to its recovery. Matches error enums,
/// never `Display` strings, which silently reclassify when openmls rewords one
/// (#680).
pub(super) fn classify_process_message_error<S>(
    e: &ProcessMessageError<S>,
) -> RecoverReason {
    use ProcessMessageError as E;
    match e {
        // Rejoin only if still a member; `may_rejoin` enforces that downstream.
        E::GroupStateError(MlsGroupStateError::UseAfterEviction) => RecoverReason::Evicted,
        // Merging our stale pending commit would diverge us further.
        E::InvalidCommit(StageCommitError::OwnCommitMismatch) => RecoverReason::ForkedTree,
        E::InvalidCommit(_) | E::ValidationError(_) => RecoverReason::ForkedTree,
        _ => RecoverReason::Unclassified,
    }
}

/// Apply one commit from the log to the local group. `provider` must serve the
/// group's suite; failures come back as [`CommitApply::Recover`], never `Err`,
/// so the replay loop is never aborted. The caller owns the local-DB guard.
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

    // A slot may hold a Commit for this group and nothing else, checked before
    // openmls sees it (#1161 C2/H5). Both fields are authenticated (AAD or
    // signed TBS), but the DS doesn't parse commit bytes, so any member can post
    // an application message here.
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
            // Our own commit echoed back (#411): openmls returns it as content,
            // not an error, and can't stage it (an author can't decrypt its own
            // PrivateMessage), so adopt it by merging our pending commit. Only
            // safe because the content-type gate above proved it is a Commit:
            // `OwnPrivateMessage` fires for any PrivateMessage from our leaf
            // (#1161 H5).
            ProcessedMessageContent::OwnPendingCommit
            | ProcessedMessageContent::OwnPrivateMessage => {
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
            // Unreachable given the content-type gate, but still a `Recover`,
            // never a free advance (#1161 C2).
            _ => {
                crate::diag!(
                    "[mls] process_pending_commits: unexpected non-commit content at epoch {commit_epoch} for {mls_group_id} — recovering via external-join"
                );
                let _ = group.delete(provider.storage());
                return CommitApply::Recover(RecoverReason::NotACommit);
            }
        },
        Err(e) => {
            // The rejoin lands past this commit, so it can't re-fail on it.
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

/// `(position, user, device, signature key)`. All four: position alone misses a
/// leaf swapped into an occupied index, identity alone a duplicate claim.
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

/// Every leaf a just-merged commit grafted: in the tree now, absent from
/// `before`. A tree diff rather than Add proposals, so an external join's
/// UpdatePath leaf is verified too (#1161 H4). An ordinary rotation changes
/// only the encryption key, so it doesn't read as grafted.
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

/// Why a replayed add was flagged, and so whether the user is told (#1127).
///
/// Both kinds get an eviction reconcile (self-correcting), but only
/// `Uncertified` is evidence worth an alarm. `Unverifiable` is usually a cert
/// row not yet replicated or an incomplete add hint; alarming on it would make
/// every teammate's enrollment look like a compromise and let a committer
/// manufacture alarms by omitting the hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlagKind {
    Uncertified,
    Unverifiable,
}

impl FlagKind {
    /// Whether this is worth a user-facing `security_event`.
    fn alarms(self) -> bool {
        matches!(self, FlagKind::Uncertified)
    }
}

/// `None` when the leaf is fine, else the reason and its [`FlagKind`].
fn flag_for(verdict: LeafVerdict) -> Option<(String, FlagKind)> {
    match verdict {
        LeafVerdict::Certified => None,
        LeafVerdict::Unverifiable(why) => Some((why.to_string(), FlagKind::Unverifiable)),
        LeafVerdict::Uncertified(why) => Some((why, FlagKind::Uncertified)),
    }
}

/// Handle flagged leaves of an already-merged commit: log a
/// `uncertified_mls_leaf` security event per alarming leaf, then kick a
/// detached reconcile that evicts any positively uncertified leaf with a
/// normal remove commit.
///
/// Residual: a rogue leaf holds the key schedule until one honest member's
/// eviction lands; nothing sooner fits an append-only log.
async fn report_uncertified_leaves(
    state: &Arc<AppState>,
    mls_group_id: &str,
    user_id: &str,
    flagged: Vec<(AddedLeaf, i64, String, FlagKind)>,
) {
    for (leaf, epoch, reason, kind) in &flagged {
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
            // The DS's no-auth fallback; with auth on it must equal the signer.
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

/// Boxed and type-erased on purpose: reconcile → catch-up → replay → this is a
/// cycle the compiler cannot prove `Send` through by inference.
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

/// Report this device's local `(generation, epoch)` as its retention high-water
/// (#539). Detached so a slow DS never holds the MLS lock; best-effort.
async fn report_applied_epoch(state: &Arc<AppState>, mls_group_id: &str, user_id: &str) {
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

/// Command entry point: resolve the MLS group, then run the interleaved
/// catch-up. Never a bare commit replay, which would strand messages at skipped
/// epochs (`max_past_epochs = 0`).
pub async fn process_pending_commits(
    state: &Arc<AppState>,
    conversation_id: String,
    user_id: String,
) -> crate::error::Result<()> {
    let mls_group_id = super::ds_reads::resolve_mls_group(state, &conversation_id).await?;
    crate::commands::messages::catch_up_mls_group_interleaved(state, &mls_group_id, &user_id).await
}

/// Whether this device holds the group. Use this, not
/// `try_mls_encrypt(..).is_none()`, which also means "a commit is staged"
/// (#1079).
pub fn has_local_group(conn: &rusqlite::Connection, conversation_id: &str) -> bool {
    load_stored_group(conn, conversation_id).is_some()
}

/// Encrypt with the conversation's group. `None`, unlogged, when there is no
/// local group or a commit is staged: sealing then would pick an epoch the log
/// hasn't decided (#1079). Only the lock-free callers (receipts, edit/delete
/// probes) can see a staged commit, and they skip.
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

/// The `(generation, epoch)` an envelope was sealed at, parsed without
/// decrypting; `None` for non-MLS bytes (e.g. an empty `delete` tombstone).
/// The ingest interleave routes envelopes by this (#418); the generation comes
/// from the authenticated `GroupId`, not the server (#454).
pub fn envelope_lineage(ciphertext: &[u8]) -> Option<(i64, u64)> {
    let mut reader: &[u8] = ciphertext;
    let msg_in = MlsMessageIn::tls_deserialize(&mut reader).ok()?;
    let protocol_msg = msg_in.try_into_protocol_message().ok()?;
    let raw_group_id = String::from_utf8_lossy(protocol_msg.group_id().as_slice()).into_owned();
    let (_, generation) = super::generation::split_mls_group_id(&raw_group_id);
    Some((generation, protocol_msg.epoch().as_u64()))
}

/// Decrypt an envelope, returning `(plaintext, sender_user_id)`.
///
/// The sender is the MLS-authenticated credential, never the server-writable
/// `sender_id` column; returning both together keeps callers from trusting the
/// column (sealed sender, `docs/metadata-minimization-design.md` §2).
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

/// Which step of opening one envelope failed, for `[loss]` diagnostics (#1520).
/// Never carries anything derived from plaintext or a key.
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

/// Why [`MlsDecryptor::decrypt`] could not open an envelope (#1520). The
/// sender's leaf and ratchet generation are encrypted and openmls doesn't
/// surface them, so only the clear-header epochs are here.
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

/// Map an application-message `process_message` error to the step that failed
/// (#1520). Only the commit classifier's eviction case carries over; enums,
/// never `Display` strings (#680).
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

/// A group loaded once for a run of envelopes sealed at its current epoch
/// (#875). Safe because openmls persists the only state a decrypt mutates (the
/// secret tree) inside `process_message`. Must not outlive its epoch: a commit
/// changes the group out from under it.
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

    /// The `(generation, epoch)` of every envelope this decryptor can open; a
    /// staged own commit does not move it until merged.
    pub fn lineage(&self) -> (i64, u64) {
        let raw = String::from_utf8_lossy(self.group.group_id().as_slice()).into_owned();
        let (_, generation) = super::generation::split_mls_group_id(&raw);
        (generation, self.group.epoch().as_u64())
    }

    /// Decrypt one envelope; same contract as [`try_mls_decrypt`].
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
        // Fail fast so a foreign-lineage envelope isn't misreported as a
        // decrypt failure by `process_message`.
        if protocol_msg.group_id() != self.group.group_id() {
            let own_raw = String::from_utf8_lossy(self.group.group_id().as_slice()).into_owned();
            let (own_base, _) = super::generation::split_mls_group_id(&own_raw);
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

    // The ciphersuite bytes an envelope reader sees where a wire format belongs.
    const CS_PQ_CODE_POINT: [u8; 2] = [0x00, 0x52];

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

    #[test]
    fn rejects_empty_payload() {
        assert!(parse_stored_group_info(&[]).is_err());
    }

    // Trying the envelope form first can never swallow a bare GroupInfo.
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
        // Including epoch 0: a sole-member creator whose create-time publish
        // was dropped.
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

    /// A successor restarts at epoch 0, below the retired lineage's last (#454).
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

    #[test]
    fn an_uncertified_leaf_alarms() {
        let (reason, kind) =
            flag_for(LeafVerdict::Uncertified("leaf key is not the certified key".into()))
                .expect("must be flagged");
        assert_eq!(kind, FlagKind::Uncertified);
        assert!(kind.alarms(), "a positively uncertified leaf must reach the user");
        assert!(reason.contains("certified key"), "the reason is carried through");
    }

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

    /// Forces a future third verdict to decide deliberately.
    #[test]
    fn only_uncertified_alarms() {
        assert!(FlagKind::Uncertified.alarms());
        assert!(!FlagKind::Unverifiable.alarms());
    }
}
