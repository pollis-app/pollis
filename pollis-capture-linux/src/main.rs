//! pollis-capture-linux
//!
//! Subprocess helper. Owns the portal handshake + the pipewire stream;
//! talks to the main Pollis binary over a Unix socket whose path the
//! parent passes on the command line.
//!
//! Wire protocol: `pollis-capture-proto`.
//!
//! `--audio` (#884) captures the default sink's monitor; the parent cancels our own playback echo.
//!
//! No reverse channel. The parent stops capture by closing the socket;
//! we observe EPIPE on next write or EOF on read and exit cleanly.

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
