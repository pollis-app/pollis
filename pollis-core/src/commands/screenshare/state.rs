//! Per-process screen-share state: the live track and source, the platform
//! capture handles, the picker session, and remote drain tasks.

use std::sync::Arc;

use libwebrtc::{audio_source::native::NativeAudioSource, video_source::native::NativeVideoSource};
use livekit::track::{LocalAudioTrack, LocalVideoTrack};

use crate::sink::EventSink;

use super::{RawSink, ScreenShareEvent};

/// A connected capture-helper session: the child and both halves of its
/// socket. The writer is parked in state while the reader drains frames;
/// dropping it would signal EOF to the helper.
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
    /// The shared-audio track, published only once the backend announces an
    /// audio format. Independent lifetime from video: a share can run
    /// video-only.
    pub local_audio_track: Option<LocalAudioTrack>,
    pub local_audio_source: Option<NativeAudioSource>,
    /// Picker phase: the helper has sent its `Sources` list and waits for the
    /// user's pick. `start_screen_share` consumes it;
    /// `cancel_screen_share_picker` discards it.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub picker_session: Option<HelperSession>,
    /// Linux/macOS: the capture helper. Killing it stops capture; the reader
    /// task observes the socket close.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub local_helper: Option<tokio::process::Child>,
    /// Kept open for the capture's lifetime so the helper's read side never
    /// sees EOF.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub local_writer: Option<tokio::net::unix::OwnedWriteHalf>,
    /// Linux/macOS: the task that reads helper frames into the LiveKit source.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub local_reader_task: Option<tokio::task::JoinHandle<()>>,

    /// Windows: the thread running the blocking WGC capture. Not
    /// `start_free_threaded`: the picked capture item owns an `HwndGuard`
    /// (`!Send`). `stop_screen_share` flips `windows_active` and the frame
    /// callback stops capture; detached, not joined.
    #[cfg(target_os = "windows")]
    pub windows_thread: Option<std::thread::JoinHandle<()>>,
    /// Windows: per-session fence checked before touching the LiveKit source;
    /// a fresh Arc per session so a stale stop can't fence a newer one.
    #[cfg(target_os = "windows")]
    pub windows_active: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Windows picker phase: the cached enumeration the in-app picker renders.
    /// `start_screen_share(Some(Selection))` looks the pick up by index.
    /// Dropped on the next enumerate, a cancel, or a successful start.
    #[cfg(target_os = "windows")]
    pub windows_picker: Option<super::WindowsPickerCache>,

    /// Per-remote-track drain task. Key = "{identity}-{sid}".
    pub remote_drain_tasks: std::collections::HashMap<String, tokio::task::JoinHandle<()>>,
}

impl Default for ScreenShareState {
    fn default() -> Self {
        Self::new()
    }
}

impl ScreenShareState {
    pub fn new() -> Self {
        Self {
            events: None,
            frames: None,
            local_track: None,
            local_source: None,
            local_audio_track: None,
            local_audio_source: None,
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
            #[cfg(target_os = "windows")]
            windows_picker: None,
            remote_drain_tasks: std::collections::HashMap::new(),
        }
    }
}
