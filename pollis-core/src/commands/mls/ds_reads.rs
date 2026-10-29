//! The client half of the Delivery Service's READ endpoints (#987).
//!
//! **One snapshot, one request.** [`conversation_snapshot`] returns GroupInfo
//! and the pending-Welcome flag from one DS transaction; splitting it would let
//! a device external-join into its own inbound Welcome and be stranded.
//!
//! **Failure biases stay at the call sites.** This module never picks a default:
//! `has_pending_welcome` fails open (no recovery deadlock) while
//! `local_user_is_member` fails closed (no membership leak).

use std::sync::Arc;

use pollis_api::reads::{
    ConversationState, ConversationStateBody, ConversationStateQuery, FetchWelcomesBody,
};

use crate::error::{Error, Result};
use crate::state::AppState;

use super::ds_client::{current_user_id, ds_post_json};

pub(crate) use crate::commands::ds_reads::decode_b64;

/// Ask about several conversations at once: one signed request per sweep pass,
/// not per conversation, so don't call it in a loop.
///
/// Entries come back in request order. An unauthorized entry is present with
/// `authorized: false` rather than failing the batch.
pub(crate) async fn conversation_states(
    state: &Arc<AppState>,
    queries: Vec<ConversationStateQuery>,
) -> Result<Vec<ConversationState>> {
    if queries.is_empty() {
        return Ok(Vec::new());
    }
    let device_id = state.device_id.lock().await.clone();
    let body = ConversationStateBody {
        queries,
        // The DS's no-auth fallback only; with auth on, the verified signature wins.
        user_id: Some(current_user_id(state).await?),
        device_id,
    };
    let resp = ds_post_json(state, &body).await?;
    Ok(resp.states)
}

/// One conversation's snapshot, all from ONE server-side read transaction. Batch
/// with [`conversation_states`] when you have several.
pub(crate) async fn conversation_snapshot(
    state: &Arc<AppState>,
    query: ConversationStateQuery,
) -> Result<ConversationState> {
    let conversation_id = query.conversation_id.clone();
    let mut states = conversation_states(state, vec![query]).await?;
    if states.is_empty() {
        return Err(Error::Other(anyhow::anyhow!(
            "conversation-state returned no entry for {conversation_id}"
        )));
    }
    Ok(states.remove(0))
}

/// A query that asks only the cheap questions: heads, the GroupInfo *head*
/// (not its blob), the Welcome flag and the two membership gates. No commit scan.
pub(crate) fn state_query(conversation_id: &str, generation: Option<i64>) -> ConversationStateQuery {
    ConversationStateQuery {
        conversation_id: conversation_id.to_string(),
        generation,
        since: None,
        want_group_info_blob: false,
        commit_at_epoch: None,
    }
}

/// The MLS group backing a conversation: a channel maps to its `group_id`, a DM
/// is its own group. Falls back to the id itself when the DS cannot resolve it.
pub(crate) async fn resolve_mls_group(
    state: &Arc<AppState>,
    conversation_id: &str,
) -> Result<String> {
    let resolved =
        crate::commands::ds_reads::catch_up(state, conversation_id, false, false).await?;
    Ok(if resolved.mls_group_id.is_empty() {
        conversation_id.to_string()
    } else {
        resolved.mls_group_id
    })
}

/// Drain this device's undelivered Welcomes, oldest first. The caller still
/// posts `/v1/welcomes/ack` afterwards; the DS scopes the drain to the verified
/// signing device.
pub(crate) async fn fetch_welcomes(
    state: &Arc<AppState>,
    user_id: &str,
    device_id: &str,
) -> Result<FetchedWelcomes> {
    let body = FetchWelcomesBody {
        user_id: Some(user_id.to_string()),
        device_id: Some(device_id.to_string()),
        // #1412: the drain doubles as the "key-package pool is low" signal.
        count_key_packages_in_suite: Some(i64::from(u16::from(super::provider::current_suite()))),
    };
    let resp = ds_post_json(state, &body).await?;
    let items = resp
        .welcomes
        .into_iter()
        .map(|w| {
            let bytes = decode_b64("welcome", &w.welcome)?;
            Ok(FetchedWelcome {
                id: w.id,
                bytes,
                conversation_id: w.conversation_id,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(FetchedWelcomes {
        items,
        unclaimed_key_packages: resp.unclaimed_key_packages,
        has_last_resort_key_package: resp.has_last_resort_key_package,
    })
}

/// One undelivered Welcome row.
pub(crate) struct FetchedWelcome {
    pub id: String,
    pub bytes: Vec<u8>,
    /// The conversation the row was filed under. `None` from a Delivery Service
    /// that predates the field. Used to re-join a conversation whose Welcome
    /// failed to apply (#1571).
    pub conversation_id: Option<String>,
}

/// One Welcome drain: the rows, plus this device's remaining KeyPackage count
/// (#1412) and last-resort state (#1571) when the Delivery Service reports them.
pub(crate) struct FetchedWelcomes {
    pub items: Vec<FetchedWelcome>,
    /// `None` from a Delivery Service that predates the field.
    pub unclaimed_key_packages: Option<i64>,
    /// `None` from a Delivery Service that predates the field.
    pub has_last_resort_key_package: Option<bool>,
}
