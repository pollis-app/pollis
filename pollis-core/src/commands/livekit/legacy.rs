use std::sync::Arc;

use crate::commands::mls::ds_livekit_token;
use crate::error::Result;
use crate::state::AppState;

// Minting is DS-side (#393); client `identity`/`display_name` args are ignored.

/// A minted LiveKit credential: the JWT and the server it is valid at.
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

/// Mint a subscribe-only, hidden JWT for the renderer-side livekit-client
/// view connection (screenshare receive) — the DS `view` kind.
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
/// Only a fallback; dial the `url` returned beside each token.
pub async fn get_livekit_url(state: &Arc<AppState>) -> Result<String> {
    Ok(state.config.livekit_url.clone())
}
