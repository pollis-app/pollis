use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::task::JoinHandle;

/// Events pushed from the Rust backend to the frontend via a Tauri Channel.
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
}

/// Held in AppState behind an Arc<Mutex<_>>.
/// Owns the frontend channel handle and all active LiveKit room connections.
pub struct LiveKitState {
    /// The Tauri Channel used to push events to the frontend.
    /// Set once by `subscribe_realtime`; updated if the user logs out and back in.
    pub channel: Option<tauri::ipc::Channel<RealtimeEvent>>,

    /// Active room connections keyed by room ID.
    /// Room is wrapped in Arc so it can be cloned out of the MutexGuard for
    /// publish operations without holding the lock across an await point.
    pub rooms: HashMap<String, (Arc<livekit::Room>, JoinHandle<()>)>,

    /// Room IDs currently being connected (between Room::connect call and map insertion).
    /// Prevents duplicate connections when connect_rooms is called concurrently.
    pub connecting: HashSet<String>,
}

impl LiveKitState {
    pub fn new() -> Self {
        Self {
            channel: None,
            rooms: HashMap::new(),
            connecting: HashSet::new(),
        }
    }
}
