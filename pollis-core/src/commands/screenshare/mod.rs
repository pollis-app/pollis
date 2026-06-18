//! Screen-share over LiveKit.
//!
//! Capture path (Linux AND macOS — one shared subprocess model):
//!   start_screen_share -> bind a Unix socket on a unique tmp path ->
//!   spawn the per-platform capture helper subprocess
//!   (`pollis-capture-linux` / `pollis-capture-macos`) passing the
//!   socket path -> helper drives the OS picker/portal/SCK and writes
//!   BGRx frames over the SHARED `pollis-capture-proto` wire protocol
//!   -> we read the negotiated Format, create a LiveKit
//!   NativeVideoSource + LocalVideoTrack and publish it -> a tokio
//!   reader task pulls frames off the socket, runs libyuv argb_to_i420,
//!   and feeds them into the source.
//!
//! Why a subprocess on BOTH:
//!   - Linux: pulling libpipewire into the same process as libwebrtc +
//!     cpal + webkit2gtk + ashpd reliably crashes inside `pw_init`.
//!   - macOS (issue #283 Phase 2): screencapturekit can throw an
//!     Objective-C `NSUnknownKeyException` on Apple's replayd XPC queue
//!     that Rust `catch_unwind` CANNOT catch — it reaches
//!     `std::terminate` and aborts the whole app. Isolating SCK in a
//!     helper means the terminate kills only the helper; the parent
//!     observes the socket close and surfaces a structured error.
//!   Isolating capture in its own process makes the linkage soup / the
//!   uncatchable-throw the kernel's problem on both platforms.
//!
//! Windows still captures in-process via `windows-capture` — WGC is a
//! clean in-proc linkage with no analogous uncatchable-exception hazard.
//!
//! Render path (any OS, triggered when a remote participant publishes a
//! screenshare track in the joined voice room):
//!   voice.rs room loop sees RemoteTrack::Video ->
//!   on_remote_video_subscribed spawns a drain task ->
//!   NativeVideoStream yields frames -> to_i420 -> header + Y/U/V
//!   planes packed as raw bytes -> Tauri Channel as
//!   InvokeResponseBody::Raw -> ArrayBuffer in webview -> WebGL canvas.
//!
//! Two Channels: `events` for low-volume JSON lifecycle, `frames` for
//! raw binary plane data.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::state::AppState;

// ── Submodules ───────────────────────────────────────────────────────────

// `codec` (convert_to_i420) and `helper_subprocess` (locate_capture_helper)
// expose a few crate-visible primitives the sibling `camera` module reuses
// so the two capture features share one frame-conversion + helper-location
// implementation. Nothing behavioural is shared — camera owns its own
// state, events, publish options, and lifecycle.
pub(crate) mod codec;
mod commands;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) mod helper_subprocess;
mod remote_video;
mod state;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod start_unix;
#[cfg(target_os = "windows")]
mod start_windows;
mod stop;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod unsupported;

// ── Public surface ───────────────────────────────────────────────────────

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

/// Re-export of the binary-bytes sink (now defined in `crate::sink` as a
/// neutral home so the terminal path can share it). Kept here so existing
/// `commands::screenshare::RawSink` imports keep working.
pub use crate::sink::RawSink;

// ── Events to the frontend ────────────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScreenShareEvent {
    LocalStarted { width: u32, height: u32 },
    LocalStopped,
    /// Capture helper exited / errored before publishing.
    LocalError { message: String },
    /// The platform genuinely cannot screen-share (distinct from a
    /// permission denial the user can fix). Today this is the Linux
    /// "Wayland session with no xdg-desktop-portal ScreenCast backend"
    /// case (Cinnamon/MATE/XFCE-on-Wayland). The frontend shows an
    /// "unsupported desktop" message, NOT a "grant permission" prompt.
    LocalUnsupported { message: String },
    RemoteStarted {
        track_key: String,
        identity: String,
        width: u32,
        height: u32,
    },
    RemoteStopped { track_key: String },
}

// ── Module-wide constants ─────────────────────────────────────────────────
//
// A 4K or ultrawide source must never reach the software VP8 encoder at
// native resolution — it pegs a core and tanks the call. The previous
// 1080p / 60fps clamp here was a defensive ceiling that's no longer
// applied: publishers send native capture resolution and native frame
// rate. VP8 software encode on a modern CPU handles a 4K/144Hz source
// at ~30-50% of a single core, which is well within thermal headroom on
// every laptop class we ship to. If that changes, the right answer is a
// user-facing setting (issue #300), not a hardcoded ceiling.
//
// Even-floored dims are still required by I420 4:2:0 chroma; that
// floor happens per-frame in `push_frame` / `push_frame_windows` before
// the conversion.

// There is no "stalled" / "paused" concept anywhere in screenshare:
// when capture is idle (static screen on Wayland, etc.) we simply
// stop pushing frames. The viewer's canvas keeps showing the last
// painted frame and the streamer's UI keeps showing "LIVE" — both
// indistinguishable from a stream of unchanging frames. A previous
// implementation had a 2-second watchdog emitting LocalStalled /
// RemoteStalled events with a "Stream paused" overlay, which
// misrepresented normal idle behaviour as a failure. Removed.

/// Track key the local outgoing capture is mirrored under so the sharer can
/// watch a low-rate preview of their own stream. Reserved sentinel — never
/// collides with a remote "{identity}-{sid}" key.
pub const LOCAL_PREVIEW_KEY: &str = "__local_preview__";

/// The self-preview answers "is my stream actually going out?", not
/// fidelity. Cap it well below capture rate so the extra I420 pack + IPC
/// stays off the hot path.
pub(super) const PREVIEW_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

// ── Shared error helper ──────────────────────────────────────────────────

/// Surface a genuine capture/permission/portal/WGC/SCK failure: keep the
/// raw cause on stderr, emit a `LocalError { message }` so the frontend
/// reacts even when the failure happens after `start_screen_share` already
/// returned (or when the caller swallows the Result), and return a
/// structured human-readable error. Used by every start failure branch.
/// Plain user cancellation does NOT go through here — that's a normal flow,
/// not an error the UI must react to.
pub(super) async fn fail_capture(state: &Arc<AppState>, human: String) -> crate::error::Error {
    eprintln!("[screenshare] capture failed: {human}");
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

