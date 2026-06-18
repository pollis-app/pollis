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
//! Camera capture is macOS-only for now. Linux (V4L2 / PipeWire camera
//! portal) and Windows (Media Foundation) land in follow-up commits; on
//! those platforms the commands return a clean "not yet supported" error
//! rather than spawning a helper that can't honour `--mode camera`.
//!
//! Remote camera frames need no new code here: every `RemoteTrack::Video`
//! the voice room loop sees — screen-share or camera — already flows
//! through the shared remote-video path. The renderer distinguishes the
//! two by their LiveKit `TrackSource`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{error::Result, sink::EventSink, state::AppState};

mod state;
#[cfg(target_os = "macos")]
mod capture;
#[cfg(not(target_os = "macos"))]
mod unsupported;

pub use state::CameraState;

#[cfg(target_os = "macos")]
pub use capture::{list_video_devices, start_camera, stop_camera};
#[cfg(not(target_os = "macos"))]
pub use unsupported::{list_video_devices, start_camera, stop_camera};

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
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
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
