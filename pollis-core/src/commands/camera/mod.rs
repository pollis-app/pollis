//! Webcam capture over LiveKit — the local publish side.
//!
//! A `TrackSource::Camera` track in the voice room; room-level E2EE covers it.
//! macOS/Linux use a capture helper (uncatchable ObjC throws, #283); Windows uses MF.
//!
//! Remote camera frames share the remote-video drain, tagged by `source`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::{error::Result, sink::EventSink, state::AppState};

/// Frame-WS key for the local webcam preview, distinct from `LOCAL_PREVIEW_KEY`.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub const LOCAL_CAMERA_PREVIEW_KEY: &str = "__local_camera_preview__";

/// Self-preview throttle; peers still get full rate.
#[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
pub(crate) const CAMERA_PREVIEW_MIN_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(100);

mod state;
#[cfg(any(target_os = "macos", target_os = "linux"))]
mod capture;
// Windows captures in-process via Media Foundation (see start_windows).
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

/// Local-camera lifecycle events.
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

/// Log the cause, emit `LocalError` and return a human error. Not for user cancels.
#[cfg_attr(
    not(any(target_os = "macos", target_os = "linux", target_os = "windows")),
    allow(dead_code)
)]
pub(super) async fn fail_capture(state: &Arc<AppState>, human: String) -> crate::error::Error {
    crate::diag!("[camera] capture failed: {human}");
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
