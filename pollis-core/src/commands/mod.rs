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
pub mod mls;
pub mod r2;
pub mod safety;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub mod sfx;
#[cfg(not(any(target_os = "ios", target_os = "android")))]
pub mod terminal;
pub mod update;
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
