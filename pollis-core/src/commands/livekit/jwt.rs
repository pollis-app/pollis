use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, Result};

// ── JWT helpers ────────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub(super) struct LiveKitClaims {
    pub iss: String,
    pub sub: String,
    pub iat: u64,
    pub exp: u64,
    pub nbf: u64,
    pub video: VideoGrants,
    pub name: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct VideoGrants {
    pub room: String,
    pub room_join: bool,
    pub can_publish: bool,
    pub can_subscribe: bool,
    pub can_publish_data: bool,
    /// LiveKit JWT field for "hidden participant": the server still routes
    /// tracks to this client but does not include it in any room roster
    /// returned to other peers. Used by the JS-side screenshare view client
    /// so it doesn't appear in the participant list.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hidden: Option<bool>,
}

pub(crate) fn make_token(config: &Config, room_name: &str, identity: &str, display_name: &str) -> Result<String> {
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
