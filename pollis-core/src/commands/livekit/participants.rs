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

/// A voice room's roster via the DS broker (#393), already resolved (#836).
/// The internal identity must match what the voice event path emits for the
/// same peer, or the roster and the live session disagree. Avatars are the
/// caller's job.
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

/// The participant count for each voice channel (0 with no active room).
/// `ListParticipants` per channel rather than `ListRooms`, whose
/// `numParticipants` lags for seconds after the last leave, so the sidebar
/// count always matches `list_voice_participants`.
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

/// The participants in a voice channel, with avatars.
pub async fn list_voice_participants(
    channel_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<VoiceParticipantInfo>> {
    let participants = ds_room_roster(state, &channel_id).await?;
    Ok(enrich_participants_with_avatars(state, participants).await)
}
