use std::collections::HashSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::db::remote::RemoteDb;

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
/// querying voice_presence in Turso. No LiveKit REST calls.
#[tauri::command]
pub async fn list_voice_room_counts(
    channel_ids: Vec<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<VoiceRoomCount>> {
    if channel_ids.is_empty() {
        return Ok(vec![]);
    }

    let counts = query_voice_counts(&state.remote_db, &channel_ids).await?;
    Ok(channel_ids
        .into_iter()
        .map(|id| {
            let count = counts.get(&id).copied().unwrap_or(0);
            VoiceRoomCount { channel_id: id, count }
        })
        .collect())
}

/// Returns the participants in a voice channel by querying voice_presence.
/// No LiveKit REST calls.
#[tauri::command]
pub async fn list_voice_participants(
    channel_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<VoiceParticipantInfo>> {
    query_voice_participants(&state.remote_db, &channel_id).await
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
        let remote_db_connect = Arc::clone(&state.remote_db);
        let app_state_connect = Arc::clone(state.inner());

        eprintln!("[realtime] connecting room {room_id}");

        tokio::spawn(async move {
            match Room::connect(&url_owned, &token, RoomOptions::default()).await {
                Ok((room, mut events)) => {
                    let room = Arc::new(room);

                    // On connect, reconcile voice_presence: delete rows for any user who
                    // has a presence record for this group but is not currently in the room.
                    // This cleans up orphaned rows from previous crash disconnects.
                    // Extract user_id from participant identities (may be "user_id:device_id").
                    let online_ids: HashSet<String> = room
                        .remote_participants()
                        .keys()
                        .map(|id| id.to_string().split(':').next().unwrap_or_default().to_string())
                        .chain(std::iter::once(user_id_owned.clone()))
                        .collect();
                    reconcile_voice_presence(&remote_db_connect, &room_id, &online_ids).await;

                    // Clone the Arc so the event task can look up the channel
                    // each time it fires — this handles the case where subscribe_realtime
                    // is called after connect_rooms (no race condition).
                    let lk_arc_task = Arc::clone(&lk_arc_connect);
                    let room_id_owned = room_id.clone();
                    let remote_db_task = Arc::clone(&remote_db_connect);
                    let app_state_task = Arc::clone(&app_state_connect);
                    let user_id_task = user_id_owned.clone();

                    let handle = tokio::spawn(async move {
                        /// Process events until the stream closes. Returns how long
                        /// the connection stayed alive (used to calibrate backoff).
                        async fn run_event_loop(
                            events: &mut tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
                            lk_arc: &Arc<tokio::sync::Mutex<crate::realtime::LiveKitState>>,
                            remote_db: &Arc<RemoteDb>,
                            app_state: &Arc<AppState>,
                            room_id: &str,
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
                                    RoomEvent::ParticipantDisconnected(participant) => {
                                        let identity = participant.identity().to_string();
                                        // Extract user_id from identity (may be "user_id:device_id").
                                        let uid = identity.split(':').next().unwrap_or(&identity);
                                        // Clean up any voice presence rows for this user in
                                        // this group and notify the frontend for each.
                                        handle_participant_disconnect(
                                            remote_db,
                                            lk_arc,
                                            room_id,
                                            uid,
                                        )
                                        .await;
                                    }
                                    _ => {}
                                }
                            }
                            started.elapsed()
                        }

                        let alive_dur = run_event_loop(&mut events, &lk_arc_task, &remote_db_task, &app_state_task, &room_id_owned, &user_id_task).await;
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

                                    // Reconcile again on reconnect.
                                    let online_ids: HashSet<String> = new_room
                                        .remote_participants()
                                        .keys()
                                        .map(|id| id.to_string().split(':').next().unwrap_or_default().to_string())
                                        .chain(std::iter::once(user_id_owned.clone()))
                                        .collect();
                                    reconcile_voice_presence(&remote_db_task, &room_id_owned, &online_ids).await;

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

                                    let alive_dur = run_event_loop(&mut new_events, &lk_arc_task, &remote_db_task, &app_state_task, &room_id_owned, &user_id_task).await;
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

/// Records a voice join or leave in the DB and notifies group members via the
/// LiveKit data channel so their participant counts update in real time.
#[tauri::command]
pub async fn publish_voice_presence(
    group_id: String,
    channel_id: String,
    user_id: String,
    display_name: String,
    joined: bool,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    // Write to DB first — this is the source of truth.
    let conn = state.remote_db.conn().await?;
    if joined {
        conn.execute(
            "INSERT OR REPLACE INTO voice_presence (user_id, group_id, channel_id, display_name, joined_at) \
             VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            libsql::params![user_id.clone(), group_id.clone(), channel_id.clone(), display_name.clone()],
        )
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("voice_presence insert: {e}")))?;
    } else {
        conn.execute(
            "DELETE FROM voice_presence WHERE user_id = ?1 AND channel_id = ?2",
            libsql::params![user_id.clone(), channel_id.clone()],
        )
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("voice_presence delete: {e}")))?;
    }

    // Notify other online group members to refetch via the data channel.
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

/// Query voice_presence counts for multiple channel IDs in one SQL statement.
async fn query_voice_counts(
    remote_db: &RemoteDb,
    channel_ids: &[String],
) -> Result<std::collections::HashMap<String, usize>> {
    let conn = remote_db.conn().await?;
    let placeholders = channel_ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT channel_id, COUNT(*) FROM voice_presence WHERE channel_id IN ({}) GROUP BY channel_id",
        placeholders
    );
    let params: Vec<libsql::Value> = channel_ids
        .iter()
        .map(|id| libsql::Value::Text(id.clone()))
        .collect();
    let mut rows = conn
        .query(&sql, libsql::params_from_iter(params))
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("query_voice_counts: {e}")))?;
    let mut map = std::collections::HashMap::new();
    while let Some(row) = rows.next().await.map_err(|e| Error::Other(anyhow::anyhow!("{e}")))? {
        let ch_id: String = row.get(0).map_err(|e| Error::Other(anyhow::anyhow!("{e}")))?;
        let cnt: i64 = row.get(1).map_err(|e| Error::Other(anyhow::anyhow!("{e}")))?;
        map.insert(ch_id, cnt as usize);
    }
    Ok(map)
}

/// Query voice_presence participant list for a single channel.
async fn query_voice_participants(
    remote_db: &RemoteDb,
    channel_id: &str,
) -> Result<Vec<VoiceParticipantInfo>> {
    let conn = remote_db.conn().await?;
    let mut rows = conn
        .query(
            "SELECT user_id, display_name FROM voice_presence WHERE channel_id = ?1",
            libsql::params![channel_id.to_owned()],
        )
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("query_voice_participants: {e}")))?;
    let mut list = Vec::new();
    while let Some(row) = rows.next().await.map_err(|e| Error::Other(anyhow::anyhow!("{e}")))? {
        let identity: String = row.get(0).map_err(|e| Error::Other(anyhow::anyhow!("{e}")))?;
        let name: String = row.get(1).map_err(|e| Error::Other(anyhow::anyhow!("{e}")))?;
        list.push(VoiceParticipantInfo { identity, name });
    }
    Ok(list)
}

/// On group room connect/reconnect: delete voice_presence rows for any user in
/// this group who is NOT currently in the group room (i.e. crashed previously).
async fn reconcile_voice_presence(
    remote_db: &RemoteDb,
    group_id: &str,
    online_ids: &HashSet<String>,
) {
    let conn = match remote_db.conn().await {
        Ok(c) => c,
        Err(e) => { eprintln!("[presence] reconcile conn error: {e}"); return; }
    };

    // Fetch all user_ids with presence rows for this group.
    let mut rows = match conn
        .query(
            "SELECT DISTINCT user_id FROM voice_presence WHERE group_id = ?1",
            libsql::params![group_id.to_owned()],
        )
        .await
    {
        Ok(r) => r,
        Err(e) => { eprintln!("[presence] reconcile query error: {e}"); return; }
    };

    let mut stale: Vec<String> = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(uid) = row.get::<String>(0) {
            if !online_ids.contains(&uid) {
                stale.push(uid);
            }
        }
    }

    for uid in stale {
        if let Err(e) = conn
            .execute(
                "DELETE FROM voice_presence WHERE user_id = ?1 AND group_id = ?2",
                libsql::params![uid.clone(), group_id.to_owned()],
            )
            .await
        {
            eprintln!("[presence] reconcile delete error for {uid}: {e}");
        } else {
            eprintln!("[presence] reconciled stale presence for {uid} in group {group_id}");
        }
    }
}

/// Called when a participant disconnects from a group room (graceful or crash).
/// Deletes their voice_presence rows and pushes VoiceLeft events to the frontend.
async fn handle_participant_disconnect(
    remote_db: &RemoteDb,
    lk_arc: &Arc<tokio::sync::Mutex<crate::realtime::LiveKitState>>,
    group_id: &str,
    user_id: &str,
) {
    let conn = match remote_db.conn().await {
        Ok(c) => c,
        Err(e) => { eprintln!("[presence] disconnect conn error: {e}"); return; }
    };

    // Find which channels this user was in before deleting.
    let mut rows = match conn
        .query(
            "SELECT channel_id FROM voice_presence WHERE user_id = ?1 AND group_id = ?2",
            libsql::params![user_id.to_owned(), group_id.to_owned()],
        )
        .await
    {
        Ok(r) => r,
        Err(e) => { eprintln!("[presence] disconnect query error: {e}"); return; }
    };

    let mut affected_channels: Vec<String> = Vec::new();
    while let Ok(Some(row)) = rows.next().await {
        if let Ok(ch_id) = row.get::<String>(0) {
            affected_channels.push(ch_id);
        }
    }

    if affected_channels.is_empty() {
        return;
    }

    // Delete the rows.
    if let Err(e) = conn
        .execute(
            "DELETE FROM voice_presence WHERE user_id = ?1 AND group_id = ?2",
            libsql::params![user_id.to_owned(), group_id.to_owned()],
        )
        .await
    {
        eprintln!("[presence] disconnect delete error: {e}");
        return;
    }

    // Notify the frontend for each affected channel.
    let channel = {
        let lk = lk_arc.lock().await;
        lk.channel.clone()
    };
    if let Some(ch) = channel {
        for channel_id in affected_channels {
            let _ = ch.send(RealtimeEvent::VoiceLeft {
                channel_id,
                user_id: user_id.to_owned(),
            });
        }
    }
}

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

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    const REMOTE_V001: &str = include_str!("../db/migrations/remote_schema.sql");

    const VOICE_PRESENCE: &str = "
        CREATE TABLE IF NOT EXISTS voice_presence (
            user_id      TEXT NOT NULL,
            group_id     TEXT NOT NULL REFERENCES groups(id) ON DELETE CASCADE,
            channel_id   TEXT NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
            display_name TEXT NOT NULL,
            joined_at    TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
            PRIMARY KEY (user_id, channel_id),
            UNIQUE (user_id, group_id)
        );
        CREATE INDEX IF NOT EXISTS idx_voice_presence_channel ON voice_presence(channel_id);
        CREATE INDEX IF NOT EXISTS idx_voice_presence_group   ON voice_presence(group_id);
    ";

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn.execute_batch(REMOTE_V001).unwrap();
        conn.execute_batch(VOICE_PRESENCE).unwrap();
        conn
    }

    fn setup(conn: &Connection) {
        conn.execute("INSERT INTO users (id, email, username) VALUES ('alice', 'alice@x.com', 'alice')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('bob',   'bob@x.com',   'bob')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('carol', 'carol@x.com', 'carol')", []).unwrap();

        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'Test Group', 'alice')", []).unwrap();
        conn.execute("INSERT INTO channels (id, group_id, name, channel_type) VALUES ('vc1', 'g1', 'voice-1', 'voice')", []).unwrap();
        conn.execute("INSERT INTO channels (id, group_id, name, channel_type) VALUES ('vc2', 'g1', 'voice-2', 'voice')", []).unwrap();
    }

    /// Simulate `publish_voice_presence(joined=true)`.
    fn join(conn: &Connection, user_id: &str, group_id: &str, channel_id: &str, display_name: &str) {
        conn.execute(
            "INSERT OR REPLACE INTO voice_presence (user_id, group_id, channel_id, display_name, joined_at) \
             VALUES (?1, ?2, ?3, ?4, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))",
            rusqlite::params![user_id, group_id, channel_id, display_name],
        ).unwrap();
    }

    /// Simulate `publish_voice_presence(joined=false)`.
    fn leave(conn: &Connection, user_id: &str, channel_id: &str) {
        conn.execute(
            "DELETE FROM voice_presence WHERE user_id = ?1 AND channel_id = ?2",
            rusqlite::params![user_id, channel_id],
        ).unwrap();
    }

    /// Simulate `query_voice_participants` — returns (user_id, display_name) pairs.
    fn participants(conn: &Connection, channel_id: &str) -> Vec<(String, String)> {
        conn.prepare("SELECT user_id, display_name FROM voice_presence WHERE channel_id = ?1")
            .unwrap()
            .query_map(rusqlite::params![channel_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    /// Simulate `query_voice_counts` for a single channel.
    fn count(conn: &Connection, channel_id: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM voice_presence WHERE channel_id = ?1",
            rusqlite::params![channel_id],
            |row| row.get(0),
        ).unwrap()
    }

    // ── basic join/leave ───────────────────────────────────────────────────

    #[test]
    fn single_user_joins_and_appears_in_channel() {
        let conn = db();
        setup(&conn);

        join(&conn, "alice", "g1", "vc1", "alice");

        let p = participants(&conn, "vc1");
        assert_eq!(p.len(), 1);
        assert_eq!(p[0], ("alice".into(), "alice".into()));
    }

    #[test]
    fn single_user_leaves_and_disappears() {
        let conn = db();
        setup(&conn);

        join(&conn, "alice", "g1", "vc1", "alice");
        leave(&conn, "alice", "vc1");

        assert_eq!(count(&conn, "vc1"), 0);
    }

    // ── multiple users join/leave at different times ───────────────────────

    #[test]
    fn multiple_users_join_same_channel() {
        let conn = db();
        setup(&conn);

        join(&conn, "alice", "g1", "vc1", "alice");
        join(&conn, "bob", "g1", "vc1", "bob");

        assert_eq!(count(&conn, "vc1"), 2);
        let p = participants(&conn, "vc1");
        let ids: Vec<&str> = p.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"alice"));
        assert!(ids.contains(&"bob"));
    }

    #[test]
    fn first_user_leaves_second_stays() {
        let conn = db();
        setup(&conn);

        join(&conn, "alice", "g1", "vc1", "alice");
        join(&conn, "bob", "g1", "vc1", "bob");
        leave(&conn, "alice", "vc1");

        assert_eq!(count(&conn, "vc1"), 1);
        let p = participants(&conn, "vc1");
        assert_eq!(p[0].0, "bob");
    }

    #[test]
    fn staggered_join_leave_join() {
        let conn = db();
        setup(&conn);

        // Alice joins, then Bob joins, then Alice leaves, then Carol joins.
        join(&conn, "alice", "g1", "vc1", "alice");
        join(&conn, "bob", "g1", "vc1", "bob");
        leave(&conn, "alice", "vc1");
        join(&conn, "carol", "g1", "vc1", "carol");

        assert_eq!(count(&conn, "vc1"), 2);
        let p = participants(&conn, "vc1");
        let ids: Vec<&str> = p.iter().map(|(id, _)| id.as_str()).collect();
        assert!(ids.contains(&"bob"));
        assert!(ids.contains(&"carol"));
        assert!(!ids.contains(&"alice"));
    }

    #[test]
    fn all_users_leave_channel_is_empty() {
        let conn = db();
        setup(&conn);

        join(&conn, "alice", "g1", "vc1", "alice");
        join(&conn, "bob", "g1", "vc1", "bob");
        join(&conn, "carol", "g1", "vc1", "carol");

        leave(&conn, "alice", "vc1");
        leave(&conn, "bob", "vc1");
        leave(&conn, "carol", "vc1");

        assert_eq!(count(&conn, "vc1"), 0);
        assert!(participants(&conn, "vc1").is_empty());
    }

    // ── multiple channels ─────────────────────────────────────────────────

    #[test]
    fn users_in_different_channels_isolated() {
        let conn = db();
        setup(&conn);

        join(&conn, "alice", "g1", "vc1", "alice");
        join(&conn, "bob", "g1", "vc2", "bob");

        assert_eq!(count(&conn, "vc1"), 1);
        assert_eq!(count(&conn, "vc2"), 1);
        assert_eq!(participants(&conn, "vc1")[0].0, "alice");
        assert_eq!(participants(&conn, "vc2")[0].0, "bob");
    }

    #[test]
    fn user_in_one_channel_only_via_unique_constraint() {
        let conn = db();
        setup(&conn);

        // UNIQUE (user_id, group_id) means joining a second channel in the
        // same group atomically evicts the row for the first channel via
        // INSERT OR REPLACE. The schema enforces single-channel-per-group
        // even if the app's leave step crashed or raced.
        join(&conn, "alice", "g1", "vc1", "alice");
        join(&conn, "alice", "g1", "vc2", "alice");

        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM voice_presence WHERE user_id = 'alice'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(total, 1, "UNIQUE(user_id, group_id) must collapse to a single row");

        // The surviving row is the most recent channel.
        assert_eq!(count(&conn, "vc1"), 0, "old channel row should be evicted");
        assert_eq!(count(&conn, "vc2"), 1, "new channel row should be present");
        let p = participants(&conn, "vc2");
        assert_eq!(p[0].0, "alice");
    }

    // ── rejoin same channel (INSERT OR REPLACE) ───────────────────────────

    #[test]
    fn rejoin_same_channel_does_not_duplicate() {
        let conn = db();
        setup(&conn);

        join(&conn, "alice", "g1", "vc1", "alice");
        join(&conn, "alice", "g1", "vc1", "alice");

        assert_eq!(count(&conn, "vc1"), 1, "INSERT OR REPLACE should not create a duplicate");
    }

    #[test]
    fn rejoin_updates_display_name() {
        let conn = db();
        setup(&conn);

        join(&conn, "alice", "g1", "vc1", "alice");
        join(&conn, "alice", "g1", "vc1", "Alice (renamed)");

        let p = participants(&conn, "vc1");
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].1, "Alice (renamed)");
    }

    // ── crash recovery: reconcile_voice_presence ──────────────────────────

    #[test]
    fn reconcile_removes_stale_users() {
        let conn = db();
        setup(&conn);

        // Simulate: alice and bob both have presence rows, but only alice
        // is actually online (bob crashed).
        join(&conn, "alice", "g1", "vc1", "alice");
        join(&conn, "bob", "g1", "vc1", "bob");

        // Reconciliation: fetch all users with presence, compare against online set.
        let online: std::collections::HashSet<String> = ["alice"].iter().map(|s| s.to_string()).collect();

        let all_present: Vec<String> = conn
            .prepare("SELECT DISTINCT user_id FROM voice_presence WHERE group_id = ?1")
            .unwrap()
            .query_map(rusqlite::params!["g1"], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();

        let stale: Vec<&String> = all_present.iter().filter(|uid| !online.contains(*uid)).collect();
        assert_eq!(stale, vec!["bob"]);

        // Delete stale
        for uid in &stale {
            conn.execute(
                "DELETE FROM voice_presence WHERE user_id = ?1 AND group_id = ?2",
                rusqlite::params![uid.as_str(), "g1"],
            ).unwrap();
        }

        assert_eq!(count(&conn, "vc1"), 1);
        assert_eq!(participants(&conn, "vc1")[0].0, "alice");
    }

    // ── participant disconnect cleanup ─────────────────────────────────────

    #[test]
    fn disconnect_removes_user_from_all_channels_in_group() {
        let conn = db();
        setup(&conn);

        // Bob is in vc2; alice is in vc1. UNIQUE(user_id, group_id) means
        // bob can only ever be in one channel per group at a time.
        join(&conn, "bob", "g1", "vc2", "bob");
        join(&conn, "alice", "g1", "vc1", "alice");

        // Simulate handle_participant_disconnect: find affected channels, then delete.
        let affected: Vec<String> = conn
            .prepare("SELECT channel_id FROM voice_presence WHERE user_id = ?1 AND group_id = ?2")
            .unwrap()
            .query_map(rusqlite::params!["bob", "g1"], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(affected.len(), 1);
        assert_eq!(affected[0], "vc2");

        conn.execute(
            "DELETE FROM voice_presence WHERE user_id = ?1 AND group_id = ?2",
            rusqlite::params!["bob", "g1"],
        ).unwrap();

        assert_eq!(count(&conn, "vc1"), 1, "alice should remain");
        assert_eq!(count(&conn, "vc2"), 0, "bob's channel should be cleared");
        assert_eq!(participants(&conn, "vc1")[0].0, "alice");
    }

    #[test]
    fn disconnect_no_presence_is_noop() {
        let conn = db();
        setup(&conn);

        // carol has no presence rows
        let affected: Vec<String> = conn
            .prepare("SELECT channel_id FROM voice_presence WHERE user_id = ?1 AND group_id = ?2")
            .unwrap()
            .query_map(rusqlite::params!["carol", "g1"], |row| row.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert!(affected.is_empty());
    }

    // ── cascade deletes ───────────────────────────────────────────────────

    #[test]
    fn deleting_channel_cascades_presence() {
        let conn = db();
        setup(&conn);

        join(&conn, "alice", "g1", "vc1", "alice");
        join(&conn, "bob", "g1", "vc1", "bob");

        conn.execute("DELETE FROM channels WHERE id = 'vc1'", []).unwrap();

        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM voice_presence WHERE channel_id = 'vc1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(total, 0, "presence should be cascade-deleted with channel");
    }

    #[test]
    fn deleting_group_cascades_presence() {
        let conn = db();
        setup(&conn);

        join(&conn, "alice", "g1", "vc1", "alice");
        join(&conn, "bob", "g1", "vc2", "bob");

        conn.execute("DELETE FROM groups WHERE id = 'g1'", []).unwrap();

        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM voice_presence",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(total, 0, "all presence should be cascade-deleted with group");
    }

    // ── leave idempotent ──────────────────────────────────────────────────

    #[test]
    fn leave_when_not_present_is_noop() {
        let conn = db();
        setup(&conn);

        // carol never joined — leave should not error
        leave(&conn, "carol", "vc1");
        assert_eq!(count(&conn, "vc1"), 0);
    }

    #[test]
    fn double_leave_is_noop() {
        let conn = db();
        setup(&conn);

        join(&conn, "alice", "g1", "vc1", "alice");
        leave(&conn, "alice", "vc1");
        leave(&conn, "alice", "vc1");

        assert_eq!(count(&conn, "vc1"), 0);
    }
}
