pub mod account_identity;
pub mod auth;
pub mod pin;
pub mod blocks;
pub mod device_enrollment;
pub mod user;
pub mod groups;
pub mod messages;
pub mod dm;
// LiveKit realtime: real Rust impl when the `media` feature is on (desktop),
// no-op stub otherwise — mobile builds with `media` off and uses the native
// LiveKit SDK (issue #185). Same public API either way so core call sites stay
// #[cfg]-free.
#[cfg(feature = "media")]
pub mod livekit;
#[cfg(not(feature = "media"))]
#[path = "livekit_stub.rs"]
pub mod livekit;
// LiveKit tokens are minted server-side by the DS now (#393); no on-device
// signer remains. Token/SendData/roster requests go through the DS client
// helpers in `commands::mls` (`ds_livekit_*`).
// LiveKit realtime signalling (wake-up) payload builders — pure serde_json,
// no native deps, always compiled (same rationale as `livekit_jwt`). Both the
// media `livekit::publish` and the headless `livekit_stub` build their wire
// payloads here so the metadata-minimized shape (§5) has one source of truth
// and unit-tests on every target.
pub mod livekit_signalling;
pub mod mls;
// Push-notification backend (#344): token registration + the content-free
// send_message fanout. Pure libsql + reqwest, so it compiles on every target.
pub mod push;
pub mod r2;
pub mod safety;
pub mod transparency;
pub mod turso_token;
#[cfg(feature = "media")]
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
#[cfg(feature = "media")]
pub mod camera;
#[cfg(feature = "media")]
pub mod screenshare;
#[cfg(feature = "media")]
pub mod voice;
#[cfg(feature = "media")]
pub mod voice_apm;
// voice_e2ee: real impl with the `media` feature on; stub otherwise. The stub
// keeps the one MLS-path call site (on_mls_epoch_changed) #[cfg]-free.
#[cfg(feature = "media")]
pub mod voice_e2ee;
#[cfg(not(feature = "media"))]
#[path = "voice_e2ee_stub.rs"]
pub mod voice_e2ee;
#[cfg(feature = "media")]
pub mod voice_denoiser;
#[cfg(feature = "media")]
pub mod voice_test;
