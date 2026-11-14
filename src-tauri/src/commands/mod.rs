// Shim modules forwarding to pollis_core::commands::*; install_kind reads Tauri bundle metadata.
pub mod auth;
// Idle auto-lock (#851).
pub mod autolock;
pub mod blocks;
pub mod bookmarks;
pub mod export;
pub mod device_enrollment;
pub mod device_link;
// Biometric unlock (#1685): test harness only, desktop is exempt (see the file).
#[cfg(feature = "test-harness")]
pub mod device_unlock;
pub mod diagnostics;
pub mod dm;
// Custom per-group emoji (#848).
pub mod emoji;
pub mod groups;
// Copy message history between two of one account's devices (#1735).
pub mod history_transfer;
pub mod install_kind;
// OS media permissions: shell-runtime only, never touches pollis-core.
#[cfg(feature = "native-shell")]
pub mod media_permissions;
pub mod messages;
pub mod mls;
pub mod overlay;
pub mod pathscope;
pub mod pin;
pub mod file_keys;
pub mod pinned_messages;
pub mod poster;
pub mod r2;
pub mod relay_serving;
pub mod safety;
pub mod staging;
pub mod terminal;
pub mod transparency;
pub mod update;
pub mod client_policy;
pub mod user;
pub mod vault;

// Gated: pollis-core's `livekit_stub` lacks these command fns.
#[cfg(feature = "media")]
pub mod camera;
#[cfg(feature = "media")]
pub mod livekit;
#[cfg(feature = "media")]
pub mod screenshare;
#[cfg(feature = "media")]
pub mod sfx;
#[cfg(feature = "media")]
pub mod voice;
#[cfg(feature = "media")]
pub mod voice_test;

// Helper modules referenced by tests and shims under short paths; voice_* are media-gated.
pub use pollis_core::commands::account_identity;
#[cfg(feature = "media")]
pub use pollis_core::commands::{voice_apm, voice_denoiser, voice_e2ee};
