use std::collections::HashSet;
use std::sync::Arc;

use livekit::prelude::*;

use crate::error::Result;
use crate::realtime::RealtimeEvent;
use crate::state::AppState;

/// Called once by the frontend on startup. Stores the typed Channel used to
/// push RealtimeEvents to the frontend. Safe to call again on re-login.
pub async fn subscribe_realtime(
    on_event: std::sync::Arc<dyn crate::sink::EventSink<RealtimeEvent>>,
    state: &Arc<AppState>,
) -> Result<()> {
    let rooms: Vec<(String, Arc<livekit::Room>)> = {
        let mut lk = state.livekit.lock().await;
        lk.channel = Some(on_event);
        lk.rooms.iter().map(|(id, (room, _))| (id.clone(), Arc::clone(room))).collect()
    };
    // A sink that arrives after the rooms (the renderer reloaded) missed their connect-time seed (#1919).
    if rooms.is_empty() {
        return Ok(());
    }
    if let Ok(user_id) = crate::commands::mls::current_user_id(state).await {
        for (room_id, room) in rooms {
            emit_room_initial_presence(&room, &room_id, state, &user_id).await;
        }
    }
    Ok(())
}

/// Called by the frontend whenever its room list changes.
/// Connects to rooms not yet joined; disconnects rooms no longer needed.
/// Safe to call with an empty list to disconnect everything (e.g. on logout).
pub async fn connect_rooms(
    room_ids: Vec<String>,
    user_id: String,
    // Unused (the DS derives it); kept for command-signature stability.
    _username: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let livekit_arc = Arc::clone(&state.livekit);

    // Diff under a brief lock, counting mid-connect rooms to avoid duplicates.
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

    // One lock per removal; never held across the async disconnect.
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

    // One task per room so one failure doesn't block the others.
    for room_id in to_connect {
        // Use the URL the DS minted this token for: it's only valid there.
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
                    let room_task = Arc::clone(&room);

                    // Look up the channel per event, so a late subscribe_realtime still works.
                    let lk_arc_task = Arc::clone(&lk_arc_connect);
                    let room_id_owned = room_id.clone();
                    let app_state_task = Arc::clone(&app_state_connect);
                    let user_id_task = user_id_owned.clone();

                    // Seed presence with whoever was already in the room.
                    emit_room_initial_presence(
                        &room,
                        &room_id_owned,
                        &app_state_connect,
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
                            room: &Arc<livekit::Room>,
                        ) -> std::time::Duration {
                            let started = std::time::Instant::now();
                            while let Some(event) = events.recv().await {
                                match event {
                                    RoomEvent::ParticipantConnected(p)
                                    | RoomEvent::ParticipantDisconnected(p) => {
                                        emit_presence(
                                            app_state,
                                            room,
                                            &p.identity().to_string(),
                                            room_id,
                                            user_id,
                                        )
                                        .await;
                                    }
                                    // #2110: a peer's session started or stopped being in use.
                                    RoomEvent::ParticipantAttributesChanged {
                                        participant: Participant::Remote(p),
                                        ..
                                    } => {
                                        emit_presence(
                                            app_state,
                                            room,
                                            &p.identity().to_string(),
                                            room_id,
                                            user_id,
                                        )
                                        .await;
                                    }
                                    RoomEvent::DataReceived { payload, participant, .. } => {
                                        // #836: resolve the publisher; `None` for DS-side emitters.
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
                                        let (channel, wake_only) = {
                                            let lk = lk_arc.lock().await;
                                            (lk.channel.clone(), lk.wake_only)
                                        };
                                        if let Some(ch) = channel {
                                            let wake = super::dispatch_data(payload.as_slice(), sender.as_ref(), ch.as_ref());
                                            // Membership wake: catch up; on a LEAVE stage the remove (`mls::apply_membership_wake`).
                                            // A wake-only listener (the bot runtime) leaves that to its next job.
                                            if let Some(super::MembershipWake { conversation_id, is_leave }) = wake.filter(|_| !wake_only) {
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
                                    // SDK healed itself without closing the stream: renderer catches up from DS.
                                    RoomEvent::Reconnected => {
                                        crate::diag!("[realtime] resumed room {room_id}");
                                        let channel = {
                                            let lk = lk_arc.lock().await;
                                            lk.channel.clone()
                                        };
                                        if let Some(ch) = channel {
                                            let _ = ch.send(RealtimeEvent::RealtimeReconnected {
                                                room_id: room_id.to_owned(),
                                            });
                                        }
                                        // Re-seed presence (the renderer wiped it).
                                        emit_room_initial_presence(room, room_id, app_state, user_id).await;
                                    }
                                    _ => {}
                                }
                            }
                            started.elapsed()
                        }

                        let alive_dur = run_event_loop(&mut events, &lk_arc_task, &app_state_task, &user_id_task, &room_id_owned, &room_task).await;
                        crate::diag!(
                            "[realtime] event stream closed for room {room_id_owned} (was alive {:.0}s), reconnecting…",
                            alive_dur.as_secs_f64()
                        );

                        let mut backoff = if alive_dur.as_secs() < 10 { 30u64 } else { 5 };
                        let gate = super::sfu_gate::sfu_gate();
                        loop {
                            // During an SFU outage only the prober dials; the rest park here and
                            // follow it back, so the DS mints one token per probe, not one per room.
                            if gate.is_down() && gate.report_down(&room_id_owned) == super::sfu_gate::Role::Waiter {
                                crate::diag!("[realtime] room {room_id_owned}: SFU down, waiting for the probe");
                                gate.wait_up(std::time::Duration::from_secs(300)).await;
                                backoff = 5;
                            } else {
                                tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
                            }

                            // Removed from the room map (left/logged out): stop reconnecting.
                            {
                                let lk = lk_arc_task.lock().await;
                                if !lk.rooms.contains_key(&room_id_owned) {
                                    crate::diag!("[realtime] room {room_id_owned} removed, stopping reconnect");
                                    gate.release_prober(&room_id_owned);
                                    return;
                                }
                            }

                            // Re-mint every reconnect: the DS may move servers; the short TTL bounds revoked access.
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
                                    gate.report_up();

                                    // Let the frontend resync state that drifted during the outage.
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
                                    // A new session starts with no attribute (#2110).
                                    publish_activity_on_join(&app_state_task, &room_id_owned).await;

                                    // Re-emit presence for who's already there.
                                    emit_room_initial_presence(
                                        &new_room,
                                        &room_id_owned,
                                        &app_state_task,
                                        &user_id_task,
                                    )
                                    .await;

                                    let alive_dur = run_event_loop(&mut new_events, &lk_arc_task, &app_state_task, &user_id_task, &room_id_owned, &new_room).await;
                                    crate::diag!(
                                        "[realtime] event stream closed again for room {room_id_owned} (was alive {:.0}s), reconnecting…",
                                        alive_dur.as_secs_f64()
                                    );
                                    backoff = if alive_dur.as_secs() < 10 { (backoff * 2).min(300) } else { 5 };
                                }
                                Err(e) => {
                                    crate::diag!("[realtime] reconnect failed for room {room_id_owned}: {e}");
                                    if super::sfu_unreachable(&e) {
                                        gate.report_down(&room_id_owned);
                                    }
                                    backoff = (backoff * 2).min(300);
                                }
                            }
                        }
                    });

                    crate::diag!("[realtime] connected room {room_id}");
                    {
                        let mut lk = lk_arc_connect.lock().await;
                        lk.connecting.remove(&room_id);
                        lk.rooms.insert(room_id.clone(), (room, handle));
                    }
                    // In the map first, so a change from here on reaches this room too (#2110).
                    publish_activity_on_join(&app_state_connect, &room_id).await;
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

/// Every remote session in `room`: its wire identity, and whether its `active` attribute says in
/// use (#2110; no attribute = in use, so a build that predates it shows online).
fn roster(room: &livekit::Room) -> Vec<(String, bool)> {
    room.remote_participants()
        .iter()
        .map(|(identity, p)| {
            let attributes = p.attributes();
            let active = crate::commands::livekit_roster::session_active(
                attributes.get(crate::commands::livekit_roster::ACTIVE_ATTRIBUTE).map(String::as_str),
            );
            (identity.to_string(), active)
        })
        .collect()
}

/// Send one PresenceChanged; `status` is the user's state in the room (`None` = gone).
async fn send_presence(state: &Arc<AppState>, user_id: &str, room_id: &str, status: Option<bool>) {
    let channel = {
        let lk = state.livekit.lock().await;
        lk.channel.clone()
    };
    if let Some(ch) = channel {
        let _ = ch.send(RealtimeEvent::PresenceChanged {
            user_id: user_id.to_string(),
            room_id: room_id.to_string(),
            present: status.is_some(),
            active: status.unwrap_or(true),
        });
    }
}

/// Report the user behind one session (pseudonymous, #836) as `room` now shows them: present and
/// in use, present and away, or gone. It reads the whole roster, so an arrival, a departure and an
/// attribute change are one path: a user is in the room while any session of theirs is (one per
/// device, and in a DM a call's session beside the realtime one, #1919), and in use while any of
/// those is (#2110). No-op for non-users and unresolved ids: a wrong name is worse than none.
pub(super) async fn emit_presence(
    state: &Arc<AppState>,
    room: &livekit::Room,
    identity: &str,
    room_id: &str,
    self_user_id: &str,
) {
    let lane = presence_lane(state).await;
    let _ordered = lane.lock().await;
    // The SDK updates the map before it reports a change, so a departed session is already gone.
    let sessions = roster(room);
    let mut wire: Vec<String> = sessions.iter().map(|(id, _)| id.clone()).collect();
    wire.push(identity.to_string());
    let resolved = super::identity::resolve_participants(state, room_id, &wire).await;
    // The one presence rule, shared with mobile's `livekit_room_presence` (#2059).
    let user_id = match resolved
        .get(identity)
        .and_then(|(internal, _)| crate::commands::livekit_roster::present_user(internal, self_user_id))
    {
        Some(uid) => uid.to_string(),
        None => return,
    };
    let users = presence_of(&sessions, &resolved, self_user_id);
    send_presence(state, &user_id, room_id, users.get(&user_id).copied()).await;
}

/// Each user `sessions` show, with whether any of their sessions is in use.
fn presence_of(
    sessions: &[(String, bool)],
    resolved: &std::collections::HashMap<String, (String, String)>,
    self_user_id: &str,
) -> std::collections::BTreeMap<String, bool> {
    crate::commands::livekit_roster::presence_by_user(
        sessions
            .iter()
            .filter_map(|(id, active)| resolved.get(id).map(|(internal, _)| (internal.as_str(), *active))),
        self_user_id,
    )
}

async fn presence_lane(state: &Arc<AppState>) -> Arc<tokio::sync::Mutex<()>> {
    Arc::clone(&state.livekit.lock().await.presence_lane)
}

/// On (re)connect, emit PresenceChanged for every user already in the room.
pub(super) async fn emit_room_initial_presence(
    room: &Arc<livekit::Room>,
    room_id: &str,
    state: &Arc<AppState>,
    self_user_id: &str,
) {
    let lane = presence_lane(state).await;
    let _ordered = lane.lock().await;
    let sessions = roster(room);
    // One batched resolve for the whole snapshot.
    let wire: Vec<String> = sessions.iter().map(|(id, _)| id.clone()).collect();
    let resolved = super::identity::resolve_participants(state, room_id, &wire).await;
    for (user_id, active) in presence_of(&sessions, &resolved, self_user_id) {
        send_presence(state, &user_id, room_id, Some(active)).await;
    }
}

/// The renderer reports whether the user is at the app (#2110): focused or used within the idle
/// window. Peers see it as this device's `active` attribute on every room it is in.
pub async fn set_presence_active(active: bool, state: &Arc<AppState>) -> Result<()> {
    update_activity(state, |lk| lk.renderer_active = active).await;
    Ok(())
}

/// The shell hid the window (closed to the tray or Dock) or showed it again (#2110). A hidden
/// window is not in use whatever the renderer last said; showing it hands back to the renderer.
pub async fn set_window_hidden(hidden: bool, state: &Arc<AppState>) -> Result<()> {
    update_activity(state, |lk| lk.window_hidden = hidden).await;
    Ok(())
}

/// Apply `change`; when it flips whether this device is in use, write the attribute. On change
/// only: no timer, no heartbeat.
async fn update_activity(state: &Arc<AppState>, change: impl FnOnce(&mut crate::realtime::LiveKitState)) {
    let flipped = {
        let mut lk = state.livekit.lock().await;
        let before = lk.in_use();
        change(&mut lk);
        lk.in_use() != before
    };
    if flipped {
        let state = Arc::clone(state);
        tokio::spawn(async move { publish_activity(&state, None).await });
    }
}

/// Write this device's current `active` attribute to every connected room, or to `only`. Writes
/// are ordered and each reads the state when it runs, so the last one always wins.
async fn publish_activity(state: &Arc<AppState>, only: Option<&str>) {
    let lane = Arc::clone(&state.livekit.lock().await.activity_lane);
    let _ordered = lane.lock().await;
    let (rooms, active) = {
        let lk = state.livekit.lock().await;
        let rooms: Vec<(String, Arc<livekit::Room>)> = lk
            .rooms
            .iter()
            .filter(|(id, _)| only.is_none_or(|o| o == id.as_str()))
            .map(|(id, (room, _))| (id.clone(), Arc::clone(room)))
            .collect();
        (rooms, lk.in_use())
    };
    for (room_id, room) in rooms {
        let attributes = std::collections::HashMap::from([(
            crate::commands::livekit_roster::ACTIVE_ATTRIBUTE.to_string(),
            crate::commands::livekit_roster::active_attribute_value(active).to_string(),
        )]);
        if let Err(e) = room.local_participant().set_attributes(attributes).await {
            crate::diag!("[realtime] activity for room {room_id} not written: {e}");
        }
    }
}

/// A room just (re)joined starts with no attribute, which reads as in use: write it only when
/// this device is not.
async fn publish_activity_on_join(state: &Arc<AppState>, room_id: &str) {
    if !state.livekit.lock().await.in_use() {
        publish_activity(state, Some(room_id)).await;
    }
}
