//! Mobile (iOS/Android) stub for the LiveKit realtime layer.
//!
//! Desktop-only Rust LiveKit client; mobile uses `@livekit/react-native` (#185).
//! Connected-room pushes are no-ops; unjoined-room pushes go via DS SendData.

use std::sync::Arc;

use crate::error::Result;
use crate::realtime::LiveKitState;
use crate::state::AppState;

type LiveKit = Arc<tokio::sync::Mutex<LiveKitState>>;

// Server-side data publish via the DS SendData broker (#393).

/// Fire-and-forget SendData to `room_name` via the DS. Non-fatal — the DS treats
/// a room with no participants (404) as success and errors are only logged.
async fn send_data_to_room(
    state: &Arc<AppState>,
    room_name: String,
    payload: serde_json::Value,
) -> Result<()> {
    let state = Arc::clone(state);
    tokio::spawn(async move {
        if let Err(e) = crate::commands::mls::ds_livekit_send_data(&state, &room_name, payload).await {
            crate::diag!("[realtime] SendData -> {room_name} error (non-fatal): {e}");
        }
    });
    Ok(())
}

/// Push a JSON event to a user's personal inbox room (`inbox-{user_id}`).
/// Real on mobile — this is the path enrollment + call invites rely on.
pub async fn publish_to_user_inbox(
    state: &Arc<AppState>,
    user_id: &str,
    payload: serde_json::Value,
) -> Result<()> {
    send_data_to_room(state, format!("inbox-{user_id}"), payload).await
}

/// Push a JSON event to an arbitrary room the caller is not joined to (e.g. a
/// user accepting a group invite notifying existing members). Real on mobile.
pub async fn publish_to_room_server(
    state: &Arc<AppState>,
    room_name: &str,
    payload: serde_json::Value,
) -> Result<()> {
    send_data_to_room(state, room_name.to_string(), payload).await
}

/// New-message wake-up via SendData, so a peer's open chat refreshes live.
pub async fn publish_new_message_to_room(
    state: &Arc<AppState>,
    room_id: &str,
    channel_id: Option<&str>,
    conversation_id: Option<&str>,
) -> Result<()> {
    let payload =
        crate::commands::livekit_signalling::new_message_payload(channel_id, conversation_id);
    send_data_to_room(state, room_id.to_string(), payload).await
}

// Connected-room pushes: no-ops on mobile (edits/deletes arrive on ingest, #185).

pub async fn publish_edited_message_to_room(
    _livekit: &LiveKit,
    _room_id: &str,
    _channel_id: Option<&str>,
    _conversation_id: Option<&str>,
    _message_id: &str,
) -> Result<()> {
    Ok(())
}

pub async fn publish_deleted_message_to_room(
    _livekit: &LiveKit,
    _room_id: &str,
    _channel_id: Option<&str>,
    _conversation_id: Option<&str>,
    _message_id: &str,
) -> Result<()> {
    Ok(())
}

// Group-structure wake-ups: real on mobile (nothing ingests them).

/// A group's channels, name or roster changed. Same routing-only payload as
/// desktop's `publish_membership_changed_to_room`, to the group's room.
pub async fn publish_membership_changed_to_room(
    state: &Arc<AppState>,
    group_id: &str,
) -> Result<()> {
    let payload = crate::commands::livekit_signalling::membership_changed_payload(group_id);
    send_data_to_room(state, group_id.to_string(), payload).await
}

/// A join request arrived for the group. Desktop sends this through SendData
/// as well (the requester is not in the room); same payload.
pub async fn publish_join_requests_changed_to_room(
    state: &Arc<AppState>,
    group_id: &str,
) -> Result<()> {
    let payload = serde_json::json!({
        "type": "join_requests_changed",
        "group_id": group_id,
    });
    send_data_to_room(state, group_id.to_string(), payload).await
}

/// A member's role changed. Same wire shape as desktop, which serializes the
/// `MemberRoleChanged` event itself.
pub async fn publish_member_role_changed_to_room(
    state: &Arc<AppState>,
    group_id: &str,
) -> Result<()> {
    let payload = serde_json::to_value(crate::realtime::RealtimeEvent::MemberRoleChanged {
        group_id: group_id.to_owned(),
    })
    .map_err(crate::error::Error::Serde)?;
    send_data_to_room(state, group_id.to_string(), payload).await
}
