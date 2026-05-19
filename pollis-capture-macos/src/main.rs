//! pollis-capture-macos
//!
//! Subprocess helper. Owns ScreenCaptureKit (SCContentSharingPicker +
//! SCStream + the SCStreamOutputTrait frame handler) and talks to the
//! main Pollis binary over a Unix socket whose path the parent passes on
//! the command line. Mirrors `pollis-capture-linux`.
//!
//! Wire protocol: see `pollis-capture-proto` — the SAME 0x01 Format /
//! 0x02 Frame / 0xFF Error framing the Linux helper uses. The parent's
//! existing frame reader, FPS handling, libyuv ARGB->I420 conversion,
//! LiveKit injection and 2 s stall heartbeat are all unchanged; only
//! "where frames originate" forks here, exactly as on Linux.
//!
//! Why this is a subprocess at all (issue #283 Phase 2): an ObjC
//! `@throw` raised on Apple's replayd XPC callback queue (observed:
//! `NSUnknownKeyException` from `SCContentSharingPicker`'s selection
//! delegate doing `valueForKey:` on a window whose owning app lacks the
//! key) is *uncatchable* from Rust and hard-kills the hosting process.
//! Putting SCK in this helper means that terminate kills only the
//! helper; the parent observes the socket close / non-zero exit and
//! surfaces a structured capture error. This retroactively de-risks
//! every SCK call.
//!
//! OPEN RISK (flagged, not silently assumed): `SCContentSharingPicker`
//! must be driven from a process with a window-server connection.
//! Whether the system picker presents correctly from THIS helper
//! process (rather than the main app) is UNVERIFIED — it was slated to
//! be a Phase 0 spike that is out of scope here. If the picker does not
//! appear from the helper, this binary must instead receive an
//! already-selected `SCContentFilter` from the parent (the parent would
//! drive the picker and hand the helper a serialized selection), OR the
//! split reverts to in-process with the [patch.crates-io] fork from
//! #283 Phase 1. See `.codesight/wiki/capture-split.md`.

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("pollis-capture-macos: this helper is macOS-only");
    std::process::exit(2);
}

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    macos::run()
}
