use std::collections::HashSet;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;

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

// ── Server-side JWT (for REST API calls) ───────────────────────────────────

/// Generates an admin JWT for use with the LiveKit Server REST API.
/// Claims are built as raw JSON to exactly match what the LiveKit Go SDK produces,
/// avoiding any risk of serde struct/rename translation issues.
fn make_admin_token(config: &Config) -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| Error::Other(anyhow::anyhow!("{e}")))?
        .as_secs();

    let claims = serde_json::json!({
        "iss": config.livekit_api_key,
        "sub": config.livekit_api_key,
        "iat": now,
        "exp": now + 60,
        "nbf": now,
        "video": {
            "roomAdmin": true,
            "roomCreate": true,
        }
    });

    let mut header = Header::new(Algorithm::HS256);
    header.typ = Some("JWT".to_string());
    let key = EncodingKey::from_secret(config.livekit_api_secret.as_bytes());
    encode(&header, &claims, &key)
        .map_err(|e| Error::Other(anyhow::anyhow!("admin JWT sign: {e}")))
}

/// Sends a JSON event to a user's personal inbox LiveKit room via the
/// LiveKit Server REST API (`/twirp/livekit.RoomService/SendData`).
/// Non-fatal — if the user is offline the room may not exist; errors are logged.
pub async fn publish_to_user_inbox(
    config: &Config,
    user_id: &str,
    payload: serde_json::Value,
) -> Result<()> {
    if config.livekit_url.is_empty() || config.livekit_api_key.is_empty() {
        return Ok(());
    }

    // Convert the WebSocket URL to HTTP for the REST API
    let http_url = config.livekit_url
        .replace("wss://", "https://")
        .replace("ws://", "http://");

    let token = make_admin_token(config)?;
    let room_name = format!("inbox-{}", user_id);

    let raw = serde_json::to_vec(&payload).map_err(Error::Serde)?;
    let data_b64 = base64::engine::general_purpose::STANDARD.encode(&raw);

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/twirp/livekit.RoomService/SendData", http_url))
        .header("Authorization", format!("Bearer {}", token))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "room": room_name,
            "data": data_b64,
        }))
        .send()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("publish_to_user_inbox http: {e}")))?;

    if !resp.status().is_success() {
        // Non-fatal: room may not exist if the user is offline.
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        eprintln!("[inbox] SendData to {room_name} returned {status}: {body}");
    }

    Ok(())
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

    // Connect new rooms. Token generation and connection happen without holding
    // the lock so other commands remain responsive during the round trip.
    for room_id in &to_connect {
        let token = match make_token(&state.config, room_id, &user_id, &username) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("[realtime] token error for room {room_id}: {e}");
                let mut lk = livekit_arc.lock().await;
                lk.connecting.remove(room_id);
                continue;
            }
        };

        eprintln!("[realtime] connecting room {room_id}");

        match Room::connect(&url, &token, RoomOptions::default()).await {
            Ok((room, mut events)) => {
                let room = Arc::new(room);

                // Clone the Arc so the event task can look up the channel
                // each time it fires — this handles the case where subscribe_realtime
                // is called after connect_rooms (no race condition).
                let lk_arc_task = Arc::clone(&livekit_arc);
                let room_id_owned = room_id.clone();
                let config = state.config.clone();
                let user_id_owned = user_id.clone();
                let username_owned = username.clone();
                let url_owned = url.clone();

                let handle = tokio::spawn(async move {
                    /// Process events until the stream closes. Returns how long
                    /// the connection stayed alive (used to calibrate backoff).
                    async fn run_event_loop(
                        events: &mut tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
                        lk_arc: &Arc<tokio::sync::Mutex<crate::realtime::LiveKitState>>,
                    ) -> std::time::Duration {
                        let started = std::time::Instant::now();
                        while let Some(event) = events.recv().await {
                            if let RoomEvent::DataReceived { payload, .. } = event {
                                let channel = {
                                    let lk = lk_arc.lock().await;
                                    lk.channel.clone()
                                };
                                if let Some(ch) = channel {
                                    dispatch_data(payload.as_slice(), &ch);
                                }
                            }
                        }
                        started.elapsed()
                    }

                    let alive_dur = run_event_loop(&mut events, &lk_arc_task).await;
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

                                // Update the room reference in the map
                                {
                                    let mut lk = lk_arc_task.lock().await;
                                    if let Some(entry) = lk.rooms.get_mut(&room_id_owned) {
                                        entry.0 = Arc::clone(&new_room);
                                    }
                                }

                                let alive_dur = run_event_loop(&mut new_events, &lk_arc_task).await;
                                eprintln!(
                                    "[realtime] event stream closed again for room {room_id_owned} (was alive {:.0}s), reconnecting…",
                                    alive_dur.as_secs_f64()
                                );
                                // If the connection was very short-lived, back off more
                                // aggressively to avoid hammering the server.
                                backoff = if alive_dur.as_secs() < 10 { (backoff * 2).min(300) } else { 5 };
                            }
                            Err(e) => {
                                eprintln!("[realtime] reconnect failed for room {room_id_owned}: {e}");
                                backoff = (backoff * 2).min(300);
                            }
                        }
                    }
                });

                let mut lk = livekit_arc.lock().await;
                lk.connecting.remove(room_id);
                lk.rooms.insert(room_id.clone(), (room, handle));
                eprintln!("[realtime] connected room {room_id}");
            }
            Err(e) => {
                let mut lk = livekit_arc.lock().await;
                lk.connecting.remove(room_id);
                eprintln!("[realtime] failed to connect room {room_id}: {e}");
            }
        }
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
        _ => {}
    }
}
