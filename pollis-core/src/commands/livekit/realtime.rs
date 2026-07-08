use std::collections::HashSet;
use std::sync::Arc;

use livekit::prelude::*;

use crate::error::Result;
use crate::realtime::RealtimeEvent;
use crate::state::AppState;

use super::identity::user_id_from_identity;

/// Called once by the frontend on startup. Stores the typed Channel used to
/// push RealtimeEvents to the frontend. Safe to call again on re-login.
pub async fn subscribe_realtime(
    on_event: std::sync::Arc<dyn crate::sink::EventSink<RealtimeEvent>>,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut lk = state.livekit.lock().await;
    lk.channel = Some(on_event);
    Ok(())
}

/// Called by the frontend whenever its room list changes.
/// Connects to rooms not yet joined; disconnects rooms no longer needed.
/// Safe to call with an empty list to disconnect everything (e.g. on logout).
pub async fn connect_rooms(
    room_ids: Vec<String>,
    user_id: String,
    // Display name is now derived server-side by the DS token endpoint; the arg
    // is kept for command-signature stability with the frontend/shim.
    _username: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let url = state.config.livekit_url.clone();
    let livekit_arc = Arc::clone(&state.livekit);

    // Per-device identity (`{user}:{device}`, so a user's devices coexist in a
    // room instead of kicking each other) is now built server-side by the DS from
    // the verified signer — the client no longer mints it.

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
        // Participant token now minted by the DS (identity derived server-side
        // from the verified signer); the LiveKit API secret is no longer on the
        // client. See `commands::mls::ds_livekit_token` / `pollis-delivery::broker`.
        let token = match crate::commands::mls::ds_livekit_token(state, &room_id, "realtime").await {
            Ok((t, _url)) => t,
            Err(e) => {
                eprintln!("[realtime] token error for room {room_id}: {e}");
                let mut lk = livekit_arc.lock().await;
                lk.connecting.remove(&room_id);
                continue;
            }
        };

        let url_owned = url.clone();
        let lk_arc_connect = Arc::clone(&livekit_arc);
        let user_id_owned = user_id.clone();
        let app_state_connect = Arc::clone(state);

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

                    // Snapshot whoever was already in the room when we joined so the
                    // frontend doesn't have to wait for the next ParticipantConnected
                    // to learn about existing members.
                    emit_room_initial_presence(
                        &room,
                        &room_id_owned,
                        &lk_arc_task,
                        &user_id_task,
                    )
                    .await;

                    let handle = tokio::spawn(async move {
                        /// Process events until the stream closes. Returns how long
                        /// the connection stayed alive (used to calibrate backoff).
                        async fn run_event_loop(
                            events: &mut tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
                            lk_arc: &Arc<tokio::sync::Mutex<crate::realtime::LiveKitState>>,
                            app_state: &Arc<AppState>,
                            user_id: &str,
                            room_id: &str,
                        ) -> std::time::Duration {
                            let started = std::time::Instant::now();
                            while let Some(event) = events.recv().await {
                                match event {
                                    RoomEvent::ParticipantConnected(p) => {
                                        emit_presence(
                                            lk_arc,
                                            &p.identity().to_string(),
                                            room_id,
                                            user_id,
                                            true,
                                        )
                                        .await;
                                    }
                                    RoomEvent::ParticipantDisconnected(p) => {
                                        emit_presence(
                                            lk_arc,
                                            &p.identity().to_string(),
                                            room_id,
                                            user_id,
                                            false,
                                        )
                                        .await;
                                    }
                                    RoomEvent::DataReceived { payload, .. } => {
                                        let channel = {
                                            let lk = lk_arc.lock().await;
                                            lk.channel.clone()
                                        };
                                        if let Some(ch) = channel {
                                            let reconcile_id = super::dispatch_data(payload.as_slice(), ch.as_ref());
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
                                                    // Group-level interleaved catch-up, not a
                                                    // bare commit-only replay: a membership commit
                                                    // advances the shared group past an epoch at
                                                    // which a channel may hold an un-ingested
                                                    // message, and (max_past_epochs = 0) its keys
                                                    // would then be gone. Interleaving decrypts en
                                                    // route. `conv_id` is the mls_group_id
                                                    // (group_id for channels, dm_channel_id for DMs).
                                                    if let Err(e) = crate::commands::messages::catch_up_mls_group_interleaved(
                                                        &state, &conv_id, &uid,
                                                    ).await {
                                                        eprintln!("[mls] catch_up_mls_group for {conv_id}: {e}");
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

                        let alive_dur = run_event_loop(&mut events, &lk_arc_task, &app_state_task, &user_id_task, &room_id_owned).await;
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

                            let token = match crate::commands::mls::ds_livekit_token(&app_state_task, &room_id_owned, "realtime").await {
                                Ok((t, _url)) => t,
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

                                    // Re-snapshot remote_participants on reconnect so
                                    // anyone who was already there gets re-emitted.
                                    emit_room_initial_presence(
                                        &new_room,
                                        &room_id_owned,
                                        &lk_arc_task,
                                        &user_id_task,
                                    )
                                    .await;

                                    let alive_dur = run_event_loop(&mut new_events, &lk_arc_task, &app_state_task, &user_id_task, &room_id_owned).await;
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

/// Send a single PresenceChanged event to the frontend, given a participant's
/// raw LiveKit identity. No-ops if the identity isn't a real user (server
/// pseudo-participants, the local user themselves) — those would just be noise.
pub(super) async fn emit_presence(
    lk_arc: &Arc<tokio::sync::Mutex<crate::realtime::LiveKitState>>,
    identity: &str,
    room_id: &str,
    self_user_id: &str,
    present: bool,
) {
    let user_id = match user_id_from_identity(identity) {
        Some(uid) => uid,
        None => return,
    };
    if user_id == self_user_id {
        return;
    }
    let channel = {
        let lk = lk_arc.lock().await;
        lk.channel.clone()
    };
    if let Some(ch) = channel {
        let _ = ch.send(RealtimeEvent::PresenceChanged {
            user_id: user_id.to_string(),
            room_id: room_id.to_string(),
            present,
        });
    }
}

/// On (re)connect, replay the room's current remote_participants list as a
/// burst of PresenceChanged{present:true} events so the frontend doesn't
/// have to wait for the next room transition to learn about existing members.
pub(super) async fn emit_room_initial_presence(
    room: &Arc<livekit::Room>,
    room_id: &str,
    lk_arc: &Arc<tokio::sync::Mutex<crate::realtime::LiveKitState>>,
    self_user_id: &str,
) {
    let participants = room.remote_participants();
    for (identity, _) in participants.iter() {
        emit_presence(lk_arc, identity.as_str(), room_id, self_user_id, true).await;
    }
}
