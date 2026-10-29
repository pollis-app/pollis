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
    /// New-message wake-up (§5): routing only; the sender comes from the MLS credential.
    NewMessage {
        channel_id: Option<String>,
        conversation_id: Option<String>,
    },
    /// Sent to a user's inbox room when a DM they belong to is created. `sender_username` names the requester.
    ///
    /// `sender_id` is the recipient's own id when another of their devices acted (#1410): refetch, no alert.
    DmCreated {
        conversation_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sender_username: Option<String>,
    },
    /// Sent to a user's inbox room when added to a group, or to a group room on membership change.
    /// `kind` decides whether to notify:
    /// - `Some("invite")`     — you've been invited to a group (ping/notify)
    /// - `Some("approval")`   — your join request was approved (silent — you asked for this)
    /// - `None` / other       — generic reconcile (silent — refetch only)
    MembershipChanged {
        conversation_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        /// Inviter and group name for the `invite` alert; absent otherwise.
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
    /// Sent to a group room when a join request is created; invalidates join requests, not members.
    JoinRequestsChanged {
        group_id: String,
    },
    /// Sent to a group or DM room when a message is edited.
    EditedMessage {
        channel_id: Option<String>,
        conversation_id: Option<String>,
        message_id: String,
    },
    /// Sent to a group room when an admin deletes a message; durable path is the `type='delete'` envelope.
    DeletedMessage {
        channel_id: Option<String>,
        conversation_id: Option<String>,
        message_id: String,
    },
    /// Sent to a user's inbox room when another of their devices awaits enrollment approval.
    ///
    /// Carries no verification code (#1096): the approver must type what the new device shows.
    ///
    /// `link_id` marks a QR device link (#1207, #1361): suppress the typed-code prompt. It can only withhold a prompt.
    EnrollmentRequested {
        request_id: String,
        new_device_id: String,
        link_id: Option<String>,
    },
    /// This account's channel mutes changed (#1431); re-read them over the signed channel.
    ChannelMutesChanged,
    /// Sent after a room's event stream recovers; missed events are not replayed, so resync.
    RealtimeReconnected {
        room_id: String,
    },
    /// Sent to the callee's personal inbox room when someone is calling them.
    /// `room_name` is the LiveKit room both sides will join on accept.
    CallInvite {
        call_id: String,
        room_name: String,
        caller_id: String,
        caller_username: String,
    },
    /// Sent when the caller hangs up before pickup, or the other side declines.
    CallCanceled {
        call_id: String,
    },
    /// Ephemeral typing signal, re-sent every few seconds; receivers age entries out on a TTL.
    Typing {
        channel_id: Option<String>,
        conversation_id: Option<String>,
        user_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        username: Option<String>,
        is_typing: bool,
    },
    /// Presence inferred from LiveKit room participation; no heartbeats.
    PresenceChanged {
        user_id: String,
        room_id: String,
        present: bool,
    },
    /// A peer's `account_id_pub` changed since the TOFU pin. Advisory; sends are not blocked.
    KeyChanged {
        peer_user_id: String,
        peer_identity_version: i64,
    },
    /// After a non-empty reconcile commit: refetch hint only; names no one (#1506).
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

    /// Arc so a Room can be cloned out without holding the lock across an await. Desktop only.
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
