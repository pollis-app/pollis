use std::sync::Arc;

use livekit::prelude::*;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::state::AppState;

/// Send a JSON event to a user's inbox room via DS SendData (#393), in the background.
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
            crate::diag!("[inbox] SendData error (non-fatal): {e}");
        }
    });
    Ok(())
}

/// Publish to a room the caller is NOT joined to, via DS SendData.
pub async fn publish_to_room_server(
    state: &Arc<AppState>,
    room_name: &str,
    payload: serde_json::Value,
) -> Result<()> {
    let room_name = room_name.to_owned();
    let state = Arc::clone(state);
    tokio::spawn(async move {
        if let Err(e) = crate::commands::mls::ds_livekit_send_data(&state, &room_name, payload).await {
            crate::diag!("[room-server] SendData to {room_name} error (non-fatal): {e}");
        }
    });
    Ok(())
}

/// Publish a `new_message` wake-up to an already-connected room (no-op if not).
///
/// §5: a bare wake-up, **no sender** (re-derived from the MLS credential).
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

/// Broadcast `edited_message` so clients invalidate their cache.
///
/// §5: no `sender_id`; the editor comes from the edit envelope.
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

/// Broadcast `deleted_message`; the durable path is the `delete` envelope.
///
/// §5: no `deleted_by`; the actor comes from the tombstone.
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
    state: &Arc<AppState>,
    group_id: &str,
) -> Result<()> {
    let room = {
        let lk = state.livekit.lock().await;
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

/// Broadcast `join_requests_changed` to admins via DS SendData (requester isn't in the room).
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

/// Broadcast `member_role_changed`; no-op if not connected to the room.
pub async fn publish_member_role_changed_to_room(
    state: &Arc<AppState>,
    group_id: &str,
) -> Result<()> {
    let room = {
        let lk = state.livekit.lock().await;
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

/// Voice join/leave nudge so members refetch participants (LiveKit is the truth).
pub async fn publish_voice_presence(
    group_id: String,
    channel_id: String,
    // #836: unused; kept for command-signature stability.
    _user_id: String,
    _display_name: String,
    joined: bool,
    state: &Arc<AppState>,
) -> Result<()> {
    let room = {
        let lk = state.livekit.lock().await;
        lk.rooms.get(&group_id).map(|(r, _)| Arc::clone(r))
    };

    if let Some(room) = room {
        // #836/§5: identity-free; recipients see the publishing participant.
        let payload = serde_json::to_vec(
            &crate::commands::livekit_signalling::voice_presence_payload(&channel_id, joined),
        )
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

/// Typing indicator; unreliable, re-sent while typing, no-op if not connected.
pub async fn publish_typing(
    room_id: String,
    channel_id: Option<String>,
    conversation_id: Option<String>,
    // See `publish_voice_presence` — identity comes from the sender now.
    _user_id: String,
    _username: Option<String>,
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

    // #836/§5: identity-free — see `voice_presence_payload`.
    let payload = serde_json::to_vec(&crate::commands::livekit_signalling::typing_payload(
        channel_id.as_deref(),
        conversation_id.as_deref(),
        is_typing,
    ))
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

/// Publish a bare `new_message` wake-up (§5) after a send.
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

#[derive(Serialize, Deserialize)]
pub struct StartCallResult {
    pub call_id: String,
    pub room_name: String,
}

/// Start a 1:1 call: mint a room and ring the callee's inbox (offline = missed).
///
/// Doesn't join the caller; the frontend does that after this returns.
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

/// Tell the other party a pending call ended (hung up or declined).
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

/// Tell this user's other devices to stop ringing (`call_canceled` to own inbox).
///
/// Reuses `call_canceled`: the renderer handler is idempotent.
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
