//! Pure builders for the LiveKit realtime wake-up payloads, the single source of
//! the wire format for both `livekit/publish.rs` and `livekit_stub.rs`.
//!
//! Metadata minimization (`docs/metadata-minimization-design.md` §5): LiveKit
//! forwards these in cleartext, so shared-room payloads are only a hint to fetch
//! and carry no sender identity; the recipient derives the sender from the
//! decrypted MLS envelope it then fetches.

use serde_json::{json, Value};

/// `new_message` wake-up. No sender: attribution comes from the envelope.
pub fn new_message_payload(channel_id: Option<&str>, conversation_id: Option<&str>) -> Value {
    json!({
        "type": "new_message",
        "channel_id": channel_id,
        "conversation_id": conversation_id,
    })
}

/// `edited_message` wake-up; the editor comes from the edit envelope on ingest.
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

/// `deleted_message` wake-up; the actor comes from the `type='delete'` tombstone.
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

/// `membership_changed` wake-up: refetch the group's membership.
pub fn membership_changed_payload(group_id: &str) -> Value {
    json!({
        "type": "membership_changed",
        "group_id": group_id,
    })
}

/// `membership_changed` wake-up for a member who left (or deleted their account).
///
/// `kind: "leave"` is load-bearing (#1081): MLS forbids removing your own leaf, so
/// an online member must stage the remove on receipt. It names no one; who left
/// is re-derived from the commit and roster refetch (§5.3).
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

/// The `kind` that marks a `membership_changed` wake-up as a leave.
pub const LEAVE_KIND: &str = "leave";

/// `roster_changed` wake-up: routing handle and epochs only, no member or device
/// id lists (§5.3).
pub fn roster_changed_payload(conversation_id: &str, epoch_before: u64, epoch_after: u64) -> Value {
    json!({
        "type": "roster_changed",
        "conversation_id": conversation_id,
        "epoch_before": epoch_before,
        "epoch_after": epoch_after,
    })
}

/// `typing` broadcast to a conversation's room. No `user_id`/`username`: the SFU
/// knows the publishing participant, so an id in the body would unmask the
/// per-room pseudonym (#836). The recipient resolves the typist from the
/// publishing participant via the DS.
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

/// `voice_joined` / `voice_left` broadcast: refetch this channel's voice roster.
/// Identity-free for the same reason as [`typing_payload`].
pub fn voice_presence_payload(channel_id: &str, joined: bool) -> Value {
    json!({
        "type": if joined { "voice_joined" } else { "voice_left" },
        "channel_id": channel_id,
    })
}

/// `device_revoked` nudge to a user's own inbox room: every device re-checks its
/// registration. Carries nothing; a `user_id` would re-identify the
/// pseudonymous inbox room (#828).
pub fn device_revoked_payload() -> Value {
    json!({ "type": "device_revoked" })
}

/// `dm_created` wake-up to the recipient's private, single-subscriber inbox room.
/// May carry the creator's public username so the alert can name them (#396); the
/// §5 routing-only rule is for shared rooms. Advisory: the DS replaces identity
/// fields with the verified signer before fan-out.
pub fn dm_created_inbox_payload(conversation_id: &str, sender_username: Option<&str>) -> Value {
    json!({
        "type": "dm_created",
        "conversation_id": conversation_id,
        "sender_username": sender_username,
    })
}

/// `membership_changed` / `kind: "invite"` wake-up to the invitee's private inbox.
/// May carry the inviter's username and the group name (#396); both are replaced
/// by the DS with verified values. Unlike [`membership_changed_payload`], which
/// must never carry identity.
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

    /// The §5 invariant: no shared-room wake-up carries an actor identity or member id
    /// list.
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

    #[test]
    fn typing_broadcast_carries_no_typist() {
        let p = typing_payload(Some("chan-1"), None, true);
        assert_eq!(p["type"], "typing");
        assert_eq!(p["channel_id"], "chan-1");
        assert_eq!(p["is_typing"], true);
        assert_no_identity(&p);
        // `display_name` is legitimate on inbox payloads, so it's not in the shared
        // blocklist; pin it here.
        assert!(p.as_object().unwrap().get("display_name").is_none());
    }

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

    /// Inbox pings are the deliberate exception to §5: single-subscriber room, public
    /// username. Pinned so a future sweep doesn't strip it.
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
