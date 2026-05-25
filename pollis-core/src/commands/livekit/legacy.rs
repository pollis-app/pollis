use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

use super::jwt::{make_token, make_view_token};

// ── Legacy commands (kept for potential future use) ────────────────────────

pub async fn get_livekit_token(
    room_name: String,
    identity: String,
    display_name: String,
    state: &Arc<AppState>,
) -> Result<String> {
    make_token(&state.config, &room_name, &identity, &display_name)
}

/// Mint a subscribe-only, hidden JWT for the renderer-side livekit-client
/// view connection. See `make_view_token` for the grant shape.
pub async fn get_livekit_view_token(
    room_name: String,
    identity: String,
    display_name: String,
    state: &Arc<AppState>,
) -> Result<String> {
    make_view_token(&state.config, &room_name, &identity, &display_name)
}

pub async fn get_livekit_url(state: &Arc<AppState>) -> Result<String> {
    Ok(state.config.livekit_url.clone())
}
