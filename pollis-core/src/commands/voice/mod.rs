//! Voice commands — split into cohesive submodules. Public surface is
//! preserved via the `pub use` re-exports below so every external caller
//! (Tauri shims, sibling `commands::*` modules, integration tests,
//! `voice_test.rs`) keeps resolving names at `pollis_core::commands::voice::*`.

mod devices;
mod gate;
mod levels;
mod lifecycle;
mod playback;
mod streams;
mod types;

// ── Shared types / state ─────────────────────────────────────────────────────
pub use types::{
    user_id_from_voice_identity, AudioDevice, JoinTimings, PlaybackState, SendableStream,
    TrackBuffers, VoiceEvent, VoiceState, VoiceWarmup,
};

// ── Push-to-talk / mute / deafen state machine (#849) ────────────────────────
pub use gate::{TransmitGate, VoiceGateState, VoiceInputMode};

// ── cpal stream builders (used by voice_test.rs) ─────────────────────────────
pub(crate) use streams::{start_mic_stream, start_speaker_stream};

// ── Device enumeration / lookup (used by voice_test.rs) ──────────────────────
pub(crate) use devices::get_device;
pub use devices::list_audio_devices;

// ── Tauri command surface ────────────────────────────────────────────────────
pub use lifecycle::{
    get_last_join_timings, get_voice_gate_state, join_voice_channel, leave_voice_channel,
    prepare_voice_connection, release_voice_ptt, set_remote_user_volume,
    set_voice_audio_processing, set_voice_input_device, set_voice_input_mode,
    set_voice_output_device, set_voice_ptt_held, subscribe_voice_events, toggle_voice_deafen,
    toggle_voice_mute,
};
