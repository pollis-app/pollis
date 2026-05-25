use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, Result};

use super::participants::VoiceParticipantInfo;

// ── LiveKit server (RoomService) API ───────────────────────────────────────
//
// The server API is a separate Twirp-over-HTTPS endpoint from the WebSocket
// URL used by the client SDK. We talk to it directly with reqwest rather
// than pulling in the `livekit-api` crate. This is the source of truth for
// "who is in a voice room right now" — our own DB used to shadow this state
// but that's been removed since LiveKit itself already tracks it and keeps
// it consistent across crashes, force-kills, and bad network.

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AdminGrants {
    pub room_admin: bool,
    pub room_list: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub room: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct AdminClaims {
    pub iss: String,
    pub sub: String,
    pub iat: u64,
    pub exp: u64,
    pub nbf: u64,
    pub video: AdminGrants,
}

pub(super) fn make_admin_token(config: &Config, room: Option<&str>) -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Error::Other(anyhow::anyhow!("{e}")))?
        .as_secs();
    let claims = AdminClaims {
        iss: config.livekit_api_key.clone(),
        sub: "pollis-backend".to_string(),
        iat: now,
        exp: now + 300,
        nbf: now,
        video: AdminGrants {
            room_admin: true,
            room_list: true,
            room: room.map(str::to_string),
        },
    };
    let mut header = Header::new(Algorithm::HS256);
    header.typ = Some("JWT".to_string());
    let key = EncodingKey::from_secret(config.livekit_api_secret.as_bytes());
    encode(&header, &claims, &key)
        .map_err(|e| Error::Other(anyhow::anyhow!("JWT sign: {e}")))
}

pub(super) fn twirp_base(livekit_url: &str) -> String {
    if let Some(rest) = livekit_url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = livekit_url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        livekit_url.to_string()
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct RsParticipantsResp {
    #[serde(default)]
    pub participants: Vec<RsParticipant>,
}

#[derive(Debug, Deserialize)]
pub(super) struct RsParticipant {
    #[serde(default)]
    pub identity: String,
    #[serde(default)]
    pub name: String,
}

pub(super) async fn room_service_list_participants(
    config: &Config,
    room: &str,
) -> Result<Vec<VoiceParticipantInfo>> {
    if config.livekit_url.is_empty() || config.livekit_api_key.is_empty() {
        return Ok(vec![]);
    }
    let token = make_admin_token(config, Some(room))?;
    let url = format!(
        "{}/twirp/livekit.RoomService/ListParticipants",
        twirp_base(&config.livekit_url)
    );
    let resp = reqwest::Client::new()
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({ "room": room }))
        .send()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ListParticipants http: {e}")))?;
    let status = resp.status();
    if !status.is_success() {
        // 404 from LiveKit means the room doesn't exist yet (no one has joined)
        // — treat as empty rather than an error so the UI just shows no voice
        // participants instead of an alert.
        if status == reqwest::StatusCode::NOT_FOUND {
            return Ok(vec![]);
        }
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::Other(anyhow::anyhow!(
            "ListParticipants {status}: {body}"
        )));
    }
    let parsed: RsParticipantsResp = resp
        .json()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("ListParticipants decode: {e}")))?;
    Ok(parsed
        .participants
        .into_iter()
        // Filter out internal "server" participants used for data-channel
        // fanout, and the renderer-side `:view` clients used for screen-share
        // receive (Phase 6) — those represent the same physical user as the
        // matching `voice-<id>` participant and would otherwise dup the
        // sidebar list.
        .filter(|p| {
            p.identity != "server"
                && p.identity != "pollis-backend"
                && !p.identity.ends_with(":view")
        })
        .map(|p| VoiceParticipantInfo {
            name: if p.name.is_empty() {
                p.identity.clone()
            } else {
                p.name.clone()
            },
            identity: p.identity,
            avatar_url: None,
        })
        .collect())
}
