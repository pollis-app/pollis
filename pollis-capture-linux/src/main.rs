//! pollis-capture-linux
//!
//! Subprocess helper. Owns the portal handshake + the pipewire stream;
//! talks to the main Pollis binary over a Unix socket whose path the
//! parent passes on the command line.
//!
//! Wire protocol: `pollis-capture-proto`, which is the single definition
//! of the framing and the only place it is documented. In screen mode this
//! helper sends `0x01 Format` / `0x02 Frame`, plus `0x07 AudioFormat` /
//! `0x08 AudioFrame` when launched with `--audio`, and `0xFF Error`.
//!
//! Audio (`--audio`, issue #884) captures the **default sink's monitor**
//! via PipeWire, on its own thread and its own connection so it serves the
//! X11 backend as well as the portal one — see `audio.rs`. It was absent
//! for a long time under issue #175 because a sink monitor contains our
//! own playback, so sharing it echoed the call back to everyone on it.
//! That is now cancelled in the parent against the exact signal it played
//! (`pollis-core`'s `screenshare::self_echo`), which is what made this
//! safe to turn back on.
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
