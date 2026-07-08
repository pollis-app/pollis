use std::sync::Arc;

use livekit::prelude::*;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::state::AppState;

/// Sends a JSON event to a user's personal inbox LiveKit room via the DS's
/// server-side `RoomService/SendData` (the admin secret stays server-side, #393).
/// Spawned in a background task so callers are never blocked. Non-fatal — the DS
/// treats an empty room (user offline) as success and errors are only logged.
pub async fn publish_to_user_inbox(
    state: &Arc<AppState>,
    user_id: &str,
    payload: serde_json::Value,
) -> Result<()> {
    let room_name = format!("inbox-{user_id}");
    let state = Arc::clone(state);
    tokio::spawn(async move {
        if let Err(e) = crate::commands::mls::ds_livekit_send_data(&state, &room_name, payload).await {
            eprintln!("[inbox] SendData error (non-fatal): {e}");
        }
    });
    Ok(())
}

/// Publishes a data packet to a room the caller is NOT joined to (e.g. a user
/// accepting an invite needs to notify existing group members). Now a DS
/// server-side SendData — no more temporary `Room::connect` (which cost a full
/// DTLS/ICE round trip and required an on-device participant token).
pub async fn publish_to_room_server(
    state: &Arc<AppState>,
    room_name: &str,
    payload: serde_json::Value,
) -> Result<()> {
    let room_name = room_name.to_owned();
    let state = Arc::clone(state);
    tokio::spawn(async move {
        if let Err(e) = crate::commands::mls::ds_livekit_send_data(&state, &room_name, payload).await {
            eprintln!("[room-server] SendData to {room_name} error (non-fatal): {e}");
        }
    });
    Ok(())
}

/// Publishes a new_message event to a LiveKit room that the current process
/// is already connected to. Used by `send_message` to notify group channel members.
/// Returns silently (non-fatal) if the room is not connected.
///
/// Metadata minimization (§5): the payload is a bare wake-up — conversation
/// routing only, **no sender**. Recipients re-derive the sender from the MLS
/// credential inside the decrypted envelope they fetch on ingest.
pub async fn publish_new_message_to_room(
    state: &Arc<AppState>,
    room_id: &str,
    channel_id: Option<&str>,
    conversation_id: Option<&str>,
) -> Result<()> {
    let room = {
        let lk = state.livekit.lock().await;
        lk.rooms.get(room_id).map(|(r, _)| Arc::clone(r))
    };

    let room = match room {
        None => return Ok(()),
        Some(r) => r,
    };

    let payload = serde_json::to_vec(&crate::commands::livekit_signalling::new_message_payload(
        channel_id,
        conversation_id,
    ))
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
///
/// Metadata minimization (§5): no `sender_id` — the editor is re-derived from
/// the durable edit envelope the recipient ingests.
pub async fn publish_edited_message_to_room(
    livekit: &Arc<tokio::sync::Mutex<crate::realtime::LiveKitState>>,
    room_id: &str,
    channel_id: Option<&str>,
    conversation_id: Option<&str>,
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

    let payload = serde_json::to_vec(&crate::commands::livekit_signalling::edited_message_payload(
        channel_id,
        conversation_id,
        message_id,
    ))
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
///
/// Metadata minimization (§5): no `deleted_by` — the actor is re-derived from
/// the authenticated tombstone envelope the recipient ingests.
pub async fn publish_deleted_message_to_room(
    livekit: &Arc<tokio::sync::Mutex<crate::realtime::LiveKitState>>,
    room_id: &str,
    channel_id: Option<&str>,
    conversation_id: Option<&str>,
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

    let payload = serde_json::to_vec(&crate::commands::livekit_signalling::deleted_message_payload(
        channel_id,
        conversation_id,
        message_id,
    ))
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

    let payload = serde_json::to_vec(
        &crate::commands::livekit_signalling::membership_changed_payload(group_id),
    )
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
/// rides the server-side `publish_to_room_server` (DS SendData) path rather than
/// a locally-connected room. Best-effort and fire-and-forget — callers log errors.
pub async fn publish_join_requests_changed_to_room(
    state: &Arc<AppState>,
    group_id: &str,
) -> Result<()> {
    publish_to_room_server(
        state,
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
///
/// Metadata minimization (§5): same bare `new_message` wake-up as
/// `publish_new_message_to_room` — conversation routing only, no sender.
pub async fn publish_ping(
    room_id: String,
    channel_id: Option<String>,
    conversation_id: Option<String>,
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

    let payload = serde_json::to_vec(&crate::commands::livekit_signalling::new_message_payload(
        channel_id.as_deref(),
        conversation_id.as_deref(),
    ))
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

    publish_to_user_inbox(state, &callee_id, payload).await?;

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

    publish_to_user_inbox(state, &other_user_id, payload).await?;

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

    publish_to_user_inbox(state, &user_id, payload).await?;

    Ok(())
}
