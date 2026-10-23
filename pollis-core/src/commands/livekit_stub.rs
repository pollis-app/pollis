//! Mobile (iOS/Android) stub for the LiveKit realtime layer.
//!
//! The Rust LiveKit/libwebrtc *client* stack (`Room::connect`, track publish,
//! libwebrtc) is desktop-only — mobile uses the native LiveKit SDK
//! (`@livekit/react-native`) for its own room connections (see issue #185).
//! This module is swapped in for `commands::livekit` on mobile targets via
//! `#[cfg]` in `commands/mod.rs`, so the core messaging/auth/group/dm/
//! enrollment call sites stay byte-identical across platforms.
//!
//! Pushes that ride an *already-connected* `Room` (new_message, typing, voice
//! presence, …) stay no-ops here — mobile keeps no Rust-side `Room`. But
//! pushes to a room this process is *not* joined to go through LiveKit's
//! server-side `RoomService/SendData` Twirp API, which is plain HTTPS + a
//! signed admin JWT and needs no native deps — so those are implemented for
//! real. That's what lets "approve from another device" enrollment (and 1:1
//! call invites) reach a sibling device's inbox room from mobile. Mirrors
//! `commands::livekit::publish::publish_to_user_inbox` on desktop.

use std::sync::Arc;

use crate::error::Result;
use crate::realtime::LiveKitState;
use crate::state::AppState;

type LiveKit = Arc<tokio::sync::Mutex<LiveKitState>>;

// ── Server-side data publish (DS SendData broker) ───────────────────────────
//
// Mobile has no Rust-side `Room`, so pushes to a room this process isn't joined
// to go through the DS's server-side `RoomService/SendData` broker (#393). The
// LiveKit admin secret used to be signed on-device (the leak); now the DS holds
// it and the client just names a room + content-free payload. Mirrors desktop's
// `commands::livekit::publish_*`.

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

/// Notify a conversation's room of a new message. Mobile has no Rust-side
/// `Room` to publish through, but new_message is exactly the event a peer's
/// open chat needs to refresh live — so we send it through the same
/// server-side `SendData` path as inbox events (it delivers to whoever is in
/// the room, e.g. a desktop client viewing the conversation). Without this a
/// mobile-sent message only surfaces on the peer after a manual re-ingest.
/// Mirrors the payload shape of desktop's `publish_new_message_to_room`.
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

// ── Connected-room pushes — still no-op on mobile ───────────────────────────
//
// These ride a `Room` this process has already joined. Mobile holds no
// Rust-side `Room`, and (unlike new_message above) edits/deletes still
// propagate durably through the DS and refresh on the next ingest — so they
// stay no-ops for now. They could move to the SendData path too if live
// edit/delete propagation from mobile becomes a priority (#185).
//
// Group-structure wake-ups are NOT in that set, and must not be: nothing
// "ingests" a channel list, a group name or a roster. A device learns those
// changed only from the wake-up, so a no-op here meant a channel created,
// renamed or deleted, a group renamed, a member removed or re-roled, or a join
// request filed on a phone stayed invisible on the user's desktop (and every
// other member's) until a hard refresh. They go through SendData below.

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

// ── Group-structure wake-ups — real on mobile, via SendData ─────────────────

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
