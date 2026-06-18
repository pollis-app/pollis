//! Pure-JWT LiveKit token minting — **always compiled, all targets**.
//!
//! The rest of the LiveKit integration (`commands::livekit`) pulls in the
//! native `livekit`/`libwebrtc` stack and is desktop-only (mobile swaps in
//! `livekit_stub`). But minting an access token is pure `jsonwebtoken` with
//! no native dependency, and mobile needs it: the JS LiveKit SDK
//! (`@livekit/react-native`) connects to the same SFU rooms in data-only
//! mode and asks the Rust core for a token via the `get_livekit_token`
//! bridge command. So the token helpers live here, outside the platform
//! gate, and the desktop `livekit/jwt.rs` simply re-exports them.

use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, Result};

// ── JWT helpers ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
struct LiveKitClaims {
    iss: String,
    sub: String,
    iat: u64,
    exp: u64,
    nbf: u64,
    video: VideoGrants,
    name: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct VideoGrants {
    room: String,
    room_join: bool,
    can_publish: bool,
    can_subscribe: bool,
    can_publish_data: bool,
    /// LiveKit JWT field for "hidden participant": the server still routes
    /// tracks to this client but does not include it in any room roster
    /// returned to other peers. Used by the JS-side screenshare view client
    /// so it doesn't appear in the participant list.
    #[serde(skip_serializing_if = "Option::is_none")]
    hidden: Option<bool>,
}

pub(crate) fn make_token(
    config: &Config,
    room_name: &str,
    identity: &str,
    display_name: &str,
) -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Error::Other(anyhow::anyhow!("{e}")))?
        .as_secs();

    let claims = LiveKitClaims {
        iss: config.livekit_api_key.clone(),
        sub: identity.to_string(),
        iat: now,
        exp: now + 3600,
        nbf: now,
        name: display_name.to_string(),
        video: VideoGrants {
            room: room_name.to_string(),
            room_join: true,
            can_publish: true,
            can_subscribe: true,
            can_publish_data: true,
            hidden: None,
        },
    };

    let mut header = Header::new(Algorithm::HS256);
    header.typ = Some("JWT".to_string());
    let key = EncodingKey::from_secret(config.livekit_api_secret.as_bytes());
    encode(&header, &claims, &key)
        .map_err(|e| Error::Other(anyhow::anyhow!("JWT sign: {e}")))
}

/// Mints a JWT for the renderer-side livekit-client view connection
/// under Electron. The JS client opens a second connection to the same
/// voice room as `${userId}:view` to receive remote screen-share video
/// and publish its own.
///
/// NOT hidden. `hidden: true` looks like the right knob — "keep this
/// participant out of other clients' rosters" — but it has a fatal side
/// effect: LiveKit's SFU refuses to ROUTE tracks from a hidden
/// participant to other clients, so the publisher's screenshare track
/// never reaches receivers. We instead keep the participant visible at
/// the LiveKit level and filter `:view` identities out of the UI roster
/// in `list_voice_participants` (see the `.ends_with(":view")` filter
/// there) — same end result for the UI, without breaking track routing.
#[cfg_attr(any(target_os = "ios", target_os = "android"), allow(dead_code))]
pub(crate) fn make_view_token(
    config: &Config,
    room_name: &str,
    identity: &str,
    display_name: &str,
) -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Error::Other(anyhow::anyhow!("{e}")))?
        .as_secs();

    let claims = LiveKitClaims {
        iss: config.livekit_api_key.clone(),
        sub: identity.to_string(),
        iat: now,
        exp: now + 3600,
        nbf: now,
        name: display_name.to_string(),
        video: VideoGrants {
            room: room_name.to_string(),
            room_join: true,
            can_publish: true,
            can_subscribe: true,
            can_publish_data: false,
            hidden: None,
        },
    };

    let mut header = Header::new(Algorithm::HS256);
    header.typ = Some("JWT".to_string());
    let key = EncodingKey::from_secret(config.livekit_api_secret.as_bytes());
    encode(&header, &claims, &key)
        .map_err(|e| Error::Other(anyhow::anyhow!("JWT sign: {e}")))
}
