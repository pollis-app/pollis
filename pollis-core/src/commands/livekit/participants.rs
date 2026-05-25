use std::sync::Arc;

use serde::Serialize;

use crate::error::Result;
use crate::state::AppState;

use super::admin_api::room_service_list_participants;
use super::identity::enrich_participants_with_avatars;

// ── Voice participant listing ──────────────────────────────────────────────

#[derive(Serialize)]
pub struct VoiceParticipantInfo {
    pub identity: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
}

#[derive(Serialize)]
pub struct VoiceRoomCount {
    pub channel_id: String,
    pub count: usize,
}

/// Returns the participant count for each of the given voice channels by
/// asking LiveKit's RoomService. Channels with no active room return count=0.
///
/// We call `ListParticipants` per channel instead of `ListRooms` because
/// `ListRooms.numParticipants` can lag behind `ListParticipants` for several
/// seconds after the last participant disconnects — the room lingers with a
/// stale count until its `empty_timeout` fires. Using the same source as
/// `list_voice_participants` guarantees the sidebar count and the member
/// list never disagree.
pub async fn list_voice_room_counts(
    channel_ids: Vec<String>,
    state: &Arc<AppState>,
) -> Result<Vec<VoiceRoomCount>> {
    if channel_ids.is_empty() {
        return Ok(vec![]);
    }

    let futs = channel_ids.iter().map(|id| {
        let id = id.clone();
        let config = state.config.clone();
        async move {
            let count = room_service_list_participants(&config, &id)
                .await
                .map(|v| v.len())
                .unwrap_or(0);
            VoiceRoomCount { channel_id: id, count }
        }
    });
    Ok(futures_util::future::join_all(futs).await)
}

/// Returns the participants in a voice channel by asking LiveKit's RoomService.
pub async fn list_voice_participants(
    channel_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<VoiceParticipantInfo>> {
    let participants = room_service_list_participants(&state.config, &channel_id).await?;
    Ok(enrich_participants_with_avatars(&state, participants).await)
}
