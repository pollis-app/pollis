//! pollis-capture-linux: capture subprocess talking to the parent over a Unix
//! socket (wire protocol: `pollis-capture-proto`). See
//! `.codesight/wiki/capture-split.md`.
//!
//! `--audio` captures the default sink's monitor, which contains our own
//! playback; the parent cancels that echo (`screenshare::self_echo`, #884).
//!
//! The parent stops capture by closing the socket; we exit on EPIPE/EOF.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("pollis-capture-linux: this helper is Linux-only");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
mod audio;
#[cfg(target_os = "linux")]
mod camera;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "linux")]
mod x11;

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    linux::run()
}
