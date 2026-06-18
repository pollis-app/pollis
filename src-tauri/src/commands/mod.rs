// Shim modules. Each forwards #[tauri::command]s to pollis_core::commands::*.
// install_kind stays in src-tauri because it inspects Tauri's bundle metadata.
pub mod auth;
pub mod blocks;
pub mod camera;
pub mod device_enrollment;
pub mod dm;
pub mod groups;
pub mod install_kind;
pub mod livekit;
pub mod messages;
pub mod mls;
pub mod pin;
pub mod r2;
pub mod safety;
pub mod screenshare;
pub mod sfx;
pub mod terminal;
pub mod transparency;
pub mod update;
pub mod user;
pub mod voice;
pub mod voice_test;

// Re-export pollis-core helper modules that have no #[tauri::command]
// surface but are referenced by tests and by other shims under their
// short paths (e.g. `crate::commands::voice_apm::ApmConfig`).
pub use pollis_core::commands::{account_identity, voice_apm, voice_denoiser, voice_e2ee};
