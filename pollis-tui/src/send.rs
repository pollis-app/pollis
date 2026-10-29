//! Conversation + group write layer: typed passthroughs over the core write
//! surface (spec §8), plus shorthands with the UI's defaults baked in.

use std::sync::Arc;

use anyhow::Result;
use pollis_core::commands::dm::{self, DmChannel};
use pollis_core::commands::groups::{self, Channel, Group};
use pollis_core::commands::messages::{self, Message};
use pollis_core::state::AppState;

/// `conversation_id` is a group channel or DM channel id; the core resolves which.
pub async fn send_message(
    conversation_id: &str,
    sender_id: &str,
    content: &str,
    reply_to_id: Option<String>,
    // Thread root ULID (#825); passed through so a thread reply is never
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

/// `create_default_*` toggle the auto-created #General / Voice Chat channels;
/// `None` takes the core default (both off).
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

/// `channel_type`: `Some("text")` / `Some("voice")`; `None` takes the core
/// default (`text`).
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

/// `member_ids` must include someone other than `creator_id`. The core dedupes
/// 2-person DMs, so this may return an existing channel.
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

/// `invitee_identifier` is a username or email. The core enforces admin-only.
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

pub async fn send_text(
    state: &Arc<AppState>,
    user_id: &str,
    username: Option<String>,
    conv_id: &str,
    text: &str,
) -> Result<Message> {
    send_message(conv_id, user_id, text, None, None, username, state).await
}

/// Send as a quote reply to `reply_to_id` and/or into the thread rooted at
/// `thread_id` (#1337).
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

/// The core decides self- vs admin-delete and refuses what the user may not do.
pub async fn delete(state: &Arc<AppState>, message_id: &str, user_id: &str) -> Result<()> {
    messages::delete_message(message_id.to_string(), user_id.to_string(), state).await?;
    Ok(())
}

/// Retry a message the durable outbox holds, under the same id (#1335).
pub async fn retry(state: &Arc<AppState>, message_id: &str) -> Result<()> {
    messages::retry_message(message_id.to_string(), state).await?;
    Ok(())
}

/// Discard an unsent message (#1335); the core withdraws it first if an attempt
/// may have reached the DS. Returns whether there was anything to discard.
pub async fn discard(state: &Arc<AppState>, message_id: &str) -> Result<bool> {
    Ok(messages::discard_unsent_message(message_id.to_string(), state).await?)
}

/// Remove `emoji` if `already_mine`, add it otherwise. Returns whether the
/// reaction is now present.
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

/// Default text channel on, no voice channel.
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
