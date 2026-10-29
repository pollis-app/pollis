use std::sync::Arc;

use livekit::prelude::*;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::state::AppState;

/// Send a JSON event to a user's inbox room via the DS's server-side
/// `SendData` (#393), in a background task. Non-fatal: an empty room is
/// success and errors are only logged.
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

/// Publish to a room the caller is not joined to (e.g. an invitee notifying
/// existing members), via the DS's server-side `SendData`.
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

/// Publish a `new_message` wake-up to a room this process is connected to; a
/// no-op if it is not. §5: routing only, no sender (recipients take it from
/// the MLS credential).
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

/// Broadcast `edited_message` so other clients invalidate their cache. §5: no
/// `sender_id`; the editor is re-derived from the edit envelope.
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

/// Broadcast `deleted_message` so other clients drop it without polling. The
/// durable path is the envelope. §5: no `deleted_by`.
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

/// Broadcast `membership_changed` so connected members refetch the roster.
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

/// Broadcast `join_requests_changed` so connected admins refetch. The requester
/// is not in the group's room, so this goes through the DS (`publish_to_room_server`).
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

/// Broadcast `member_role_changed` so connected members refresh; a no-op if
/// not connected.
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

/// Broadcast a voice join/leave nudge so members refetch the participant list.
/// LiveKit is the source of truth for who is in a voice room; nothing is
/// written.
pub async fn publish_voice_presence(
    group_id: String,
    channel_id: String,
    // #836: unused; the actor is the publishing participant. Kept for
    // command-signature stability.
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
        // #836/§5: identity-free. A raw `user_id` in the body would tell the
        // SFU which account is behind the pseudonymous participant.
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

/// Publish a typing indicator; a no-op if not connected to `room_id`.
/// `reliable: false`: the sender re-emits while typing and receivers TTL stale
/// entries.
pub async fn publish_typing(
    room_id: String,
    channel_id: Option<String>,
    conversation_id: Option<String>,
    // Unused; see `publish_voice_presence`.
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

    // #836/§5: identity-free; the typist is the publishing participant.
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

/// Publish a `new_message` wake-up after a send (from the frontend). §5:
/// routing only, no sender.
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

/// Start a 1:1 call: mint a room name and post `call_invite` to the callee's
/// inbox. An offline callee misses the ring. Does not join the caller; the
/// frontend calls `join_voice_channel` next.
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

/// Tell the other party a pending call is over (hung up before answer, or
/// declined). Either side may call it.
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

/// Tell this user's other devices the incoming call was handled here, so they
/// stop ringing: `call_canceled` to the user's own inbox. The renderer's
/// handler is idempotent, so the originating device re-receiving it is
/// harmless. Separate from `cancel_call`, which addresses the other party.
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
