//! pollis-capture-macos: capture subprocess talking to the parent over a Unix
//! socket (wire protocol: `pollis-capture-proto`). `--mode screen` (default,
//! ScreenCaptureKit, `macos.rs`) or `--mode camera` (AVFoundation, `camera.rs`).
//!
//! A subprocess because SCK can throw an Objective-C exception that
//! `catch_unwind` can't catch, and the system picker always does, so Pollis
//! uses an in-app picker (#283, `.codesight/wiki/capture-split.md`).
//!
//! The helper runs as an Accessory app in `NSApp.run()`: per-window `SCStream`
//! start asserts in CoreGraphics (`CGS_REQUIRE_INIT`) without a window-server
//! connection.

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
