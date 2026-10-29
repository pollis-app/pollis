pub mod account_identity;
pub mod auth;
pub mod pin;
pub mod autolock;
pub mod blocks;
pub mod bookmarks;
pub mod device_enrollment;
pub mod device_link;
pub mod diagnostics;
pub mod export;
// Separate module so `export.rs` can be scanned as local.
pub mod export_fetch;
/// The client half of the DS directory + account READ endpoints (#987).
pub(crate) mod ds_reads;
pub mod emoji;
pub mod user;
pub mod groups;
pub mod messages;
pub mod dm;
// LiveKit: real impl with `media` (desktop), no-op stub otherwise (#185).
#[cfg(feature = "media")]
pub mod livekit;
#[cfg(not(feature = "media"))]
#[path = "livekit_stub.rs"]
pub mod livekit;
// Pure wire-payload builders, outside the media gate so they test on every target.
pub mod livekit_signalling;
// Participant-identity grammar (#836); pure, outside the media gate.
pub mod livekit_identity;
pub mod mls;
pub mod overlay;
// NOT `pin` — that module is the device-unlock PIN.
pub mod file_keys;
pub mod pinned_messages;
pub mod push;
pub mod r2;
pub mod vault;
// Serving relays for others (#813); independent of `overlay`.
pub mod relay_serving;
pub mod safety;
pub mod staging;
pub mod transparency;
#[cfg(feature = "media")]
pub mod sfx;
// Windows stub until ConPTY is wired; gated out on mobile.
#[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
#[path = "terminal_unix.rs"]
pub mod terminal;
#[cfg(target_os = "windows")]
#[path = "terminal_windows.rs"]
pub mod terminal;
// Not behind the PTY cfg: the settings UI needs the flag everywhere.
pub mod terminal_gate;
pub mod update;
#[cfg(feature = "media")]
pub mod camera;
#[cfg(feature = "media")]
pub mod screenshare;
#[cfg(feature = "media")]
pub mod voice;
#[cfg(feature = "media")]
pub mod voice_apm;
// The stub keeps the MLS-path call site #[cfg]-free.
#[cfg(feature = "media")]
pub mod voice_e2ee;
#[cfg(not(feature = "media"))]
#[path = "voice_e2ee_stub.rs"]
pub mod voice_e2ee;
// Outside the media gate so the mapping is tested on every target.
pub mod voice_key_ring;
#[cfg(feature = "media")]
pub mod voice_denoiser;
#[cfg(feature = "media")]
pub mod voice_test;
