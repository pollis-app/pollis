//! Windows webcam capture (Media Foundation) — IN-PROCESS.
//!
//! ⚠️ SCAFFOLD / STUB. The three command entry points compile and are wired
//! into the gate (`mod.rs`), but the actual capture is `TODO`. This exists so
//! whoever takes Windows starts from a building module with a precise plan
//! instead of a blank file. `list_video_devices` / `start_camera` return a
//! clear "not implemented" error today; `stop_camera` is already a correct
//! idempotent no-op (the leave-voice teardown calls it unconditionally).
//!
//! ── Why in-process (NOT a helper subprocess like macOS/Linux) ─────────────
//!
//! The macOS (`pollis-capture-macos`) and Linux (`pollis-capture-linux`)
//! camera helpers exist because their platforms FORCE isolation:
//!   - macOS: an AVFoundation/CoreMediaIO ObjC `@throw` is uncatchable by
//!     Rust `catch_unwind` and aborts the whole app (#283).
//!   - Linux: libpipewire can't co-link with libwebrtc + cpal + webkit2gtk.
//! Windows has neither hazard: Media Foundation is clean in-process COM that
//! links fine alongside libwebrtc and the webview — exactly like WGC screen
//! capture (`screenshare/start_windows.rs`), which is also in-process. So
//! Windows mirrors the WGC pattern: capture on an owned thread, push frames
//! straight into the LiveKit `NativeVideoSource`, no socket, no proto, no
//! sidecar. See `.codesight/wiki/capture-split.md`.
//!
//! ── Implementation plan (mirror `screenshare/start_windows.rs`) ───────────
//!
//! Reuse from the shared camera path (`camera/capture.rs`), unchanged:
//!   - `resolve_camera_encoding`, the LiveKit publish (`TrackSource::Camera`,
//!     VP8, `is_screencast=false`), `convert_to_i420`, `push_frame`, and the
//!     local self-preview mirroring under `LOCAL_CAMERA_PREVIEW_KEY`.
//!   - The stop fence + owned-thread model from screenshare's `start_windows`
//!     (`windows_active: Arc<AtomicBool>` checked in the read loop; thread
//!     detached on stop). Add the matching fields to `camera/state.rs` behind
//!     `#[cfg(target_os = "windows")]` (see TODOs there).
//!
//! New Windows code (the only genuinely platform-specific part):
//!
//!   list_video_devices:
//!     - `MFStartup(MF_VERSION, MFSTARTUP_FULL)` (once; pair MFShutdown).
//!     - `MFCreateAttributes`, set `MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE` =
//!       `MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID`.
//!     - `MFEnumDeviceSources(attrs) -> [IMFActivate]`. For each:
//!         name = GetAllocatedString(MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME)
//!         id   = GetAllocatedString(
//!                  MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK)
//!       The symbolic link IS the opaque `CameraSource::id` the proto already
//!       documents — echo it back verbatim to `start_camera`.
//!
//!   start_camera(device_id):
//!     - Re-resolve the IMFActivate by symbolic link, `ActivateObject::<
//!       IMFMediaSource>()`, `MFCreateSourceReaderFromMediaSource(src)`.
//!     - FORMAT FORK (resolve on real hardware early):
//!         A) Easy path — set the reader's output media type to
//!            `MFVideoFormat_RGB32`; MF inserts the Video Processor MFT and
//!            you receive BGRA directly → reuse `push_frame` with ZERO new
//!            conversion.
//!         B) Fallback — if a device/driver refuses RGB32, negotiate its
//!            native subtype and convert: YUY2 + MJPG conversions already
//!            exist (lifted from `pollis-capture-linux/src/camera.rs`); only
//!            NV12 → BGRx is net-new (~40 lines).
//!     - Read `MF_MT_FRAME_SIZE` for dimensions, publish the track, then on
//!       an owned thread loop `ReadSample` → `ConvertToContiguousBuffer` →
//!       `Lock` → convert/push → `Unlock`, gated on the stop fence.
//!
//!   stop_camera: flip the fence (Release), unpublish, drop the thread —
//!     same shape as `screenshare::stop` for Windows.
//!
//! Cargo: `pollis-core/Cargo.toml` enables the `windows` crate's
//! `Win32_Media_MediaFoundation` + `Win32_System_Com` features for the calls
//! above (added alongside this stub).
//!
//! Permission: Windows camera privacy (Settings → Privacy → Camera) makes
//! `ReadSample` fail when denied — map it onto the same friendly error the
//! frontend already shows (`friendlyCameraError` matches "permission"/
//! "denied"); `fail_capture` in `mod.rs` is the helper for that.

use std::sync::Arc;

use crate::{error::Result, state::AppState};

/// TODO(windows): Media Foundation device enumeration — see the module plan.
pub async fn list_video_devices(
    _state: &Arc<AppState>,
) -> Result<pollis_capture_proto::CameraList> {
    Err(crate::error::Error::Other(anyhow::anyhow!(
        "Windows webcam capture is not implemented yet (Media Foundation TODO)"
    )))
}

/// TODO(windows): open the IMFSourceReader, negotiate RGB32, publish the
/// `TrackSource::Camera` track, and pump frames on an owned thread.
pub async fn start_camera(_state: &Arc<AppState>, _device_id: String) -> Result<()> {
    Err(crate::error::Error::Other(anyhow::anyhow!(
        "Windows webcam capture is not implemented yet (Media Foundation TODO)"
    )))
}

/// Idempotent no-op until capture lands: there is never a live MF session to
/// tear down yet, and callers (leave-voice cleanup) must be able to call this
/// unconditionally. Once capture exists, this flips the stop fence + unpublishes.
pub async fn stop_camera(_state: &Arc<AppState>) -> Result<()> {
    Ok(())
}
