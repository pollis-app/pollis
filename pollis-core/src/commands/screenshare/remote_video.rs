//! Remote video track plumbing — called from the voice room loop when a
//! remote participant publishes/unpublishes a video track (screen share or
//! webcam), and on room disconnect for blanket cleanup. Both video kinds
//! share this one drain + frame WebSocket; the `source` tag on the
//! `RemoteStarted` event is what lets the renderer tell them apart.

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
    eprintln!("[screenshare] remote video subscribed: {track_key} (source={source:?})");

    let (events, frames) = {
        let ss = state.screenshare.lock().await;
        (ss.events.clone(), ss.frames.clone())
    };
    // WebSocket fan-out for the Tauri/WebKitGTK render path (spike/tauri-revival).
    // Cheap to clone (a broadcast Sender). The frontend reads these over the
    // loopback media server's `/screenshare/<token>` route; see `media_server.rs`.
    let frame_tx = state.screenshare_frame_tx.clone();

    let mut stream = NativeVideoStream::new(track.rtc_track());
    let track_key_for_task = track_key.clone();
    let identity_clone = participant_identity.clone();
    let events_for_task = events.clone();
    let task = tokio::spawn(async move {
        use futures_util::StreamExt;
        let mut announced: Option<(u32, u32)> = None;
        // No stall watchdog — when the remote streamer's capture is
        // idle, frames simply stop arriving and our local canvas keeps
        // showing the last paint. The track stays subscribed; the next
        // real frame is rendered when it arrives. Stream ending (track
        // unpublished) exits this loop and RemoteStopped is emitted by
        // the unsubscribe path.
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
            // Pack once. Broadcast to any WebSocket subscribers (Tauri render
            // path) and, when a legacy Channel sink is still registered, mirror
            // to it too. `frame_tx` has no active receivers when nobody's
            // watching, in which case `send` is a cheap no-op (returns Err).
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

/// Tear down any screenshare a participant was publishing when they leave the
/// room. LiveKit doesn't reliably emit per-track `TrackUnsubscribed` on a
/// `ParticipantDisconnected`, so without this the remote stream's drain task
/// and the frontend's `screenShareRemotes` entry linger — and if the participant
/// later rejoins (without resharing) their tile renders a dead, black canvas
/// instead of falling back to the avatar. Track keys are `{identity}-{sid}`, so
/// every key prefixed with this participant's identity is theirs.
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
