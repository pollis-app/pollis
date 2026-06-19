//! pollis-capture-linux
//!
//! Subprocess helper. Owns the portal handshake + the pipewire stream;
//! talks to the main Pollis binary over a Unix socket whose path the
//! parent passes on the command line.
//!
//! Wire protocol (all little-endian):
//!
//!   message := [ u8 type ][ u32 payload_len ][ payload ]
//!
//!   type 0x01  Format
//!     payload := [ u32 width ][ u32 height ]
//!     Sent once when the pipewire format is negotiated.
//!
//!   type 0x02  Frame
//!     payload := [ u32 width ][ u32 height ][ u32 stride ]
//!                [ i64 timestamp_us ][ BGRx bytes ... ]
//!     Pixel format is BGRx (4 bpp), top-down. The parent does the
//!     I420 conversion + LiveKit publish.
//!
//!   type 0xFF  Error
//!     payload := utf-8 message
//!
//! Audio is intentionally absent. See issue #175 — proper per-window
//! audio routing requires the portal's `accept_audio` option which
//! ashpd doesn't expose, so it needs raw zbus calls to SelectSources
//! to land safely (system-monitor capture loops back through the call).
//!
//! No reverse channel. The parent stops capture by closing the socket;
//! we observe EPIPE on next write or EOF on read and exit cleanly.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("pollis-capture-linux: this helper is Linux-only");
    std::process::exit(2);
}

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
