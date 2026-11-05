//! Voice commands; re-exports keep `pollis_core::commands::voice::*` stable.

mod capture;
mod devices;
mod gate;
mod join;
mod levels;
mod lifecycle;
#[cfg(test)]
mod lifecycle_tests;
mod limiter;
mod mixer;
mod playback;
mod resampler;
mod streams;
mod types;

pub use types::{
    user_id_from_voice_identity, AudioBandsEntry, AudioDevice, JoinTimings, PlaybackState,
    SendableStream, VoiceEvent, VoiceState, VoiceWarmup,
};

pub use capture::CaptureTask;

pub use gate::{TransmitGate, VoiceGateState, VoiceInputMode};

pub use join::{JoinCancel, VoiceJoinError, VOICE_JOIN_DEADLINE, VOICE_RECONNECT_WINDOW};

pub(crate) use streams::{start_mic_stream, start_speaker_stream};

pub(crate) use devices::get_device;
pub use devices::list_audio_devices;

pub use lifecycle::{
    get_last_join_timings, get_voice_gate_state, join_voice_channel, leave_voice_channel,
    prepare_voice_connection, release_voice_ptt, set_remote_user_volume,
    set_voice_audio_processing, set_voice_input_device, set_voice_input_mode,
    set_voice_output_device, set_voice_ptt_held, subscribe_voice_events, toggle_voice_deafen,
    toggle_voice_mute,
};
