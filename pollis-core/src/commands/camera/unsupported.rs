//! Platforms without camera capture (mobile and others): the commands return
//! a structured "not yet supported" error.

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

/// Settings self-preview (#434).
pub async fn start_camera_preview(_state: &Arc<AppState>, _device_id: String) -> Result<()> {
    Err(unsupported())
}

/// Idempotent no-op, so callers (e.g. leave-voice cleanup) can call it
/// unconditionally.
pub async fn stop_camera(_state: &Arc<AppState>) -> Result<()> {
    Ok(())
}

/// Idempotent no-op — see [`stop_camera`].
pub async fn stop_camera_preview(_state: &Arc<AppState>) -> Result<()> {
    Ok(())
}
