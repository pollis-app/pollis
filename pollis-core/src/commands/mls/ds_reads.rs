//! The client half of the Delivery Service's READ endpoints (#987).
//!
//! Every question the MLS join/replay path used to ask its own Turso connection
//! is asked here instead, over the same device-signed POST transport the writes
//! already use. Two properties are worth stating at the seam, because both are
//! easy to lose in a refactor and neither fails loudly:
//!
//! **One snapshot, one request.** [`conversation_snapshot`] returns GroupInfo,
//! the pending-Welcome flag, the lineage heads and the commit batch together
//! because the DS read them together, in one transaction. Splitting this into
//! two calls would restore the exact hazard the endpoint exists to close: a
//! device that sees a GroupInfo without the Welcome written atomically beside it
//! external-joins into its own inbound Welcome and is stranded. Do not add a
//! second round trip between the two.
//!
//! **Failure biases stay at the call sites.** This module returns
//! `Result<ConversationState>` and nothing else — it never picks a default. The
//! gates it feeds have OPPOSITE biases seven lines apart (`has_pending_welcome`
//! fails OPEN so a transient blip cannot deadlock recovery; `local_user_is_member`
//! fails CLOSED so it cannot leak membership), so collapsing them into one
//! "returns false on error" helper here would silently invert one of them. HTTP
//! makes those failures common where a libsql read failure was rare, which is
//! precisely why the distinction now matters more than it did.

use std::sync::Arc;

use pollis_api::reads::{
    ConversationState, ConversationStateBody, ConversationStateQuery, FetchWelcomesBody,
};

use crate::error::{Error, Result};
use crate::state::AppState;

use super::ds_client::{current_user_id, ds_post_json};

/// Ask about several conversations at once.
///
/// Batched because the cold-launch sweep runs the catch-up chain per
/// conversation, serially: at ~5 dependent round trips each, a 20-group launch
/// was 60–80 sequential hops. One signed request per *pass* rather than per
/// *conversation* is the whole point — a caller that loops this per conversation
/// has moved the latency problem rather than fixed it.
///
/// Entries come back in request order, one per query. An entry the caller is not
/// authorized for is present with `authorized: false` rather than failing the
/// batch — a sweep naming one stale conversation must not blind the client to
/// the other nineteen.
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
        // The DS's no-auth fallback only. With auth on both halves come from the
        // verified signature and these are ignored — though still signature-bound,
        // since the signature covers the whole body.
        user_id: Some(current_user_id(state).await?),
        device_id,
    };
    let resp = ds_post_json(state, &body).await?;
    Ok(resp.states)
}

/// One conversation's snapshot — GroupInfo, Welcome flag, heads, commits, and
/// the two membership gates, all from ONE server-side read transaction.
///
/// The single-conversation shape of [`conversation_states`]; it still costs one
/// round trip, so a caller with several conversations in hand should batch
/// instead of calling this in a loop.
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

/// The MLS group backing a conversation: a channel maps to its `group_id` (all
/// channels of a group share ONE MLS group), a DM is its own group.
///
/// Falls back to the id itself when the DS cannot resolve it, which is what the
/// direct `SELECT group_id FROM channels` did by returning no row — the caller
/// then treats the id as its own MLS group, and the paths that need membership
/// re-check it themselves.
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

/// Decode one base64 blob from a read response.
pub(crate) fn decode_b64(what: &str, s: &str) -> Result<Vec<u8>> {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| Error::Other(anyhow::anyhow!("{what}: invalid base64 from DS: {e}")))
}

/// Drain this device's undelivered Welcomes: `(welcome_id, welcome_bytes)`,
/// oldest first.
///
/// The read counterpart of `POST /v1/welcomes/ack`, which the caller still posts
/// afterwards to mark them delivered. The DS scopes the drain to the VERIFIED
/// signing device, so the `device_id` below is only the no-auth path's input.
pub(crate) async fn fetch_welcomes(
    state: &Arc<AppState>,
    user_id: &str,
    device_id: &str,
) -> Result<Vec<(String, Vec<u8>)>> {
    let body = FetchWelcomesBody {
        user_id: Some(user_id.to_string()),
        device_id: Some(device_id.to_string()),
    };
    let resp = ds_post_json(state, &body).await?;
    resp.welcomes
        .into_iter()
        .map(|w| {
            let bytes = decode_b64("welcome", &w.welcome)?;
            Ok((w.id, bytes))
        })
        .collect()
}
