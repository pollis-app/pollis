//! Mobile (iOS/Android) stub for the LiveKit realtime layer.
//!
//! The Rust LiveKit/libwebrtc *client* stack (`Room::connect`, track publish,
//! libwebrtc) is desktop-only — mobile uses the native LiveKit SDK
//! (`@livekit/react-native`) for its own room connections (see issue #185).
//! This module is swapped in for `commands::livekit` on mobile targets via
//! `#[cfg]` in `commands/mod.rs`, so the core messaging/auth/group/dm/
//! enrollment call sites stay byte-identical across platforms.
//!
//! Pushes that ride an *already-connected* `Room` (new_message, typing, voice
//! presence, …) stay no-ops here — mobile keeps no Rust-side `Room`. But
//! pushes to a room this process is *not* joined to go through LiveKit's
//! server-side `RoomService/SendData` Twirp API, which is plain HTTPS + a
//! signed admin JWT and needs no native deps — so those are implemented for
//! real. That's what lets "approve from another device" enrollment (and 1:1
//! call invites) reach a sibling device's inbox room from mobile. Mirrors
//! `commands::livekit::publish::publish_to_user_inbox` on desktop.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde::Serialize;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::realtime::LiveKitState;
use crate::state::AppState;

type LiveKit = Arc<tokio::sync::Mutex<LiveKitState>>;

// ── Server-side data publish (Twirp RoomService/SendData) ───────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AdminGrants {
    room_admin: bool,
    room_list: bool,
    room: Option<String>,
}

#[derive(Serialize)]
struct AdminClaims {
    iss: String,
    sub: String,
    iat: u64,
    exp: u64,
    nbf: u64,
    video: AdminGrants,
}

/// Mint a short-lived admin JWT scoped to `room`, granting the
/// `RoomService/SendData` call. Pure `jsonwebtoken` — no native deps.
fn make_admin_token(config: &Config, room: &str) -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Error::Other(anyhow::anyhow!("{e}")))?
        .as_secs();
    let claims = AdminClaims {
        iss: config.livekit_api_key.clone(),
        sub: "pollis-mobile".to_string(),
        iat: now,
        exp: now + 300,
        nbf: now,
        video: AdminGrants {
            room_admin: true,
            room_list: true,
            room: Some(room.to_string()),
        },
    };
    let mut header = Header::new(Algorithm::HS256);
    header.typ = Some("JWT".to_string());
    let key = EncodingKey::from_secret(config.livekit_api_secret.as_bytes());
    encode(&header, &claims, &key).map_err(|e| Error::Other(anyhow::anyhow!("JWT sign: {e}")))
}

/// The server API is a Twirp-over-HTTPS endpoint, distinct from the `wss://`
/// URL the client SDK dials. Translate the configured client URL into it.
fn twirp_base(livekit_url: &str) -> String {
    if let Some(rest) = livekit_url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = livekit_url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        livekit_url.to_string()
    }
}

/// Fire-and-forget `RoomService/SendData` to `room_name`. Non-fatal: a 404
/// just means the room currently has no participants (the recipient picks the
/// change up via its poll fallback when it next comes online), so we log only
/// genuine errors and never block the caller.
async fn send_data_to_room(
    config: &Config,
    room_name: String,
    payload: serde_json::Value,
) -> Result<()> {
    if config.livekit_url.is_empty() || config.livekit_api_key.is_empty() {
        return Ok(());
    }

    let token = make_admin_token(config, &room_name)?;
    let url = format!(
        "{}/twirp/livekit.RoomService/SendData",
        twirp_base(&config.livekit_url)
    );
    let raw = serde_json::to_vec(&payload).map_err(Error::Serde)?;
    let body = serde_json::json!({
        "room": room_name,
        "data": base64::engine::general_purpose::STANDARD.encode(&raw),
        "kind": "RELIABLE",
    });

    tokio::spawn(async move {
        match reqwest::Client::new()
            .post(&url)
            .bearer_auth(&token)
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if !status.is_success() && status != reqwest::StatusCode::NOT_FOUND {
                    let body_text = resp.text().await.unwrap_or_default();
                    eprintln!("[inbox] SendData {status}: {body_text}");
                }
            }
            Err(e) => eprintln!("[inbox] SendData http error: {e}"),
        }
    });

    Ok(())
}

/// Push a JSON event to a user's personal inbox room (`inbox-{user_id}`).
/// Real on mobile — this is the path enrollment + call invites rely on.
pub async fn publish_to_user_inbox(
    config: &Config,
    user_id: &str,
    payload: serde_json::Value,
) -> Result<()> {
    send_data_to_room(config, format!("inbox-{user_id}"), payload).await
}

/// Push a JSON event to an arbitrary room the caller is not joined to (e.g. a
/// user accepting a group invite notifying existing members). Real on mobile.
pub async fn publish_to_room_server(
    config: &Config,
    room_name: &str,
    payload: serde_json::Value,
) -> Result<()> {
    send_data_to_room(config, room_name.to_string(), payload).await
}

/// Notify a conversation's room of a new message. Mobile has no Rust-side
/// `Room` to publish through, but new_message is exactly the event a peer's
/// open chat needs to refresh live — so we send it through the same
/// server-side `SendData` path as inbox events (it delivers to whoever is in
/// the room, e.g. a desktop client viewing the conversation). Without this a
/// mobile-sent message only surfaces on the peer after a manual re-ingest.
/// Mirrors the payload shape of desktop's `publish_new_message_to_room`.
pub async fn publish_new_message_to_room(
    state: &Arc<AppState>,
    room_id: &str,
    channel_id: Option<&str>,
    conversation_id: Option<&str>,
    sender_id: &str,
    sender_username: Option<&str>,
) -> Result<()> {
    let payload = serde_json::json!({
        "type": "new_message",
        "channel_id": channel_id,
        "conversation_id": conversation_id,
        "sender_id": sender_id,
        "sender_username": sender_username,
    });
    send_data_to_room(&state.config, room_id.to_string(), payload).await
}

// ── Connected-room pushes — still no-op on mobile ───────────────────────────
//
// These ride a `Room` this process has already joined. Mobile holds no
// Rust-side `Room`, and (unlike new_message above) edits/deletes/membership
// changes still propagate durably through Turso and refresh on the next
// ingest — so they stay no-ops for now. They could move to the SendData path
// too if live edit/delete propagation from mobile becomes a priority (#185).

pub async fn publish_edited_message_to_room(
    _livekit: &LiveKit,
    _room_id: &str,
    _channel_id: Option<&str>,
    _conversation_id: Option<&str>,
    _sender_id: &str,
    _message_id: &str,
) -> Result<()> {
    Ok(())
}

pub async fn publish_deleted_message_to_room(
    _livekit: &LiveKit,
    _room_id: &str,
    _channel_id: Option<&str>,
    _conversation_id: Option<&str>,
    _deleted_by: &str,
    _message_id: &str,
) -> Result<()> {
    Ok(())
}

pub async fn publish_membership_changed_to_room(
    _livekit: &LiveKit,
    _group_id: &str,
) -> Result<()> {
    Ok(())
}

pub async fn publish_member_role_changed_to_room(
    _livekit: &LiveKit,
    _group_id: &str,
) -> Result<()> {
    Ok(())
}
