//! Pure builders for the LiveKit realtime signalling (wake-up) payloads.
//!
//! Outside the media gate so the wire format compiles and is tested everywhere.
//!
//! §5: packets are cleartext fetch hints, so they carry **no sender identity**.

use serde_json::{json, Value};

/// `new_message` wake-up: "conversation X has a new message — refresh it".
/// No sender: the client attributes the message from the decrypted envelope.
pub fn new_message_payload(channel_id: Option<&str>, conversation_id: Option<&str>) -> Value {
    json!({
        "type": "new_message",
        "channel_id": channel_id,
        "conversation_id": conversation_id,
    })
}

/// `edited_message` wake-up: which message in which conversation changed.
/// The editor is re-derived from the durable edit envelope on ingest.
pub fn edited_message_payload(
    channel_id: Option<&str>,
    conversation_id: Option<&str>,
    message_id: &str,
) -> Value {
    json!({
        "type": "edited_message",
        "channel_id": channel_id,
        "conversation_id": conversation_id,
        "message_id": message_id,
    })
}

/// `deleted_message` wake-up: which message in which conversation was removed.
/// The actor is re-derived from the `delete` tombstone on ingest.
pub fn deleted_message_payload(
    channel_id: Option<&str>,
    conversation_id: Option<&str>,
    message_id: &str,
) -> Value {
    json!({
        "type": "deleted_message",
        "channel_id": channel_id,
        "conversation_id": conversation_id,
        "message_id": message_id,
    })
}

/// `membership_changed` wake-up: the named group's membership changed, refetch.
pub fn membership_changed_payload(group_id: &str) -> Value {
    json!({
        "type": "membership_changed",
        "group_id": group_id,
    })
}

/// `membership_changed` wake-up for a member who LEFT — voluntarily, or by
/// deleting their account.
///
/// `kind: "leave"` is load-bearing (#1081): a leaver can't remove its own leaf,
/// so a device still online must stage the remove. Names no one (§5.3).
pub fn member_left_group_payload(group_id: &str) -> Value {
    json!({
        "type": "membership_changed",
        "group_id": group_id,
        "kind": LEAVE_KIND,
    })
}

/// [`member_left_group_payload`] for a DM / conversation-keyed room.
pub fn member_left_conversation_payload(conversation_id: &str) -> Value {
    json!({
        "type": "membership_changed",
        "conversation_id": conversation_id,
        "kind": LEAVE_KIND,
    })
}

/// The `kind` that marks a `membership_changed` wake-up as a leave. Declared
/// once so the emitters and the handler that reconciles on it cannot drift.
pub const LEAVE_KIND: &str = "leave";

/// `roster_changed` wake-up: routing handle + epochs only, no id lists (§5.3).
pub fn roster_changed_payload(conversation_id: &str, epoch_before: u64, epoch_after: u64) -> Value {
    json!({
        "type": "roster_changed",
        "conversation_id": conversation_id,
        "epoch_before": epoch_before,
        "epoch_after": epoch_after,
    })
}

/// `typing` broadcast to a conversation's LiveKit room.
///
/// Shared-room, so identity-free (§5, #836): the typist is the publishing participant.
pub fn typing_payload(
    channel_id: Option<&str>,
    conversation_id: Option<&str>,
    is_typing: bool,
) -> Value {
    json!({
        "type": "typing",
        "channel_id": channel_id,
        "conversation_id": conversation_id,
        "is_typing": is_typing,
    })
}

/// `voice_joined` / `voice_left` broadcast to a group's LiveKit room: "the voice
/// roster for this channel changed — refetch it".
///
/// Identity-free like [`typing_payload`].
pub fn voice_presence_payload(channel_id: &str, joined: bool) -> Value {
    json!({
        "type": if joined { "voice_joined" } else { "voice_left" },
        "channel_id": channel_id,
    })
}

/// `device_revoked` nudge sent to a user's own inbox room: "one of your devices
/// was revoked — every device re-check whether it is still registered".
///
/// Carries nothing: recipients re-check authoritatively; `user_id` would undo #828.
pub fn device_revoked_payload() -> Value {
    json!({ "type": "device_revoked" })
}

/// `dm_created` to a PRIVATE inbox room; MAY carry the creator's public username (#396).
///
/// Advisory: the DS stamps the verified signer, and only the stamp is read.
pub fn dm_created_inbox_payload(conversation_id: &str, sender_username: Option<&str>) -> Value {
    json!({
        "type": "dm_created",
        "conversation_id": conversation_id,
        "sender_username": sender_username,
    })
}

/// Invite wake-up to the invitee's PRIVATE inbox; MAY name inviter and group (#396).
///
/// Both strings are advisory; the DS replaces them with verified values.
pub fn group_invite_inbox_payload(
    group_id: &str,
    inviter_username: Option<&str>,
    group_name: Option<&str>,
) -> Value {
    json!({
        "type": "membership_changed",
        "group_id": group_id,
        "kind": "invite",
        "inviter_username": inviter_username,
        "group_name": group_name,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §5 invariant: no wake-up payload carries an actor identity or member id list.
    fn assert_no_identity(payload: &Value) {
        let obj = payload.as_object().expect("payload is a JSON object");
        for leaky in [
            "sender_id",
            "sender_username",
            "deleted_by",
            "user_id",
            "username",
            "joined_user_ids",
            "left_user_ids",
            "devices_added",
            "devices_removed",
        ] {
            assert!(
                !obj.contains_key(leaky),
                "signalling payload leaks identifying field `{leaky}`: {payload}"
            );
        }
    }

    #[test]
    fn new_message_ping_carries_no_sender() {
        let p = new_message_payload(Some("chan-1"), Some("conv-1"));
        assert_eq!(p["type"], "new_message");
        assert_eq!(p["conversation_id"], "conv-1");
        assert_no_identity(&p);
    }

    #[test]
    fn edited_message_ping_carries_no_sender() {
        let p = edited_message_payload(None, Some("conv-1"), "msg-1");
        assert_eq!(p["type"], "edited_message");
        assert_eq!(p["message_id"], "msg-1");
        assert_no_identity(&p);
    }

    #[test]
    fn deleted_message_ping_carries_no_actor() {
        let p = deleted_message_payload(Some("chan-1"), None, "msg-1");
        assert_eq!(p["type"], "deleted_message");
        assert_eq!(p["message_id"], "msg-1");
        assert_no_identity(&p);
    }

    /// A leave wake-up must carry the kind the handler reconciles on — and still
    /// name nobody (§5.3): which member left is re-derived from the commit.
    #[test]
    fn leave_pings_carry_the_kind_and_no_leaver() {
        let g = member_left_group_payload("group-1");
        assert_eq!(g["type"], "membership_changed");
        assert_eq!(g["group_id"], "group-1");
        assert_eq!(g["kind"], LEAVE_KIND);
        assert_no_identity(&g);

        let d = member_left_conversation_payload("dm-1");
        assert_eq!(d["type"], "membership_changed");
        assert_eq!(d["conversation_id"], "dm-1");
        assert_eq!(d["kind"], LEAVE_KIND);
        assert_no_identity(&d);
    }

    /// #836: the typing broadcast must carry no identity.
    #[test]
    fn typing_broadcast_carries_no_typist() {
        let p = typing_payload(Some("chan-1"), None, true);
        assert_eq!(p["type"], "typing");
        assert_eq!(p["channel_id"], "chan-1");
        assert_eq!(p["is_typing"], true);
        assert_no_identity(&p);
        // `display_name` is not in the shared blocklist (it is legitimate on the
        // inbox payloads); pin it here, where it is not.
        assert!(p.as_object().unwrap().get("display_name").is_none());
    }

    /// Same for the voice roster nudge, which carried `user_id` + `display_name`.
    #[test]
    fn voice_presence_broadcast_carries_no_actor() {
        for joined in [true, false] {
            let p = voice_presence_payload("chan-1", joined);
            assert_eq!(p["type"], if joined { "voice_joined" } else { "voice_left" });
            assert_eq!(p["channel_id"], "chan-1");
            assert_no_identity(&p);
            assert!(p.as_object().unwrap().get("display_name").is_none());
        }
    }

    /// The nudge must stay empty: `user_id` here would undo #828's pseudonymous
    /// inbox room name, and `device_id` was never read by anyone.
    #[test]
    fn device_revoked_nudge_is_empty() {
        let p = device_revoked_payload();
        assert_eq!(p["type"], "device_revoked");
        assert_no_identity(&p);
        assert!(p.as_object().unwrap().get("device_id").is_none());
        assert_eq!(p.as_object().unwrap().len(), 1, "nudge must carry only its type: {p}");
    }

    #[test]
    fn membership_changed_ping_is_routing_only() {
        let p = membership_changed_payload("group-1");
        assert_eq!(p["type"], "membership_changed");
        assert_eq!(p["group_id"], "group-1");
        assert_no_identity(&p);
    }

    #[test]
    fn roster_changed_broadcast_drops_id_lists() {
        let p = roster_changed_payload("conv-1", 4, 5);
        assert_eq!(p["type"], "roster_changed");
        assert_eq!(p["conversation_id"], "conv-1");
        assert_eq!(p["epoch_after"], 5);
        assert_no_identity(&p);
    }

    /// Private-inbox pings DO carry a public username (the §5 exception); keep it.
    #[test]
    fn dm_created_inbox_ping_carries_sender_username() {
        let p = dm_created_inbox_payload("conv-1", Some("alice"));
        assert_eq!(p["type"], "dm_created");
        assert_eq!(p["conversation_id"], "conv-1");
        assert_eq!(p["sender_username"], "alice");
    }

    #[test]
    fn dm_created_inbox_ping_tolerates_missing_username() {
        let p = dm_created_inbox_payload("conv-1", None);
        assert!(p["sender_username"].is_null());
    }

    #[test]
    fn group_invite_inbox_ping_carries_inviter_and_group() {
        let p = group_invite_inbox_payload("group-1", Some("bob"), Some("Design"));
        assert_eq!(p["type"], "membership_changed");
        assert_eq!(p["group_id"], "group-1");
        assert_eq!(p["kind"], "invite");
        assert_eq!(p["inviter_username"], "bob");
        assert_eq!(p["group_name"], "Design");
    }
}
