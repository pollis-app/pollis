use std::collections::HashSet;
use std::sync::Arc;

use livekit::prelude::*;

use crate::error::Result;
use crate::realtime::RealtimeEvent;
use crate::state::AppState;

use super::identity::user_id_from_identity;

/// Store the channel used to push RealtimeEvents to the frontend. Called on
/// startup; safe to call again on re-login.
pub async fn subscribe_realtime(
    on_event: std::sync::Arc<dyn crate::sink::EventSink<RealtimeEvent>>,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut lk = state.livekit.lock().await;
    lk.channel = Some(on_event);
    Ok(())
}

/// Connect rooms not yet joined and disconnect rooms no longer listed (an
/// empty list disconnects everything, e.g. on logout).
pub async fn connect_rooms(
    room_ids: Vec<String>,
    user_id: String,
    // Unused: the DS derives the display name. Kept for command-signature
    // stability.
    _username: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let livekit_arc = Arc::clone(&state.livekit);

    // Compute the diff under a brief lock; rooms mid-connection count as
    // present, so concurrent calls never connect the same room twice.
    let (to_remove, to_connect) = {
        let mut lk = livekit_arc.lock().await;
        let next: HashSet<String> = room_ids.into_iter().collect();
        let current: HashSet<String> = lk.rooms.keys().cloned().collect();

        let remove: Vec<String> = current.difference(&next).cloned().collect();
        let connect: Vec<String> = next.difference(&current)
            .filter(|id| !lk.connecting.contains(*id))
            .cloned()
            .collect();

        for id in &connect {
            lk.connecting.insert(id.clone());
        }

        (remove, connect)
    };

    // One lock acquisition per removal, never held across the disconnect.
    for room_id in &to_remove {
        let removed = {
            let mut lk = livekit_arc.lock().await;
            lk.connecting.remove(room_id);
            lk.rooms.remove(room_id)
        };
        if let Some((_room, handle)) = removed {
            handle.abort();
            crate::diag!("[realtime] disconnected room {room_id}");
        }
    }

    // One task per room, so a timeout on one never delays the others.
    for room_id in to_connect {
        // The DS mints the token (identity from the verified signer, #393) and
        // names the server it is valid at; dial that one, not a compiled-in
        // URL, so the SFU can move without shipping a client.
        let (token, ds_url) =
            match crate::commands::mls::ds_livekit_token(state, &room_id, "realtime").await {
                Ok(pair) => pair,
                Err(e) => {
                    crate::diag!("[realtime] token error for room {room_id}: {e}");
                    let mut lk = livekit_arc.lock().await;
                    lk.connecting.remove(&room_id);
                    continue;
                }
            };

        let url_owned = ds_url;
        let lk_arc_connect = Arc::clone(&livekit_arc);
        let user_id_owned = user_id.clone();
        let app_state_connect = Arc::clone(state);

        crate::diag!("[realtime] connecting room {room_id}");

        tokio::spawn(async move {
            match Room::connect(&url_owned, &token, RoomOptions::default()).await {
                Ok((room, mut events)) => {
                    let room = Arc::new(room);

                    // The event task looks the channel up on every event, so a
                    // later `subscribe_realtime` still takes effect.
                    let lk_arc_task = Arc::clone(&lk_arc_connect);
                    let room_id_owned = room_id.clone();
                    let app_state_task = Arc::clone(&app_state_connect);
                    let user_id_task = user_id_owned.clone();

                    // Snapshot members already in the room.
                    emit_room_initial_presence(
                        &room,
                        &room_id_owned,
                        &app_state_connect,
                        &user_id_task,
                    )
                    .await;

                    let handle = tokio::spawn(async move {
                        /// Process events until the stream closes; returns how
                        /// long it was alive (to calibrate backoff).
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
                                            app_state,
                                            &p.identity().to_string(),
                                            room_id,
                                            user_id,
                                            true,
                                        )
                                        .await;
                                    }
                                    RoomEvent::ParticipantDisconnected(p) => {
                                        emit_presence(
                                            app_state,
                                            &p.identity().to_string(),
                                            room_id,
                                            user_id,
                                            false,
                                        )
                                        .await;
                                    }
                                    RoomEvent::DataReceived { payload, participant, .. } => {
                                        // #836: broadcasts no longer name their actor, so
                                        // resolve the publisher. `None` for DS-sent packets.
                                        let sender = match participant {
                                            Some(p) => {
                                                let wire = p.identity().to_string();
                                                let (internal, name) =
                                                    super::identity::resolve_participant(
                                                        app_state, room_id, &wire,
                                                    )
                                                    .await;
                                                super::identity::user_id_from_identity(&internal)
                                                    .map(|uid| super::PacketSender {
                                                        user_id: uid.to_string(),
                                                        username: (!name.is_empty()).then_some(name),
                                                    })
                                            }
                                            None => None,
                                        };
                                        let channel = {
                                            let lk = lk_arc.lock().await;
                                            lk.channel.clone()
                                        };
                                        if let Some(ch) = channel {
                                            let wake = super::dispatch_data(payload.as_slice(), sender.as_ref(), ch.as_ref());
                                            // On a LEAVE this also stages the remove nobody
                                            // else will (`mls::apply_membership_wake`).
                                            if let Some(super::MembershipWake { conversation_id, is_leave }) = wake {
                                                let state = Arc::clone(app_state);
                                                let uid = user_id.to_owned();
                                                tokio::spawn(async move {
                                                    crate::commands::mls::apply_membership_wake(
                                                        &state, &conversation_id, &uid, is_leave,
                                                    ).await;
                                                });
                                            }
                                        }
                                    }
                                    // The SDK healed the connection itself (signal
                                    // resume, ICE restart) without closing the
                                    // stream, so the reconnect path below never
                                    // runs. Wake-ups sent meanwhile are lost (often
                                    // a message from this user's other device, which
                                    // raises no push), so tell the renderer to
                                    // catch the room up, as a full reconnect does.
                                    RoomEvent::Reconnected => {
                                        crate::diag!("[realtime] resumed room {room_id}");
                                        let (channel, room) = {
                                            let lk = lk_arc.lock().await;
                                            (lk.channel.clone(), lk.rooms.get(room_id).map(|(r, _)| Arc::clone(r)))
                                        };
                                        if let Some(ch) = channel {
                                            let _ = ch.send(RealtimeEvent::RealtimeReconnected {
                                                room_id: room_id.to_owned(),
                                            });
                                        }
                                        // The renderer wipes the room's presence on
                                        // that event; re-seed it.
                                        if let Some(room) = room {
                                            emit_room_initial_presence(&room, room_id, app_state, user_id).await;
                                        }
                                    }
                                    _ => {}
                                }
                            }
                            started.elapsed()
                        }

                        let alive_dur = run_event_loop(&mut events, &lk_arc_task, &app_state_task, &user_id_task, &room_id_owned).await;
                        crate::diag!(
                            "[realtime] event stream closed for room {room_id_owned} (was alive {:.0}s), reconnecting…",
                            alive_dur.as_secs_f64()
                        );

                        let mut backoff = if alive_dur.as_secs() < 10 { 30u64 } else { 5 };
                        loop {
                            tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;

                            // Removed from the map (left, or logged out): stop.
                            {
                                let lk = lk_arc_task.lock().await;
                                if !lk.rooms.contains_key(&room_id_owned) {
                                    crate::diag!("[realtime] room {room_id_owned} removed, stopping reconnect");
                                    return;
                                }
                            }

                            // Re-mint the token and URL on every reconnect: the
                            // DS may name a different server (failover), and the
                            // short token TTL (`broker::LIVEKIT_TOKEN_TTL_SECS`)
                            // only bounds a revoked member's re-entry because
                            // membership is re-checked at every mint.
                            let (token, reconnect_url) = match crate::commands::mls::ds_livekit_token(&app_state_task, &room_id_owned, "realtime").await {
                                Ok(pair) => pair,
                                Err(e) => {
                                    crate::diag!("[realtime] reconnect token error for room {room_id_owned}: {e}");
                                    backoff = (backoff * 2).min(300);
                                    continue;
                                }
                            };

                            match Room::connect(&reconnect_url, &token, RoomOptions::default()).await {
                                Ok((new_room, mut new_events)) => {
                                    let new_room = Arc::new(new_room);
                                    crate::diag!("[realtime] reconnected room {room_id_owned}");

                                    // Let the frontend resync what drifted during
                                    // the outage.
                                    {
                                        let lk = lk_arc_task.lock().await;
                                        if let Some(ch) = lk.channel.clone() {
                                            let _ = ch.send(RealtimeEvent::RealtimeReconnected {
                                                room_id: room_id_owned.clone(),
                                            });
                                        }
                                    }

                                    {
                                        let mut lk = lk_arc_task.lock().await;
                                        if let Some(entry) = lk.rooms.get_mut(&room_id_owned) {
                                            entry.0 = Arc::clone(&new_room);
                                        }
                                    }

                                    emit_room_initial_presence(
                                        &new_room,
                                        &room_id_owned,
                                        &app_state_task,
                                        &user_id_task,
                                    )
                                    .await;

                                    let alive_dur = run_event_loop(&mut new_events, &lk_arc_task, &app_state_task, &user_id_task, &room_id_owned).await;
                                    crate::diag!(
                                        "[realtime] event stream closed again for room {room_id_owned} (was alive {:.0}s), reconnecting…",
                                        alive_dur.as_secs_f64()
                                    );
                                    backoff = if alive_dur.as_secs() < 10 { (backoff * 2).min(300) } else { 5 };
                                }
                                Err(e) => {
                                    crate::diag!("[realtime] reconnect failed for room {room_id_owned}: {e}");
                                    backoff = (backoff * 2).min(300);
                                }
                            }
                        }
                    });

                    crate::diag!("[realtime] connected room {room_id}");
                    let mut lk = lk_arc_connect.lock().await;
                    lk.connecting.remove(&room_id);
                    lk.rooms.insert(room_id, (room, handle));
                }
                Err(e) => {
                    crate::diag!("[realtime] failed to connect room {room_id}: {e}");
                    let mut lk = lk_arc_connect.lock().await;
                    lk.connecting.remove(&room_id);
                }
            }
        });
    }

    Ok(())
}

/// Send a PresenceChanged event for a participant's LiveKit identity, resolved
/// through the DS first (#836). A no-op for internal participants, the local
/// user, or an unresolvable identity: showing the wrong person beside a green
/// dot is worse than showing nobody.
pub(super) async fn emit_presence(
    state: &Arc<AppState>,
    identity: &str,
    room_id: &str,
    self_user_id: &str,
    present: bool,
) {
    let (internal, _) = super::identity::resolve_participant(state, room_id, identity).await;
    let user_id = match user_id_from_identity(&internal) {
        Some(uid) => uid,
        None => return,
    };
    if user_id == self_user_id {
        return;
    }
    let channel = {
        let lk = state.livekit.lock().await;
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

/// On (re)connect, replay the room's current participants as
/// PresenceChanged{present:true}.
pub(super) async fn emit_room_initial_presence(
    room: &Arc<livekit::Room>,
    room_id: &str,
    state: &Arc<AppState>,
    self_user_id: &str,
) {
    let participants = room.remote_participants();
    // One batched resolve for the whole snapshot.
    let wire: Vec<String> = participants.keys().map(|k| k.to_string()).collect();
    let _ = super::identity::resolve_participants(state, room_id, &wire).await;
    for identity in wire {
        emit_presence(state, &identity, room_id, self_user_id, true).await;
    }
}
