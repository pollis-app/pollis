//! Screen-share over LiveKit.
//!
//! Linux and macOS capture in a helper subprocess speaking
//! `pollis-capture-proto` over a Unix socket: on Linux libpipewire crashes
//! when linked beside libwebrtc + cpal + webkit2gtk; on macOS
//! ScreenCaptureKit can throw an Objective-C exception `catch_unwind` cannot
//! catch, aborting the app (#283). Windows captures in-process via WGC, which
//! has neither hazard. Remote screenshares are drained to raw I420 frames for
//! the renderer. See `.codesight/wiki/capture-split.md`.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::state::AppState;

// `codec` and `helper_subprocess` expose primitives the `camera` module
// reuses; nothing behavioural is shared.
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

/// Re-export of `crate::sink::RawSink`, kept for existing imports.
pub use crate::sink::RawSink;

/// Which kind of video a remote `RemoteStarted` track carries, read from the
/// publication's `TrackSource`, so the renderer routes frames to a screenshare
/// or a camera tile. Defaults to `Screen` for events from an older backend.
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
    /// The platform genuinely cannot screen-share, as opposed to a fixable
    /// permission denial: today a Linux Wayland session with no
    /// xdg-desktop-portal ScreenCast backend. The UI shows "unsupported
    /// desktop", not a permission prompt.
    LocalUnsupported { message: String },
    RemoteStarted {
        track_key: String,
        identity: String,
        width: u32,
        height: u32,
        /// Screen share vs webcam; defaulted for compatibility.
        #[serde(default)]
        source: RemoteVideoSource,
    },
    RemoteStopped { track_key: String },
    /// The shared-audio track is live. Separate from `LocalStarted` because
    /// audio can take longer to negotiate, and video should not wait for it.
    LocalAudioStarted,
    /// Audio was requested and cannot be delivered (no capturable output, no
    /// PipeWire, an old Windows build). The share keeps running video-only.
    LocalAudioUnavailable { message: String },
}

// Publishers send native resolution and frame rate (no 1080p/60fps clamp); a
// ceiling, if ever needed, belongs in a user setting (#300). Dims are
// even-floored per frame for I420. Idle capture simply stops pushing frames;
// there is deliberately no "stalled" state.

/// Track key the local capture is mirrored under for the sharer's own preview.
/// Never collides with a remote "{identity}-{sid}" key.
pub const LOCAL_PREVIEW_KEY: &str = "__local_preview__";

/// The self-preview only answers "is my stream going out?", so it runs well
/// below capture rate.
pub(super) const PREVIEW_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// Surface a genuine capture failure: log it, emit `LocalError` (the failure
/// may come after `start_screen_share` returned), and return a readable error.
/// Not for plain user cancellation.
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

