use std::sync::Arc;

use livekit::prelude::*;
use serde::{Deserialize, Serialize};

use crate::config::Config;
use crate::error::{Error, Result};
use crate::state::AppState;

use super::admin_api::{make_admin_token, twirp_base};
use super::jwt::make_token;

/// Sends a JSON event to a user's personal inbox LiveKit room by making a
/// one-shot room connection. Joins `inbox-{user_id}` as identity "server",
/// publishes the data packet, then drops the room (auto-disconnects).
/// Spawned in a background task so callers are never blocked.
/// Non-fatal — errors are only logged.
pub async fn publish_to_user_inbox(
    config: &Config,
    user_id: &str,
    payload: serde_json::Value,
) -> Result<()> {
    if config.livekit_url.is_empty() || config.livekit_api_key.is_empty() {
        return Ok(());
    }

    let room_name = format!("inbox-{}", user_id);
    let token = make_admin_token(config, Some(&room_name))?;
    let url = format!(
        "{}/twirp/livekit.RoomService/SendData",
        twirp_base(&config.livekit_url)
    );

    let raw = serde_json::to_vec(&payload).map_err(Error::Serde)?;
    let body = serde_json::json!({
        "room": room_name,
        "data": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &raw),
        "kind": "RELIABLE",
    });

    // Fire-and-forget over HTTP — a single Twirp POST instead of a full
    // Room::connect + DTLS + ICE round trip (which was costing ~2-5s of
    // ring latency). The HTTP path is what `publish_ping` / `publish_typing`
    // can't use (they ride the already-connected Room), but for inbox
    // wakeups (call invites, etc.) where no persistent connection exists,
    // SendData is the right tool.
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
                if !status.is_success() {
                    // 404 → room doesn't exist (user offline). Treat as advisory.
                    if status != reqwest::StatusCode::NOT_FOUND {
                        let body_text = resp.text().await.unwrap_or_default();
                        eprintln!("[inbox] SendData {status}: {body_text}");
                    }
                }
            }
            Err(e) => {
                eprintln!("[inbox] SendData http error: {e}");
            }
        }
    });

    Ok(())
}

/// Connects to a LiveKit room as a temporary "server" participant and publishes
/// a data packet. Used when the caller is not already in the room (e.g. a user
/// accepting an invite needs to notify existing group members).
pub async fn publish_to_room_server(
    config: &Config,
    room_name: &str,
    payload: serde_json::Value,
) -> Result<()> {
    if config.livekit_url.is_empty() || config.livekit_api_key.is_empty() {
        return Ok(());
    }

    let token = make_token(config, room_name, "server", "server")?;
    let url = config.livekit_url.clone();
    let room_owned = room_name.to_owned();

    tokio::spawn(async move {
        match Room::connect(&url, &token, RoomOptions::default()).await {
            Ok((room, _events)) => {
                let raw = match serde_json::to_vec(&payload) {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("[room-server] serialize error for {room_owned}: {e}");
                        return;
                    }
                };
                let result = room
                    .local_participant()
                    .publish_data(DataPacket {
                        payload: raw,
                        reliable: true,
                        ..Default::default()
                    })
                    .await;
                if let Err(e) = result {
                    eprintln!("[room-server] publish_data to {room_owned} failed: {e}");
                }
            }
            Err(e) => {
                eprintln!("[room-server] connect to {room_owned} failed: {e}");
            }
        }
    });

    Ok(())
}

/// Publishes a new_message event to a LiveKit room that the current process
/// is already connected to. Used by `send_message` to notify group channel members.
/// Returns silently (non-fatal) if the room is not connected.
pub async fn publish_new_message_to_room(
    state: &Arc<AppState>,
    room_id: &str,
    channel_id: Option<&str>,
    conversation_id: Option<&str>,
    sender_id: &str,
    sender_username: Option<&str>,
) -> Result<()> {
    let room = {
        let lk = state.livekit.lock().await;
        lk.rooms.get(room_id).map(|(r, _)| Arc::clone(r))
    };

    let room = match room {
        None => return Ok(()),
        Some(r) => r,
    };

    let payload = serde_json::to_vec(&serde_json::json!({
        "type": "new_message",
        "channel_id": channel_id,
        "conversation_id": conversation_id,
        "sender_id": sender_id,
        "sender_username": sender_username,
    }))
    .map_err(Error::Serde)?;

    room.local_participant()
        .publish_data(DataPacket {
            payload,
            reliable: true,
            ..Default::default()
        })
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("publish_new_message: {e}")))?;

    Ok(())
}

/// Broadcasts an `edited_message` event to a LiveKit room so other clients
/// invalidate their message cache. Non-fatal — callers should log errors.
pub async fn publish_edited_message_to_room(
    livekit: &Arc<tokio::sync::Mutex<crate::realtime::LiveKitState>>,
    room_id: &str,
    channel_id: Option<&str>,
    conversation_id: Option<&str>,
    sender_id: &str,
    message_id: &str,
) -> Result<()> {
    let room = {
        let lk = livekit.lock().await;
        lk.rooms.get(room_id).map(|(r, _)| Arc::clone(r))
    };

    let room = match room {
        None => return Ok(()),
        Some(r) => r,
    };

    let payload = serde_json::to_vec(&serde_json::json!({
        "type": "edited_message",
        "channel_id": channel_id,
        "conversation_id": conversation_id,
        "sender_id": sender_id,
        "message_id": message_id,
    }))
    .map_err(Error::Serde)?;

    room.local_participant()
        .publish_data(DataPacket {
            payload,
            reliable: true,
            ..Default::default()
        })
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("publish_edited_message: {e}")))?;

    Ok(())
}

/// Broadcasts a `deleted_message` event to a LiveKit room so other clients
/// soft-delete the message from their cache without polling. Non-fatal —
/// callers should log errors. The durable propagation path is the
/// `type='delete'` envelope written to Turso.
pub async fn publish_deleted_message_to_room(
    livekit: &Arc<tokio::sync::Mutex<crate::realtime::LiveKitState>>,
    room_id: &str,
    channel_id: Option<&str>,
    conversation_id: Option<&str>,
    deleted_by: &str,
    message_id: &str,
) -> Result<()> {
    let room = {
        let lk = livekit.lock().await;
        lk.rooms.get(room_id).map(|(r, _)| Arc::clone(r))
    };

    let room = match room {
        None => return Ok(()),
        Some(r) => r,
    };

    let payload = serde_json::to_vec(&serde_json::json!({
        "type": "deleted_message",
        "channel_id": channel_id,
        "conversation_id": conversation_id,
        "message_id": message_id,
        "deleted_by": deleted_by,
    }))
    .map_err(Error::Serde)?;

    room.local_participant()
        .publish_data(DataPacket {
            payload,
            reliable: true,
            ..Default::default()
        })
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("publish_deleted_message: {e}")))?;

    Ok(())
}

/// Broadcasts a `membership_changed` event to a group's LiveKit room so
/// existing members refetch the member list (e.g. after a join-request approval).
pub async fn publish_membership_changed_to_room(
    livekit: &Arc<tokio::sync::Mutex<crate::realtime::LiveKitState>>,
    group_id: &str,
) -> Result<()> {
    let room = {
        let lk = livekit.lock().await;
        lk.rooms.get(group_id).map(|(r, _)| Arc::clone(r))
    };

    let room = match room {
        None => return Ok(()),
        Some(r) => r,
    };

    let payload = serde_json::to_vec(&serde_json::json!({
        "type": "membership_changed",
        "group_id": group_id,
    }))
    .map_err(Error::Serde)?;

    room.local_participant()
        .publish_data(DataPacket {
            payload,
            reliable: true,
            ..Default::default()
        })
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("publish_membership_changed: {e}")))?;

    Ok(())
}

/// Broadcasts a `join_requests_changed` event to a group's LiveKit room so
/// connected admins refetch the pending join-request list when a new request
/// arrives. Unlike `publish_membership_changed_to_room`, the requester is NOT
/// a member of the group and is therefore not connected to its room, so this
/// rides the server-side `publish_to_room_server` path (a temporary "server"
/// participant) rather than a locally-connected room. Best-effort and
/// fire-and-forget — callers should log errors.
pub async fn publish_join_requests_changed_to_room(
    config: &Config,
    group_id: &str,
) -> Result<()> {
    publish_to_room_server(
        config,
        group_id,
        serde_json::json!({
            "type": "join_requests_changed",
            "group_id": group_id,
        }),
    )
    .await
}

/// Broadcasts a `member_role_changed` event to a group's LiveKit room so
/// connected members refresh the member list when an admin/member role
/// changes. Non-fatal — callers log errors. Silently no-ops if the process
/// isn't connected to the group's room.
pub async fn publish_member_role_changed_to_room(
    livekit: &Arc<tokio::sync::Mutex<crate::realtime::LiveKitState>>,
    group_id: &str,
) -> Result<()> {
    let room = {
        let lk = livekit.lock().await;
        lk.rooms.get(group_id).map(|(r, _)| Arc::clone(r))
    };

    let room = match room {
        None => return Ok(()),
        Some(r) => r,
    };

    let payload = serde_json::to_vec(&crate::realtime::RealtimeEvent::MemberRoleChanged {
        group_id: group_id.to_owned(),
    })
    .map_err(Error::Serde)?;

    room.local_participant()
        .publish_data(DataPacket {
            payload,
            reliable: true,
            ..Default::default()
        })
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("publish_member_role_changed: {e}")))?;

    Ok(())
}

/// Broadcasts a voice join/leave event into the group's data channel so other
/// online members refetch the participant list. LiveKit is the source of
/// truth for who is actually in a voice room — this command does not write
/// to any DB; it only pushes the realtime nudge that triggers observers to
/// call `list_voice_participants` / `list_voice_room_counts` again.
pub async fn publish_voice_presence(
    group_id: String,
    channel_id: String,
    user_id: String,
    display_name: String,
    joined: bool,
    state: &Arc<AppState>,
) -> Result<()> {
    let room = {
        let lk = state.livekit.lock().await;
        lk.rooms.get(&group_id).map(|(r, _)| Arc::clone(r))
    };

    if let Some(room) = room {
        let payload = if joined {
            serde_json::to_vec(&serde_json::json!({
                "type": "voice_joined",
                "channel_id": channel_id,
                "user_id": user_id,
                "display_name": display_name,
            }))
        } else {
            serde_json::to_vec(&serde_json::json!({
                "type": "voice_left",
                "channel_id": channel_id,
                "user_id": user_id,
            }))
        }
        .map_err(Error::Serde)?;

        let _ = room
            .local_participant()
            .publish_data(DataPacket {
                payload,
                reliable: true,
                ..Default::default()
            })
            .await;
    }

    Ok(())
}

/// Publishes a typing indicator into a LiveKit room. Cheap fire-and-forget;
/// silently no-ops if the caller isn't connected to `room_id` yet (e.g. they
/// switched away mid-keystroke). Marked `reliable: false` so a single dropped
/// packet doesn't cost anything — the sender re-emits every few seconds while
/// still typing and the receiver TTLs stale entries anyway.
pub async fn publish_typing(
    room_id: String,
    channel_id: Option<String>,
    conversation_id: Option<String>,
    user_id: String,
    username: Option<String>,
    is_typing: bool,
    state: &Arc<AppState>,
) -> Result<()> {
    let room = {
        let lk = state.livekit.lock().await;
        lk.rooms.get(&room_id).map(|(r, _)| Arc::clone(r))
    };

    let room = match room {
        None => return Ok(()),
        Some(r) => r,
    };

    let payload = serde_json::to_vec(&serde_json::json!({
        "type": "typing",
        "channel_id": channel_id,
        "conversation_id": conversation_id,
        "user_id": user_id,
        "username": username,
        "is_typing": is_typing,
    }))
    .map_err(Error::Serde)?;

    let _ = room
        .local_participant()
        .publish_data(DataPacket {
            payload,
            reliable: false,
            ..Default::default()
        })
        .await;

    Ok(())
}

/// Publishes a data ping to a LiveKit room.
/// Called by the frontend after a message is successfully sent.
pub async fn publish_ping(
    room_id: String,
    channel_id: Option<String>,
    conversation_id: Option<String>,
    sender_id: String,
    sender_username: Option<String>,
    state: &Arc<AppState>,
) -> Result<()> {
    let room = {
        let lk = state.livekit.lock().await;
        lk.rooms.get(&room_id).map(|(r, _)| Arc::clone(r))
    };

    let room = match room {
        // Not connected to this room yet — silently skip rather than error.
        None => return Ok(()),
        Some(r) => r,
    };

    #[derive(Serialize)]
    struct Ping<'a> {
        #[serde(rename = "type")]
        event_type: &'a str,
        channel_id: Option<String>,
        conversation_id: Option<String>,
        sender_id: String,
        sender_username: Option<String>,
    }

    let payload = serde_json::to_vec(&Ping {
        event_type: "new_message",
        channel_id,
        conversation_id,
        sender_id,
        sender_username,
    })
    .map_err(Error::Serde)?;

    room.local_participant()
        .publish_data(DataPacket {
            payload,
            reliable: true,
            ..Default::default()
        })
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("publish_data: {e}")))?;

    Ok(())
}

// ── Calls (1:1 voice ringing) ──────────────────────────────────────────────

#[derive(Serialize, Deserialize)]
pub struct StartCallResult {
    pub call_id: String,
    pub room_name: String,
}

/// Initiates a 1:1 call by minting a fresh LiveKit room name and posting a
/// `call_invite` data packet to the callee's personal inbox room. Recipients
/// who are connected to their inbox receive it instantly via `dispatch_data`;
/// recipients who are offline simply miss the ring (treated as unanswered).
///
/// This does NOT join the caller to the room — the caller's frontend handles
/// that via `join_voice_channel` once `start_call` returns.
pub async fn start_call(
    callee_id: String,
    caller_id: String,
    caller_username: String,
    state: &Arc<AppState>,
) -> Result<StartCallResult> {
    if state.config.livekit_url.is_empty() {
        return Err(Error::Other(anyhow::anyhow!("LiveKit is not configured")));
    }

    let call_id = ulid::Ulid::new().to_string();
    let room_name = format!("call-{call_id}");

    let payload = serde_json::json!({
        "type": "call_invite",
        "call_id": call_id,
        "room_name": room_name,
        "caller_id": caller_id,
        "caller_username": caller_username,
    });

    publish_to_user_inbox(&state.config, &callee_id, payload).await?;

    Ok(StartCallResult { call_id, room_name })
}

/// Tells the callee that a pending call is no longer active — caller hung up
/// before answer, or callee declined. Either side can invoke it; the payload
/// is posted to the OTHER side's inbox.
pub async fn cancel_call(
    other_user_id: String,
    call_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    if state.config.livekit_url.is_empty() {
        return Ok(());
    }

    let payload = serde_json::json!({
        "type": "call_canceled",
        "call_id": call_id,
    });

    publish_to_user_inbox(&state.config, &other_user_id, payload).await?;

    Ok(())
}

/// Tells THIS user's other devices that the incoming call has been handled
/// here (answered or declined), so they should stop ringing. Posts a
/// `call_canceled` payload to the caller's own inbox room — every device
/// the user has connected receives it through the existing realtime path.
///
/// Reuses the `call_canceled` event variant rather than introducing a new
/// one: the renderer's existing handler (`useLiveKitRealtime.ts`) is already
/// idempotent — it no-ops when local `incomingCall` is null — so the
/// originating device safely re-receives its own dismissal, and every other
/// device clears its alert + stops the ring within the data-packet RTT.
///
/// Distinct from `cancel_call` because that one posts to the OTHER party.
/// This one posts to ourselves. Calling `cancel_call(self_id, …)` would
/// work mechanically but conflates the "tell the peer" semantics with the
/// "fan out to my own devices" semantics, which the type signature should
/// keep separate.
pub async fn dismiss_call_on_my_devices(
    user_id: String,
    call_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    if state.config.livekit_url.is_empty() {
        return Ok(());
    }

    let payload = serde_json::json!({
        "type": "call_canceled",
        "call_id": call_id,
    });

    publish_to_user_inbox(&state.config, &user_id, payload).await?;

    Ok(())
}
