use std::collections::HashSet;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
use std::collections::HashMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
#[cfg(not(any(target_os = "ios", target_os = "android")))]
use tokio::task::JoinHandle;

use crate::sink::EventSink;

/// Events pushed from the Rust backend to the frontend via an EventSink.
/// New variants can be added here as the app grows (e.g. AudioLevel for visualizers).
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RealtimeEvent {
    NewMessage {
        channel_id: Option<String>,
        conversation_id: Option<String>,
        sender_id: String,
        sender_username: Option<String>,
    },
    /// Sent to a user's personal inbox room when a DM channel is created
    /// and they are a member, so they can fetch it without refreshing.
    DmCreated {
        conversation_id: String,
    },
    /// Sent to a user's personal inbox room when they are added to a group
    /// (via invite acceptance or join-request approval), or to a group room
    /// when its membership changes for any other reason.
    ///
    /// `kind` discriminates the cause so the frontend can decide whether to
    /// raise a user-facing notification:
    /// - `Some("invite")`     — you've been invited to a group (ping/notify)
    /// - `Some("approval")`   — your join request was approved (silent — you asked for this)
    /// - `None` / other       — generic reconcile (silent — refetch only)
    MembershipChanged {
        conversation_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
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
    /// Sent to a group room (or DM room) when a message is edited, so recipients
    /// can invalidate their message cache without polling.
    EditedMessage {
        channel_id: Option<String>,
        conversation_id: Option<String>,
        message_id: String,
        sender_id: String,
    },
    /// Sent to a group room when an admin deletes another member's message,
    /// so connected clients soft-delete it from their cache immediately.
    /// Durable propagation still flows through the `type='delete'` envelope —
    /// this event is just an immediate nudge for online recipients.
    DeletedMessage {
        channel_id: Option<String>,
        conversation_id: Option<String>,
        message_id: String,
        deleted_by: String,
    },
    /// Sent to a user's personal inbox room when one of their OTHER devices
    /// has just posted a `device_enrollment_request` row and is waiting for
    /// approval. Interrupts the UI on every receiving device so the user can
    /// confirm (the verification code must match the other screen) or reject.
    EnrollmentRequested {
        request_id: String,
        new_device_id: String,
        verification_code: String,
    },
    /// Sent after a room's event stream recovers from a drop. The frontend
    /// uses this to resync state that may have changed during the outage
    /// (voice presence, etc.) since the event stream itself doesn't replay
    /// missed events.
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
    /// Sent to the callee's personal inbox room when the caller hangs up
    /// before pickup, or to either side when the other side declines.
    /// Frontends use this to clear the incoming-call slot.
    CallCanceled {
        call_id: String,
    },
    /// Ephemeral signal that a user is composing a message in the named
    /// channel/conversation. Senders re-emit `is_typing: true` every few
    /// seconds while still typing (and `false` on send/blur); receivers
    /// also age out stale entries on a TTL since this event is never
    /// persisted and a user dropping offline must clear naturally.
    Typing {
        channel_id: Option<String>,
        conversation_id: Option<String>,
        user_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        username: Option<String>,
        is_typing: bool,
    },
    /// Inferred online/offline derived from LiveKit room participation:
    /// emitted when a participant joins or leaves a room the local user is
    /// already subscribed to (groups / DMs / inbox). The frontend tracks
    /// per-user → set-of-rooms; the user is "online" while at least one
    /// room shows them as present. No heartbeats or explicit publishes —
    /// LiveKit's keep-alive does the work.
    PresenceChanged {
        user_id: String,
        room_id: String,
        present: bool,
    },
    /// A peer's `account_id_pub` has changed since the last TOFU pin
    /// (Signal-style "safety number changed"). Emitted by
    /// `check_and_pin_account_key` whenever it observes a mismatch.
    /// Advisory — sends are not blocked. The frontend uses this to
    /// surface an inline, dismissable banner in any open conversation
    /// with this peer so the user can re-verify out-of-band.
    KeyChanged {
        peer_user_id: String,
        peer_identity_version: i64,
    },
    /// Emitted after `reconcile_group_mls_impl` produces a non-empty
    /// commit (members or devices added/removed, with a corresponding
    /// epoch bump). The diff is split into user-level vs device-level
    /// transitions so the frontend can render inline "X joined / X
    /// added a device / X left" banners directly, without consulting
    /// any other state.
    ///
    /// Locally: published by the reconciling client to its own sink so
    /// its open channel view picks up the banner immediately.
    /// Remotely: also broadcast to the conversation's LiveKit room via
    /// `publish_to_room_server` so already-connected peers see the
    /// banner without needing to refetch.
    RosterChanged {
        conversation_id: String,
        epoch_before: u64,
        epoch_after: u64,
        /// `user_id`s who had no leaves in the MLS tree before this
        /// commit and now have at least one — i.e. "X joined".
        joined_user_ids: Vec<String>,
        /// `user_id`s whose last device was removed by this commit —
        /// i.e. "X left". If a user removed one device but still has
        /// another in the tree, they appear in `devices_removed` instead.
        left_user_ids: Vec<String>,
        /// `(user_id, device_id)` device additions for users who were
        /// already in the tree — i.e. "X added a device".
        devices_added: Vec<(String, String)>,
        /// `(user_id, device_id)` device removals for users still in
        /// the tree after this commit — i.e. "X removed a device".
        devices_removed: Vec<(String, String)>,
    },
}

/// Held in AppState behind an Arc<Mutex<_>>.
/// Owns the frontend event sink and all active LiveKit room connections.
pub struct LiveKitState {
    /// The sink used to push events to the frontend.
    /// Set once by `subscribe_realtime`; updated if the user logs out and back in.
    pub channel: Option<Arc<dyn EventSink<RealtimeEvent>>>,

    /// Active room connections keyed by room ID.
    /// Room is wrapped in Arc so it can be cloned out of the MutexGuard for
    /// publish operations without holding the lock across an await point.
    /// Desktop only — mobile uses the native LiveKit SDK, not the Rust one.
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    pub rooms: HashMap<String, (Arc<livekit::Room>, JoinHandle<()>)>,

    /// Room IDs currently being connected (between Room::connect call and map insertion).
    /// Prevents duplicate connections when connect_rooms is called concurrently.
    pub connecting: HashSet<String>,
}

impl LiveKitState {
    pub fn new() -> Self {
        Self {
            channel: None,
            #[cfg(not(any(target_os = "ios", target_os = "android")))]
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
