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
    /// (via invite acceptance or join-request approval).
    MembershipChanged {},
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
