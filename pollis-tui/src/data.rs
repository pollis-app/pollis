//! Conversation + message READ layer (M2).
//!
//! Thin, typed wrappers over the exact `pollis_core::commands::*` read surface
//! the desktop app reaches over Tauri `invoke` (spec §8, the command→screen
//! table). We do **not** reshape or fork the core return types — a wrapper just
//! fixes the argument order the UI wants and, for the conversation tree, bundles
//! the three list calls (§6's "conversation enumeration") behind one call so the
//! sync loop and the left pane share one source of truth.
//!
//! Reads go direct to Turso (the core functions open `state.remote_db`); the
//! message reads additionally pull + decrypt any newly-delivered envelopes
//! (`get_channel_messages` / `get_dm_messages` run ingest before returning),
//! which is what surfaces a peer's message locally.

use std::sync::Arc;

use anyhow::Result;
use pollis_core::commands::dm::{list_dm_channels, list_dm_requests, DmChannel};
use pollis_core::commands::groups::{list_user_groups_with_channels, GroupWithChannels};
use pollis_core::commands::messages::{
    get_channel_messages, get_dm_messages, MessageCursor, MessagePage,
};
use pollis_core::state::AppState;

/// The default page size for a message fetch, matching the core default
/// (`read.rs` uses 50 when `limit` is `None`). Surfaced here so the UI and the
/// sync loop agree on "one page".
pub const DEFAULT_PAGE: i64 = 50;

/// Every conversation a user participates in, in one snapshot: the groups (each
/// carrying its channels), the accepted DM channels, and the still-pending DM
/// requests. This is exactly the enumeration §6 wants before running
/// `process_pending_commits` per conversation, and it's what the left pane
/// renders.
///
/// DM *requests* are included deliberately: a member added to a DM sees it as a
/// pending request until they accept, but the MLS group already exists and may
/// have commits/messages to catch up on — so the sync loop must process it too.
#[derive(Debug, Clone)]
pub struct ConversationTree {
    pub groups: Vec<GroupWithChannels>,
    pub dm_channels: Vec<DmChannel>,
    pub dm_requests: Vec<DmChannel>,
}

impl ConversationTree {
    /// The per-conversation ids `process_pending_commits` must be run for: every
    /// channel id (group channels each map to their group's MLS group; the core
    /// resolves that), plus every DM id (channel + request). Commit processing is
    /// keyed by MLS group / conversation, so this is the exact set §6 iterates.
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
    /// for a "3 conversations" status line and for tests.
    pub fn len(&self) -> usize {
        self.conversation_ids().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Load the full conversation tree for `user_id` (§6 enumeration; the §8 "Group
/// list" and "DM list" rows). One call fans out to the three core list functions
/// so the caller never forgets the DM-requests leg.
pub async fn load_conversations(state: &Arc<AppState>, user_id: &str) -> Result<ConversationTree> {
    let groups = list_user_groups_with_channels(user_id.to_string(), state).await?;
    let dm_channels = list_dm_channels(user_id.to_string(), state).await?;
    let dm_requests = list_dm_requests(user_id.to_string(), state).await?;
    Ok(ConversationTree {
        groups,
        dm_channels,
        dm_requests,
    })
}

/// Read one page of a group channel's messages, newest-first (§8 "Open channel").
/// Runs envelope ingest first, so a peer's just-delivered message shows up. Pass
/// `cursor = page.next_cursor` from a prior page to scroll into history; pass
/// `None` for the first page.
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

#[cfg(test)]
mod tests {
    use super::*;

    // A tree with no conversations yields no ids and reports empty — the
    // fresh-signup state the sync loop starts from.
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
}
