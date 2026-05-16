//! Voice commands — split into cohesive submodules. Public surface is
//! preserved via the `pub use` re-exports below so every external caller
//! (Tauri shims, sibling `commands::*` modules, integration tests,
//! `voice_test.rs`) keeps resolving names at `pollis_core::commands::voice::*`.

mod devices;
mod lifecycle;
mod playback;
mod streams;
mod types;

// ── Shared types / state ─────────────────────────────────────────────────────
pub use types::{
    user_id_from_voice_identity, AudioDevice, JoinTimings, PlaybackState, SendableStream,
    TrackBuffers, VoiceEvent, VoiceState, VoiceWarmup,
};

// ── cpal stream builders (used by voice_test.rs) ─────────────────────────────
pub(crate) use streams::{start_mic_stream, start_speaker_stream};

// ── Device enumeration / lookup (used by voice_test.rs) ──────────────────────
pub(crate) use devices::get_device;
pub use devices::list_audio_devices;

// ── Tauri command surface ────────────────────────────────────────────────────
pub use lifecycle::{
    get_last_join_timings, join_voice_channel, leave_voice_channel, prepare_voice_connection,
    set_remote_user_volume, set_voice_audio_processing, set_voice_input_device,
    set_voice_output_device, subscribe_voice_events, toggle_voice_mute,
};
