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
//! Wire protocol: `pollis-capture-proto`.
//!
//! ## Why a subprocess
//!
//! SCK can throw an ObjC exception `catch_unwind` cannot catch, aborting the process (#283).
//!
//! ## Why an in-app picker (no `SCContentSharingPicker`)
//!
//! The crate's system-picker bridge throws `NSUnknownKeyException` on every selection.
//!
//! ## Accessory app
//!
//! Per-window `SCStream` needs a window-server connection, so the helper runs as an Accessory app.

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
