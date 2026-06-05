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

/// Warm-cached LiveKit credentials for a single channel. Issued by
/// `prepare_voice_connection` on user "intent" (hover, route entry) and
/// consumed by `join_voice_channel` to skip the synchronous JWT mint.
///
/// The DNS/TLS warmth is the bigger win — we kick off a one-shot HTTPS
/// request to the LiveKit host so its address and TLS session are in the
/// process-wide cache by the time `Room::connect` opens the WebSocket.
pub struct VoiceWarmup {
    pub channel_id: String,
    pub token: String,
    /// When the prepared token was created. Used to discard stale entries.
    pub created_at: Instant,
    /// When the underlying user identity was captured. Mismatches against
    /// the join-time identity invalidate the cached token.
    pub user_id: String,
    pub display_name: String,
    /// Background warmup task (DNS/TLS). Aborted if a new prep supersedes
    /// this one so spamming hover doesn't pile up requests.
    pub task: Option<tokio::task::JoinHandle<()>>,
}

impl Drop for VoiceWarmup {
    fn drop(&mut self) {
        if let Some(t) = self.task.take() {
            t.abort();
        }
    }
}

/// Warm tokens are cheap to mint but not free; cap freshness to 5 min so a
/// long-stale prep can't deliver an expired credential to `join_voice_channel`.
pub(crate) const VOICE_WARMUP_TTL: Duration = Duration::from_secs(300);

// ── cpal::Stream is Send on Linux and macOS. On Windows WASAPI it is not,
// so we wrap it with an explicit unsafe impl to allow storage in AppState.
pub struct SendableStream(pub cpal::Stream);
unsafe impl Send for SendableStream {}
unsafe impl Sync for SendableStream {}

impl Drop for SendableStream {
    fn drop(&mut self) {
        // cpal::Stream::Drop alone doesn't reliably stop the backend audio
        // thread — on ALSA the I/O thread can be parked in snd_pcm_readi and
        // miss the drop signal, leaving the OS mic-in-use indicator on until
        // the process exits. Explicit pause() wakes the backend and releases
        // the device promptly.
        use cpal::traits::StreamTrait;
        let _ = self.0.pause();
    }
}

// ── Events pushed to the frontend ─────────────────────────────────────────

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
    SpeakingStarted { identity: String },
    SpeakingStopped { identity: String },
    /// LiveKit's per-participant categorical connection quality. The Rust
    /// SDK doesn't expose RTT in ms here — this is the only signal it
    /// surfaces — so the UI shows a lagging-dot rather than a number.
    /// `quality` is one of "excellent" | "good" | "poor" | "lost".
    ConnectionQualityChanged {
        identity: String,
        quality: String,
    },
    /// Voice MLS epoch advanced and a fresh E2EE key was derived. The Rust
    /// voice path's `KeyProvider` is already updated; this event lets the
    /// renderer's screen-share view client (which holds its own
    /// `ExternalE2EEKeyProvider`) rotate to the new key. Without it the
    /// view client keeps encrypting/decrypting with the key from its
    /// connect-time epoch and drifts out of sync with the audio path on
    /// any commit.
    VoiceE2eeKeyRotated {
        key: Vec<u8>,
        key_index: i32,
        epoch: u64,
        mls_group_id: String,
    },
    Disconnected,
}

// ── Audio device descriptor returned to the frontend ─────────────────────

#[derive(Serialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
    pub kind: String, // "input" | "output"
}

// ── Playback pipeline ─────────────────────────────────────────────────────
//
// All remote audio funnels through a single mixer → one cpal output stream.
// That gives us one stable point at which to tap the "what's about to hit
// the speaker" signal as APM's render (AEC) reference. The previous
// per-track stream model worked for output but had no single mix point.
//
// Layout:
//
//   [NativeAudioStream per remote track] ─ drain task ─→ track_buffers[k]
//                                                            │
//                                              mixer task (10ms tick)
//                                                            ▼
//                                         output_ring (interleaved f32)
//                                                            │
//                                               cpal output stream callback

/// Per-track f32 mono ring buffers, keyed by `"{identity}-{sid}"`. Drain
/// tasks push into them; the mixer drains them every 10 ms. Held in an
/// `Arc<Mutex<…>>` because both the drain tasks and the mixer task reach
/// into it from different tokio workers; the lock window is always a
/// single insert/drain so it never overlaps an `await`.
pub type TrackBuffers = Arc<Mutex<HashMap<String, VecDeque<f32>>>>;

/// Capacity per per-track buffer. 200 ms at 48 kHz mono = 9_600 samples;
/// anything older than that is dropped to keep latency low. Same number
/// the previous per-track ring used.
pub(crate) const TRACK_BUFFER_CAP_SAMPLES: usize = 9_600;

pub struct PlaybackState {
    /// Per-remote-track f32 mono ring buffers fed by drain tasks.
    pub track_buffers: TrackBuffers,
    /// The single shared cpal output stream. `None` until `start_playback`.
    pub output_stream: Option<SendableStream>,
    /// The interleaved-f32 ring the cpal output callback drains. Same Arc
    /// shared with the mixer task so both speak to one buffer.
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
    /// Per-remote-user output gain multiplier, keyed by `user_id` (NOT the
    /// LiveKit identity — see `user_id_from_voice_identity` for the
    /// conversion). Read on the mixer hot path; absence means unity (1.0).
    /// Shared with the mixer task via `Arc::clone`.
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

    /// Stop and drop everything: per-track drain tasks, the mixer, the
    /// cpal output stream. Track refs are kept so a follow-up
    /// `start_playback` (e.g. on output-device switch) can reattach.
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

// ── Join-path timing instrumentation ──────────────────────────────────────
//
// Each phase below measures wall time around a discrete chunk of the join
// flow. All values are milliseconds. `total_join_ms` is wall time from
// `join_voice_channel` entry until first publish_track resolves. Stored on
// `VoiceState.last_join_timings` and exposed via the `get_last_join_timings`
// Tauri command so the frontend can dump them to the dev console.

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct JoinTimings {
    pub channel_id: String,
    pub jwt_mint_ms: u64,
    pub room_connect_ms: u64,
    pub mic_init_ms: u64,
    pub first_publish_ms: u64,
    pub total_join_ms: u64,
    /// UNIX epoch ms when `join_voice_channel` started — useful for
    /// correlating with frontend click timestamps later.
    pub join_started_at_ms: u64,
}

// ── Top-level voice state held in AppState ────────────────────────────────

pub struct VoiceState {
    pub channel: Option<std::sync::Arc<dyn crate::sink::EventSink<VoiceEvent>>>,
    pub room: Option<Arc<Room>>,
    pub local_track: Option<LocalAudioTrack>,
    pub audio_source: Option<NativeAudioSource>,
    pub input_stream: Option<SendableStream>,
    pub frame_task: Option<tokio::task::JoinHandle<()>>,
    pub room_task: Option<tokio::task::JoinHandle<()>>,
    pub playback: Arc<Mutex<PlaybackState>>,
    pub is_muted: Arc<AtomicBool>,
    /// Set while a `join_voice_channel` call is in flight. Blocks a second
    /// concurrent invocation from racing the first and tripping LiveKit's
    /// DuplicateIdentity eviction (which would disconnect both attempts).
    pub joining: Arc<AtomicBool>,
    pub current_input_device: Option<String>,
    /// APM stage for the current voice session. Owns the WebRTC AudioProcessing
    /// `Processor` and its config. `None` outside a call.
    pub apm: Option<voice_apm::ApmStage>,
    /// Optional RNNoise denoiser running upstream of APM. Held behind an
    /// `Arc<Mutex<…>>` because the frame_task needs `&mut` access (RNNoise
    /// is stateful) and `set_voice_audio_processing` mutates the slot when
    /// the user toggles Click Suppression mid-call. `None` outside a call,
    /// or when the mic isn't running at 48 kHz (RNNoise is rate-locked).
    pub denoiser: Arc<Mutex<Option<voice_denoiser::DenoiserStage>>>,
    /// Optional precomputed token + DNS/TLS warmer for the next channel
    /// the user is likely to join. See [`VoiceWarmup`].
    pub warmup: Option<VoiceWarmup>,
    /// Most recent `join_voice_channel` timing record. Populated at the end
    /// of every successful join; read by `get_last_join_timings`.
    pub last_join_timings: Arc<Mutex<Option<JoinTimings>>>,
    /// Live `KeyProvider` from `livekit::e2ee`, retained so MLS epoch
    /// changes can rotate the shared key without rebuilding the room.
    pub e2ee_key_provider: Option<KeyProvider>,
    /// MLS group id whose exporter backs `e2ee_key_provider`. Used to match
    /// epoch-change events for the currently-joined room.
    pub e2ee_mls_group_id: Option<String>,
    /// MLS epoch the current voice key was derived at. Suppresses duplicate
    /// rotations and lets the rotation hook skip when nothing has changed.
    pub e2ee_epoch: u64,
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
            is_muted: Arc::new(AtomicBool::new(false)),
            joining: Arc::new(AtomicBool::new(false)),
            current_input_device: None,
            apm: None,
            denoiser: Arc::new(Mutex::new(None)),
            warmup: None,
            last_join_timings: Arc::new(Mutex::new(None)),
            e2ee_key_provider: None,
            e2ee_mls_group_id: None,
            e2ee_epoch: 0,
        }
    }
}

/// Extract the bare `user_id` from a voice-channel LiveKit identity.
///
/// Identity formats produced by this module:
///   - `voice-{user_id}` (today)
///   - `voice-{user_id}:{device_id}` (reserved for #140; not emitted yet)
///
/// Anything that doesn't match the `voice-` prefix is returned unchanged so
/// the helper degrades to a no-op if some other identity scheme leaks in.
///
/// NOTE (#140 — multi-device voice): if/when a single user can be present
/// in a voice channel from multiple devices, decide whether `user_volumes`
/// should remain user-scoped (current behavior — simpler, "Bob is Bob") or
/// shift to per-device (`{user_id}:{device_id}`). If you change the key
/// shape, update both this helper and the frontend writers in
/// `RemoteUserVolumeSlider.tsx` / `useVoiceChannel.ts`.
pub fn user_id_from_voice_identity(identity: &str) -> &str {
    let stripped = identity.strip_prefix("voice-").unwrap_or(identity);
    match stripped.split_once(':') {
        Some((uid, _device)) => uid,
        None => stripped,
    }
}
