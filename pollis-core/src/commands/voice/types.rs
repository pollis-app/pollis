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

/// Warm-cached LiveKit credentials for one channel, issued by
/// `prepare_voice_connection` on user intent (hover, route entry) so
/// `join_voice_channel` skips the JWT mint. The bigger win is the DNS/TLS
/// warmth from a one-shot HTTPS request to the LiveKit host.
pub struct VoiceWarmup {
    pub channel_id: String,
    pub token: String,
    /// The server `token` is valid at: a JWT works only where it was issued,
    /// so the two are cached as a pair.
    pub url: String,
    /// When the prepared token was created. Used to discard stale entries.
    pub created_at: Instant,
    /// A mismatch against the join-time identity invalidates the token.
    pub user_id: String,
    pub display_name: String,
    /// Background DNS/TLS warmup, aborted when a new prep supersedes this one.
    pub task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for VoiceWarmup {
    fn drop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}

/// Cap warm-token freshness so a stale prep cannot hand `join_voice_channel`
/// an expired credential.
pub(crate) const VOICE_WARMUP_TTL: Duration = Duration::from_secs(300);

// cpal::Stream is not Send on Windows WASAPI (it is on Linux and macOS);
// wrapped so it can live in AppState.
pub struct SendableStream(pub cpal::Stream);
unsafe impl Send for SendableStream {}
unsafe impl Sync for SendableStream {}

impl Drop for SendableStream {
    fn drop(&mut self) {
        // Drop alone doesn't reliably stop the backend thread: on ALSA it can
        // be parked in snd_pcm_readi, leaving the OS mic indicator on until
        // exit. pause() wakes it and releases the device.
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
    /// Emitted once at join: `available: false` means the mic failed to open
    /// and we joined listen-only (connected, receiving, not publishing).
    /// Always the local identity.
    MicAvailability { identity: String, available: bool },
    SpeakingStarted { identity: String },
    SpeakingStopped { identity: String },
    /// Per-source band levels (0..1 each), ~20 Hz, for the tile meter.
    /// Cosmetic; drop freely. Band count is `levels::BAND_COUNT`.
    AudioBands { identity: String, bands: Vec<f32> },
    /// LiveKit's categorical connection quality ("excellent" | "good" |
    /// "poor" | "lost"); the Rust SDK exposes no RTT.
    ConnectionQualityChanged {
        identity: String,
        quality: String,
    },
    /// A fresh voice E2EE key after an MLS epoch advance. The Rust path's
    /// `KeyProvider` is already updated; the renderer's screen-share view
    /// client holds its own key provider and must rotate too, or it drifts out
    /// of sync on any commit.
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

// All remote audio goes through one mixer and one output stream, the single
// point to tap the speaker signal as APM's render (AEC) reference:
//
//   [NativeAudioStream per remote track] ─ drain task ─→ track_buffers[k]
//                                                            │
//                                              mixer task (10ms tick)
//                                                            ▼
//                                         output_ring (interleaved f32)
//                                                            │
//                                               cpal output stream callback

/// Per-track f32 mono ring buffers, keyed by `"{identity}-{sid}"`: drain tasks
/// push, the mixer drains every 10 ms. Each lock is a single insert or drain,
/// never held across an `await`.
pub type TrackBuffers = Arc<Mutex<HashMap<String, VecDeque<f32>>>>;

/// Per-track buffer cap: 200 ms at 48 kHz mono. Older samples are dropped to
/// keep latency low.
pub(crate) const TRACK_BUFFER_CAP_SAMPLES: usize = 9_600;

pub struct PlaybackState {
    /// Per-remote-track f32 mono ring buffers fed by drain tasks.
    pub track_buffers: TrackBuffers,
    /// The single shared cpal output stream. `None` until `start_playback`.
    pub output_stream: Option<SendableStream>,
    /// The interleaved-f32 ring the output callback drains, shared with the
    /// mixer.
    pub output_ring: Option<Arc<Mutex<VecDeque<f32>>>>,
    pub output_channels: u32,
    pub output_sample_rate: u32,
    pub output_device_name: Option<String>,
    /// Per-track NativeAudioStream drain tasks.
    pub drain_tasks: HashMap<String, tokio::task::JoinHandle<()>>,
    /// Kept so `set_voice_output_device` can rebuild the speaker stream
    /// without losing subscriptions.
    pub rtc_tracks: HashMap<String, libwebrtc::audio_track::RtcAudioTrack>,
    pub identities: HashMap<String, String>,
    /// Per-remote-user output gain, keyed by `user_id` (not the LiveKit
    /// identity; see `user_id_from_voice_identity`). Absent means 1.0.
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

    /// Stop the drain tasks, the mixer and the output stream. Track refs are
    /// kept so `start_playback` can reattach (e.g. on an output-device switch).
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

// Join-path phase timings in milliseconds, exposed via `get_last_join_timings`
// for the dev console. `total_join_ms` runs from entry to the first
// publish_track.

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct JoinTimings {
    pub channel_id: String,
    pub jwt_mint_ms: u64,
    /// MLS resolution, catch-up and exporter derivation. Runs before the
    /// connect (the key goes in `RoomOptions`): pure added latency.
    pub e2ee_ms: u64,
    pub room_connect_ms: u64,
    pub mic_init_ms: u64,
    pub first_publish_ms: u64,
    /// Pseudonym resolution plus the batched avatar lookup for the roster.
    pub roster_ms: u64,
    pub total_join_ms: u64,
    /// UNIX epoch ms at `join_voice_channel` entry, to correlate with frontend
    /// click timestamps.
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
    /// Push-to-talk / mute / deafen state machine (#849), the authority for
    /// every local gating decision. The atomics below are its derived booleans,
    /// so the audio hot paths never take a lock.
    pub gate: Arc<Mutex<super::gate::TransmitGate>>,
    /// Derived: the capture callback drops its frames (mute, deafen, or an
    /// un-held push-to-talk key).
    pub transmit_muted: Arc<AtomicBool>,
    /// Derived: the mixer zeroes its output. Remote tracks keep draining, so
    /// undeafening resumes live rather than flushing a backlog.
    pub deafened: Arc<AtomicBool>,
    /// Derived: the user's explicit mute only. For the mic-test harness
    /// (`voice_test.rs`): `transmit_muted` is `true` outside a call in
    /// push-to-talk mode, which would make a mic test record nothing.
    pub self_muted: Arc<AtomicBool>,
    /// Set while a join is in flight, so a concurrent one cannot trip
    /// LiveKit's DuplicateIdentity eviction (which disconnects both).
    pub joining: Arc<AtomicBool>,
    pub current_input_device: Option<String>,
    /// APM stage for the current session; `None` outside a call.
    pub apm: Option<voice_apm::ApmStage>,
    /// Optional RNNoise denoiser upstream of APM, behind a mutex because the
    /// frame task needs `&mut` and the click-suppression toggle can swap it
    /// mid-call. `None` outside a call or when the mic isn't at 48 kHz.
    pub denoiser: Arc<Mutex<Option<voice_denoiser::DenoiserStage>>>,
    /// Precomputed token and DNS/TLS warmer for the next likely channel.
    pub warmup: Option<VoiceWarmup>,
    /// The latest successful join's timings.
    pub last_join_timings: Arc<Mutex<Option<JoinTimings>>>,
    /// Retained so MLS epoch changes rotate the key without rebuilding the
    /// room.
    pub e2ee_key_provider: Option<KeyProvider>,
    /// MLS group id whose exporter backs `e2ee_key_provider`.
    pub e2ee_mls_group_id: Option<String>,
    /// MLS epoch the current key was derived at; suppresses duplicate
    /// rotations.
    pub e2ee_epoch: u64,
    /// Key-ring slot every local sender `FrameCryptor` must use, re-applied
    /// to each newly published track: libwebrtc creates every cryptor on slot
    /// 0 and never moves it.
    pub e2ee_key_index: i32,
    /// Where a screen share capturing system audio parks its echo canceller,
    /// so the mixer can feed it the speaker signal (`screenshare::self_echo`).
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

// The identity grammar lives in `commands::livekit_identity` (#836), in one
// place so the wire and internal identity spaces cannot drift.
pub use crate::commands::livekit_identity::user_id_from_voice_identity;
