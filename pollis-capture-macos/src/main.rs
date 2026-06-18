//! pollis-capture-macos
//!
//! Subprocess helper. Talks to the main Pollis binary over a Unix socket
//! whose path the parent passes on the command line. Mirrors
//! `pollis-capture-linux`. Runs in one of two modes (`--mode`):
//!
//! - **screen** (default, `macos.rs`): owns the ScreenCaptureKit surface
//!   — both `SCShareableContent` enumeration (for our in-app picker) and
//!   the `SCStream` + `SCStreamOutputTrait` capture pipeline.
//! - **camera** (`camera.rs`): owns an AVFoundation `AVCaptureSession`
//!   with an `AVCaptureVideoDataOutput`. ScreenCaptureKit can't capture
//!   cameras, so the webcam path is AVFoundation end to end.
//!
//! Wire protocol: `pollis-capture-proto`. Screen mode uses `0x03 Sources`
//! / `0x04 Select`; camera mode uses `0x05 Cameras` / `0x06 SelectCamera`.
//! Both then stream `0x01 Format` / `0x02 Frame` / `0xFF Error` — the
//! frame path is identical (32BGRA == the BGRx the parent expects).
//!
//! ## Why a subprocess
//!
//! `screencapturekit` 2.x can throw an *Objective-C*
//! `NSUnknownKeyException` from inside SCK's picker delegate, dispatched
//! on replayd's XPC queue (issue #283). Rust `catch_unwind` does NOT
//! catch an ObjC `@throw`; it reaches `std::terminate` and aborts the
//! whole process. Isolating SCK in this helper means that terminate
//! kills only the helper; the parent observes the socket close /
//! non-zero exit and surfaces a structured error.
//!
//! ## Why an in-app picker (no `SCContentSharingPicker`)
//!
//! The crate's `PickerResult.init(filter:)` Swift bridge calls
//! `[filter valueForKey:@"includedDisplays"]` on `SCContentFilter`, a
//! class that doesn't expose that key — so EVERY system-picker
//! selection (display, window, app) throws `NSUnknownKeyException` and
//! aborts the helper. This was confirmed on macOS 14.7. The
//! industry-standard answer used by Slack, Discord, Zoom and OBS —
//! enumerate via `SCShareableContent.current()` and present an in-app
//! picker — also dodges this code path entirely. That's what Pollis
//! does. The picker lives in `frontend/src/components/Voice/
//! ScreenSharePicker.tsx`; this helper just enumerates + builds the
//! resulting `SCContentFilter`.
//!
//! ## Accessory app
//!
//! The helper promotes itself to `NSApplicationActivationPolicy::Accessory`
//! at runtime and parks the main thread in `NSApp.run()`. Display
//! capture works without this (Mach services), but per-window
//! `SCStream` start asserts in CoreGraphics (`CGS_REQUIRE_INIT`)
//! unless the process has a window-server connection. No Dock icon,
//! no menu bar, no Info.plist — purely runtime.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("pollis-capture-macos: this helper is macOS-only");
    std::process::exit(2);
}

#[cfg(target_os = "macos")]
mod camera;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    macos::run()
}
