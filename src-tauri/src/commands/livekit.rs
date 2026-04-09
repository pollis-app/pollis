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
        let token = match make_token(&state.config, &room_id, &user_id, &username) {
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
        let user_id_owned = user_id.clone();
        let username_owned = username.clone();
        let remote_db_connect = Arc::clone(&state.remote_db);

        eprintln!("[realtime] connecting room {room_id}");

        tokio::spawn(async move {
            match Room::connect(&url_owned, &token, RoomOptions::default()).await {
                Ok((room, mut events)) => {
                    let room = Arc::new(room);

                    // On connect, reconcile voice_presence: delete rows for any user who
                    // has a presence record for this group but is not currently in the room.
                    // This cleans up orphaned rows from previous crash disconnects.
                    let online_ids: HashSet<String> = room
                        .remote_participants()
                        .keys()
                        .map(|id| id.to_string())
                        .chain(std::iter::once(user_id_owned.clone()))
                        .collect();
                    reconcile_voice_presence(&remote_db_connect, &room_id, &online_ids).await;

                    // Clone the Arc so the event task can look up the channel
                    // each time it fires — this handles the case where subscribe_realtime
                    // is called after connect_rooms (no race condition).
                    let lk_arc_task = Arc::clone(&lk_arc_connect);
                    let room_id_owned = room_id.clone();
                    let remote_db_task = Arc::clone(&remote_db_connect);

                    let handle = tokio::spawn(async move {
                        /// Process events until the stream closes. Returns how long
                        /// the connection stayed alive (used to calibrate backoff).
                        async fn run_event_loop(
                            events: &mut tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
                            lk_arc: &Arc<tokio::sync::Mutex<crate::realtime::LiveKitState>>,
                            remote_db: &Arc<RemoteDb>,
                            room_id: &str,
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
                                            dispatch_data(payload.as_slice(), &ch);
                                        }
                                    }
                                    RoomEvent::ParticipantDisconnected(participant) => {
                                        let identity = participant.identity().to_string();
                                        // Clean up any voice presence rows for this user in
                                        // this group and notify the frontend for each.
                                        handle_participant_disconnect(
                                            remote_db,
                                            lk_arc,
                                            room_id,
                                            &identity,
                                        )
                                        .await;
                                    }
                                    _ => {}
                                }
                            }
                            started.elapsed()
                        }

                        let alive_dur = run_event_loop(&mut events, &lk_arc_task, &remote_db_task, &room_id_owned).await;
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

                            let token = match make_token(&config, &room_id_owned, &user_id_owned, &username_owned) {
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
                                        .map(|id| id.to_string())
                                        .chain(std::iter::once(user_id_owned.clone()))
                                        .collect();
                                    reconcile_voice_presence(&remote_db_task, &room_id_owned, &online_ids).await;

                                    // Update the room reference in the map
                                    {
                                        let mut lk = lk_arc_task.lock().await;
                                        if let Some(entry) = lk.rooms.get_mut(&room_id_owned) {
                                            entry.0 = Arc::clone(&new_room);
                                        }
                                    }

                                    let alive_dur = run_event_loop(&mut new_events, &lk_arc_task, &remote_db_task, &room_id_owned).await;
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
fn dispatch_data(payload: &[u8], channel: &tauri::ipc::Channel<RealtimeEvent>) {
    let text = match std::str::from_utf8(payload) {
        Ok(s) => s,
        Err(_) => return,
    };
    let data: serde_json::Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(_) => return,
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
            let _ = channel.send(RealtimeEvent::MembershipChanged {});
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
        _ => {}
    }
}
