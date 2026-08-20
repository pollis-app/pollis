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
    let body = pollis_api::messages::AddReaction(pollis_api::messages::ReactionBody {
        message_id,
        emoji,
        user_id: Some(user_id),
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
    let body = pollis_api::messages::RemoveReaction(pollis_api::messages::ReactionBody {
        message_id,
        emoji,
        user_id: Some(user_id),
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
