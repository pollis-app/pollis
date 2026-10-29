use serde::{Deserialize, Serialize};
use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

/// Aggregated emoji reaction for a message.
#[derive(Debug, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: String,
    pub user_ids: Vec<String>,
    pub count: u32,
}

/// The conversation this device has the message filed under, if it holds it.
///
/// The DS membership-gates a reaction through the envelope, which GC collects
/// once every device has fetched it (#1161). The local row proves membership:
/// it exists only because this device decrypted the message. With `None` the DS
/// falls back to the envelope and, failing that, refuses.
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
/// Idempotent (UNIQUE constraint).
pub async fn add_reaction(
    message_id: String,
    user_id: String,
    emoji: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // The DS generates the row id and timestamp and binds the user to the
    // authenticated identity.
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
/// Idempotent.
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

/// Reactions on one message, grouped by emoji on the DS (#987), in insertion
/// order.
pub async fn get_reactions(
    message_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<Reaction>> {
    let batched = crate::commands::ds_reads::message_reactions(state, vec![message_id]).await?;
    Ok(batched
        .into_iter()
        .next()
        .map(|m| group_wire(m.reactions))
        .unwrap_or_default())
}

/// Reactions on several messages in ONE Delivery Service round trip (#1337).
///
/// Batched for the terminal client, which has no realtime path and re-reads
/// the visible page each sync round. A message the caller may not see, or with
/// no reactions, is absent.
pub async fn get_reactions_for_messages(
    message_ids: Vec<String>,
    state: &Arc<AppState>,
) -> Result<Vec<(String, Vec<Reaction>)>> {
    let batched = crate::commands::ds_reads::message_reactions(state, message_ids).await?;
    Ok(batched
        .into_iter()
        .map(|m| (m.message_id, group_wire(m.reactions)))
        .collect())
}

fn group_wire(wire: Vec<pollis_api::directory::ReactionWire>) -> Vec<Reaction> {
    wire.into_iter()
        .map(|r| Reaction {
            count: r.user_ids.len() as u32,
            emoji: r.emoji,
            user_ids: r.user_ids,
        })
        .collect()
}
