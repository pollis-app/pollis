//! Conversation + group WRITE layer (M3).
//!
//! Thin wrappers over the core write surface (routed through the DS): typed passthroughs plus
//! shorthands with the UI's defaults baked in.

use std::sync::Arc;

use anyhow::Result;
use pollis_core::commands::dm::{self, DmChannel};
use pollis_core::commands::groups::{self, Channel, Group};
use pollis_core::commands::messages::{self, Message};
use pollis_core::state::AppState;

/// Send a text message to a conversation (§8 "Send message"). `conversation_id`
/// is a group channel id or a DM channel id; the core resolves which.
pub async fn send_message(
    conversation_id: &str,
    sender_id: &str,
    content: &str,
    reply_to_id: Option<String>,
    // Thread root (#825); passed through so a thread reply is never flattened into the channel.
    thread_id: Option<String>,
    sender_username: Option<String>,
    state: &Arc<AppState>,
) -> Result<Message> {
    let message = messages::send_message(
        conversation_id.to_string(),
        sender_id.to_string(),
        content.to_string(),
        reply_to_id,
        thread_id,
        sender_username,
        state,
    )
    .await?;
    Ok(message)
}

/// Create a group owned by `owner_id` (§8 "Create group"). `create_default_*`
/// toggle the auto-created text / voice channels; `None` takes the core default (both off).
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
/// than `creator_id`; 2-person DMs are deduped by the core.
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
/// `invitee_identifier` is a username/email; the core enforces admin-only.
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

/// Send `text` from `user_id` (display name `username`) to conversation
/// `conv_id`, no reply. The everyday "type and hit Enter" path.
pub async fn send_text(
    state: &Arc<AppState>,
    user_id: &str,
    username: Option<String>,
    conv_id: &str,
    text: &str,
) -> Result<Message> {
    send_message(conv_id, user_id, text, None, None, username, state).await
}

/// Send `text` into `conv_id` as a quote reply to `reply_to_id` and/or into
/// the thread rooted at `thread_id` (#1337).
pub async fn send_text_with(
    state: &Arc<AppState>,
    user_id: &str,
    username: Option<String>,
    conv_id: &str,
    text: &str,
    reply_to_id: Option<String>,
    thread_id: Option<String>,
) -> Result<Message> {
    send_message(conv_id, user_id, text, reply_to_id, thread_id, username, state).await
}

/// Edit the user's own message (core `edit_message`: stores the edit in the
/// edit outbox, and reads show it as pending until the DS has it (#1555).
pub async fn edit(
    state: &Arc<AppState>,
    conv_id: &str,
    message_id: &str,
    user_id: &str,
    new_content: &str,
) -> Result<()> {
    messages::edit_message(
        conv_id.to_string(),
        message_id.to_string(),
        user_id.to_string(),
        new_content.to_string(),
        state,
    )
    .await?;
    Ok(())
}

/// Delete a message (core `delete_message`; the core decides self-delete vs
/// admin-delete and refuses what the user may not do).
pub async fn delete(state: &Arc<AppState>, message_id: &str, user_id: &str) -> Result<()> {
    messages::delete_message(message_id.to_string(), user_id.to_string(), state).await?;
    Ok(())
}

/// One more delivery attempt for a message the durable outbox holds (#1335),
/// under the same id — the CLI's `R`. Same core call as the apps' Retry.
pub async fn retry(state: &Arc<AppState>, message_id: &str) -> Result<()> {
    messages::retry_message(message_id.to_string(), state).await?;
    Ok(())
}

/// Throw away an unsent message (#1335) — the CLI's `x` on a row marked not
/// sent; the core withdraws it first if it may have reached the DS.
pub async fn discard(state: &Arc<AppState>, message_id: &str) -> Result<bool> {
    Ok(messages::discard_unsent_message(message_id.to_string(), state).await?)
}

/// Toggle `emoji` on a message the way desktop's reaction pill does: remove it
/// when the user already reacted with it (`already_mine`), add it otherwise.
/// Returns whether the reaction is now present.
pub async fn toggle_reaction(
    state: &Arc<AppState>,
    message_id: &str,
    user_id: &str,
    emoji: &str,
    already_mine: bool,
) -> Result<bool> {
    if already_mine {
        messages::remove_reaction(
            message_id.to_string(),
            user_id.to_string(),
            emoji.to_string(),
            state,
        )
        .await?;
        Ok(false)
    } else {
        messages::add_reaction(
            message_id.to_string(),
            user_id.to_string(),
            emoji.to_string(),
            state,
        )
        .await?;
        Ok(true)
    }
}

/// Create a group named `name` owned by `user_id`, with the default text
/// channel on and no voice channel.
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
