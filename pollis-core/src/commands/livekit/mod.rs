//! LiveKit integration: realtime data-channel fanout (presence, message pings,
//! typing, voice presence, calls) and the connect/reconnect loop for each
//! group/inbox room. Tokens and RoomService calls are minted by the DS (#393);
//! this module holds no LiveKit secret.

use crate::realtime::RealtimeEvent;

mod identity;
mod legacy;
mod participants;
mod publish;
mod realtime;

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

/// The publisher of a data packet, resolved from its opaque LiveKit identity
/// (#836). `None` for a server-side emitter (the DS's `SendData`) or an
/// unresolvable participant. Shared-room broadcasts are attributed from this,
/// never from a self-declared id in the payload.
pub(super) struct PacketSender {
    pub user_id: String,
    pub username: Option<String>,
}

/// Who a packet is FROM, for the private-inbox events that name their actor
/// (DM request, group invite, incoming call).
///
/// Two trustworthy sources only: a participant-published packet is attributed
/// to that participant; a packet with no participant came from the DS, which
/// stamps the verified signer as `sender_id` / `sender_username` after
/// stripping client-sent identity keys (`pollis-delivery/src/broker.rs`). Body
/// fields like `caller_id` are never read: that let any account ring a device
/// "from" anyone.
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
    /// A member leaving (or deleting their account). A leaver cannot commit its
    /// own removal, so the receiver has to (#1081).
    pub(super) is_leave: bool,
}

/// Parse a raw DataReceived payload and forward it to the frontend channel.
/// Returns a [`MembershipWake`] when the caller should act on a membership
/// change.
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
            // §5: the wake-up carries no sender; the recipient attributes the
            // message from the MLS credential. An old client's `sender_id` is
            // ignored.
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
            let _ = channel.send(event);
        }
        Some("dm_created") => {
            if let Some(conversation_id) = data
                .get("conversation_id")
                .and_then(|v| v.as_str())
            {
                let who = attributed(sender, &data);
                let _ = channel.send(RealtimeEvent::DmCreated {
                    conversation_id: conversation_id.to_owned(),
                    sender_id: who.as_ref().map(|w| w.user_id.clone()),
                    sender_username: who.and_then(|w| w.username),
                });
            }
        }
        Some("membership_changed") => {
            let conv_id = data
                .get("group_id")
                .or_else(|| data.get("conversation_id"))
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            let kind = data.get("kind").and_then(|v| v.as_str()).map(str::to_owned);
            let is_leave =
                kind.as_deref() == Some(crate::commands::livekit_signalling::LEAVE_KIND);
            // The inviter is the attributed actor; the DS resolves `group_name`.
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
            // §5/#836: the actor is the sender, not a field in the packet.
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
            // §5: the editor is re-derived from the edit envelope on ingest.
            if let Some(message_id) = data.get("message_id").and_then(|v| v.as_str()) {
                let _ = channel.send(RealtimeEvent::EditedMessage {
                    channel_id: data.get("channel_id").and_then(|v| v.as_str()).map(str::to_owned),
                    conversation_id: data.get("conversation_id").and_then(|v| v.as_str()).map(str::to_owned),
                    message_id: message_id.to_owned(),
                });
            }
        }
        Some("deleted_message") => {
            // §5: the actor is re-derived from the tombstone on ingest.
            if let Some(message_id) = data.get("message_id").and_then(|v| v.as_str()) {
                let _ = channel.send(RealtimeEvent::DeletedMessage {
                    channel_id: data.get("channel_id").and_then(|v| v.as_str()).map(str::to_owned),
                    conversation_id: data.get("conversation_id").and_then(|v| v.as_str()).map(str::to_owned),
                    message_id: message_id.to_owned(),
                });
            }
        }
        Some("member_role_changed") => {
            if let Some(group_id) = data.get("group_id").and_then(|v| v.as_str()) {
                let _ = channel.send(RealtimeEvent::MemberRoleChanged {
                    group_id: group_id.to_owned(),
                });
            }
        }
        Some("enrollment_requested") => {
            // DS-originated only: a participant-published copy (in an inbox
            // room, one of this user's own devices) is dropped.
            if sender.is_some() {
                return None;
            }
            // `verification_code` is deliberately not read (#1096): the approval
            // UI must not pre-fill the comparison with a server-supplied value.
            if let (Some(request_id), Some(new_device_id)) = (
                data.get("request_id").and_then(|v| v.as_str()),
                data.get("new_device_id").and_then(|v| v.as_str()),
            ) {
                // `link_id` (#1207/#1361) marks a QR-link request, which the UI
                // must not answer with the typed-code takeover.
                let link_id = data
                    .get("link_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                let _ = channel.send(RealtimeEvent::EnrollmentRequested {
                    request_id: request_id.to_owned(),
                    new_device_id: new_device_id.to_owned(),
                    link_id,
                });
            }
        }
        Some("channel_mutes_changed") => {
            // DS-originated only, like `enrollment_requested`.
            if sender.is_some() {
                return None;
            }
            let _ = channel.send(RealtimeEvent::ChannelMutesChanged);
        }
        Some("call_invite") => {
            // A ring with no attributable caller is dropped.
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
            // A bare refetch hint (§5.3): the conversation and its epochs.
            let conversation_id = data
                .get("conversation_id")
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            let epoch_before = data.get("epoch_before").and_then(|v| v.as_u64());
            let epoch_after = data.get("epoch_after").and_then(|v| v.as_u64());
            if let (Some(conversation_id), Some(epoch_before), Some(epoch_after)) =
                (conversation_id, epoch_before, epoch_after)
            {
                let _ = channel.send(RealtimeEvent::RosterChanged {
                    conversation_id,
                    epoch_before,
                    epoch_after,
                });
            }
        }
        Some("typing") => {
            // §5/#836: the typist is the sender; an unattributable packet is
            // dropped.
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

    /// Every wake-up a client publishes reaches the renderer, rather than
    /// falling through to `_ => {}`.
    #[test]
    fn every_published_wake_up_reaches_the_renderer() {
        use crate::commands::livekit_signalling as sig;
        let published = [
            sig::new_message_payload(Some("c1"), Some("g1")),
            sig::edited_message_payload(Some("c1"), Some("g1"), "m1"),
            sig::deleted_message_payload(Some("c1"), Some("g1"), "m1"),
            sig::membership_changed_payload("g1"),
            serde_json::json!({ "type": "join_requests_changed", "group_id": "g1" }),
            serde_json::to_value(RealtimeEvent::MemberRoleChanged { group_id: "g1".into() })
                .unwrap(),
        ];
        for payload in published {
            let got = run(payload.clone(), None);
            assert_eq!(got.len(), 1, "dropped on receipt: {payload}");
            let event = serde_json::to_value(&got[0]).unwrap();
            assert_eq!(event["type"], payload["type"], "forwarded as the wrong event");
        }
    }

    /// #1081: only a leave asks the receiver to commit.
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

        // A future kind must not trigger a remove commit on every receiver.
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

    /// A server-relayed packet naming a caller only in its body must not ring.
    #[test]
    fn call_invite_with_only_body_identity_is_dropped() {
        assert!(run(forged_call_invite(), None).is_empty());
    }

    /// Attributed from the DS stamp; body `caller_*` claims are ignored.
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

    /// Attributed to the participant; the body, even a fake `sender_id`, is
    /// ignored.
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

    /// Inbox pings name their actor from the stamp or the participant only.
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
            RealtimeEvent::DmCreated { sender_id, sender_username, .. } => {
                assert_eq!(sender_id.as_deref(), Some("alice"));
                assert_eq!(sender_username.as_deref(), Some("alice-name"));
            }
            _ => panic!("expected DmCreated"),
        }
    }

    /// The approval takeover is honoured only from the DS.
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

    /// #1431: the mute nudge is honoured only from the DS.
    #[test]
    fn channel_mutes_changed_is_honoured_only_from_the_ds() {
        let payload = serde_json::json!({ "type": "channel_mutes_changed" });
        let events = run(payload.clone(), None);
        assert!(matches!(events[..], [RealtimeEvent::ChannelMutesChanged]));
        let someone = PacketSender {
            user_id: "alice".into(),
            username: None,
        };
        assert!(run(payload, Some(&someone)).is_empty());
    }

    /// #1096: the server's copy of the verification code is dropped, or a DS
    /// swapping the ephemeral key and its stored code together would pass.
    #[test]
    fn an_enrollment_takeover_carries_no_server_supplied_code() {
        let events = run(
            serde_json::json!({
                "type": "enrollment_requested",
                "request_id": "r1",
                "new_device_id": "d2",
                "verification_code": "ABCD2345",
            }),
            None,
        );
        let [RealtimeEvent::EnrollmentRequested { request_id, new_device_id, link_id }] = &events[..] else {
            panic!("expected exactly one EnrollmentRequested, got {} event(s)", events.len());
        };
        assert_eq!(request_id, "r1");
        assert_eq!(new_device_id, "d2");
        assert_eq!(link_id, &None, "a typed-code request carries no link id");
        // A packet with a code still dispatches: shipped DS packets send one.
        let serialized = serde_json::to_string(&events[0]).expect("serialize");
        assert!(
            !serialized.contains("ABCD2345"),
            "the server's code reached the UI event: {serialized}"
        );
    }

    /// A packet with no code still raises the takeover.
    #[test]
    fn an_enrollment_takeover_needs_no_code_in_the_packet() {
        let events = run(
            serde_json::json!({
                "type": "enrollment_requested",
                "request_id": "r1",
                "new_device_id": "d2",
            }),
            None,
        );
        assert_eq!(events.len(), 1, "a packet with no code must still raise the takeover");
    }

    /// A QR-link nudge keeps its `link_id`, or the UI asks for a code the new
    /// device never shows.
    #[test]
    fn a_qr_link_enrollment_nudge_keeps_its_link_id() {
        let events = run(
            serde_json::json!({
                "type": "enrollment_requested",
                "request_id": "r1",
                "new_device_id": "d2",
                "verification_code": "ABCD2345",
                "link_id": "link-1",
            }),
            None,
        );
        let [RealtimeEvent::EnrollmentRequested { link_id, .. }] = &events[..] else {
            panic!("expected exactly one EnrollmentRequested, got {} event(s)", events.len());
        };
        assert_eq!(link_id.as_deref(), Some("link-1"));
        let serialized = serde_json::to_value(&events[0]).expect("serialize");
        assert_eq!(serialized["link_id"], "link-1");

        for absent in [serde_json::Value::Null, serde_json::json!("")] {
            let events = run(
                serde_json::json!({
                    "type": "enrollment_requested",
                    "request_id": "r1",
                    "new_device_id": "d2",
                    "link_id": absent,
                }),
                None,
            );
            let [RealtimeEvent::EnrollmentRequested { link_id, .. }] = &events[..] else {
                panic!("expected exactly one EnrollmentRequested");
            };
            assert_eq!(link_id, &None);
        }
    }
}
