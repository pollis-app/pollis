//! Mobile (iOS/Android) stub for the LiveKit realtime layer.
//!
//! The Rust LiveKit/libwebrtc stack is desktop-only — mobile uses the native
//! LiveKit SDK (`@livekit/react-native`) instead (see issue #185). This module
//! is swapped in for `commands::livekit` on mobile targets via `#[cfg]` in
//! `commands/mod.rs`, so the core messaging/auth/group/dm/enrollment call
//! sites stay byte-identical across platforms and need no `#[cfg]` of their
//! own. Every realtime *push* here is a no-op: durable delivery still flows
//! through the Turso `message_envelope` path, and recipients fall back to
//! polling — "messages must work" holds. Realtime push returns on mobile once
//! the native SDK is wired in.

use std::sync::Arc;

use crate::config::Config;
use crate::error::Result;
use crate::realtime::LiveKitState;

type LiveKit = Arc<tokio::sync::Mutex<LiveKitState>>;

pub async fn publish_to_user_inbox(
    _config: &Config,
    _user_id: &str,
    _payload: serde_json::Value,
) -> Result<()> {
    Ok(())
}

pub async fn publish_to_room_server(
    _config: &Config,
    _room_name: &str,
    _payload: serde_json::Value,
) -> Result<()> {
    Ok(())
}

pub async fn publish_new_message_to_room(
    _livekit: &LiveKit,
    _room_id: &str,
    _channel_id: Option<&str>,
    _conversation_id: Option<&str>,
    _sender_id: &str,
    _sender_username: Option<&str>,
) -> Result<()> {
    Ok(())
}

pub async fn publish_edited_message_to_room(
    _livekit: &LiveKit,
    _room_id: &str,
    _channel_id: Option<&str>,
    _conversation_id: Option<&str>,
    _sender_id: &str,
    _message_id: &str,
) -> Result<()> {
    Ok(())
}

pub async fn publish_deleted_message_to_room(
    _livekit: &LiveKit,
    _room_id: &str,
    _channel_id: Option<&str>,
    _conversation_id: Option<&str>,
    _deleted_by: &str,
    _message_id: &str,
) -> Result<()> {
    Ok(())
}

pub async fn publish_membership_changed_to_room(
    _livekit: &LiveKit,
    _group_id: &str,
) -> Result<()> {
    Ok(())
}

pub async fn publish_member_role_changed_to_room(
    _livekit: &LiveKit,
    _group_id: &str,
) -> Result<()> {
    Ok(())
}
