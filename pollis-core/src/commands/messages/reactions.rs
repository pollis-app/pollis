use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

/// Aggregated emoji reaction for a message.
/// `user_ids` is the list of users who reacted with this emoji.
#[derive(Debug, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: String,
    pub user_ids: Vec<String>,
    pub count: u32,
}

/// The conversation this device has the message filed under, if it holds it.
///
/// The DS membership-gates a reaction through the message's `message_envelope`
/// row, but envelope GC collects that row once every member device has fetched
/// it — so for any message older than the current fetch window the server has
/// nothing left to resolve the conversation from, and before #1161 it skipped
/// the check entirely. This device does still know: the local `message` row
/// carries `conversation_id`, and it got there by decrypting the message, which
/// is proof of membership no attacker composing a request can manufacture.
///
/// `None` only when this device has no local copy, which for a reaction means
/// the user is reacting to something they cannot see. The DS then falls back to
/// the envelope and, failing that, refuses.
async fn local_conversation_of(state: &Arc<AppState>, message_id: &str) -> Result<Option<String>> {
    use rusqlite::OptionalExtension as _;
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
    Ok(db
        .conn()
        .query_row(
            "SELECT conversation_id FROM message WHERE id = ?1",
            rusqlite::params![message_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?)
}

/// Add an emoji reaction to a message.
/// Silently succeeds if the reaction already exists (UNIQUE constraint).
pub async fn add_reaction(
    message_id: String,
    user_id: String,
    emoji: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // DS seam: the server generates the row id + timestamp and binds the
    // reacting user to the authenticated identity.
    let conversation_id = local_conversation_of(state, &message_id).await?;
    let body = pollis_api::messages::AddReaction(pollis_api::messages::ReactionBody {
        message_id,
        emoji,
        user_id: Some(user_id),
        conversation_id,
    });
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

/// Remove an emoji reaction from a message.
/// Silently succeeds if the reaction does not exist.
pub async fn remove_reaction(
    message_id: String,
    user_id: String,
    emoji: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let conversation_id = local_conversation_of(state, &message_id).await?;
    let body = pollis_api::messages::RemoveReaction(pollis_api::messages::ReactionBody {
        message_id,
        emoji,
        user_id: Some(user_id),
        conversation_id,
    });
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

/// Get all reactions for a message, grouped by emoji.
/// Reactions on one message, grouped by emoji.
///
/// Since #987 the grouping happens on the DS (`POST /v1/messages/lookup`),
/// gated on membership of the message's conversation — a question the client
/// could not previously ask about someone else's message and can no longer
/// answer for itself. The wire order is insertion order rather than the old
/// `HashMap` iteration order, so pill order is now stable; callers that sorted
/// to compensate still sort correctly.
pub async fn get_reactions(
    message_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<Reaction>> {
    let batched = crate::commands::ds_reads::message_reactions(state, vec![message_id]).await?;
    Ok(batched
        .into_iter()
        .next()
        .map(|m| {
            m.reactions
                .into_iter()
                .map(|r| Reaction {
                    count: r.user_ids.len() as u32,
                    emoji: r.emoji,
                    user_ids: r.user_ids,
                })
                .collect()
        })
        .unwrap_or_default())
}
