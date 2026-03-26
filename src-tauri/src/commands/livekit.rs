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

fn make_token(config: &Config, room_name: &str, identity: &str, display_name: &str) -> Result<String> {
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

    if data.get("type").and_then(|v| v.as_str()) != Some("new_message") {
        return;
    }

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
