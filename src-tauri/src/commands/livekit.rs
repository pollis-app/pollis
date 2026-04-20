use std::collections::HashSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use livekit::prelude::*;
use serde::{Deserialize, Serialize};
use tauri::State;

use crate::config::Config;
use crate::error::{Error, Result};
use crate::realtime::RealtimeEvent;
use crate::state::AppState;

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
}

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
struct AdminGrants {
    room_admin: bool,
    room_list: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    room: Option<String>,
}

#[derive(Debug, Serialize)]
struct AdminClaims {
    iss: String,
    sub: String,
    iat: u64,
    exp: u64,
    nbf: u64,
    video: AdminGrants,
}

fn make_admin_token(config: &Config, room: Option<&str>) -> Result<String> {
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

fn twirp_base(livekit_url: &str) -> String {
    if let Some(rest) = livekit_url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = livekit_url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        livekit_url.to_string()
    }
}

#[derive(Debug, Deserialize)]
struct RsParticipantsResp {
    #[serde(default)]
    participants: Vec<RsParticipant>,
}

#[derive(Debug, Deserialize)]
struct RsParticipant {
    #[serde(default)]
    identity: String,
    #[serde(default)]
    name: String,
}

async fn room_service_list_participants(
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
        // Filter out internal "server" participants used for data-channel fanout.
        .filter(|p| p.identity != "server" && p.identity != "pollis-backend")
        .map(|p| VoiceParticipantInfo {
            name: if p.name.is_empty() {
                p.identity.clone()
            } else {
                p.name.clone()
            },
            identity: p.identity,
        })
        .collect())
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
        },
    };

    let mut header = Header::new(Algorithm::HS256);
    header.typ = Some("JWT".to_string());
    let key = EncodingKey::from_secret(config.livekit_api_secret.as_bytes());
    encode(&header, &claims, &key)
        .map_err(|e| Error::Other(anyhow::anyhow!("JWT sign: {e}")))
}

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
    let token = make_token(config, &room_name, "server", "server")?;
    let url = config.livekit_url.clone();

    tauri::async_runtime::spawn(async move {
        match Room::connect(&url, &token, RoomOptions::default()).await {
            Ok((room, _events)) => {
                let raw = match serde_json::to_vec(&payload) {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("[inbox] serialize error for {room_name}: {e}");
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
                    eprintln!("[inbox] publish_data to {room_name} failed: {e}");
                }
                // Dropping `room` here causes the SDK to disconnect automatically.
            }
            Err(e) => {
                // Non-fatal: room may not exist if the user is offline.
                eprintln!("[inbox] connect to {room_name} failed (user may be offline): {e}");
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

    tauri::async_runtime::spawn(async move {
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

// ── Voice participant listing ──────────────────────────────────────────────

#[derive(Serialize)]
pub struct VoiceParticipantInfo {
    pub identity: String,
    pub name: String,
}

#[derive(Serialize)]
pub struct VoiceRoomCount {
    pub channel_id: String,
    pub count: usize,
}

/// Returns the participant count for each of the given voice channels by
/// asking LiveKit's RoomService. Channels with no active room return count=0.
///
/// We call `ListParticipants` per channel instead of `ListRooms` because
/// `ListRooms.numParticipants` can lag behind `ListParticipants` for several
/// seconds after the last participant disconnects — the room lingers with a
/// stale count until its `empty_timeout` fires. Using the same source as
/// `list_voice_participants` guarantees the sidebar count and the member
/// list never disagree.
#[tauri::command]
pub async fn list_voice_room_counts(
    channel_ids: Vec<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<VoiceRoomCount>> {
    if channel_ids.is_empty() {
        return Ok(vec![]);
    }

    let futs = channel_ids.iter().map(|id| {
        let id = id.clone();
        let config = state.config.clone();
        async move {
            let count = room_service_list_participants(&config, &id)
                .await
                .map(|v| v.len())
                .unwrap_or(0);
            VoiceRoomCount { channel_id: id, count }
        }
    });
    Ok(futures_util::future::join_all(futs).await)
}

/// Returns the participants in a voice channel by asking LiveKit's RoomService.
#[tauri::command]
pub async fn list_voice_participants(
    channel_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<VoiceParticipantInfo>> {
    room_service_list_participants(&state.config, &channel_id).await
}

// ── Legacy commands (kept for potential future use) ────────────────────────

#[tauri::command]
pub async fn get_livekit_token(
    room_name: String,
    identity: String,
    display_name: String,
    state: State<'_, Arc<AppState>>,
) -> Result<String> {
    make_token(&state.config, &room_name, &identity, &display_name)
}

#[tauri::command]
pub async fn get_livekit_url(state: State<'_, Arc<AppState>>) -> Result<String> {
    Ok(state.config.livekit_url.clone())
}

// ── Realtime commands ──────────────────────────────────────────────────────

/// Called once by the frontend on startup. Stores the typed Channel used to
/// push RealtimeEvents to the frontend. Safe to call again on re-login.
#[tauri::command]
pub async fn subscribe_realtime(
    on_event: tauri::ipc::Channel<RealtimeEvent>,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let mut lk = state.livekit.lock().await;
    lk.channel = Some(on_event);
    Ok(())
}

/// Called by the frontend whenever its room list changes.
/// Connects to rooms not yet joined; disconnects rooms no longer needed.
/// Safe to call with an empty list to disconnect everything (e.g. on logout).
#[tauri::command]
pub async fn connect_rooms(
    room_ids: Vec<String>,
    user_id: String,
    username: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let url = state.config.livekit_url.clone();
    let livekit_arc = Arc::clone(&state.livekit);

    // Build a per-device identity so multiple devices for the same user can
    // coexist in the same LiveKit room without kicking each other.
    let identity = match state.device_id.lock().await.clone() {
        Some(did) => format!("{user_id}:{did}"),
        None => user_id.clone(),
    };

    // Compute the diff while holding the lock briefly, then release.
    // Include rooms that are mid-connection to prevent duplicate connects.
    let (to_remove, to_connect) = {
        let mut lk = livekit_arc.lock().await;
        let next: HashSet<String> = room_ids.into_iter().collect();
        let current: HashSet<String> = lk.rooms.keys().cloned().collect();

        let remove: Vec<String> = current.difference(&next).cloned().collect();
        let connect: Vec<String> = next.difference(&current)
            .filter(|id| !lk.connecting.contains(*id))
            .cloned()
            .collect();

        // Mark these rooms as connecting before releasing the lock so
        // concurrent calls won't try to connect the same rooms.
        for id in &connect {
            lk.connecting.insert(id.clone());
        }

        (remove, connect)
    };

    // Disconnect removed rooms — each removal is a separate lock acquisition so
    // we don't hold the lock across the async disconnect call.
    for room_id in &to_remove {
        let removed = {
            let mut lk = livekit_arc.lock().await;
            lk.connecting.remove(room_id);
            lk.rooms.remove(room_id)
        };
        if let Some((_room, handle)) = removed {
            handle.abort();
            eprintln!("[realtime] disconnected room {room_id}");
        }
    }

    // Connect new rooms in parallel — each room gets its own task so a timeout
    // or failure on one room does not delay or block the others.
    for room_id in to_connect {
        let token = match make_token(&state.config, &room_id, &identity, &username) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[realtime] token error for room {room_id}: {e}");
                let mut lk = livekit_arc.lock().await;
                lk.connecting.remove(&room_id);
                continue;
            }
        };

        let url_owned = url.clone();
        let lk_arc_connect = Arc::clone(&livekit_arc);
        let config = state.config.clone();
        let identity_owned = identity.clone();
        let user_id_owned = user_id.clone();
        let username_owned = username.clone();
        let app_state_connect = Arc::clone(state.inner());

        eprintln!("[realtime] connecting room {room_id}");

        tokio::spawn(async move {
            match Room::connect(&url_owned, &token, RoomOptions::default()).await {
                Ok((room, mut events)) => {
                    let room = Arc::new(room);

                    // Clone the Arc so the event task can look up the channel
                    // each time it fires — this handles the case where subscribe_realtime
                    // is called after connect_rooms (no race condition).
                    let lk_arc_task = Arc::clone(&lk_arc_connect);
                    let room_id_owned = room_id.clone();
                    let app_state_task = Arc::clone(&app_state_connect);
                    let user_id_task = user_id_owned.clone();

                    let handle = tokio::spawn(async move {
                        /// Process events until the stream closes. Returns how long
                        /// the connection stayed alive (used to calibrate backoff).
                        async fn run_event_loop(
                            events: &mut tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
                            lk_arc: &Arc<tokio::sync::Mutex<crate::realtime::LiveKitState>>,
                            app_state: &Arc<AppState>,
                            user_id: &str,
                        ) -> std::time::Duration {
                            let started = std::time::Instant::now();
                            while let Some(event) = events.recv().await {
                                match event {
                                    RoomEvent::DataReceived { payload, .. } => {
                                        let channel = {
                                            let lk = lk_arc.lock().await;
                                            lk.channel.clone()
                                        };
                                        if let Some(ch) = channel {
                                            let reconcile_id = dispatch_data(payload.as_slice(), &ch);
                                            // On membership changes: process inbound commits
                                            // so this device advances to the current epoch,
                                            // and poll Welcomes in case this device was just
                                            // added to the group. Reconcile is NOT needed
                                            // here — it already ran on the device that made
                                            // the change.
                                            if let Some(conv_id) = reconcile_id {
                                                let state = Arc::clone(app_state);
                                                let uid = user_id.to_owned();
                                                tokio::spawn(async move {
                                                    let did = state.device_id.lock().await.clone();
                                                    if let Some(ref did) = did {
                                                        if let Err(e) = crate::commands::mls::poll_mls_welcomes_inner(
                                                            &state, &uid, did,
                                                        ).await {
                                                            eprintln!("[mls] poll_welcomes for {conv_id}: {e}");
                                                        }
                                                    }
                                                    if let Err(e) = crate::commands::mls::process_pending_commits_inner(
                                                        &state, &conv_id, &uid,
                                                    ).await {
                                                        eprintln!("[mls] process_pending_commits for {conv_id}: {e}");
                                                    }
                                                });
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            started.elapsed()
                        }

                        let alive_dur = run_event_loop(&mut events, &lk_arc_task, &app_state_task, &user_id_task).await;
                        eprintln!(
                            "[realtime] event stream closed for room {room_id_owned} (was alive {:.0}s), reconnecting…",
                            alive_dur.as_secs_f64()
                        );

                        let mut backoff = if alive_dur.as_secs() < 10 { 30u64 } else { 5 };
                        loop {
                            tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;

                            // Check if we've been removed from the room map (e.g. user
                            // left the channel or logged out). If so, stop reconnecting.
                            {
                                let lk = lk_arc_task.lock().await;
                                if !lk.rooms.contains_key(&room_id_owned) {
                                    eprintln!("[realtime] room {room_id_owned} removed, stopping reconnect");
                                    return;
                                }
                            }

                            let token = match make_token(&config, &room_id_owned, &identity_owned, &username_owned) {
                                Ok(t) => t,
                                Err(e) => {
                                    eprintln!("[realtime] reconnect token error for room {room_id_owned}: {e}");
                                    backoff = (backoff * 2).min(300);
                                    continue;
                                }
                            };

                            match Room::connect(&url_owned, &token, RoomOptions::default()).await {
                                Ok((new_room, mut new_events)) => {
                                    let new_room = Arc::new(new_room);
                                    eprintln!("[realtime] reconnected room {room_id_owned}");

                                    // Notify frontend so it can resync state that
                                    // may have drifted during the outage.
                                    {
                                        let lk = lk_arc_task.lock().await;
                                        if let Some(ch) = lk.channel.clone() {
                                            let _ = ch.send(RealtimeEvent::RealtimeReconnected {
                                                room_id: room_id_owned.clone(),
                                            });
                                        }
                                    }

                                    // Update the room reference in the map
                                    {
                                        let mut lk = lk_arc_task.lock().await;
                                        if let Some(entry) = lk.rooms.get_mut(&room_id_owned) {
                                            entry.0 = Arc::clone(&new_room);
                                        }
                                    }

                                    let alive_dur = run_event_loop(&mut new_events, &lk_arc_task, &app_state_task, &user_id_task).await;
                                    eprintln!(
                                        "[realtime] event stream closed again for room {room_id_owned} (was alive {:.0}s), reconnecting…",
                                        alive_dur.as_secs_f64()
                                    );
                                    backoff = if alive_dur.as_secs() < 10 { (backoff * 2).min(300) } else { 5 };
                                }
                                Err(e) => {
                                    eprintln!("[realtime] reconnect failed for room {room_id_owned}: {e}");
                                    backoff = (backoff * 2).min(300);
                                }
                            }
                        }
                    });

                    eprintln!("[realtime] connected room {room_id}");
                    let mut lk = lk_arc_connect.lock().await;
                    lk.connecting.remove(&room_id);
                    lk.rooms.insert(room_id, (room, handle));
                }
                Err(e) => {
                    eprintln!("[realtime] failed to connect room {room_id}: {e}");
                    let mut lk = lk_arc_connect.lock().await;
                    lk.connecting.remove(&room_id);
                }
            }
        });
    }

    Ok(())
}

/// Publishes a new_message event to a LiveKit room that the current process
/// is already connected to. Used by `send_message` to notify group channel members.
/// Returns silently (non-fatal) if the room is not connected.
pub async fn publish_new_message_to_room(
    livekit: &Arc<tokio::sync::Mutex<crate::realtime::LiveKitState>>,
    room_id: &str,
    channel_id: Option<&str>,
    conversation_id: Option<&str>,
    sender_id: &str,
    sender_username: Option<&str>,
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

/// Broadcasts a voice join/leave event into the group's data channel so other
/// online members refetch the participant list. LiveKit is the source of
/// truth for who is actually in a voice room — this command does not write
/// to any DB; it only pushes the realtime nudge that triggers observers to
/// call `list_voice_participants` / `list_voice_room_counts` again.
#[tauri::command]
pub async fn publish_voice_presence(
    group_id: String,
    channel_id: String,
    user_id: String,
    display_name: String,
    joined: bool,
    state: State<'_, Arc<AppState>>,
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

/// Publishes a data ping to a LiveKit room.
/// Called by the frontend after a message is successfully sent.
#[tauri::command]
pub async fn publish_ping(
    room_id: String,
    channel_id: Option<String>,
    conversation_id: Option<String>,
    sender_id: String,
    sender_username: Option<String>,
    state: State<'_, Arc<AppState>>,
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

// ── Internal helpers ───────────────────────────────────────────────────────

/// Parses a raw DataReceived payload and forwards it to the frontend channel.
/// Returns a conversation_id when a `membership_changed` event indicates
/// MLS reconcile should be triggered by the caller.
fn dispatch_data(payload: &[u8], channel: &tauri::ipc::Channel<RealtimeEvent>) -> Option<String> {
    let text = match std::str::from_utf8(payload) {
        Ok(s) => s,
        Err(_) => return None,
    };
    let data: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return None,
    };

    match data.get("type").and_then(|v| v.as_str()) {
        Some("new_message") => {
            let event = RealtimeEvent::NewMessage {
                channel_id: data
                    .get("channel_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                conversation_id: data
                    .get("conversation_id")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
                sender_id: data
                    .get("sender_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned(),
                sender_username: data
                    .get("sender_username")
                    .and_then(|v| v.as_str())
                    .map(str::to_owned),
            };
            // Errors here mean the frontend channel was dropped (e.g. logout). Ignore.
            let _ = channel.send(event);
        }
        Some("dm_created") => {
            if let Some(conversation_id) = data
                .get("conversation_id")
                .and_then(|v| v.as_str())
            {
                let _ = channel.send(RealtimeEvent::DmCreated {
                    conversation_id: conversation_id.to_owned(),
                });
            }
        }
        Some("membership_changed") => {
            // Extract conversation_id from payload (group_id or conversation_id).
            let conv_id = data
                .get("group_id")
                .or_else(|| data.get("conversation_id"))
                .and_then(|v| v.as_str())
                .map(str::to_owned);
            let _ = channel.send(RealtimeEvent::MembershipChanged {
                conversation_id: conv_id.clone(),
            });
            return conv_id;
        }
        Some("voice_joined") => {
            if let (Some(channel_id), Some(user_id)) = (
                data.get("channel_id").and_then(|v| v.as_str()),
                data.get("user_id").and_then(|v| v.as_str()),
            ) {
                let display_name = data
                    .get("display_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(user_id)
                    .to_owned();
                let _ = channel.send(RealtimeEvent::VoiceJoined {
                    channel_id: channel_id.to_owned(),
                    user_id: user_id.to_owned(),
                    display_name,
                });
            }
        }
        Some("voice_left") => {
            if let (Some(channel_id), Some(user_id)) = (
                data.get("channel_id").and_then(|v| v.as_str()),
                data.get("user_id").and_then(|v| v.as_str()),
            ) {
                let _ = channel.send(RealtimeEvent::VoiceLeft {
                    channel_id: channel_id.to_owned(),
                    user_id: user_id.to_owned(),
                });
            }
        }
        Some("edited_message") => {
            if let Some(message_id) = data.get("message_id").and_then(|v| v.as_str()) {
                let _ = channel.send(RealtimeEvent::EditedMessage {
                    channel_id: data.get("channel_id").and_then(|v| v.as_str()).map(str::to_owned),
                    conversation_id: data.get("conversation_id").and_then(|v| v.as_str()).map(str::to_owned),
                    message_id: message_id.to_owned(),
                    sender_id: data.get("sender_id").and_then(|v| v.as_str()).unwrap_or_default().to_owned(),
                });
            }
        }
        Some("enrollment_requested") => {
            if let (Some(request_id), Some(new_device_id), Some(verification_code)) = (
                data.get("request_id").and_then(|v| v.as_str()),
                data.get("new_device_id").and_then(|v| v.as_str()),
                data.get("verification_code").and_then(|v| v.as_str()),
            ) {
                let _ = channel.send(RealtimeEvent::EnrollmentRequested {
                    request_id: request_id.to_owned(),
                    new_device_id: new_device_id.to_owned(),
                    verification_code: verification_code.to_owned(),
                });
            }
        }
        _ => {}
    }
    None
}

