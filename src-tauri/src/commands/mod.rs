pub mod account_identity;
pub mod auth;
pub mod blocks;
pub mod device_enrollment;
pub mod user;
pub mod groups;
pub mod messages;
pub mod dm;
pub mod livekit;
pub mod mls;
pub mod r2;
pub mod sfx;
// The `update` module exposes the `mark_update_required` / `is_update_required`
// commands that coordinate the self-updater. MAS builds compile the updater
// out entirely, so the module is not included under `feature = "mas"`.
#[cfg(not(feature = "mas"))]
pub mod update;
pub mod voice;
pub mod voice_test;
