use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::AtomicBool,
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use libwebrtc::{
    audio_source::native::NativeAudioSource,
};
use livekit::{
    e2ee::key_provider::KeyProvider,
    prelude::*,
    track::LocalAudioTrack,
};
use serde::{Deserialize, Serialize};

use crate::commands::{
    voice_apm,
    voice_denoiser,
};

/// Warm-cached LiveKit token + DNS/TLS warmup for one channel.
pub struct VoiceWarmup {
    pub channel_id: String,
    pub token: String,
    /// The ws URL `token` was minted for; a JWT is only valid at its issuer.
    pub url: String,
    /// When the prepared token was created. Used to discard stale entries.
    pub created_at: Instant,
    /// Identity captured at prep; a mismatch at join invalidates the token.
    pub user_id: String,
    pub display_name: String,
    /// Background DNS/TLS task; aborted when superseded.
    pub task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for VoiceWarmup {
    fn drop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}

/// Max warm-token age, so a stale prep can't hand join an expired JWT.
pub(crate) const VOICE_WARMUP_TTL: Duration = Duration::from_secs(300);

// cpal::Stream is Send on Linux and macOS. On Windows WASAPI it is not,
// so we wrap it with an explicit unsafe impl to allow storage in AppState.
pub struct SendableStream(pub cpal::Stream);
unsafe impl Send for SendableStream {}
unsafe impl Sync for SendableStream {}

impl Drop for SendableStream {
    fn drop(&mut self) {
        // pause() first: Drop alone can leave ALSA parked, mic light on.
        use cpal::traits::StreamTrait;
        let _ = self.0.pause();
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VoiceEvent {
    ParticipantJoined {
        identity: String,
        name: String,
        is_muted: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        avatar_url: Option<String>,
    },
    ParticipantLeft { identity: String },
    Muted { identity: String },
    Unmuted { identity: String },
    /// Whether the local session has a working mic; `false` = listen-only.
    MicAvailability { identity: String, available: bool },
    SpeakingStarted { identity: String },
    SpeakingStopped { identity: String },
    /// ~20 Hz 0..1 band levels; `levels::BAND_COUNT` must match the renderer.
    AudioBands { identity: String, bands: Vec<f32> },
    /// LiveKit's categorical quality: "excellent" | "good" | "poor" | "lost".
    ConnectionQualityChanged {
        identity: String,
        quality: String,
    },
    /// New voice epoch key; lets the renderer's screen-share view client rotate.
    VoiceE2eeKeyRotated {
        key: Vec<u8>,
        key_index: i32,
        epoch: u64,
        mls_group_id: String,
    },
    Disconnected,
}

#[derive(Serialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub kind: String, // "input" | "output"
}

// All remote audio mixes into one cpal stream: the single AEC render tap.

/// Per-track f32 mono ring buffers, keyed by `"{identity}-{sid}"`.
pub type TrackBuffers = Arc<Mutex<HashMap<String, VecDeque<f32>>>>;

/// 200 ms at 48 kHz mono; older samples are dropped.
pub(crate) const TRACK_BUFFER_CAP_SAMPLES: usize = 9_600;

pub struct PlaybackState {
    /// Per-remote-track f32 mono ring buffers fed by drain tasks.
    pub track_buffers: TrackBuffers,
    /// The single shared cpal output stream. `None` until `start_playback`.
    pub output_stream: Option<SendableStream>,
    /// Interleaved-f32 ring drained by the cpal callback, shared with the mixer.
    pub output_ring: Option<Arc<Mutex<VecDeque<f32>>>>,
    pub output_channels: u32,
    pub output_sample_rate: u32,
    pub output_device_name: Option<String>,
    /// Per-track NativeAudioStream drain tasks.
    pub drain_tasks: HashMap<String, tokio::task::JoinHandle<()>>,
    /// Raw RtcAudioTrack refs, kept so `set_voice_output_device` can rebuild
    /// the speaker stream while preserving subscriptions.
    pub rtc_tracks: HashMap<String, libwebrtc::audio_track::RtcAudioTrack>,
    /// Identity per `track_key`, used by `set_voice_output_device` to
    /// re-emit speaking events on the new pipeline if needed.
    pub identities: HashMap<String, String>,
    /// Per-user gain keyed by bare `user_id`; absent = unity.
    pub user_volumes: Arc<Mutex<HashMap<String, f32>>>,
    /// Single mixer task running at 10 ms cadence.
    pub mixer_task: Option<tokio::task::JoinHandle<()>>,
}

impl PlaybackState {
    fn new() -> Self {
        Self {
            track_buffers: Arc::new(Mutex::new(HashMap::new())),
            output_stream: None,
            output_ring: None,
            output_channels: 0,
            output_sample_rate: 0,
            output_device_name: None,
            drain_tasks: HashMap::new(),
            rtc_tracks: HashMap::new(),
            identities: HashMap::new(),
            user_volumes: Arc::new(Mutex::new(HashMap::new())),
            mixer_task: None,
        }
    }

    /// Stop everything but keep track refs, so `start_playback` can reattach.
    pub(crate) fn stop_all(&mut self) {
        for (_, t) in self.drain_tasks.drain() {
            t.abort();
        }
        if let Some(t) = self.mixer_task.take() {
            t.abort();
        }
        self.output_stream = None;
        self.output_ring = None;
        self.output_sample_rate = 0;
        self.output_channels = 0;
        if let Ok(mut buffers) = self.track_buffers.lock() {
            buffers.clear();
        }
    }
}

// Join-path phase timings, in ms (see `get_last_join_timings`).

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct JoinTimings {
    pub channel_id: String,
    pub jwt_mint_ms: u64,
    /// MLS resolve + exporter; runs before connect, so pure added latency.
    pub e2ee_ms: u64,
    pub room_connect_ms: u64,
    pub mic_init_ms: u64,
    pub first_publish_ms: u64,
    /// Seeding the participant list: pseudonym resolution + the batched avatar
    /// lookup for everyone already in the room.
    pub roster_ms: u64,
    pub total_join_ms: u64,
    /// UNIX epoch ms when `join_voice_channel` started.
    pub join_started_at_ms: u64,
}

pub struct VoiceState {
    pub channel: Option<std::sync::Arc<dyn crate::sink::EventSink<VoiceEvent>>>,
    pub room: Option<Arc<Room>>,
    pub local_track: Option<LocalAudioTrack>,
    pub audio_source: Option<NativeAudioSource>,
    pub input_stream: Option<SendableStream>,
    pub frame_task: Option<tokio::task::JoinHandle<()>>,
    pub room_task: Option<tokio::task::JoinHandle<()>>,
    pub playback: Arc<Mutex<PlaybackState>>,
    /// PTT/mute/deafen state machine (#849); the atomics below derive from it.
    pub gate: Arc<Mutex<super::gate::TransmitGate>>,
    /// Derived: capture callback drops frames (mute, deafen or idle PTT).
    pub transmit_muted: Arc<AtomicBool>,
    /// Derived: mixer outputs silence; tracks keep draining to stay live.
    pub deafened: Arc<AtomicBool>,
    /// Derived: explicit mute only. For the mic test, which must ignore PTT.
    pub self_muted: Arc<AtomicBool>,
    /// Join in flight; blocks a racing join (DuplicateIdentity).
    pub joining: Arc<AtomicBool>,
    pub current_input_device: Option<String>,
    /// APM stage for the current session; `None` outside a call.
    pub apm: Option<voice_apm::ApmStage>,
    /// RNNoise ahead of APM; `None` outside a call or when the mic isn't 48 kHz.
    pub denoiser: Arc<Mutex<Option<voice_denoiser::DenoiserStage>>>,
    /// Next channel's precomputed token + DNS/TLS warmer.
    pub warmup: Option<VoiceWarmup>,
    /// Most recent `join_voice_channel` timing record.
    pub last_join_timings: Arc<Mutex<Option<JoinTimings>>>,
    /// Live `KeyProvider` from `livekit::e2ee`, retained so MLS epoch
    /// changes can rotate the shared key without rebuilding the room.
    pub e2ee_key_provider: Option<KeyProvider>,
    /// MLS group id whose exporter backs `e2ee_key_provider`. Used to match
    /// epoch-change events for the currently-joined room.
    pub e2ee_mls_group_id: Option<String>,
    /// MLS epoch of the current voice key; suppresses duplicate rotations.
    pub e2ee_epoch: u64,
    /// Ring slot local cryptors must use; libwebrtc starts each on slot 0.
    pub e2ee_key_index: i32,
    /// Shared-audio echo canceller fed by the mixer; see `screenshare::self_echo`.
    pub shared_audio_render: crate::commands::screenshare::self_echo::SelfEchoSlot,
}

impl Default for VoiceState {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceState {
    pub fn new() -> Self {
        Self {
            channel: None,
            room: None,
            local_track: None,
            audio_source: None,
            input_stream: None,
            frame_task: None,
            room_task: None,
            playback: Arc::new(Mutex::new(PlaybackState::new())),
            gate: Arc::new(Mutex::new(super::gate::TransmitGate::new())),
            transmit_muted: Arc::new(AtomicBool::new(false)),
            deafened: Arc::new(AtomicBool::new(false)),
            self_muted: Arc::new(AtomicBool::new(false)),
            joining: Arc::new(AtomicBool::new(false)),
            current_input_device: None,
            apm: None,
            denoiser: Arc::new(Mutex::new(None)),
            warmup: None,
            last_join_timings: Arc::new(Mutex::new(None)),
            e2ee_key_provider: None,
            e2ee_mls_group_id: None,
            e2ee_epoch: 0,
            e2ee_key_index: 0,
            shared_audio_render: Arc::new(Mutex::new(None)),
        }
    }
}

// Identity grammar lives in `commands::livekit_identity` (#836).
pub use crate::commands::livekit_identity::user_id_from_voice_identity;
