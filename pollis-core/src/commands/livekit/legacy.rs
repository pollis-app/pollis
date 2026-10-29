use std::sync::Arc;

use crate::commands::mls::ds_livekit_token;
use crate::error::Result;
use crate::state::AppState;

// Minted by the DS (#393) from the verified signer, so the `identity` /
// `display_name` args are ignored (kept for command-signature stability).
/// A minted LiveKit credential: the JWT and the server it is valid at.
///
/// Returned together: a LiveKit JWT is only accepted by the server that issued
/// it.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LivekitCredential {
    pub token: String,
    pub url: String,
}

pub async fn get_livekit_token(
    room_name: String,
    _identity: String,
    _display_name: String,
    state: &Arc<AppState>,
) -> Result<LivekitCredential> {
    let (token, url) = ds_livekit_token(state, &room_name, "realtime").await?;
    Ok(LivekitCredential { token, url })
}

/// A subscribe-only, hidden token for the renderer's screenshare-receive
/// connection (the DS `view` kind).
pub async fn get_livekit_view_token(
    room_name: String,
    _identity: String,
    _display_name: String,
    state: &Arc<AppState>,
) -> Result<LivekitCredential> {
    let (token, url) = ds_livekit_token(state, &room_name, "view").await?;
    Ok(LivekitCredential { token, url })
}

/// The compiled-in DEFAULT LiveKit address.
///
/// Only the fallback for a self-hosted DS without `LIVEKIT_URL`; dial the `url`
/// returned beside a room's token instead.
pub async fn get_livekit_url(state: &Arc<AppState>) -> Result<String> {
    Ok(state.config.livekit_url.clone())
}
