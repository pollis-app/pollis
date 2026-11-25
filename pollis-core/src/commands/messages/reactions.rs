use serde::{Deserialize, Serialize};
use std::collections::HashMap;
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
/// The DS gates on this once envelope GC has collected the row (#1161); decryption proved membership.
async fn local_conversation_of(state: &Arc<AppState>, message_id: &str) -> Result<Option<String>> {
    use rusqlite::OptionalExtension as _;
    super::with_local_conn(state, |conn| {
        Ok(conn
            .query_row(
                "SELECT conversation_id FROM message WHERE id = ?1",
                rusqlite::params![message_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?)
    })
    .await
}

/// Add an emoji reaction to a message.
/// Silently succeeds if the reaction already exists (UNIQUE constraint).
pub async fn add_reaction(
    message_id: String,
    user_id: String,
    emoji: String,
    state: &Arc<AppState>,
) -> Result<()> {
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

/// Reactions on one message, grouped by emoji.
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

/// Most ids one DS read carries. A page is 50; the DS loops per id, so bound its work per request.
const REACTIONS_BATCH_MAX: usize = 200;

/// Reactions on several messages, keyed by id, in ONE Delivery Service round trip per
/// `REACTIONS_BATCH_MAX` ids (#1337, #2115).
///
/// Every requested id has an entry: an empty list is "loaded, no reactions", which a
/// caller must be able to tell from "not loaded yet" (#2115).
pub async fn get_reactions_for_messages(
    message_ids: Vec<String>,
    state: &Arc<AppState>,
) -> Result<HashMap<String, Vec<Reaction>>> {
    let (mut out, unique) = empty_entries(message_ids);
    for chunk in unique.chunks(REACTIONS_BATCH_MAX) {
        let batched = crate::commands::ds_reads::message_reactions(state, chunk.to_vec()).await?;
        merge_batched(&mut out, batched);
    }
    Ok(out)
}

/// An empty entry per requested id, plus the ids deduplicated in request order.
fn empty_entries(message_ids: Vec<String>) -> (HashMap<String, Vec<Reaction>>, Vec<String>) {
    let mut out: HashMap<String, Vec<Reaction>> = HashMap::with_capacity(message_ids.len());
    let mut unique = Vec::with_capacity(message_ids.len());
    for id in message_ids {
        if !out.contains_key(&id) {
            out.insert(id.clone(), Vec::new());
            unique.push(id);
        }
    }
    (out, unique)
}

/// Fill the requested ids' entries from a DS reply; an id that was not requested is dropped.
fn merge_batched(
    out: &mut HashMap<String, Vec<Reaction>>,
    batched: Vec<pollis_api::directory::MessageReactions>,
) {
    for m in batched {
        if let Some(slot) = out.get_mut(&m.message_id) {
            *slot = group_wire(m.reactions);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use pollis_api::directory::{MessageReactions, ReactionWire};

    fn ids(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn every_requested_id_has_an_entry_even_without_reactions() {
        let (mut out, unique) = empty_entries(ids(&["m1", "m2", "m3", "m2"]));
        assert_eq!(unique, ids(&["m1", "m2", "m3"]));
        merge_batched(
            &mut out,
            vec![
                MessageReactions {
                    message_id: "m2".into(),
                    reactions: vec![ReactionWire {
                        emoji: "👍".into(),
                        user_ids: ids(&["u1", "u2"]),
                    }],
                },
                MessageReactions {
                    message_id: "not-requested".into(),
                    reactions: vec![],
                },
            ],
        );
        assert_eq!(out.len(), 3);
        assert!(out["m1"].is_empty());
        assert!(out["m3"].is_empty());
        assert_eq!(out["m2"].len(), 1);
        assert_eq!(out["m2"][0].count, 2);
        assert!(!out.contains_key("not-requested"));
    }

    #[test]
    fn no_ids_is_an_empty_map() {
        let (out, unique) = empty_entries(Vec::new());
        assert!(out.is_empty());
        assert!(unique.is_empty());
    }
}
