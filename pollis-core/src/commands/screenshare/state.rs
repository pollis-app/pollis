//! Per-process screen-share state.

use std::sync::Arc;

use libwebrtc::{audio_source::native::NativeAudioSource, video_source::native::NativeVideoSource};
use livekit::track::{LocalAudioTrack, LocalVideoTrack};

use crate::sink::EventSink;

use super::{RawSink, ScreenShareEvent};

/// A connected capture-helper session: child plus split socket halves.
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
    /// Shared-audio track; `None` until an audio format arrives (own lifetime).
    pub local_audio_track: Option<LocalAudioTrack>,
    pub local_audio_source: Option<NativeAudioSource>,
    /// macOS picker phase: helper sent `Sources`, awaiting the user's pick.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub picker_session: Option<HelperSession>,
    /// Linux/macOS: the capture helper; killing it ends capture.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub local_helper: Option<tokio::process::Child>,
    /// Held open so the helper doesn't see EOF and exit early.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub local_writer: Option<tokio::net::unix::OwnedWriteHalf>,
    /// Linux/macOS: the task reading helper frames into the LiveKit source.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub local_reader_task: Option<tokio::task::JoinHandle<()>>,

    /// Windows: the dedicated WGC thread (picker item is `!Send`); detached on stop.
    #[cfg(target_os = "windows")]
    pub windows_thread: Option<std::thread::JoinHandle<()>>,
    /// Windows: fresh per-session fence, so a stale stop can't fence a newer one.
    #[cfg(target_os = "windows")]
    pub windows_active: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Windows in-app picker cache from `enumerate_screen_sources`.
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
