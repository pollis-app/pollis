//! Conversation and group writes: thin wrappers over the same
//! `pollis_core::commands::*` the desktop app invokes, never reshaping their
//! logic. Typed passthroughs take `&str` for UI handlers; the shorthands
//! ([`send_text`], [`new_group`], …) bake in the UI's defaults.

use std::sync::Arc;

use anyhow::Result;
use pollis_core::commands::dm::{self, DmChannel};
use pollis_core::commands::groups::{self, Channel, Group};
use pollis_core::commands::messages::{self, Message};
use pollis_core::state::AppState;

/// Send a message to a group channel or DM channel (the core resolves which).
pub async fn send_message(
    conversation_id: &str,
    sender_id: &str,
    content: &str,
    reply_to_id: Option<String>,
    // Kept as a passthrough (#825) so a thread reply can never be silently
    // flattened into the channel by this path.
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

/// Create a group. `create_default_*` toggle the #General / Voice Chat
/// channels; `None` takes the core default (both off).
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

/// Create a channel; `channel_type` `None` takes the core default (`text`).
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

/// Open a DM; `member_ids` must include someone other than `creator_id`.
/// 2-person DMs are deduped by the core, so this may return an existing one.
pub async fn create_dm_channel(
    creator_id: &str,
    member_ids: Vec<String>,
    state: &Arc<AppState>,
) -> Result<DmChannel> {
    let channel =
        dm::create_dm_channel(creator_id.to_string(), member_ids, state).await?;
    Ok(channel)
}

pub async fn accept_dm_request(
    dm_channel_id: &str,
    user_id: &str,
    state: &Arc<AppState>,
) -> Result<()> {
    dm::accept_dm_request(dm_channel_id.to_string(), user_id.to_string(), state).await?;
    Ok(())
}

/// Invite a user (username or email) to a group; the core enforces admin-only.
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

/// Send a plain message: no reply, no thread.
pub async fn send_text(
    state: &Arc<AppState>,
    user_id: &str,
    username: Option<String>,
    conv_id: &str,
    text: &str,
) -> Result<Message> {
    send_message(conv_id, user_id, text, None, None, username, state).await
}

/// Send `text` as a quote reply to `reply_to_id` and/or into the thread rooted
/// at `thread_id` (#1337).
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

/// Edit the user's own message. The core queues it in the edit outbox; reads
/// show it as pending until the DS has it (#1555).
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

/// Delete a message; the core picks self- vs admin-delete and refuses what the
/// user may not do.
pub async fn delete(state: &Arc<AppState>, message_id: &str, user_id: &str) -> Result<()> {
    messages::delete_message(message_id.to_string(), user_id.to_string(), state).await?;
    Ok(())
}

/// Retry delivery of a message held in the durable outbox, under the same id
/// (#1335).
pub async fn retry(state: &Arc<AppState>, message_id: &str) -> Result<()> {
    messages::retry_message(message_id.to_string(), state).await?;
    Ok(())
}

/// Discard an unsent message (#1335); the core withdraws it from everyone if
/// an attempt may have reached the DS. Returns whether there was anything.
pub async fn discard(state: &Arc<AppState>, message_id: &str) -> Result<bool> {
    Ok(messages::discard_unsent_message(message_id.to_string(), state).await?)
}

/// Toggle `emoji` like desktop's reaction pill. Returns whether the reaction is
/// now present.
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

/// Create a group with the default text channel and no voice channel.
pub async fn new_group(state: &Arc<AppState>, user_id: &str, name: &str) -> Result<Group> {
    create_group(name, None, user_id, Some(true), Some(false), state).await
}

pub async fn new_channel(
    state: &Arc<AppState>,
    group_id: &str,
    user_id: &str,
    name: &str,
) -> Result<Channel> {
    create_channel(group_id, name, None, Some("text".to_string()), user_id, state).await
}

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

pub async fn accept_dm(state: &Arc<AppState>, user_id: &str, dm_id: &str) -> Result<()> {
    accept_dm_request(dm_id, user_id, state).await
}

pub async fn invite(
    state: &Arc<AppState>,
    group_id: &str,
    inviter_id: &str,
    invitee_id: &str,
) -> Result<()> {
    invite_to_group(group_id, inviter_id, invitee_id, state).await
}
