//! Webcam capture over LiveKit — the local publish side.
//!
//! A user's webcam is published as a *third* track into the **same voice
//! room** their mic and screen-share already use, tagged
//! `TrackSource::Camera` so peers can tell it apart from the screen-share
//! video track. Room-level E2EE (configured once at room connect in
//! `voice/lifecycle.rs`) encrypts it automatically — there is no
//! camera-specific crypto.
//!
//! Capture path (macOS today): mirror of the screen-share helper model.
//!   `list_video_devices` spawns `pollis-capture-macos --mode camera`,
//!   reads its `Cameras` enumeration, and parks the helper waiting for a
//!   pick. `start_camera(device_id)` sends `SelectCamera`, reads the
//!   negotiated `Format`, creates a LiveKit `NativeVideoSource` +
//!   `LocalVideoTrack`, publishes it, and spawns a reader task that runs
//!   `argb_to_i420` on each BGRA frame and feeds the source.
//!
//! Why a subprocess (same rationale as screen capture, #283): an
//! AVFoundation / CoreMediaIO Objective-C `@throw` — e.g. from a
//! misbehaving virtual-camera plugin — is uncatchable by Rust
//! `catch_unwind` and would abort the whole app. Isolating it in the
//! helper means such a throw kills only the helper; the parent observes
//! the socket close and surfaces a structured error.
//!
//! Capture is live on macOS (AVFoundation helper), Linux (V4L2 helper), and
//! Windows (`start_windows.rs` — Media Foundation, in-process, no helper: the
//! same divergence WGC screen capture takes). Mobile / other platforms fall
//! through to `unsupported.rs`, whose commands return a clean "not yet
//! supported" error.
//!
//! Remote camera frames reuse the shared remote-video path: every
//! `RemoteTrack::Video` the voice room loop sees — screen-share or camera —
//! flows through the same `on_remote_video_subscribed` drain and the same
//! frame WebSocket. The Tauri renderer has no JS LiveKit client, so it
//! can't read `TrackSource` itself; instead the voice loop reads the
//! publication's `TrackSource` and tags the `RemoteStarted` event with a
//! `source` (`screen` | `camera`), and the renderer routes the track_key's
//! frames to a camera tile or a screenshare tile accordingly.
//!
//! Local self-preview: the capture reader task mirrors each outgoing webcam
//! frame to the renderer over that same frame WebSocket under
//! [`LOCAL_CAMERA_PREVIEW_KEY`] (throttled), exactly as screen share mirrors
//! its own under `LOCAL_PREVIEW_KEY`. Distinct keys so sharing screen and
//! webcam at once doesn't cross the two previews.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{error::Result, sink::EventSink, state::AppState};

/// Reserved frame-WS track key the local outgoing webcam capture is
/// mirrored under for the sharer's own preview (mirrors screen share's
/// `LOCAL_PREVIEW_KEY`). Kept distinct so a simultaneous screen share +
/// webcam don't collide in the renderer's per-key frame router.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub const LOCAL_CAMERA_PREVIEW_KEY: &str = "__local_camera_preview__";

/// Minimum gap between mirrored self-preview frames. The webcam publishes
/// to peers at full rate; the local preview only needs to look live, so
/// throttle it to spare the renderer (matches screen share's cadence).
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub(crate) const CAMERA_PREVIEW_MIN_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(100);

mod state;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod capture;
// Windows captures in-process via Media Foundation (no helper subprocess) —
// the same divergence as screen share's WGC path. See start_windows.
#[cfg(target_os = "windows")]
mod start_windows;
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
mod unsupported;

pub use state::CameraState;

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub use capture::{
    list_video_devices, start_camera, start_camera_preview, stop_camera, stop_camera_preview,
};
#[cfg(target_os = "windows")]
pub use start_windows::{
    list_video_devices, start_camera, start_camera_preview, stop_camera, stop_camera_preview,
};
#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
pub use unsupported::{
    list_video_devices, start_camera, start_camera_preview, stop_camera, stop_camera_preview,
};

// ── Events to the frontend ────────────────────────────────────────────────

/// Local-camera lifecycle events. Remote camera tiles are driven by the
/// renderer's LiveKit view client (it reads `TrackSource::Camera`
/// directly), so — unlike `ScreenShareEvent` — there are no `Remote*`
/// variants here.
#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CameraEvent {
    LocalStarted { width: u32, height: u32 },
    LocalStopped,
    /// Capture helper exited / errored before or during publish.
    LocalError { message: String },
}

pub async fn subscribe_camera_events(
    sink: Arc<dyn EventSink<CameraEvent>>,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut cam = state.camera.lock().await;
    cam.events = Some(sink);
    Ok(())
}

// ── Shared error helper ────────────────────────────────────────────────────

/// Surface a genuine capture/permission failure: log the cause, emit a
/// `LocalError { message }` so the frontend reacts even when the failure
/// happens after `start_camera` already returned, and return a structured
/// human-readable error. Plain user cancellation does NOT go through here.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux", target_os = "windows")),
    allow(dead_code)
)]
pub(super) async fn fail_capture(state: &Arc<AppState>, human: String) -> crate::error::Error {
    eprintln!("[camera] capture failed: {human}");
    let ev = {
        let cam = state.camera.lock().await;
        cam.events.clone()
    };
    if let Some(ev) = ev {
        let _ = ev.send(CameraEvent::LocalError {
            message: human.clone(),
        });
    }
    crate::error::Error::Other(anyhow::anyhow!(human))
}
