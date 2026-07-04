// Shim modules. Each forwards #[tauri::command]s to pollis_core::commands::*.
// install_kind stays in src-tauri because it inspects Tauri's bundle metadata.
pub mod auth;
pub mod blocks;
pub mod device_enrollment;
pub mod dm;
pub mod groups;
pub mod install_kind;
// OS-level media permissions (camera/mic/screen). Like tray.rs it is built
// from shell-runtime concerns (TCC, the ConsentStore registry, ms-settings
// deep-links), so it's native-shell-only and never touches pollis-core.
#[cfg(feature = "native-shell")]
pub mod media_permissions;
pub mod messages;
pub mod mls;
pub mod pin;
pub mod r2;
pub mod safety;
pub mod terminal;
pub mod transparency;
pub mod update;
pub mod user;

// Media command shims. Each forwards to a pollis-core module that only exists
// with the `media` feature on (pollis-core's `livekit_stub` lacks the command
// fns these shims call), so gate the shims to match.
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

// Re-export pollis-core helper modules that have no #[tauri::command]
// surface but are referenced by tests and by other shims under their
// short paths (e.g. `crate::commands::voice_apm::ApmConfig`).
// account_identity is not media-gated; the voice_* helpers are.
pub use pollis_core::commands::account_identity;
#[cfg(feature = "media")]
pub use pollis_core::commands::{voice_apm, voice_denoiser, voice_e2ee};
