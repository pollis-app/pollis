//! Camera capture is macOS-only for now. On Linux and Windows the
//! commands return a clean, structured error instead of spawning a helper
//! that can't honour `--mode camera`. Linux (V4L2 / PipeWire camera
//! portal) and Windows (Media Foundation) land in follow-up commits.

use std::sync::Arc;

use crate::{error::Result, state::AppState};

fn unsupported() -> crate::error::Error {
    crate::error::Error::Other(anyhow::anyhow!(
        "webcam capture is not yet supported on this platform"
    ))
}

pub async fn list_video_devices(
    _state: &Arc<AppState>,
) -> Result<pollis_capture_proto::CameraList> {
    Err(unsupported())
}

pub async fn start_camera(_state: &Arc<AppState>, _device_id: String) -> Result<()> {
    Err(unsupported())
}

/// Settings self-preview (issue #434) — same "unsupported here" story as capture.
pub async fn start_camera_preview(_state: &Arc<AppState>, _device_id: String) -> Result<()> {
    Err(unsupported())
}

/// Idempotent no-op: there is never a live camera capture to tear down on
/// an unsupported platform, and callers (e.g. leave-voice cleanup) must be
/// able to call this unconditionally.
pub async fn stop_camera(_state: &Arc<AppState>) -> Result<()> {
    Ok(())
}

/// Idempotent no-op — see [`stop_camera`].
pub async fn stop_camera_preview(_state: &Arc<AppState>) -> Result<()> {
    Ok(())
}
