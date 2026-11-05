//! Remote video (screen share and webcam) drain + frame WebSocket plumbing.

use std::sync::Arc;

use libwebrtc::{video_frame::VideoBuffer, video_stream::native::NativeVideoStream};
use livekit::track::RemoteVideoTrack;

use crate::state::AppState;

use super::{
    codec::pack_frame_bytes,
    stop::stop_screen_share,
    wants::{wants_channel, WantsRx},
    RemoteVideoSource, ScreenShareEvent,
};

pub async fn on_remote_video_subscribed(
    track: RemoteVideoTrack,
    participant_identity: String,
    source: RemoteVideoSource,
    state: &Arc<AppState>,
) {
    let track_key = format!("{}-{}", participant_identity, track.sid());
    crate::diag!("[screenshare] remote video subscribed: {track_key} (source={source:?})");

    // The publication is what carries a tile's wants to the SFU; it is looked up by sid
    // because the identity in `track_key` is the resolved one, not LiveKit's.
    let publication = {
        let room = state.voice.lock().await.room.clone();
        room.and_then(|room| {
            let sid = track.sid();
            room.remote_participants()
                .values()
                .find_map(|p| p.get_track_publication(&sid))
        })
    };

    let (events, frames, wants_rx) = {
        let mut ss = state.screenshare.lock().await;
        let rx = wants_channel(&mut ss, &track_key);
        if let Some(publication) = publication {
            ss.remote_publications
                .insert(track_key.clone(), publication);
        }
        (ss.events.clone(), ss.frames.clone(), rx)
    };
    // WebSocket fan-out, served at `/screenshare/<token>` (see `media_server.rs`).
    let frame_tx = state.screenshare_frame_tx.clone();

    let mut stream = NativeVideoStream::new(track.rtc_track());
    let track_key_for_task = track_key.clone();
    let identity_clone = participant_identity.clone();
    let events_for_task = events.clone();
    let task = tokio::spawn(async move {
        use futures_util::StreamExt;
        let mut announced: Option<(u32, u32)> = None;
        let mut last_packed: Option<std::time::Instant> = None;
        let wants: WantsRx = wants_rx;
        // No stall watchdog: idle capture just stops frames; the canvas keeps the last.
        while let Some(frame) = stream.next().await {
            let want = *wants.borrow();
            // A hidden track is paused at the SFU; anything still in flight is not worth
            // a conversion either.
            if want.is_hidden() {
                continue;
            }
            // Pace to the fastest tile; the dropped frames were never going to paint.
            if let Some(interval) = want.min_interval() {
                if last_packed.is_some_and(|t| t.elapsed() < interval) {
                    continue;
                }
            }
            let mut i420 = frame.buffer.to_i420();
            let w = i420.width();
            let h = i420.height();
            if announced != Some((w, h)) {
                announced = Some((w, h));
                if let Some(ev) = &events_for_task {
                    let _ = ev.send(ScreenShareEvent::RemoteStarted {
                        track_key: track_key_for_task.clone(),
                        identity: identity_clone.clone(),
                        width: w,
                        height: h,
                        source,
                    });
                }
            }
            // Pack once for WS subscribers and any legacy Channel sink.
            if frame_tx.receiver_count() > 0 || frames.is_some() {
                last_packed = Some(std::time::Instant::now());
                let packed = match want.pack_size((w, h)) {
                    Some((tw, th)) => i420.scale(tw as i32, th as i32),
                    None => i420,
                };
                let bytes = pack_frame_bytes(
                    &track_key_for_task,
                    packed.width(),
                    packed.height(),
                    frame.timestamp_us,
                    &packed,
                );
                let bytes = std::sync::Arc::new(bytes);
                let _ = frame_tx.send(bytes.clone());
                if let Some(sink) = &frames {
                    let _ = sink.send((*bytes).clone());
                }
            }
        }
    });

    let mut ss = state.screenshare.lock().await;
    if let Some(prev) = ss.remote_drain_tasks.insert(track_key, task) {
        prev.abort();
    }
}

/// Forget a remote track: its drain task, its publication and its wants.
fn forget_remote(ss: &mut super::ScreenShareState, track_key: &str) {
    if let Some(t) = ss.remote_drain_tasks.remove(track_key) {
        t.abort();
    }
    ss.remote_publications.remove(track_key);
    ss.video_wants.remove(track_key);
}

pub async fn on_remote_video_unsubscribed(
    track: RemoteVideoTrack,
    participant_identity: String,
    state: &Arc<AppState>,
) {
    let track_key = format!("{}-{}", participant_identity, track.sid());
    let mut ss = state.screenshare.lock().await;
    forget_remote(&mut ss, &track_key);
    if let Some(ev) = &ss.events {
        let _ = ev.send(ScreenShareEvent::RemoteStopped { track_key });
    }
}

/// Tear down a leaving participant's screenshares (no reliable `TrackUnsubscribed`).
pub async fn on_participant_left(participant_identity: &str, state: &Arc<AppState>) {
    let prefix = format!("{participant_identity}-");
    let (events, stopped_keys) = {
        let mut ss = state.screenshare.lock().await;
        let keys: Vec<String> = ss
            .remote_drain_tasks
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect();
        for k in &keys {
            forget_remote(&mut ss, k);
        }
        (ss.events.clone(), keys)
    };
    if let Some(ev) = events {
        for track_key in stopped_keys {
            let _ = ev.send(ScreenShareEvent::RemoteStopped { track_key });
        }
    }
}

pub async fn on_room_disconnected(state: &Arc<AppState>) {
    let _ = stop_screen_share(state).await;
    let mut ss = state.screenshare.lock().await;
    let keys: Vec<String> = ss.remote_drain_tasks.keys().cloned().collect();
    for k in &keys {
        forget_remote(&mut ss, k);
    }
}
