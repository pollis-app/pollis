use std::sync::Arc;

use crate::commands::mls::ds_livekit_token;
use crate::error::Result;
use crate::state::AppState;

// ── LiveKit token commands ─────────────────────────────────────────────────
//
// Minting moved server-side to the DS (#393): identity + display name are
// derived from the verified signer there, so the client-supplied `identity` /
// `display_name` args are ignored (kept for Tauri command-signature stability).

pub async fn get_livekit_token(
    room_name: String,
    _identity: String,
    _display_name: String,
    state: &Arc<AppState>,
) -> Result<String> {
    let (token, _url) = ds_livekit_token(state, &room_name, "realtime").await?;
    Ok(token)
}

/// Mint a subscribe-only, hidden JWT for the renderer-side livekit-client
/// view connection (screenshare receive) — the DS `view` kind.
pub async fn get_livekit_view_token(
    room_name: String,
    _identity: String,
    _display_name: String,
    state: &Arc<AppState>,
) -> Result<String> {
    let (token, _url) = ds_livekit_token(state, &room_name, "view").await?;
    Ok(token)
}

pub async fn get_livekit_url(state: &Arc<AppState>) -> Result<String> {
    Ok(state.config.livekit_url.clone())
}
