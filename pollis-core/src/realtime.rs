use std::collections::HashSet;
#[cfg(feature = "media")]
use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
#[cfg(feature = "media")]
use tokio::task::JoinHandle;

use crate::sink::EventSink;

/// Events pushed from the Rust backend to the frontend via an EventSink.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RealtimeEvent {
    /// A new-message wake-up: routing only, no sender; the recipient derives
    /// the sender from the MLS credential of the envelope it fetches.
    NewMessage {
        channel_id: Option<String>,
        conversation_id: Option<String>,
    },
    /// Sent to a user's inbox room when a DM channel they belong to is created.
    ///
    /// `sender_username` names the requester for the alert (a DM request is
    /// pre-MLS-join, so there is no credential to derive it from). `sender_id`
    /// is the DS-stamped actor; when it is the recipient's own id, another of
    /// their devices made the DM (#1410) and it is a refetch, not an alert.
    DmCreated {
        conversation_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_username: Option<String>,
    },
    /// Sent to a user's inbox room when they are added to a group, or to a
    /// group room when its membership changes.
    ///
    /// `kind`: `Some("invite")` notifies; `Some("approval")` and anything else
    /// are silent refetches.
    MembershipChanged {
        conversation_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        /// Inviter's username and the group name, on the `invite` kind only.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        inviter_username: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group_name: Option<String>,
    },
    /// Sent to a group room when a user joins a voice channel in that group.
    VoiceJoined {
        channel_id: String,
        user_id: String,
        display_name: String,
    },
    /// Sent to a group room when a user leaves a voice channel in that group.
    VoiceLeft {
        channel_id: String,
        user_id: String,
    },
    /// Sent to a group room when a member's role changes (admin ↔ member).
    MemberRoleChanged {
        group_id: String,
    },
    /// Sent to a group room when a join request is created, so admins refetch
    /// the join-request list (not the member list).
    JoinRequestsChanged {
        group_id: String,
    },
    /// Sent to a group or DM room when a message is edited.
    EditedMessage {
        channel_id: Option<String>,
        conversation_id: Option<String>,
        message_id: String,
    },
    /// Sent to a group room when an admin deletes another member's message.
    /// An immediate nudge only; the `type='delete'` envelope is the durable path.
    DeletedMessage {
        channel_id: Option<String>,
        conversation_id: Option<String>,
        message_id: String,
    },
    /// Sent to a user's inbox room when another of their devices awaits
    /// enrollment approval.
    ///
    /// Carries no verification code (#1096): the approver must type what the
    /// new device shows. `link_id` marks a QR device link (#1207, #1361), which
    /// shows no code, so the UI must not raise the typed-code prompt; it can
    /// only withhold a prompt, never approve anything.
    EnrollmentRequested {
        request_id: String,
        new_device_id: String,
        link_id: Option<String>,
    },
    /// This account's channel mutes changed on one of its devices (#1431);
    /// re-read them over the signed channel.
    ChannelMutesChanged,
    /// Sent after a room's event stream recovers from a drop, so the frontend
    /// resyncs state (voice presence etc.); missed events are not replayed.
    RealtimeReconnected {
        room_id: String,
    },
    /// Sent to the callee's inbox room; `room_name` is the LiveKit room to join.
    CallInvite {
        call_id: String,
        room_name: String,
        caller_id: String,
        caller_username: String,
    },
    /// Caller hung up before pickup, or the other side declined.
    CallCanceled {
        call_id: String,
    },
    /// Ephemeral typing signal. Senders re-emit `true` every few seconds;
    /// receivers age entries out on a TTL, as the event is never persisted.
    Typing {
        channel_id: Option<String>,
        conversation_id: Option<String>,
        user_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        username: Option<String>,
        is_typing: bool,
    },
    /// Presence inferred from LiveKit room participation; a user is online while
    /// at least one subscribed room shows them. No heartbeats.
    PresenceChanged {
        user_id: String,
        room_id: String,
        present: bool,
    },
    /// A peer's `account_id_pub` changed since the last TOFU pin. Advisory:
    /// sends are not blocked; the frontend shows a re-verify banner.
    KeyChanged {
        peer_user_id: String,
        peer_identity_version: i64,
    },
    /// A reconcile produced a non-empty commit. A refetch hint only: it names no
    /// one, since membership changes are never announced in a timeline (#1506).
    /// Also broadcast to the conversation's LiveKit room.
    RosterChanged {
        conversation_id: String,
        epoch_before: u64,
        epoch_after: u64,
    },
}

/// Owns the frontend event sink and all active LiveKit room connections.
pub struct LiveKitState {
    /// Set by `subscribe_realtime`; replaced on re-login.
    pub channel: Option<Arc<dyn EventSink<RealtimeEvent>>>,

    /// Active rooms by ID. `Arc` so a room can be cloned out of the guard
    /// without holding the lock across an await. Desktop only (`media`);
    /// mobile uses the native LiveKit SDK.
    #[cfg(feature = "media")]
    pub rooms: HashMap<String, (Arc<livekit::Room>, JoinHandle<()>)>,

    /// Rooms mid-connect, so concurrent `connect_rooms` calls don't duplicate.
    pub connecting: HashSet<String>,
}

impl LiveKitState {
    pub fn new() -> Self {
        Self {
            channel: None,
            #[cfg(feature = "media")]
            rooms: HashMap::new(),
            connecting: HashSet::new(),
        }
    }
}

impl Default for LiveKitState {
    fn default() -> Self {
        Self::new()
    }
}
