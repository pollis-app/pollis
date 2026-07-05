//! Conversation + group WRITE layer (M3).
//!
//! Thin, typed wrappers over the exact `pollis_core::commands::*` **write**
//! surface the desktop app reaches over Tauri `invoke` (spec §8, the
//! command→screen table). As with [`crate::data`], we do **not** fork or reshape
//! any command logic — every wrapper forwards straight into the one core
//! function, which routes the write through the Delivery Service (spec §2:
//! "writes go through the DS"). The wrappers exist so the M3b compose/create UI
//! calls exactly one function per action.
//!
//! Two layers, matching the two audiences:
//! - **Typed passthroughs** ([`send_message`], [`create_group`],
//!   [`create_channel`], [`create_dm_channel`], [`accept_dm_request`],
//!   [`invite_to_group`]) — one wrapper per core write, same argument shape the
//!   §8 table names, taking `&str` where the core takes an owned `String` so a
//!   UI event handler can pass borrowed input directly.
//! - **Ergonomic shorthands** ([`send_text`], [`new_group`], [`new_channel`],
//!   [`start_dm`], [`accept_dm`], [`invite`]) — the defaults the UI wants baked
//!   in (a group gets a default text channel, a new channel is `text`, a DM has
//!   exactly the two members), so M3b is a single call per keystroke-action.

use std::sync::Arc;

use anyhow::Result;
use pollis_core::commands::dm::{self, DmChannel};
use pollis_core::commands::groups::{self, Channel, Group};
use pollis_core::commands::messages::{self, Message};
use pollis_core::state::AppState;

// ── Typed passthroughs (one per core write; §8 command→screen map) ────────────

/// Send a text message to a conversation (§8 "Send message"). `conversation_id`
/// is a group channel id or a DM channel id — the core resolves which. Returns
/// the stored [`Message`]. The write is routed through the DS by the core.
pub async fn send_message(
    conversation_id: &str,
    sender_id: &str,
    content: &str,
    reply_to_id: Option<String>,
    sender_username: Option<String>,
    state: &Arc<AppState>,
) -> Result<Message> {
    let message = messages::send_message(
        conversation_id.to_string(),
        sender_id.to_string(),
        content.to_string(),
        reply_to_id,
        sender_username,
        state,
    )
    .await?;
    Ok(message)
}

/// Create a group owned by `owner_id` (§8 "Create group"). `create_default_*`
/// toggle the auto-created #General text / Voice Chat channels — pass `None` to
/// take the core default (both off). Returns the new [`Group`].
pub async fn create_group(
    name: &str,
    description: Option<String>,
    owner_id: &str,
    create_default_text_channel: Option<bool>,
    create_default_voice_channel: Option<bool>,
    state: &Arc<AppState>,
) -> Result<Group> {
    let group = groups::create_group(
        name.to_string(),
        description,
        owner_id.to_string(),
        create_default_text_channel,
        create_default_voice_channel,
        state,
    )
    .await?;
    Ok(group)
}

/// Create a channel in `group_id` (§8 "Create channel"). `channel_type` is
/// `Some("text")` / `Some("voice")`; `None` takes the core default (`text`).
/// Returns the new [`Channel`].
pub async fn create_channel(
    group_id: &str,
    name: &str,
    description: Option<String>,
    channel_type: Option<String>,
    creator_id: &str,
    state: &Arc<AppState>,
) -> Result<Channel> {
    let channel = groups::create_channel(
        group_id.to_string(),
        name.to_string(),
        description,
        channel_type,
        creator_id.to_string(),
        state,
    )
    .await?;
    Ok(channel)
}

/// Open a DM (§8 "Start DM"). `member_ids` must include at least one user other
/// than `creator_id`; the core reconciles the other members into the MLS tree
/// and queues their Welcome. 2-person DMs are deduped by the core. Returns the
/// [`DmChannel`] (existing or freshly created).
pub async fn create_dm_channel(
    creator_id: &str,
    member_ids: Vec<String>,
    state: &Arc<AppState>,
) -> Result<DmChannel> {
    let channel =
        dm::create_dm_channel(creator_id.to_string(), member_ids, state).await?;
    Ok(channel)
}

/// Accept a pending DM request (§8 "Accept DM request"). Flips the accepting
/// user's own `accepted_at` via the DS.
pub async fn accept_dm_request(
    dm_channel_id: &str,
    user_id: &str,
    state: &Arc<AppState>,
) -> Result<()> {
    dm::accept_dm_request(dm_channel_id.to_string(), user_id.to_string(), state).await?;
    Ok(())
}

/// Invite a user to a group (§8 "Invite to group"; core `send_group_invite`).
/// `invitee_identifier` is the username/email the invite form collects. Only a
/// group admin may invite — the core enforces it.
pub async fn invite_to_group(
    group_id: &str,
    inviter_id: &str,
    invitee_identifier: &str,
    state: &Arc<AppState>,
) -> Result<()> {
    groups::send_group_invite(
        group_id.to_string(),
        inviter_id.to_string(),
        invitee_identifier.to_string(),
        state,
    )
    .await?;
    Ok(())
}

// ── Ergonomic shorthands (M3b calls exactly one per action) ───────────────────

/// Send `text` from `user_id` (display name `username`) to conversation
/// `conv_id`, no reply. The everyday "type and hit Enter" path.
pub async fn send_text(
    state: &Arc<AppState>,
    user_id: &str,
    username: Option<String>,
    conv_id: &str,
    text: &str,
) -> Result<Message> {
    send_message(conv_id, user_id, text, None, username, state).await
}

/// Create a group named `name` owned by `user_id`, with the default text
/// channel **on** and no voice channel — the sensible default for a text TUI.
pub async fn new_group(state: &Arc<AppState>, user_id: &str, name: &str) -> Result<Group> {
    create_group(name, None, user_id, Some(true), Some(false), state).await
}

/// Create a text channel named `name` in `group_id`, created by `user_id`.
pub async fn new_channel(
    state: &Arc<AppState>,
    group_id: &str,
    user_id: &str,
    name: &str,
) -> Result<Channel> {
    create_channel(group_id, name, None, Some("text".to_string()), user_id, state).await
}

/// Start a 2-person DM from `user_id` to `other_user_id`.
pub async fn start_dm(
    state: &Arc<AppState>,
    user_id: &str,
    other_user_id: &str,
) -> Result<DmChannel> {
    create_dm_channel(
        user_id,
        vec![user_id.to_string(), other_user_id.to_string()],
        state,
    )
    .await
}

/// Accept the pending DM request `dm_id` as `user_id`.
pub async fn accept_dm(state: &Arc<AppState>, user_id: &str, dm_id: &str) -> Result<()> {
    accept_dm_request(dm_id, user_id, state).await
}

/// Invite `invitee_id` to `group_id` as `inviter_id`.
pub async fn invite(
    state: &Arc<AppState>,
    group_id: &str,
    inviter_id: &str,
    invitee_id: &str,
) -> Result<()> {
    invite_to_group(group_id, inviter_id, invitee_id, state).await
}
