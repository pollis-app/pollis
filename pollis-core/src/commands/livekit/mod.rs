//! LiveKit integration: JWT minting, RoomService admin calls, realtime data
//! channel fanout (presence, message pings, typing, voice presence, calls),
//! and the connect/reconnect loop that keeps each group/inbox room
//! subscribed.
//!
//! Submodules:
//!   - `jwt`           — participant JWT helpers (`make_token`, `make_view_token`).
//!   - `admin_api`     — Twirp admin token + RoomService `ListParticipants`.
//!   - `identity`      — parse a LiveKit identity into a Pollis user_id +
//!                       avatar lookups against the remote DB.
//!   - `participants`  — `list_voice_participants` / `list_voice_room_counts`.
//!   - `publish`       — every outbound data-packet helper
//!                       (`publish_to_user_inbox`, `publish_to_room_server`,
//!                       per-event publishers, typing, voice presence,
//!                       ping, 1:1 call invite/cancel).
//!   - `realtime`      — `subscribe_realtime` + `connect_rooms` connect /
//!                       reconnect loop and presence emission.
//!   - `legacy`        — thin `get_livekit_token` / `get_livekit_view_token`
//!                       / `get_livekit_url` shims kept for the frontend.
//!
//! `dispatch_data` (defined here in `mod.rs`) parses an inbound DataReceived
//! payload and forwards it as a typed `RealtimeEvent` on the frontend
//! channel — it's the shared "wire format → RealtimeEvent" decoder used
//! by `realtime::connect_rooms`.

use crate::realtime::RealtimeEvent;

// ── Submodules ───────────────────────────────────────────────────────────

mod admin_api;
mod identity;
mod jwt;
mod legacy;
mod participants;
mod publish;
mod realtime;

// ── Public surface ───────────────────────────────────────────────────────

pub(crate) use identity::{lookup_avatar_url, lookup_avatar_url_for_identity};
pub(crate) use jwt::make_token;

pub use legacy::{get_livekit_token, get_livekit_url, get_livekit_view_token};
pub use participants::{
    list_voice_participants, list_voice_room_counts, VoiceParticipantInfo, VoiceRoomCount,
};
pub use publish::{
    cancel_call, publish_deleted_message_to_room, publish_edited_message_to_room,
    publish_member_role_changed_to_room, publish_membership_changed_to_room,
    publish_new_message_to_room, publish_ping, publish_to_room_server, publish_to_user_inbox,
    publish_typing, publish_voice_presence, start_call, StartCallResult,
};
pub use realtime::{connect_rooms, subscribe_realtime};

// ── Internal helpers ───────────────────────────────────────────────────────

/// Parses a raw DataReceived payload and forwards it to the frontend channel.
/// Returns a conversation_id when a `membership_changed` event indicates
/// MLS reconcile should be triggered by the caller.
pub(super) fn dispatch_data(payload: &[u8], channel: &dyn crate::sink::EventSink<RealtimeEvent>) -> Option<String> {
    let text = match std::str::from_utf8(payload) {
        Ok(s) => s,
        Err(_) => return None,
    };
    let data: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return None,
    };

    match data.get("type").and_then(|v| v.as_str()) {
        Some("new_message") => {
            let event = RealtimeEvent::NewMessage {
                channel_id: data
                    .get("channel_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                conversation_id: data
                    .get("conversation_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                sender_id: data
                    .get("sender_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                sender_username: data
                    .get("sender_username")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
            };
            // Errors here mean the frontend channel was dropped (e.g. logout). Ignore.
            let _ = channel.send(event);
        }
        Some("dm_created") => {
            if let Some(conversation_id) = data
                .get("conversation_id")
                .and_then(|v| v.as_str())
            {
                let _ = channel.send(RealtimeEvent::DmCreated {
                    conversation_id: conversation_id.to_owned(),
                });
            }
        }
        Some("membership_changed") => {
            // Extract conversation_id from payload (group_id or conversation_id).
            let conv_id = data
                .get("group_id")
                .or_else(|| data.get("conversation_id"))
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            let kind = data.get("kind").and_then(|v| v.as_str()).map(str::to_owned);
            let _ = channel.send(RealtimeEvent::MembershipChanged {
                conversation_id: conv_id.clone(),
                kind,
            });
            return conv_id;
        }
        Some("voice_joined") => {
            if let (Some(channel_id), Some(user_id)) = (
                data.get("channel_id").and_then(|v| v.as_str()),
                data.get("user_id").and_then(|v| v.as_str()),
            ) {
                let display_name = data
                    .get("display_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(user_id)
                    .to_owned();
                let _ = channel.send(RealtimeEvent::VoiceJoined {
                    channel_id: channel_id.to_owned(),
                    user_id: user_id.to_owned(),
                    display_name,
                });
            }
        }
        Some("voice_left") => {
            if let (Some(channel_id), Some(user_id)) = (
                data.get("channel_id").and_then(|v| v.as_str()),
                data.get("user_id").and_then(|v| v.as_str()),
            ) {
                let _ = channel.send(RealtimeEvent::VoiceLeft {
                    channel_id: channel_id.to_owned(),
                    user_id: user_id.to_owned(),
                });
            }
        }
        Some("edited_message") => {
            if let Some(message_id) = data.get("message_id").and_then(|v| v.as_str()) {
                let _ = channel.send(RealtimeEvent::EditedMessage {
                    channel_id: data.get("channel_id").and_then(|v| v.as_str()).map(str::to_owned),
                    conversation_id: data.get("conversation_id").and_then(|v| v.as_str()).map(str::to_owned),
                    message_id: message_id.to_owned(),
                    sender_id: data.get("sender_id").and_then(|v| v.as_str()).unwrap_or_default().to_owned(),
                });
            }
        }
        Some("enrollment_requested") => {
            if let (Some(request_id), Some(new_device_id), Some(verification_code)) = (
                data.get("request_id").and_then(|v| v.as_str()),
                data.get("new_device_id").and_then(|v| v.as_str()),
                data.get("verification_code").and_then(|v| v.as_str()),
            ) {
                let _ = channel.send(RealtimeEvent::EnrollmentRequested {
                    request_id: request_id.to_owned(),
                    new_device_id: new_device_id.to_owned(),
                    verification_code: verification_code.to_owned(),
                });
            }
        }
        Some("call_invite") => {
            if let (Some(call_id), Some(room_name), Some(caller_id)) = (
                data.get("call_id").and_then(|v| v.as_str()),
                data.get("room_name").and_then(|v| v.as_str()),
                data.get("caller_id").and_then(|v| v.as_str()),
            ) {
                let caller_username = data
                    .get("caller_username")
                    .and_then(|v| v.as_str())
                    .unwrap_or(caller_id)
                    .to_owned();
                let _ = channel.send(RealtimeEvent::CallInvite {
                    call_id: call_id.to_owned(),
                    room_name: room_name.to_owned(),
                    caller_id: caller_id.to_owned(),
                    caller_username,
                });
            }
        }
        Some("call_canceled") => {
            if let Some(call_id) = data.get("call_id").and_then(|v| v.as_str()) {
                let _ = channel.send(RealtimeEvent::CallCanceled {
                    call_id: call_id.to_owned(),
                });
            }
        }
        Some("typing") => {
            if let Some(user_id) = data.get("user_id").and_then(|v| v.as_str()) {
                let _ = channel.send(RealtimeEvent::Typing {
                    channel_id: data.get("channel_id").and_then(|v| v.as_str()).map(str::to_owned),
                    conversation_id: data.get("conversation_id").and_then(|v| v.as_str()).map(str::to_owned),
                    user_id: user_id.to_owned(),
                    username: data.get("username").and_then(|v| v.as_str()).map(str::to_owned),
                    is_typing: data.get("is_typing").and_then(|v| v.as_bool()).unwrap_or(false),
                });
            }
        }
        _ => {}
    }
    None
}
