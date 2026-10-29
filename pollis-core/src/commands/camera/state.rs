//! Per-process webcam state, independent of `ScreenShareState`.

use std::sync::Arc;

use libwebrtc::video_source::native::NativeVideoSource;
use livekit::track::LocalVideoTrack;

use crate::sink::EventSink;

use super::CameraEvent;

pub struct CameraState {
    pub events: Option<Arc<dyn EventSink<CameraEvent>>>,

    pub local_track: Option<LocalVideoTrack>,
    pub local_source: Option<NativeVideoSource>,

    /// Enumeration phase: helper sent `Cameras`, awaiting `start_camera`.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub picker_session: Option<crate::commands::screenshare::HelperSession>,
    /// The capture helper; killing it ends capture.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub local_helper: Option<tokio::process::Child>,
    /// Held open so the helper doesn't see EOF and exit early.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub local_writer: Option<tokio::net::unix::OwnedWriteHalf>,
    /// Task reading helper frames into the LiveKit source.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub local_reader_task: Option<tokio::task::JoinHandle<()>>,

    /// Windows: the dedicated MF capture thread; detached on stop (fenced).
    #[cfg(target_os = "windows")]
    pub windows_thread: Option<std::thread::JoinHandle<()>>,
    /// Windows: fresh per-session fence, so a stale stop can't fence a newer one.
    #[cfg(target_os = "windows")]
    pub windows_active: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,

    // Settings self-preview (#434): own slots, so it coexists with an in-call camera.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub preview_helper: Option<tokio::process::Child>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub preview_writer: Option<tokio::net::unix::OwnedWriteHalf>,
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub preview_reader_task: Option<tokio::task::JoinHandle<()>>,
}

impl CameraState {
    pub fn new() -> Self {
        Self {
            events: None,
            local_track: None,
            local_source: None,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            picker_session: None,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            local_helper: None,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            local_writer: None,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            local_reader_task: None,
            #[cfg(target_os = "windows")]
            windows_thread: None,
            #[cfg(target_os = "windows")]
            windows_active: None,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            preview_helper: None,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            preview_writer: None,
            #[cfg(any(target_os = "macos", target_os = "linux"))]
            preview_reader_task: None,
        }
    }
}

impl Default for CameraState {
    fn default() -> Self {
        Self::new()
    }
}
