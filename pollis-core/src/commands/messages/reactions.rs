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
    let body = serde_json::json!({
        "message_id": message_id,
        "emoji": emoji,
        "user_id": user_id,
    });
    crate::commands::mls::ds_post_ok(state, "/v1/reactions/add", &body).await?;

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
    let body = serde_json::json!({
        "message_id": message_id,
        "emoji": emoji,
        "user_id": user_id,
    });
    crate::commands::mls::ds_post_ok(state, "/v1/reactions/remove", &body).await?;

    Ok(())
}

/// Get all reactions for a message, grouped by emoji.
pub async fn get_reactions(
    message_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<Reaction>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT emoji, user_id FROM message_reaction WHERE message_id = ?1 ORDER BY created_at ASC",
        libsql::params![message_id],
    ).await?;

    // Collect all (emoji, user_id) rows and group by emoji.
    let mut grouped: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    while let Some(row) = rows.next().await? {
        let emoji: String = row.get(0)?;
        let uid: String = row.get(1)?;
        grouped.entry(emoji).or_default().push(uid);
    }

    let reactions: Vec<Reaction> = grouped
        .into_iter()
        .map(|(emoji, user_ids)| {
            let count = user_ids.len() as u32;
            Reaction { emoji, user_ids, count }
        })
        .collect();

    Ok(reactions)
}
