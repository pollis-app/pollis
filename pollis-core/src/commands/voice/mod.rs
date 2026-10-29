//! Voice commands, re-exported at `pollis_core::commands::voice::*`.

mod devices;
mod gate;
mod levels;
mod lifecycle;
mod playback;
mod streams;
mod types;

pub use types::{
    user_id_from_voice_identity, AudioDevice, JoinTimings, PlaybackState, SendableStream,
    TrackBuffers, VoiceEvent, VoiceState, VoiceWarmup,
};

pub use gate::{TransmitGate, VoiceGateState, VoiceInputMode};

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
