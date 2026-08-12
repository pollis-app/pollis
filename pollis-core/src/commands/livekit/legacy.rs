use std::sync::Arc;

use crate::commands::mls::ds_livekit_token;
use crate::error::Result;
use crate::state::AppState;

// ── LiveKit token commands ─────────────────────────────────────────────────
//
// Minting moved server-side to the DS (#393): identity + display name are
// derived from the verified signer there, so the client-supplied `identity` /
// `display_name` args are ignored (kept for Tauri command-signature stability).

/// A minted LiveKit credential: the JWT and the server it is valid at.
///
/// These are returned together on purpose. A LiveKit JWT is only accepted by the
/// server that issued it, so handing back a bare token forces the caller to guess
/// the address — which is how the SFU address ended up compiled into every shipped
/// binary in the first place.
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
/// Deliberately not authoritative: it is only the fallback used when a
/// self-hosted DS leaves `LIVEKIT_URL` unset. Anything that dials a room should
/// use the `url` returned beside that room's token (`LivekitCredential`), because
/// only the issuing server will accept the JWT.
pub async fn get_livekit_url(state: &Arc<AppState>) -> Result<String> {
    Ok(state.config.livekit_url.clone())
}
