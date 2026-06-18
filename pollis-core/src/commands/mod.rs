pub mod account_identity;
pub mod auth;
pub mod pin;
pub mod blocks;
pub mod device_enrollment;
pub mod user;
pub mod groups;
pub mod messages;
pub mod dm;
// LiveKit realtime: real Rust impl on desktop, no-op stub on mobile (mobile
// uses the native LiveKit SDK — issue #185). Same public API either way so
// core call sites stay #[cfg]-free.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub mod livekit;
#[cfg(any(target_os = "ios", target_os = "android"))]
#[path = "livekit_stub.rs"]
pub mod livekit;
// LiveKit access-token minting — pure jsonwebtoken, no native deps, so it
// compiles on every target (unlike the `livekit` module above). Desktop's
// `livekit/jwt.rs` re-exports from here; mobile's `get_livekit_token` bridge
// arm calls it directly.
pub mod livekit_jwt;
pub mod mls;
// Push-notification backend (#344): token registration + the content-free
// send_message fanout. Pure libsql + reqwest, so it compiles on every target.
pub mod push;
pub mod r2;
pub mod safety;
pub mod transparency;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub mod sfx;
// Terminal pane: real PTY backend on Unix desktop, Windows stub until
// ConPTY is wired. Gated out entirely on mobile.
#[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
#[path = "terminal_unix.rs"]
pub mod terminal;
#[cfg(target_os = "windows")]
#[path = "terminal_windows.rs"]
pub mod terminal;
pub mod update;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub mod screenshare;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub mod voice;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub mod voice_apm;
// voice_e2ee: real impl on desktop; mobile stub keeps the one MLS-path call
// site (on_mls_epoch_changed) #[cfg]-free.
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub mod voice_e2ee;
#[cfg(any(target_os = "ios", target_os = "android"))]
#[path = "voice_e2ee_stub.rs"]
pub mod voice_e2ee;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub mod voice_denoiser;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub mod voice_test;
