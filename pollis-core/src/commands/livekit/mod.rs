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
    is_view_identity, lookup_avatar_url_for_identity, lookup_avatar_urls_for_identities,
    pin_local_identity, resolve_participant, resolve_participants,
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

/// Who a packet is FROM, for the private-inbox events that name their actor
/// (DM request, group invite, incoming call).
///
/// Two trustworthy sources, and only two: a packet published by a participant
/// is attributed to that participant (the SFU saw it connect under a pseudonym
/// the DS resolved); a packet with no participant came from the DS's own
/// `SendData`, and the DS stamps the VERIFIED signer into it as `sender_id` /
/// `sender_username` after stripping every identity key the client sent
/// (`pollis-delivery/src/broker.rs`, `livekit_send_data`). So `caller_id`,
/// `caller_username`, `inviter_username` and friends are never read here: a
/// self-declared identity in the body is exactly what let any account ring a
/// device "from" anyone.
fn attributed(sender: Option<&PacketSender>, data: &serde_json::Value) -> Option<PacketSender> {
    if let Some(who) = sender {
        return Some(PacketSender {
            user_id: who.user_id.clone(),
            username: who.username.clone(),
        });
    }
    let user_id = data.get("sender_id").and_then(|v| v.as_str())?;
    Some(PacketSender {
        user_id: user_id.to_owned(),
        username: data
            .get("sender_username")
            .and_then(|v| v.as_str())
            .map(str::to_owned),
    })
}

/// What a `membership_changed` wake-up asks the receiving device to do, beyond
/// forwarding the event to the UI.
pub(super) struct MembershipWake {
    /// The `mls_group_id` — `group_id` for channels, `dm_channel_id` for DMs.
    pub(super) conversation_id: String,
    /// `true` when the change was a member LEAVING (or deleting their account).
    /// A leaver cannot commit its own removal, so unlike every other membership
    /// change there is no committer yet and the receiver has to be one (#1081).
    pub(super) is_leave: bool,
}

pub(super) fn dispatch_data(
    payload: &[u8],
    sender: Option<&PacketSender>,
    channel: &dyn crate::sink::EventSink<RealtimeEvent>,
) -> Option<MembershipWake> {
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
                    sender_username: attributed(sender, &data).and_then(|who| who.username),
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
            let is_leave =
                kind.as_deref() == Some(crate::commands::livekit_signalling::LEAVE_KIND);
            // The inviter is the attributed actor (see `attributed`); the DS
            // resolves `group_name` from the group row the invite names.
            let _ = channel.send(RealtimeEvent::MembershipChanged {
                conversation_id: conv_id.clone(),
                kind,
                inviter_username: attributed(sender, &data).and_then(|who| who.username),
                group_name: data
                    .get("group_name")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
            });
            return conv_id.map(|conversation_id| MembershipWake {
                conversation_id,
                is_leave,
            });
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
            // DS-originated only (`bootstrap::enrollment_request`): the DS refuses
            // this type from clients, and a participant-published copy — which
            // in an inbox room can only be one of this user's own devices — is
            // dropped rather than allowed to raise the approval takeover.
            if sender.is_some() {
                return None;
            }
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
            // The caller is the attributed actor — a ring with no attributable
            // caller is dropped, never shown as "from" whoever the body names.
            if let (Some(call_id), Some(room_name), Some(caller)) = (
                data.get("call_id").and_then(|v| v.as_str()),
                data.get("room_name").and_then(|v| v.as_str()),
                attributed(sender, &data),
            ) {
                let caller_username = caller.username.unwrap_or_else(|| caller.user_id.clone());
                let _ = channel.send(RealtimeEvent::CallInvite {
                    call_id: call_id.to_owned(),
                    room_name: room_name.to_owned(),
                    caller_id: caller.user_id,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct Recording(Mutex<Vec<RealtimeEvent>>);

    impl crate::sink::EventSink<RealtimeEvent> for Recording {
        fn send(&self, event: RealtimeEvent) -> Result<(), String> {
            self.0.lock().unwrap().push(event);
            Ok(())
        }
    }

    fn run(payload: serde_json::Value, sender: Option<&PacketSender>) -> Vec<RealtimeEvent> {
        let sink = Recording(Mutex::new(Vec::new()));
        dispatch_data(payload.to_string().as_bytes(), sender, &sink);
        sink.0.into_inner().unwrap()
    }

    fn wake(payload: serde_json::Value) -> Option<MembershipWake> {
        let sink = Recording(Mutex::new(Vec::new()));
        dispatch_data(payload.to_string().as_bytes(), None, &sink)
    }

    /// #1081: a leave is the one membership change with no committer, so the
    /// handler has to be able to tell it apart. Classification lives here; what
    /// the flag makes the device do lives in `mls::apply_membership_wake`.
    #[test]
    fn only_a_leave_wake_asks_the_receiver_to_commit() {
        let leave = wake(crate::commands::livekit_signalling::member_left_group_payload("g1"))
            .expect("a leave names its conversation");
        assert_eq!(leave.conversation_id, "g1");
        assert!(leave.is_leave);

        let dm_leave = wake(
            crate::commands::livekit_signalling::member_left_conversation_payload("dm1"),
        )
        .expect("a DM leave names its conversation");
        assert_eq!(dm_leave.conversation_id, "dm1");
        assert!(dm_leave.is_leave);

        // Everything else was committed by the device that made it.
        let plain = wake(crate::commands::livekit_signalling::membership_changed_payload("g1"))
            .expect("a plain membership change still names its conversation");
        assert!(!plain.is_leave, "a plain membership change needs no remove commit here");

        let invite = wake(crate::commands::livekit_signalling::group_invite_inbox_payload(
            "g1",
            Some("alice"),
            Some("Group"),
        ))
        .expect("an invite names its conversation");
        assert!(!invite.is_leave);

        // An unknown kind is not a leave — a future kind must not accidentally
        // trigger a remove commit on every receiver.
        let unknown = wake(serde_json::json!({
            "type": "membership_changed",
            "group_id": "g1",
            "kind": "something-new",
        }))
        .expect("names its conversation");
        assert!(!unknown.is_leave);
    }

    fn forged_call_invite() -> serde_json::Value {
        serde_json::json!({
            "type": "call_invite",
            "call_id": "c1",
            "room_name": "call-c1",
            "caller_id": "ceo",
            "caller_username": "The CEO",
        })
    }

    /// The spoof: a server-relayed packet naming a caller in its body, with no
    /// DS stamp. It must not ring at all — not as "The CEO", not as anyone.
    #[test]
    fn call_invite_with_only_body_identity_is_dropped() {
        assert!(run(forged_call_invite(), None).is_empty());
    }

    /// A DS-relayed packet is attributed from the stamp the DS added, and the
    /// body's own `caller_*` claims are ignored even when present.
    #[test]
    fn call_invite_is_attributed_from_the_ds_stamp() {
        let mut payload = forged_call_invite();
        payload["sender_id"] = "alice".into();
        payload["sender_username"] = "alice-name".into();
        let events = run(payload, None);
        assert_eq!(events.len(), 1);
        match &events[0] {
            RealtimeEvent::CallInvite { call_id, room_name, caller_id, caller_username } => {
                assert_eq!(call_id, "c1");
                assert_eq!(room_name, "call-c1");
                assert_eq!(caller_id, "alice");
                assert_eq!(caller_username, "alice-name");
            }
            _ => panic!("expected CallInvite"),
        }
    }

    /// A participant-published packet is attributed to the participant, and the
    /// body — `caller_*` and even a fake `sender_id` — is ignored.
    #[test]
    fn call_invite_from_a_participant_is_attributed_to_that_participant() {
        let mut payload = forged_call_invite();
        payload["sender_id"] = "not-bob".into();
        let bob = PacketSender {
            user_id: "bob".into(),
            username: Some("bob-name".into()),
        };
        let events = run(payload, Some(&bob));
        match &events[0] {
            RealtimeEvent::CallInvite { caller_id, caller_username, .. } => {
                assert_eq!(caller_id, "bob");
                assert_eq!(caller_username, "bob-name");
            }
            _ => panic!("expected CallInvite"),
        }
    }

    /// `dm_created` / invite pings name their actor the same way: the stamp, or
    /// the participant — never `inviter_username` / `sender_username` alone.
    #[test]
    fn inbox_pings_take_their_name_from_the_attribution() {
        let events = run(
            serde_json::json!({
                "type": "membership_changed",
                "group_id": "g-1",
                "kind": "invite",
                "inviter_username": "Trusted Admin",
                "group_name": "Design",
            }),
            None,
        );
        match &events[0] {
            RealtimeEvent::MembershipChanged { inviter_username, group_name, kind, .. } => {
                assert_eq!(kind.as_deref(), Some("invite"));
                assert_eq!(inviter_username, &None, "unstamped inviter must not be named");
                assert_eq!(group_name.as_deref(), Some("Design"));
            }
            _ => panic!("expected MembershipChanged"),
        }

        let events = run(
            serde_json::json!({
                "type": "dm_created",
                "conversation_id": "dm-1",
                "sender_id": "alice",
                "sender_username": "alice-name",
            }),
            None,
        );
        match &events[0] {
            RealtimeEvent::DmCreated { sender_username, .. } => {
                assert_eq!(sender_username.as_deref(), Some("alice-name"));
            }
            _ => panic!("expected DmCreated"),
        }
    }

    /// The enrollment-approval takeover is honoured only from the DS's own
    /// emitter (no participant); a participant-published copy is dropped.
    #[test]
    fn enrollment_requested_is_honoured_only_from_the_ds() {
        let payload = serde_json::json!({
            "type": "enrollment_requested",
            "request_id": "r1",
            "new_device_id": "d2",
            "verification_code": "123456",
        });
        assert_eq!(run(payload.clone(), None).len(), 1);
        let someone = PacketSender {
            user_id: "alice".into(),
            username: None,
        };
        assert!(run(payload, Some(&someone)).is_empty());
    }
}
