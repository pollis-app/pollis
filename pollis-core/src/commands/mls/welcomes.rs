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
use super::provider::{
    welcome_ciphersuite, with_suite_provider, MlsProvider,
};

/// Internal: deserialise a TLS-encoded `MlsMessageOut` (welcome wire format)
/// and persist the resulting MLS group state locally.
///
/// The bytes stored in `mls_welcome.welcome_data` are TLS-serialised
/// `MlsMessageOut`.  We deserialise to `MlsMessageIn`, extract the inner
/// `Welcome` via `MlsMessageIn::extract()`, then call
/// `StagedWelcome::new_from_welcome`.
///
/// Returns the conversation id we just joined, so the caller can follow up on
/// the join — notably with the post-join self-update that merges this brand-new
/// (and therefore unmerged) leaf into the tree (issue #666).
pub async fn apply_welcome(state: &Arc<AppState>, welcome_bytes: &[u8]) -> Result<String> {
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
    // itself. An unreadable/unknown code point is a hard failure, never a
    // classic fallback: `PollisProvider` panics on a hybrid Welcome.
    let suite = welcome_ciphersuite(&welcome).ok_or_else(|| {
        crate::error::Error::Other(anyhow::anyhow!(
            "welcome names a ciphersuite this build does not implement"
        ))
    })?;

    with_suite_provider!(db.conn(), suite, |provider| {
        join_from_welcome(&provider, welcome)
    })
}

/// Materialise group state from an already-suite-matched `Welcome`, returning
/// the joined conversation id (the GroupId is the conversation id's UTF-8
/// bytes — see `GroupId::from_slice(conversation_id.as_bytes())` in
/// `group_state`).
///
/// Split into ProcessedWelcome → delete stale group → stage → into_group:
/// openmls checks for duplicate GroupIds inside `into_staged_welcome`, so any
/// existing group must be deleted *before* that call.
fn join_from_welcome<C>(provider: &MlsProvider<'_, C>, welcome: Welcome) -> Result<String>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let join_config = MlsGroupJoinConfig::builder()
        .use_ratchet_tree_extension(true)
        .build();

    let processed = ProcessedWelcome::new_from_welcome(provider, &join_config, welcome)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("process welcome: {e}")))?;

    let new_group_id = processed.unverified_group_info().group_id().clone();
    if let Ok(Some(mut old_group)) = MlsGroup::load(provider.storage(), &new_group_id) {
        eprintln!("[mls] apply_welcome: deleting stale group {:?} before re-joining", new_group_id);
        let _ = old_group.delete(provider.storage());
    }

    let staged = processed.into_staged_welcome(provider, None)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("stage welcome: {e}")))?;

    staged.into_group(provider)
        .map_err(|e| crate::error::Error::Other(anyhow::anyhow!("into group: {e}")))?;

    String::from_utf8(new_group_id.as_slice().to_vec()).map_err(|_| {
        crate::error::Error::Other(anyhow::anyhow!("welcome carries a non-UTF-8 group id"))
    })
}

/// Poll the remote `mls_welcome` table for undelivered Welcome messages
/// addressed to `user_id`.  Each one is applied locally and then marked
/// `delivered = 1` so it is not processed again.
///
/// Called on startup and from `poll_pending_messages`.
pub async fn poll_mls_welcomes_inner(state: &Arc<AppState>, user_id: &str, device_id: &str) -> Result<()> {
    // R6: read undelivered welcomes from the read-only log DB (falls back to
    // remote_db when the log DB isn't configured). The delivered=1 write below
    // no longer shares this connection — it routes through the DS (W5 seam).
    let read_conn = state.log_db.conn().await?;

    // Fetch welcomes targeted at this specific device, plus legacy rows
    // (recipient_device_id IS NULL) from before multi-device was deployed.
    let mut rows = read_conn.query(
        "SELECT id, welcome_data FROM mls_welcome \
         WHERE recipient_id = ?1 AND delivered = 0 \
         AND (recipient_device_id = ?2 OR recipient_device_id IS NULL) \
         ORDER BY created_at ASC",
        libsql::params![user_id, device_id],
    ).await?;

    // Drain into owned Vec so `rows` is dropped before local-DB awaits below.
    let mut items: Vec<(String, Vec<u8>)> = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let bytes: Vec<u8> = row.get(1)?;
        items.push((id, bytes));
    }
    drop(rows);

    let had_welcomes = !items.is_empty();
    // IDs to mark delivered. We ack even apply failures — the private key for a
    // failed Welcome was likely orphaned by a DB wipe and will never succeed;
    // the repair mechanism generates a fresh Welcome. (Same semantics as before.)
    let mut processed_ids: Vec<String> = Vec::new();
    // Conversations this pass actually joined — the post-join self-update targets
    // (issue #666). Deduplicated because a repaired join can arrive as two
    // Welcomes for the same group, and rotating twice buys nothing.
    let mut joined: Vec<String> = Vec::new();
    for (id, bytes) in items {
        match apply_welcome(state, &bytes).await {
            Ok(conversation_id) => {
                eprintln!("[mls] poll_mls_welcomes: applied welcome {id}");
                if !joined.contains(&conversation_id) {
                    joined.push(conversation_id);
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
        let body = serde_json::json!({ "welcome_ids": processed_ids });
        match super::ds_client::ds_post(state, "/v1/welcomes/ack", &body).await {
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

    self_update_joined_groups(state, user_id, &joined).await;

    Ok(())
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
async fn self_update_joined_groups(state: &Arc<AppState>, user_id: &str, joined: &[String]) {
    // A device recovering from a wipe re-processes every Welcome it ever
    // received, so cap the burst for the same reason the sweep does: rotation is
    // a deadline, not a heartbeat, and the rest are picked up by the next sweep.
    for conversation_id in joined.iter().take(super::self_update::MAX_SELF_UPDATES_PER_SWEEP) {
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
    let body = serde_json::json!({ "device_id": maybe_device_id });
    match super::ds_client::ds_post(state, "/v1/welcomes/reset", &body).await {
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
