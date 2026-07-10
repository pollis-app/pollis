//! Per-process webcam-capture state. Owns the live LiveKit camera track +
//! source and (on the helper-subprocess platforms, macOS + Linux) the
//! capture-helper handles. Independent of `ScreenShareState`: a user can
//! screen-share and webcam simultaneously, so the two never share a track,
//! source, or helper slot.

use std::sync::Arc;

use libwebrtc::video_source::native::NativeVideoSource;
use livekit::track::LocalVideoTrack;

use crate::sink::EventSink;

use super::CameraEvent;

pub struct CameraState {
    pub events: Option<Arc<dyn EventSink<CameraEvent>>>,

    pub local_track: Option<LocalVideoTrack>,
    pub local_source: Option<NativeVideoSource>,

    /// Enumeration phase: helper spawned in `--mode camera` and has sent
    /// its `Cameras` list; we're waiting for `start_camera` to send the
    /// pick. `start_camera` consumes this; `stop_camera` discards it.
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

    /// Windows: the dedicated thread running the blocking Media Foundation
    /// `IMFSourceReader::ReadSample` capture loop (no helper subprocess — MF
    /// is clean in-process COM, mirroring WGC screen capture). `stop_camera`
    /// flips `windows_active`; the loop observes it, tears down its own MF +
    /// COM state, and exits. The handle is detached, not force-joined — the
    /// fence guarantees no post-stop source access.
    #[cfg(target_os = "windows")]
    pub windows_thread: Option<std::thread::JoinHandle<()>>,
    /// Windows: per-session fence, same role as the reader task on the helper
    /// platforms. The MF loop checks it before touching the LiveKit source; a
    /// fresh Arc per session so a stale stop can't fence a newer one.
    #[cfg(target_os = "windows")]
    pub windows_active: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
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
        }
    }
}

impl Default for CameraState {
    fn default() -> Self {
        Self::new()
    }
}
