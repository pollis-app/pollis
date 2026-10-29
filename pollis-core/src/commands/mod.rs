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
pub mod export_fetch;
pub(crate) mod ds_reads;
pub mod emoji;
pub mod user;
pub mod groups;
pub mod messages;
pub mod dm;
// Real LiveKit with `media` (desktop); a stub with the same API otherwise, since
// mobile uses the native LiveKit SDK (#185).
#[cfg(feature = "media")]
pub mod livekit;
#[cfg(not(feature = "media"))]
#[path = "livekit_stub.rs"]
pub mod livekit;
// Pure signalling payload builders, outside the media gate so they are shared
// by `livekit` and the stub and unit-tested on every target.
pub mod livekit_signalling;
// Participant-identity grammar (#836); outside the media gate for the same
// reason as `livekit_signalling`.
pub mod livekit_identity;
pub mod mls;
pub mod overlay;
pub mod file_keys;
pub mod pinned_messages;
pub mod push;
pub mod r2;
pub mod vault;
pub mod relay_serving;
pub mod safety;
pub mod staging;
pub mod transparency;
#[cfg(feature = "media")]
pub mod sfx;
// Real PTY backend on Unix desktop, a stub on Windows, absent on mobile.
#[cfg(all(unix, not(any(target_os = "ios", target_os = "android"))))]
#[path = "terminal_unix.rs"]
pub mod terminal;
#[cfg(target_os = "windows")]
#[path = "terminal_windows.rs"]
pub mod terminal;
// Not behind the PTY cfg: the settings command for this flag must exist
// wherever the settings UI does.
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
#[cfg(feature = "media")]
pub mod voice_e2ee;
#[cfg(not(feature = "media"))]
#[path = "voice_e2ee_stub.rs"]
pub mod voice_e2ee;
// Pure arithmetic, outside the media gate so it is unit-tested on every target.
pub mod voice_key_ring;
#[cfg(feature = "media")]
pub mod voice_denoiser;
#[cfg(feature = "media")]
pub mod voice_test;
