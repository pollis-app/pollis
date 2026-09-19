//! Inbound MLS `Welcome` handling.
//!
//! Welcomes are how new members are added to an existing MLS group. The
//! committer generates a Welcome blob targeting each invitee's KeyPackage;
//! invitees process it locally to materialise the group state.

use openmls::prelude::*;
use openmls_traits::OpenMlsProvider;

use std::sync::Arc;
use tls_codec::Deserialize as TlsDeserialize;

use crate::error::Result;
use crate::state::AppState;

use super::key_packages::replenish_key_packages;
use super::provider::{welcome_ciphersuite, MlsProvider, PollisProvider};

/// Internal: deserialise a TLS-encoded `MlsMessageOut` (welcome wire format)
/// and persist the resulting MLS group state locally.
///
/// The bytes stored in `mls_welcome.welcome_data` are TLS-serialised
/// `MlsMessageOut`.  We deserialise to `MlsMessageIn`, extract the inner
/// `Welcome` via `MlsMessageIn::extract()`, then call
/// `StagedWelcome::new_from_welcome`.
///
/// Returns the `(conversation_id, generation)` we just joined, so the caller can
/// follow up on the join — notably with the post-join self-update that merges
/// this brand-new (and therefore unmerged) leaf into the tree (issue #666).
///
/// The generation is decoded from the GroupId rather than assumed to be the
/// conversation's current one: a Welcome into a migrated group (#454 P4) admits
/// us to the *successor* lineage, whose GroupId carries a `#gN` suffix. Returning
/// the bare conversation id keeps every caller — catch-up, self-update, the
/// dedupe below — addressing the conversation, not the lineage.
pub async fn apply_welcome(
    state: &Arc<AppState>,
    welcome_bytes: &[u8],
    expected_conversation_id: Option<&str>,
) -> Result<(String, i64)> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!("Not signed in"))
    })?;

    let mut reader: &[u8] = welcome_bytes;
    let msg_in = MlsMessageIn::tls_deserialize(&mut reader)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("welcome msg deserialize: {e}")))?;

    let welcome = match msg_in.extract() {
        MlsMessageBodyIn::Welcome(w) => w,
        _ => return Err(crate::error::Error::Other(anyhow::anyhow!(
            "expected Welcome message in mls_welcome"
        ))),
    };

    // The joiner has no local group yet, so the suite comes off the Welcome
    // itself. An unreadable or unknown code point is a hard failure, never a
    // classic fallback — joining under the wrong suite is not a downgrade, it
    // is a group this device can never read.
    welcome_ciphersuite(&welcome).ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!(
            "welcome names a ciphersuite this build does not implement"
        ))
    })?;

    let provider = PollisProvider::new(db.conn());
    let raw_group_id = join_from_welcome(&provider, welcome, expected_conversation_id)?;
    Ok(super::generation::split_mls_group_id(&raw_group_id))
}

/// Materialise group state from an already-suite-matched `Welcome`, returning
/// the joined group's RAW GroupId string — the conversation id for generation 0,
/// `"{conversation_id}#g{generation}"` for a successor lineage (see
/// [`super::generation`]).
///
/// Split into ProcessedWelcome → delete stale group → stage → into_group:
/// openmls checks for duplicate GroupIds inside `into_staged_welcome`, so any
/// existing group must be deleted *before* that call. Note the delete is scoped
/// to the *incoming* GroupId, so a Welcome into a successor lineage leaves the
/// predecessor group intact — under `max_past_epochs = 0` that group holds the
/// only keys that can still decrypt envelopes sealed before the migration, and
/// it is dropped only once the predecessor has been drained
/// (`adopt_generation_locked`).
///
/// That delete is the dangerous half (#1161 C1). The GroupId comes out of the
/// Welcome blob — attacker-supplied, since a Welcome is just a row anyone who can
/// write one can address at us — so it names which of THIS device's groups is
/// destroyed. Two guards, in order:
///
/// * `expected`, when the caller knows which conversation the Welcome was filed
///   under, must match the GroupId's conversation. A Welcome that disagrees with
///   its own row is refused before anything is touched.
/// * A local group at an epoch **at or above** the incoming Welcome's is never
///   replaced. A Welcome that genuinely re-admits us is produced by a commit
///   LATER than anything we hold, so it always arrives above our epoch; one at or
///   below is a replay of an old Welcome, or a fabrication, and adopting it would
///   trade a working group for an older (or foreign) one. Refusing costs nothing:
///   the group we keep is the one the commit log corroborates, and the next
///   catch-up advances it.
pub(super) fn join_from_welcome<C>(
    provider: &MlsProvider<'_, C>,
    welcome: Welcome,
    expected: Option<&str>,
) -> Result<String>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let join_config = MlsGroupJoinConfig::builder()
        .use_ratchet_tree_extension(true)
        .build();

    let processed = ProcessedWelcome::new_from_welcome(provider, &join_config, welcome)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("process welcome: {e}")))?;

    let new_group_id = processed.unverified_group_info().group_id().clone();
    let welcome_epoch = processed.unverified_group_info().epoch().as_u64();

    let raw_group_id = String::from_utf8(new_group_id.as_slice().to_vec()).map_err(|_| {
        crate::error::Error::Other(anyhow::anyhow!("welcome carries a non-UTF-8 group id"))
    })?;
    let (conversation_id, _generation) = super::generation::split_mls_group_id(&raw_group_id);
    if let Some(expected) = expected {
        if conversation_id != expected {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "welcome filed under {expected} carries a group id for {conversation_id} — refusing"
            )));
        }
    }

    if let Ok(Some(mut old_group)) = MlsGroup::load(provider.storage(), &new_group_id) {
        let local_epoch = old_group.epoch().as_u64();
        if local_epoch >= welcome_epoch {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "welcome for {raw_group_id} is at epoch {welcome_epoch} but this device already \
                 holds that group at epoch {local_epoch} — refusing to replace a group the commit \
                 log has already carried past it"
            )));
        }
        eprintln!(
            "[mls] apply_welcome: replacing group {raw_group_id} at epoch {local_epoch} with the \
             Welcome's epoch {welcome_epoch}"
        );
        let _ = old_group.delete(provider.storage());
    }

    let staged = processed.into_staged_welcome(provider, None)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("stage welcome: {e}")))?;

    staged.into_group(provider)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("into group: {e}")))?;

    Ok(raw_group_id)
}

/// Poll the remote `mls_welcome` table for undelivered Welcome messages
/// addressed to `user_id`.  Each one is applied locally and then marked
/// `delivered = 1` so it is not processed again.
///
/// Called on startup and from `poll_pending_messages`.
pub async fn poll_mls_welcomes_inner(state: &Arc<AppState>, user_id: &str, device_id: &str) -> Result<()> {
    // The drain is a DS read since #987 (`POST /v1/welcomes/fetch`) — the read
    // counterpart of the `delivered = 1` ack below, which has routed through the
    // DS since the W5 seam. The DS scopes it to the VERIFIED signing device and
    // includes the legacy device-agnostic rows (`recipient_device_id IS NULL`)
    // from before multi-device, so the selection is unchanged; what is gone is
    // the client-side credential that used to run it.
    let items: Vec<(String, Vec<u8>)> =
        super::ds_reads::fetch_welcomes(state, user_id, device_id).await?;

    let had_welcomes = !items.is_empty();
    // IDs to mark delivered. We ack even apply failures — the private key for a
    // failed Welcome was likely orphaned by a DB wipe and will never succeed;
    // the repair mechanism generates a fresh Welcome. (Same semantics as before.)
    let mut processed_ids: Vec<String> = Vec::new();
    // Conversations this pass actually joined, with the LINEAGE each Welcome
    // admitted us to — the post-join self-update targets (issue #666) and the
    // generation-adoption input (#454 P4). Deduplicated on the conversation
    // because a repaired join can arrive as two Welcomes for the same group, and
    // rotating twice buys nothing; the highest generation wins, since a Welcome
    // into a newer lineage supersedes one into an older.
    let mut joined: Vec<(String, i64)> = Vec::new();
    for (id, bytes) in items {
        // `None`: the drain (`POST /v1/welcomes/fetch`) does not report the
        // `conversation_id` the row was filed under, so there is nothing yet to
        // hold the blob's own GroupId against. The DS half of #1161 C1 adds it;
        // `join_from_welcome` already enforces it the moment it arrives. Until
        // then the corroboration below is what stands in for it.
        match apply_welcome(state, &bytes, None).await {
            Ok((conversation_id, generation)) => {
                eprintln!("[mls] poll_mls_welcomes: applied welcome {id}");
                if welcome_is_corroborated(state, &conversation_id, generation).await {
                    match joined.iter_mut().find(|(c, _)| c == &conversation_id) {
                        Some(entry) => entry.1 = entry.1.max(generation),
                        None => joined.push((conversation_id, generation)),
                    }
                }
            }
            Err(e) => {
                eprintln!("[mls] poll_mls_welcomes: failed to apply welcome {id}: {e}");
            }
        }
        processed_ids.push(id);
    }

    // W5 seam: mark delivered=1 through the Delivery Service (sole writer).
    if !processed_ids.is_empty() {
        let body = pollis_api::writes::AckBody {
            welcome_ids: processed_ids,
            // The DS's no-auth fallback for the acting user
            // (`pollis_delivery::writes::resolve_actor`): auth on → the signed
            // user and this must EQUAL it; auth off → this IS the actor, and a
            // body without it is refused outright. Sending it never widens what
            // the caller may do.
            user_id: Some(user_id.to_string()),
        };
        match super::ds_client::ds_post(state, &body).await {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => {
                let s = resp.status();
                let txt = resp.text().await.unwrap_or_default();
                eprintln!("[mls] poll_mls_welcomes: DS ack {s}: {txt}");
            }
            Err(e) => {
                eprintln!("[mls] poll_mls_welcomes: DS ack failed: {e}");
            }
        }
    }

    // Each processed welcome consumed a KP — top back up to TARGET.
    if had_welcomes {
        if let Err(e) = replenish_key_packages(state, user_id, device_id).await {
            eprintln!("[mls] KP replenishment failed (non-fatal): {e}");
        }
    }

    adopt_undrainable_joins(state, &joined).await;

    // Adopt each joined conversation's pin key (#99) BEFORE the post-join
    // self-update below advances the epoch. Right now this device sits at
    // exactly the epoch the adder's re-wrap targeted, so the unwrap succeeds;
    // one commit later the window is gone and the device would have to wait
    // for another member's re-wrap. Once adopted, this device's own epoch
    // hooks keep the server wrap current — including for the self-update it
    // is about to make.
    for (conversation_id, _) in &joined {
        crate::commands::pinned_messages::try_adopt_pin_key(state, conversation_id).await;
    }

    self_update_joined_groups(state, user_id, &joined).await;

    Ok(())
}

/// Does the conversation a just-applied Welcome claims actually list us as a
/// member? If not, drop the group it materialised (#1161 C1).
///
/// A Welcome is a row, and the GroupId inside it is chosen by whoever wrote that
/// row — so absent the DS-side authorization half, anyone able to write one can
/// point this device at a conversation of their choosing and have it stand up a
/// group under that conversation's name, which every later send then seals for.
/// `join_from_welcome` refuses to *replace* a group we already hold; this is the
/// other case, where we held nothing and would simply have adopted the blob.
///
/// The second check is on the LINEAGE. `join_from_welcome` refuses a Welcome at
/// or below our current epoch, so the only blob that can still displace a real
/// group is one claiming an epoch ABOVE it — and that claim is checkable: the
/// commit that produced a genuine Welcome is written to the log in the same DS
/// transaction as the Welcome itself, so the log head for that lineage is never
/// behind the epoch the Welcome lands us at. A head below it means the commit
/// does not exist and the blob was fabricated. (The check has to run after the
/// join rather than before: the GroupId lives inside the encrypted GroupInfo, so
/// nothing about the blob is readable until the KeyPackage has opened it.)
///
/// Fails OPEN on anything short of a definite "no" — an unreachable DS, or a
/// snapshot that does not answer the membership question, leaves the join alone.
/// A false negative would be self-healing anyway (the no-group recovery path
/// external-joins behind the same membership gate), but a legitimate join must
/// not be spent on a network blip.
async fn welcome_is_corroborated(
    state: &Arc<AppState>,
    conversation_id: &str,
    generation: i64,
) -> bool {
    let snap = match super::ds_reads::conversation_snapshot(
        state,
        super::ds_reads::state_query(conversation_id, Some(generation)),
    )
    .await
    {
        Ok(snap) => snap,
        Err(e) => {
            eprintln!(
                "[mls] poll_mls_welcomes: cannot corroborate the welcome for {conversation_id} \
                 ({e}) — keeping the join"
            );
            return true;
        }
    };
    let joined_epoch = {
        let guard = state.local_db.lock().await;
        guard.as_ref().and_then(|db| {
            super::provider::load_stored_group_at(db.conn(), conversation_id, generation)
                .map(|g| g.epoch().as_u64())
        })
    };
    let epoch_is_in_the_log = match joined_epoch {
        Some(epoch) => u64::try_from(snap.head).is_ok_and(|head| head >= epoch),
        // Nothing stored — nothing to corroborate or discard.
        None => true,
    };
    if snap.authorized && snap.is_member != Some(false) && epoch_is_in_the_log {
        return true;
    }
    eprintln!(
        "[mls] poll_mls_welcomes: a welcome admitted this device to {conversation_id} generation \
         {generation} at epoch {joined_epoch:?}, but the conversation does not corroborate it \
         (authorized={}, is_member={:?}, log head={}) — discarding the group it created",
        snap.authorized, snap.is_member, snap.head
    );
    if let Err(e) =
        super::group_state::forget_local_mls_group_at(state, conversation_id, generation).await
    {
        eprintln!(
            "[mls] poll_mls_welcomes: could not discard the uncorroborated group for \
             {conversation_id}: {e}"
        );
    }
    false
}

/// Adopt a newly-joined lineage in the one case the catch-up backstop cannot
/// reach: we hold **no group at all** on our recorded generation (#454 P4).
///
/// The normal path is the backstop in `process_pending_commits`: drain the
/// predecessor to its head, then hop to the successor. That ordering is
/// mandatory whenever a predecessor group exists, because adoption deletes its
/// key material and under `max_past_epochs = 0` those are the only keys that can
/// still open envelopes sealed before the migration.
///
/// But a member added to an ALREADY-migrated conversation never had the
/// predecessor. Its recorded generation is 0, `process_pending_commits` finds no
/// group there and stops before it can ever hop, and the device would sit
/// permanently pointed at a lineage it does not have. Nothing can be lost by
/// adopting immediately — there is no group to drain — so it adopts here.
///
/// Deliberately NOT capped like the self-update burst below: a rotation deferred
/// to the next sweep costs efficiency, whereas a deferred adoption leaves the
/// device unable to read the conversation at all, and no later sweep would fix
/// it for exactly the reason above.
async fn adopt_undrainable_joins(state: &Arc<AppState>, joined: &[(String, i64)]) {
    for (conversation_id, generation) in joined {
        let predecessor = {
            let guard = state.local_db.lock().await;
            match guard.as_ref() {
                Some(db) => {
                    let local = super::generation::local_generation(db.conn(), conversation_id);
                    let drainable =
                        super::provider::load_stored_group_at(db.conn(), conversation_id, local)
                            .is_some();
                    if *generation > local && !drainable {
                        Some(local)
                    } else {
                        None
                    }
                }
                None => None,
            }
        };
        if let Some(predecessor) = predecessor {
            eprintln!(
                "[mls] poll_mls_welcomes: {conversation_id} joined at generation {generation} \
                 with no group on {predecessor} — adopting immediately"
            );
            super::group_state::adopt_generation_locked(
                state,
                conversation_id,
                predecessor,
                *generation,
            )
            .await;
        }
    }
}

/// Post-join self-update (issue #666): rotate our own leaf in each group we just
/// joined, so it stops being an *unmerged* leaf.
///
/// A member added by someone else sits unmerged in every ancestor node on the
/// adder's direct path until it commits for itself. Each unmerged leaf costs one
/// extra HPKE ciphertext in every subsequent commit's copath resolution — which
/// is how a group whose commits should grow with log(N) ends up growing with N.
/// On #454's post-quantum suite each of those ciphertexts is ~1,270 B, so this
/// one commit per join is what keeps large hybrid groups viable at all.
///
/// Best-effort and strictly after the DS ack + KP replenish: a failure here
/// costs efficiency (and delays PCS to the next periodic rotation), never
/// correctness, so it must not be able to strand a Welcome as undelivered.
async fn self_update_joined_groups(state: &Arc<AppState>, user_id: &str, joined: &[(String, i64)]) {
    // A device recovering from a wipe re-processes every Welcome it ever
    // received, so cap the burst for the same reason the sweep does: rotation is
    // a deadline, not a heartbeat, and the rest are picked up by the next sweep.
    for (conversation_id, _) in joined.iter().take(super::self_update::MAX_SELF_UPDATES_PER_SWEEP) {
        // Catch up first. Our Welcome names the epoch we were added at, but the
        // group may already have moved past it; committing from a stale epoch
        // would just lose the compare-and-swap. Interleaved, never a bare commit
        // replay, so messages sealed at the epochs we skip are still ingested.
        if let Err(e) =
            crate::commands::messages::catch_up_mls_group_interleaved(state, conversation_id, user_id)
                .await
        {
            eprintln!("[mls] post-join self-update: catch-up for {conversation_id} failed: {e}");
            continue;
        }
        match super::self_update::self_update_if_due(state, conversation_id, user_id).await {
            Ok(true) => {
                eprintln!("[mls] post-join self-update: merged own leaf in {conversation_id}");
            }
            Ok(false) => {}
            Err(e) => {
                eprintln!("[mls] post-join self-update for {conversation_id} failed: {e}");
            }
        }
    }
    if joined.len() > super::self_update::MAX_SELF_UPDATES_PER_SWEEP {
        eprintln!(
            "[mls] post-join self-update: {} of {} groups deferred to the next sweep",
            joined.len() - super::self_update::MAX_SELF_UPDATES_PER_SWEEP,
            joined.len()
        );
    }
}

/// Re-arm welcome delivery so `poll_mls_welcomes` re-processes Welcomes and a
/// freshly-created/wiped local DB recovers all MLS group memberships.
///
/// Called by `set_pin` / `unlock` when `load_user_db_with_key` reports an empty
/// `mls_kv`, and crucially AFTER `ensure_device_cert` has (re)published this
/// device's `mls_signature_pub` — so the signed DS reset authenticates against
/// the registered device key. Routes through the Delivery Service (sole writer
/// of the log DB).
///
/// Scoped to THIS device when a `device_id` exists: resetting a user's entire
/// welcome set would make sibling devices re-process and clobber their working
/// MLS state. On first-ever login no device row exists yet, so an all-devices
/// reset is safe — there is no sibling to disturb.
pub async fn reset_welcome_delivery(state: &Arc<AppState>, user_id: &str) -> Result<()> {
    let maybe_device_id = state
        .keystore
        .load_for_user("device_id", user_id)
        .await
        .ok()
        .flatten()
        .and_then(|b| String::from_utf8(b).ok());

    // The DS derives the recipient from the authenticated user; the body
    // only carries the device scope (null ⇒ all of this user's welcomes).
    let body = pollis_api::writes::ResetBody {
        device_id: maybe_device_id,
        // The DS's no-auth fallback for the acting user
        // (`pollis_delivery::writes::resolve_actor`): auth on → the signed
        // user and this must EQUAL it; auth off → this IS the actor, and a
        // body without it is refused outright. Sending it never widens what
        // the caller may do.
        user_id: Some(user_id.to_string()),
    };
    match super::ds_client::ds_post(state, &body).await {
        Ok(resp) if resp.status().is_success() => {}
        Ok(resp) => {
            let s = resp.status();
            let txt = resp.text().await.unwrap_or_default();
            eprintln!("[mls] reset_welcome_delivery: DS reset {s}: {txt}");
        }
        Err(e) => {
            eprintln!("[mls] reset_welcome_delivery: DS reset failed: {e}");
        }
    }

    Ok(())
}

pub async fn poll_mls_welcomes(
    state: &Arc<AppState>,
    user_id: String,
) -> Result<()> {
    let device_id = state.device_id.lock().await.clone()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("device_id not set")))?;
    poll_mls_welcomes_inner(state, &user_id, &device_id).await
}
