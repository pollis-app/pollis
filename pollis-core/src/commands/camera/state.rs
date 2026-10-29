//! Per-process webcam-capture state, independent of `ScreenShareState` so a
//! user can screen-share and webcam at once.

use std::sync::Arc;

use libwebrtc::video_source::native::NativeVideoSource;
use livekit::track::LocalVideoTrack;

use crate::sink::EventSink;

use super::CameraEvent;

pub struct CameraState {
    pub events: Option<Arc<dyn EventSink<CameraEvent>>>,

    pub local_track: Option<LocalVideoTrack>,
    pub local_source: Option<NativeVideoSource>,

    /// Helper spawned in `--mode camera` that has sent its `Cameras` list,
    /// waiting for `start_camera` to send the pick.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub picker_session: Option<crate::commands::screenshare::HelperSession>,
    /// Handle to the capture helper subprocess. Killing it terminates
    /// capture; the reader task observes the socket close.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub local_helper: Option<tokio::process::Child>,
    /// Kept open for the capture's lifetime so the helper's read side
    /// doesn't see EOF and exit early. Dropped on stop.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub local_writer: Option<tokio::net::unix::OwnedWriteHalf>,
    /// The supervising task that reads frames off the helper socket and
    /// pushes them into the LiveKit source.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    pub local_reader_task: Option<tokio::task::JoinHandle<()>>,

    /// Windows: the thread running the blocking Media Foundation capture loop.
    /// `stop_camera` flips `windows_active` and the loop tears itself down;
    /// detached, not joined.
    #[cfg(target_os = "windows")]
    pub windows_thread: Option<std::thread::JoinHandle<()>>,
    /// Windows: per-session fence checked before touching the LiveKit source;
    /// a fresh Arc per session so a stale stop can't fence a newer one.
    #[cfg(target_os = "windows")]
    pub windows_active: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,

    // Settings self-preview (#434): no voice room or published track, in its
    // own slots so it coexists with a live in-call camera.
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
