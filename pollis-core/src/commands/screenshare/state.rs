//! Per-process screen-share state. Owns the live LiveKit track + source,
//! the per-platform capture handles (helper subprocess on Linux/macOS,
//! dedicated WGC thread on Windows), the picker session (macOS in-app
//! picker phase), and the drain tasks for incoming remote screenshares.

use std::sync::Arc;

use libwebrtc::video_source::native::NativeVideoSource;
use livekit::track::LocalVideoTrack;

use crate::sink::EventSink;

use super::{RawSink, ScreenShareEvent};

/// A connected capture-helper session — the spawned child plus the two
/// halves of its Unix-socket connection. We split the stream because the
/// parent both **writes** (Select message on macOS) and **reads** (Format
/// + Frames). Owning both halves lets us park the writer in state while
/// the reader task drains frames; dropping the writer would signal EOF
/// to the helper's reader and risk an early exit.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub struct HelperSession {
    pub child: tokio::process::Child,
    pub reader: tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
    pub writer: tokio::net::unix::OwnedWriteHalf,
}

pub struct ScreenShareState {
    pub events: Option<Arc<dyn EventSink<ScreenShareEvent>>>,
    pub frames: Option<Arc<dyn RawSink>>,

    pub local_track: Option<LocalVideoTrack>,
    pub local_source: Option<NativeVideoSource>,
    /// macOS picker phase: helper is spawned and has sent its `Sources`
    /// list; we're waiting on the user's pick from the in-app picker.
    /// `start_screen_share` consumes this (sends `Select`, transitions
    /// to capture); `cancel_screen_share_picker` discards it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub picker_session: Option<HelperSession>,
    /// Linux/macOS: handle to the capture helper subprocess.
    /// Linux for libpipewire isolation; macOS for SCK uncatchable-ObjC
    /// isolation (#283 Phase 2). Dropping/killing it terminates capture;
    /// the reader task observes the socket close.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub local_helper: Option<tokio::process::Child>,
    /// Kept open for the lifetime of the capture so the helper's read
    /// side doesn't see EOF and exit early. Dropped on stop.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub local_writer: Option<tokio::net::unix::OwnedWriteHalf>,
    /// Linux/macOS: the supervising task that reads from the helper's
    /// socket and pushes frames into the LiveKit source.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub local_reader_task: Option<tokio::task::JoinHandle<()>>,

    /// Windows: the dedicated thread running the blocking WGC capture
    /// (`GraphicsCaptureApiHandler::start`). We can't use
    /// `start_free_threaded` here: it requires the capture item be `Send`,
    /// but `windows-capture`'s picker hands back a `PickedGraphicsCaptureItem`
    /// that owns an `HwndGuard` (`!Send`). So the picker + capture run on
    /// their own thread and `stop_screen_share` ends them by flipping
    /// `windows_active` (the frame callback then calls
    /// `InternalCaptureControl::stop`). The handle is detached, not
    /// force-joined — the fence guarantees no post-stop source access.
    #[cfg(target_os = "windows")]
    pub windows_thread: Option<std::thread::JoinHandle<()>>,
    /// Windows: per-session fence, same role as `macos_active`. The WGC
    /// frame callback checks it before touching the LiveKit source; a
    /// fresh Arc per session so a stale stop can't fence a newer one.
    #[cfg(target_os = "windows")]
    pub windows_active: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,

    /// Per-remote-track drain task. Key = "{identity}-{sid}".
    pub remote_drain_tasks: std::collections::HashMap<String, tokio::task::JoinHandle<()>>,
}

impl ScreenShareState {
    pub fn new() -> Self {
        Self {
            events: None,
            frames: None,
            local_track: None,
            local_source: None,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            picker_session: None,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            local_helper: None,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            local_writer: None,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            local_reader_task: None,
            #[cfg(target_os = "windows")]
            windows_thread: None,
            #[cfg(target_os = "windows")]
            windows_active: None,
            remote_drain_tasks: std::collections::HashMap::new(),
        }
    }
}
