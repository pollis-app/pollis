//! Screen-share over LiveKit.
//!
//! Linux/macOS capture runs in a helper subprocess over `pollis-capture-proto`.
//! Subprocess because libpipewire crashes in-process (`pw_init`) and SCK
//! can throw an uncatchable ObjC exception (#283) that kills the app.
//! Windows captures in-process via `windows-capture` (WGC).

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::state::AppState;

// `codec` and `helper_subprocess` also serve the sibling `camera` module.
pub(crate) mod audio;
pub(crate) mod codec;
mod commands;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod helper_subprocess;
mod remote_video;
pub(crate) mod self_echo;
mod state;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod start_unix;
#[cfg(target_os = "windows")]
mod start_windows;
#[cfg(target_os = "windows")]
mod windows_audio;
mod stop;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod unsupported;

pub use commands::{
    screenshare_ws_url, subscribe_screen_share_events, subscribe_screen_share_frames,
};
pub use remote_video::{
    on_participant_left, on_remote_video_subscribed, on_remote_video_unsubscribed,
    on_room_disconnected,
};
pub use state::ScreenShareState;
pub use stop::stop_screen_share;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use state::HelperSession;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use start_unix::{cancel_screen_share_picker, enumerate_screen_sources, start_screen_share};
#[cfg(target_os = "windows")]
pub use start_windows::{cancel_screen_share_picker, enumerate_screen_sources, start_screen_share};
#[cfg(target_os = "windows")]
pub(super) use start_windows::WindowsPickerCache;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub use unsupported::{cancel_screen_share_picker, enumerate_screen_sources, start_screen_share};

/// Re-export kept for `commands::screenshare::RawSink` imports.
pub use crate::sink::RawSink;

/// Screen vs camera for a remote video track, from its LiveKit `TrackSource`.
/// Defaults to `Screen` for older backends that send no tag.
#[derive(Clone, Copy, Serialize, Deserialize, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteVideoSource {
    #[default]
    Screen,
    Camera,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScreenShareEvent {
    LocalStarted { width: u32, height: u32 },
    LocalStopped,
    /// Capture helper exited / errored before publishing.
    LocalError { message: String },
    /// The platform can't screen-share at all (e.g. Wayland without a portal backend).
    LocalUnsupported { message: String },
    RemoteStarted {
        track_key: String,
        identity: String,
        width: u32,
        height: u32,
        /// Screen share vs webcam — see [`RemoteVideoSource`].
        #[serde(default)]
        source: RemoteVideoSource,
    },
    RemoteStopped { track_key: String },
    /// The shared-audio track is live; arrives separately from `LocalStarted`.
    LocalAudioStarted,
    /// Audio unavailable; **the share keeps running** as video-only.
    LocalAudioUnavailable { message: String },
}

/// Sentinel track key for the sharer's own preview.
pub const LOCAL_PREVIEW_KEY: &str = "__local_preview__";

/// Self-preview rate cap; it only answers "is my stream going out?".
pub(super) const PREVIEW_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// Log the cause, emit `LocalError` and return a human error. Not for user cancels.
pub(super) async fn fail_capture(state: &Arc<AppState>, human: String) -> crate::error::Error {
    crate::diag!("[screenshare] capture failed: {human}");
    let ev = {
        let ss = state.screenshare.lock().await;
        ss.events.clone()
    };
    if let Some(ev) = ev {
        let _ = ev.send(ScreenShareEvent::LocalError {
            message: human.clone(),
        });
    }
    crate::error::Error::Other(anyhow::anyhow!(human))
}

