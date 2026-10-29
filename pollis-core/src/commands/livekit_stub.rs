//! Mobile stub for `commands::livekit`: mobile uses the native LiveKit SDK
//! (#185) and keeps no Rust-side `Room`, so pushes over a joined room are no-ops.
//! Pushes to a room this process is not in go through the DS `SendData` broker
//! (#393) and are real, which is what lets enrollment approvals and call invites
//! reach a sibling device from mobile.

use std::sync::Arc;

use crate::error::Result;
use crate::realtime::LiveKitState;
use crate::state::AppState;

type LiveKit = Arc<tokio::sync::Mutex<LiveKitState>>;

/// Fire-and-forget SendData via the DS. Errors are only logged; a room with no
/// participants counts as success.
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

/// Push a JSON event to a user's inbox room (`inbox-{user_id}`).
pub async fn publish_to_user_inbox(
    state: &Arc<AppState>,
    user_id: &str,
    payload: serde_json::Value,
) -> Result<()> {
    send_data_to_room(state, format!("inbox-{user_id}"), payload).await
}

/// Push a JSON event to a room the caller is not joined to.
pub async fn publish_to_room_server(
    state: &Arc<AppState>,
    room_name: &str,
    payload: serde_json::Value,
) -> Result<()> {
    send_data_to_room(state, room_name.to_string(), payload).await
}

/// Notify a conversation's room of a new message via SendData, so a peer's open
/// chat refreshes live. Same payload as desktop's `publish_new_message_to_room`.
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

// Edits and deletes still reach peers durably via the DS on the next ingest, so
// these stay no-ops. Group-structure wake-ups below must not be: nothing
// ingests a channel list or roster, so the wake-up is the only signal.

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

/// A group's channels, name or roster changed.
pub async fn publish_membership_changed_to_room(
    state: &Arc<AppState>,
    group_id: &str,
) -> Result<()> {
    let payload = crate::commands::livekit_signalling::membership_changed_payload(group_id);
    send_data_to_room(state, group_id.to_string(), payload).await
}

/// A join request arrived for the group.
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

/// A member's role changed. Same wire shape as desktop.
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
