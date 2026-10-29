//! Conversation + message READ layer (M2).
//!
//! Thin wrappers over the core read surface, which goes through the DS (#987); message reads
//! also ingest new envelopes.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use pollis_core::commands::dm::{list_dm_channels, list_dm_requests, DmChannel};
use pollis_core::commands::groups::{list_user_groups_with_channels, GroupWithChannels};
use pollis_core::commands::messages::{
    get_channel_messages, get_dm_messages, get_reactions_for_messages, list_thread_summaries,
    read_thread_messages, ChannelMessage, MessageCursor, MessagePage, Reaction,
};
use pollis_core::state::AppState;

/// The default page size for a message fetch, matching the core default
/// (`read.rs` uses 50), so the UI and the sync loop agree on one page.
pub const DEFAULT_PAGE: i64 = 50;

/// Every conversation a user participates in, in one snapshot: the groups (each
/// carrying its channels), accepted DMs and pending DM requests, which the sync loop must
/// process too.
#[derive(Debug, Clone)]
pub struct ConversationTree {
    pub groups: Vec<GroupWithChannels>,
    pub dm_channels: Vec<DmChannel>,
    pub dm_requests: Vec<DmChannel>,
}

impl ConversationTree {
    /// The per-conversation ids `process_pending_commits` must be run for: every
    /// channel id plus every DM id (channel and request), the exact set §6 iterates.
    pub fn conversation_ids(&self) -> Vec<String> {
        let mut ids = Vec::new();
        for group in &self.groups {
            for channel in &group.channels {
                ids.push(channel.id.clone());
            }
        }
        for dm in self.dm_channels.iter().chain(self.dm_requests.iter()) {
            ids.push(dm.id.clone());
        }
        ids
    }

    /// Total conversation count (channels + DM channels + DM requests) — handy
    /// for the status line, which calls it every frame, so counted without cloning ids.
    pub fn len(&self) -> usize {
        self.groups.iter().map(|g| g.channels.len()).sum::<usize>()
            + self.dm_channels.len()
            + self.dm_requests.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Load the full conversation tree for `user_id` (§6 enumeration; the §8 "Group
/// list" and "DM list" rows), so the caller never forgets the DM-requests leg.
pub async fn load_conversations(state: &Arc<AppState>, user_id: &str) -> Result<ConversationTree> {
    let mut groups = list_user_groups_with_channels(user_id.to_string(), state).await?;
    drop_voice_channels(&mut groups);
    let dm_channels = list_dm_channels(user_id.to_string(), state).await?;
    let dm_requests = list_dm_requests(user_id.to_string(), state).await?;
    Ok(ConversationTree {
        groups,
        dm_channels,
        dm_requests,
    })
}

/// Read one page of a group channel's messages, newest-first (§8 "Open channel").
/// Runs envelope ingest first; pass `next_cursor` from a prior page to page back.
pub async fn channel_messages(
    state: &Arc<AppState>,
    user_id: &str,
    channel_id: &str,
    cursor: Option<MessageCursor>,
) -> Result<MessagePage> {
    let page = get_channel_messages(
        user_id.to_string(),
        channel_id.to_string(),
        Some(DEFAULT_PAGE),
        cursor,
        state,
    )
    .await?;
    Ok(page)
}

/// Read one page of a DM's messages, newest-first (§8 "Open DM"). Same
/// ingest-then-read + cursor semantics as [`channel_messages`].
pub async fn dm_messages(
    state: &Arc<AppState>,
    user_id: &str,
    dm_channel_id: &str,
    cursor: Option<MessageCursor>,
) -> Result<MessagePage> {
    let page = get_dm_messages(
        user_id.to_string(),
        dm_channel_id.to_string(),
        Some(DEFAULT_PAGE),
        cursor,
        state,
    )
    .await?;
    Ok(page)
}

/// Reactions on `message_ids`, in one DS round trip (#1337). Messages with
/// none are absent.
pub async fn reactions(
    state: &Arc<AppState>,
    message_ids: Vec<String>,
) -> Result<HashMap<String, Vec<Reaction>>> {
    if message_ids.is_empty() {
        return Ok(HashMap::new());
    }
    Ok(get_reactions_for_messages(message_ids, state)
        .await?
        .into_iter()
        .collect())
}

/// Thread root id → reply count for one conversation (local, #825).
pub async fn thread_counts(state: &Arc<AppState>, conv_id: &str) -> Result<HashMap<String, i64>> {
    Ok(list_thread_summaries(conv_id.to_string(), state)
        .await?
        .into_iter()
        .map(|t| (t.thread_id, t.reply_count))
        .collect())
}

/// The TUI is built without `media`, so a voice channel has nothing to open —
/// drop them from the tree, as mobile does; they carry no messages.
fn drop_voice_channels(groups: &mut [GroupWithChannels]) {
    for group in groups {
        group.channels.retain(|c| c.channel_type != "voice");
    }
}

/// One thread's replies, oldest first (local, #825). The root is not included.
pub async fn thread_replies(state: &Arc<AppState>, root_id: &str) -> Result<Vec<ChannelMessage>> {
    Ok(read_thread_messages(root_id.to_string(), state).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    // An empty tree yields no ids and reports empty.
    #[test]
    fn empty_tree_has_no_conversations() {
        let tree = ConversationTree {
            groups: vec![],
            dm_channels: vec![],
            dm_requests: vec![],
        };
        assert!(tree.is_empty());
        assert_eq!(tree.len(), 0);
        assert!(tree.conversation_ids().is_empty());
    }

    // Voice channels never reach the tree; text channels survive.
    #[test]
    fn voice_channels_are_dropped_from_the_tree() {
        use pollis_core::commands::groups::Channel;
        let channel = |id: &str, kind: &str| Channel {
            id: id.to_string(),
            group_id: "g1".to_string(),
            name: id.to_string(),
            description: None,
            channel_type: kind.to_string(),
        };
        let mut groups = vec![GroupWithChannels {
            id: "g1".to_string(),
            name: "g".to_string(),
            description: None,
            owner_id: "o".to_string(),
            created_at: "t".to_string(),
            current_user_role: "member".to_string(),
            channels: vec![
                channel("general", "text"),
                channel("voice-chat", "voice"),
                channel("dev", "text"),
            ],
        }];
        drop_voice_channels(&mut groups);
        let tree = ConversationTree {
            groups,
            dm_channels: vec![],
            dm_requests: vec![],
        };
        assert_eq!(
            tree.conversation_ids(),
            vec!["general".to_string(), "dev".to_string()]
        );
    }
}
