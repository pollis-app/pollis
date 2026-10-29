use std::sync::Arc;

use serde::Serialize;

use crate::commands::mls::ds_livekit_participants;
use crate::error::Result;
use crate::state::AppState;

use super::identity::{enrich_participants_with_avatars, internal_identity, ParticipantKind};

#[derive(Serialize)]
pub struct VoiceParticipantInfo {
    pub identity: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
}

/// Fetch a voice room's roster via the DS broker (#393); avatars added by the caller.
///
/// The DS pre-resolves (#836); identities must match the voice event path's.
async fn ds_room_roster(state: &Arc<AppState>, room: &str) -> Result<Vec<VoiceParticipantInfo>> {
    let resolved = ds_livekit_participants(state, room).await?;
    Ok(resolved
        .into_iter()
        .map(|r| {
            let kind = ParticipantKind::from_wire(r.kind.as_deref().unwrap_or("voice"));
            VoiceParticipantInfo {
                identity: internal_identity(&r.user_id, &r.identity, kind),
                name: r.name,
                avatar_url: None,
            }
        })
        .collect())
}

#[derive(Serialize)]
pub struct VoiceRoomCount {
    pub channel_id: String,
    pub count: usize,
}

/// Returns the participant count for each of the given voice channels by
/// asking LiveKit's RoomService. Channels with no active room return count=0.
///
/// Per-channel `ListParticipants`, not `ListRooms` (its count lags after leaves).
pub async fn list_voice_room_counts(
    channel_ids: Vec<String>,
    state: &Arc<AppState>,
) -> Result<Vec<VoiceRoomCount>> {
    if channel_ids.is_empty() {
        return Ok(vec![]);
    }

    let futs = channel_ids.iter().map(|id| {
        let id = id.clone();
        let st = Arc::clone(state);
        async move {
            let count = ds_room_roster(&st, &id).await.map(|v| v.len()).unwrap_or(0);
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
    let participants = ds_room_roster(state, &channel_id).await?;
    Ok(enrich_participants_with_avatars(state, participants).await)
}
