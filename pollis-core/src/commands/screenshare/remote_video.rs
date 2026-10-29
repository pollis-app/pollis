//! Remote video tracks (screen share and webcam) from the voice room loop:
//! one drain and frame WebSocket for both, told apart by the `source` tag on
//! `RemoteStarted`.

use std::sync::Arc;

use libwebrtc::{video_frame::VideoBuffer, video_stream::native::NativeVideoStream};
use livekit::track::RemoteVideoTrack;

use crate::state::AppState;

use super::{codec::pack_frame_bytes, stop::stop_screen_share, RemoteVideoSource, ScreenShareEvent};

pub async fn on_remote_video_subscribed(
    track: RemoteVideoTrack,
    participant_identity: String,
    source: RemoteVideoSource,
    state: &Arc<AppState>,
) {
    let track_key = format!("{}-{}", participant_identity, track.sid());
    crate::diag!("[screenshare] remote video subscribed: {track_key} (source={source:?})");

    let (events, frames) = {
        let ss = state.screenshare.lock().await;
        (ss.events.clone(), ss.frames.clone())
    };
    // Fan-out to the loopback media server's `/screenshare/<token>` WebSocket
    // (`media_server.rs`).
    let frame_tx = state.screenshare_frame_tx.clone();

    let mut stream = NativeVideoStream::new(track.rtc_track());
    let track_key_for_task = track_key.clone();
    let identity_clone = participant_identity.clone();
    let events_for_task = events.clone();
    let task = tokio::spawn(async move {
        use futures_util::StreamExt;
        let mut announced: Option<(u32, u32)> = None;
        // No stall watchdog: idle capture just stops frames, and the canvas
        // keeps the last paint. The unsubscribe path emits RemoteStopped.
        while let Some(frame) = stream.next().await {
            let i420 = frame.buffer.to_i420();
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
            // Pack once, for WebSocket subscribers and any legacy Channel sink;
            // `send` with no receivers is a cheap no-op.
            if frame_tx.receiver_count() > 0 || frames.is_some() {
                let bytes = pack_frame_bytes(
                    &track_key_for_task,
                    w,
                    h,
                    frame.timestamp_us,
                    &i420,
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

pub async fn on_remote_video_unsubscribed(
    track: RemoteVideoTrack,
    participant_identity: String,
    state: &Arc<AppState>,
) {
    let track_key = format!("{}-{}", participant_identity, track.sid());
    let mut ss = state.screenshare.lock().await;
    if let Some(t) = ss.remote_drain_tasks.remove(&track_key) {
        t.abort();
    }
    if let Some(ev) = &ss.events {
        let _ = ev.send(ScreenShareEvent::RemoteStopped { track_key });
    }
}

/// Tear down a leaving participant's remote video. LiveKit doesn't reliably
/// emit `TrackUnsubscribed` on `ParticipantDisconnected`, and a lingering entry
/// renders a black tile if they rejoin. Keys are `{identity}-{sid}`.
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
            if let Some(t) = ss.remote_drain_tasks.remove(k) {
                t.abort();
            }
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
    for (_, t) in ss.remote_drain_tasks.drain() {
        t.abort();
    }
}
