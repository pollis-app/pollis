//! Remote video track plumbing — called from the voice room loop when a
//! remote participant publishes/unpublishes a screenshare track, and on
//! room disconnect for blanket cleanup.

use std::sync::Arc;

use libwebrtc::{video_frame::VideoBuffer, video_stream::native::NativeVideoStream};
use livekit::track::RemoteVideoTrack;

use crate::state::AppState;

use super::{codec::pack_frame_bytes, stop::stop_screen_share, ScreenShareEvent};

pub async fn on_remote_video_subscribed(
    track: RemoteVideoTrack,
    participant_identity: String,
    state: &Arc<AppState>,
) {
    let track_key = format!("{}-{}", participant_identity, track.sid());
    eprintln!("[screenshare] remote video subscribed: {track_key}");

    let (events, frames) = {
        let ss = state.screenshare.lock().await;
        (ss.events.clone(), ss.frames.clone())
    };

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
                    });
                }
            }
            if let Some(sink) = &frames {
                let bytes = pack_frame_bytes(
                    &track_key_for_task,
                    w,
                    h,
                    frame.timestamp_us,
                    &i420,
                );
                let _ = sink.send(bytes);
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

pub async fn on_room_disconnected(state: &Arc<AppState>) {
    let _ = stop_screen_share(state).await;
    let mut ss = state.screenshare.lock().await;
    for (_, t) in ss.remote_drain_tasks.drain() {
        t.abort();
    }
}
