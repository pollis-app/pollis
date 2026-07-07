//! Pure builders for the LiveKit realtime signalling (wake-up) payloads.
//!
//! Kept out of the media-gated `livekit` module (mirroring `livekit_jwt`,
//! `commands/mod.rs`) so the exact JSON shape compiles — and unit-tests —
//! on every target, including the headless `--no-default-features` build
//! that swaps in `livekit_stub`. Both the real publisher (`livekit/publish.rs`)
//! and the mobile/headless stub (`livekit_stub.rs`) build their payloads here,
//! so the wire format has a single source of truth.
//!
//! Metadata minimization (`docs/metadata-minimization-design.md` §5): these
//! packets are only a **hint to fetch**. LiveKit forwards them in cleartext,
//! so they deliberately carry **no sender identity** — just enough to route
//! the recipient to the conversation to refresh. The recipient re-derives the
//! true sender from the MLS credential inside the decrypted envelope it then
//! fetches (§1, §2.1), so `sender_id` / `sender_username` / `deleted_by` in the
//! ping would be pure leakage with no functional need.

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
/// The actor (`deleted_by`) is re-derived from the authenticated `type='delete'`
/// tombstone envelope on ingest — never needed in the cleartext ping.
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

/// `roster_changed` wake-up broadcast to a conversation's LiveKit room. Carries
/// only the routing handle + epochs so already-connected peers refetch the
/// member list — the per-user `joined`/`left`/device id lists are deliberately
/// omitted from the cleartext broadcast (§5.3). The reconciling client still
/// emits those lists to its OWN local sink to render inline banners; remote
/// peers re-derive the diff from the authenticated MLS commit / member refetch.
pub fn roster_changed_payload(conversation_id: &str, epoch_before: u64, epoch_after: u64) -> Value {
    json!({
        "type": "roster_changed",
        "conversation_id": conversation_id,
        "epoch_before": epoch_before,
        "epoch_after": epoch_after,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The metadata-minimization invariant (§5): no realtime wake-up payload
    /// may carry a sender / actor identity or a membership id list. This test
    /// tries to find such a field in every builder and fails if one reappears.
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
}
