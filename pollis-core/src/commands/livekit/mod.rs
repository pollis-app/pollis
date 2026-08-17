//! LiveKit integration: JWT minting, RoomService admin calls, realtime data
//! channel fanout (presence, message pings, typing, voice presence, calls),
//! and the connect/reconnect loop that keeps each group/inbox room
//! subscribed.
//!
//! LiveKit tokens + RoomService admin calls are minted **server-side** by the
//! Delivery Service now (#393) — this module holds no LiveKit API secret. Token
//! requests go through `commands::mls::ds_livekit_token`; SendData / roster go
//! through `ds_livekit_send_data` / `ds_livekit_participants`.
//!
//! Submodules:
//!   - `identity`      — parse a LiveKit identity into a Pollis user_id +
//!     avatar lookups against the remote DB.
//!   - `participants`  — `list_voice_participants` / `list_voice_room_counts`.
//!   - `publish`       — every outbound data-packet helper
//!     (`publish_to_user_inbox`, `publish_to_room_server`,
//!     per-event publishers, typing, voice presence,
//!     ping, 1:1 call invite/cancel).
//!   - `realtime`      — `subscribe_realtime` + `connect_rooms` connect /
//!     reconnect loop and presence emission.
//!   - `legacy`        — thin `get_livekit_token` / `get_livekit_view_token`
//!     / `get_livekit_url` shims kept for the frontend.
//!
//! `dispatch_data` (defined here in `mod.rs`) parses an inbound DataReceived
//! payload and forwards it as a typed `RealtimeEvent` on the frontend
//! channel — it's the shared "wire format → RealtimeEvent" decoder used
//! by `realtime::connect_rooms`.

use crate::realtime::RealtimeEvent;

// ── Submodules ───────────────────────────────────────────────────────────

mod identity;
mod legacy;
mod participants;
mod publish;
mod realtime;

// ── Public surface ───────────────────────────────────────────────────────

pub(crate) use identity::{
    is_view_identity, lookup_avatar_url, lookup_avatar_url_for_identity, pin_local_identity,
    resolve_participant, resolve_participants,
};

pub use legacy::{get_livekit_token, get_livekit_url, get_livekit_view_token, LivekitCredential};
pub use participants::{
    list_voice_participants, list_voice_room_counts, VoiceParticipantInfo, VoiceRoomCount,
};
pub use publish::{
    cancel_call, dismiss_call_on_my_devices, publish_deleted_message_to_room,
    publish_edited_message_to_room, publish_join_requests_changed_to_room,
    publish_member_role_changed_to_room,
    publish_membership_changed_to_room, publish_new_message_to_room, publish_ping,
    publish_to_room_server, publish_to_user_inbox, publish_typing, publish_voice_presence,
    start_call, StartCallResult,
};
pub use realtime::{connect_rooms, subscribe_realtime};

// ── Internal helpers ───────────────────────────────────────────────────────

/// Parses a raw DataReceived payload and forwards it to the frontend channel.
/// Returns a conversation_id when a `membership_changed` event indicates
/// MLS reconcile should be triggered by the caller.
/// The publisher of a data packet, already resolved from its opaque LiveKit
/// identity (#836). `None` when the packet came from a server-side emitter (the
/// DS's own `SendData`) or from a participant we could not resolve.
///
/// This is what shared-room broadcasts are attributed from now. Previously the
/// actor rode inside the payload as a raw `user_id`, which both violated the §5
/// routing-only rule for shared rooms and — after #836 — pointed straight back
/// at the account behind the pseudonym that published it. Taking it from the
/// sender is also the stronger claim: a self-declared id in the body is
/// unauthenticated, this one is at least what the SFU saw connect.
pub(super) struct PacketSender {
    pub user_id: String,
    pub username: Option<String>,
}

pub(super) fn dispatch_data(
    payload: &[u8],
    sender: Option<&PacketSender>,
    channel: &dyn crate::sink::EventSink<RealtimeEvent>,
) -> Option<String> {
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
            // §5: the wake-up carries no sender — the recipient attributes the
            // message from the MLS credential in the envelope it ingests. Tolerate
            // an old client still sending `sender_id`/`sender_username` by simply
            // ignoring those fields.
            let event = RealtimeEvent::NewMessage {
                channel_id: data
                    .get("channel_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                conversation_id: data
                    .get("conversation_id")
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
                    sender_username: data
                        .get("sender_username")
                        .and_then(|v| v.as_str())
                        .map(str::to_owned),
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
                inviter_username: data
                    .get("inviter_username")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                group_name: data
                    .get("group_name")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
            });
            return conv_id;
        }
        Some("join_requests_changed") => {
            if let Some(group_id) = data.get("group_id").and_then(|v| v.as_str()) {
                let _ = channel.send(RealtimeEvent::JoinRequestsChanged {
                    group_id: group_id.to_owned(),
                });
            }
        }
        Some("voice_joined") => {
            // §5/#836: the actor is the sender, not a field in the packet. An old
            // client's `user_id`/`display_name` are simply ignored — the same
            // tolerance `new_message` applies to `sender_id`.
            if let (Some(channel_id), Some(who)) = (
                data.get("channel_id").and_then(|v| v.as_str()),
                sender,
            ) {
                let _ = channel.send(RealtimeEvent::VoiceJoined {
                    channel_id: channel_id.to_owned(),
                    user_id: who.user_id.clone(),
                    display_name: who
                        .username
                        .clone()
                        .unwrap_or_else(|| who.user_id.clone()),
                });
            }
        }
        Some("voice_left") => {
            if let (Some(channel_id), Some(who)) = (
                data.get("channel_id").and_then(|v| v.as_str()),
                sender,
            ) {
                let _ = channel.send(RealtimeEvent::VoiceLeft {
                    channel_id: channel_id.to_owned(),
                    user_id: who.user_id.clone(),
                });
            }
        }
        Some("edited_message") => {
            // §5: no `sender_id` — the editor is re-derived from the durable edit
            // envelope on ingest. An old client's `sender_id` is ignored.
            if let Some(message_id) = data.get("message_id").and_then(|v| v.as_str()) {
                let _ = channel.send(RealtimeEvent::EditedMessage {
                    channel_id: data.get("channel_id").and_then(|v| v.as_str()).map(str::to_owned),
                    conversation_id: data.get("conversation_id").and_then(|v| v.as_str()).map(str::to_owned),
                    message_id: message_id.to_owned(),
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
        Some("roster_changed") => {
            // Mirror of the RosterChanged variant in realtime.rs. Diffs
            // are wire-format string arrays (joined/left) and
            // `[user_id, device_id]` pair arrays (devices_added/removed).
            // Pull them through `serde_json::from_value` rather than
            // re-parsing by hand so a wire-shape drift fails fast in dev.
            let conversation_id = data
                .get("conversation_id")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            let epoch_before = data.get("epoch_before").and_then(|v| v.as_u64());
            let epoch_after = data.get("epoch_after").and_then(|v| v.as_u64());
            let joined_user_ids: Vec<String> = data
                .get("joined_user_ids")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            let left_user_ids: Vec<String> = data
                .get("left_user_ids")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            let devices_added: Vec<(String, String)> = data
                .get("devices_added")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            let devices_removed: Vec<(String, String)> = data
                .get("devices_removed")
                .cloned()
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default();
            if let (Some(conversation_id), Some(epoch_before), Some(epoch_after)) =
                (conversation_id, epoch_before, epoch_after)
            {
                let _ = channel.send(RealtimeEvent::RosterChanged {
                    conversation_id,
                    epoch_before,
                    epoch_after,
                    joined_user_ids,
                    left_user_ids,
                    devices_added,
                    devices_removed,
                });
            }
        }
        Some("typing") => {
            // §5/#836: the typist is the sender. A packet we cannot attribute is
            // dropped rather than shown as "someone is typing" — an indicator
            // with no name is not worth an unattributed event.
            if let Some(who) = sender {
                let _ = channel.send(RealtimeEvent::Typing {
                    channel_id: data.get("channel_id").and_then(|v| v.as_str()).map(str::to_owned),
                    conversation_id: data.get("conversation_id").and_then(|v| v.as_str()).map(str::to_owned),
                    user_id: who.user_id.clone(),
                    username: who.username.clone(),
                    is_typing: data.get("is_typing").and_then(|v| v.as_bool()).unwrap_or(false),
                });
            }
        }
        _ => {}
    }
    None
}
