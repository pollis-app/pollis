use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use futures_util::StreamExt;
use libwebrtc::{
    audio_source::native::NativeAudioSource,
    audio_stream::native::NativeAudioStream,
    prelude::{AudioFrame, AudioSourceOptions, RtcAudioSource},
};
use livekit::{
    options::TrackPublishOptions,
    prelude::*,
    track::{LocalAudioTrack, LocalTrack, RemoteTrack},
};
use serde::{Deserialize, Serialize};
use tauri::State;
use tokio::time::MissedTickBehavior;
use webrtc_audio_processing::Processor as ApmProcessor;

use crate::{
    commands::{livekit::make_token, voice_apm, voice_denoiser},
    error::Result,
    state::AppState,
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
const VOICE_WARMUP_TTL: Duration = Duration::from_secs(300);

// ── cpal::Stream is Send on Linux and macOS. On Windows WASAPI it is not,
// so we wrap it with an explicit unsafe impl to allow storage in AppState.
pub(crate) struct SendableStream(pub(crate) cpal::Stream);
unsafe impl Send for SendableStream {}
unsafe impl Sync for SendableStream {}

// ── Events pushed to the frontend ─────────────────────────────────────────

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VoiceEvent {
    ParticipantJoined { identity: String, name: String, is_muted: bool },
    ParticipantLeft { identity: String },
    Muted { identity: String },
    Unmuted { identity: String },
    SpeakingStarted { identity: String },
    SpeakingStopped { identity: String },
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
const TRACK_BUFFER_CAP_SAMPLES: usize = 9_600;

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
            mixer_task: None,
        }
    }

    /// Stop and drop everything: per-track drain tasks, the mixer, the
    /// cpal output stream. Track refs are kept so a follow-up
    /// `start_playback` (e.g. on output-device switch) can reattach.
    fn stop_all(&mut self) {
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
    pub channel: Option<tauri::ipc::Channel<VoiceEvent>>,
    pub room: Option<Arc<Room>>,
    pub local_track: Option<LocalAudioTrack>,
    pub audio_source: Option<NativeAudioSource>,
    pub input_stream: Option<SendableStream>,
    pub frame_task: Option<tokio::task::JoinHandle<()>>,
    pub room_task: Option<tokio::task::JoinHandle<()>>,
    pub playback: Arc<Mutex<PlaybackState>>,
    pub is_muted: Arc<AtomicBool>,
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
            current_input_device: None,
            apm: None,
            denoiser: Arc::new(Mutex::new(None)),
            warmup: None,
            last_join_timings: Arc::new(Mutex::new(None)),
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────

pub(crate) fn get_device(host: &cpal::Host, name: Option<&str>, is_input: bool) -> Result<cpal::Device> {
    // Frontend sends "default" (or "") to mean "use the OS default" rather
    // than a real device id. Treat those as None so we don't go looking
    // for a device literally named "default".
    let name = name.filter(|n| !n.is_empty() && *n != "default");
    let device = match name {
        None => {
            // On Linux, ALSA's system default may be a virtual device like
            // "vdownmix" (surround downmix) that crashes when opened for capture.
            // Strategy:
            //   1. Try well-known audio-server PCMs: pipewire, pulse, default.
            //   2. Fall back to first device that passes is_useful_device.
            //   3. Return error rather than opening a blocked device.
            #[cfg(target_os = "linux")]
            {
                // Virtual ALSA devices known to crash or produce no audio.
                let blocked: &[&str] = &["vdownmix", "upmix", "speex", "speexrate"];
                let preferred: &[&str] = &["pipewire", "pulse", "default"];

                let iter = if is_input {
                    host.input_devices().map_err(|e| anyhow::anyhow!("enumerate devices: {e}"))?
                } else {
                    host.output_devices().map_err(|e| anyhow::anyhow!("enumerate devices: {e}"))?
                };
                // Collect, filter blocked names, log what we see.
                let devices: Vec<cpal::Device> = iter
                    .filter(|d| {
                        d.name()
                            .ok()
                            .map(|n| !blocked.contains(&n.as_str()))
                            .unwrap_or(true)
                    })
                    .collect();

                let names: Vec<String> = devices.iter().filter_map(|d| d.name().ok()).collect();
                eprintln!("[voice] available {} devices (blocked filtered): {:?}",
                    if is_input { "input" } else { "output" }, names);

                // 1. Preferred by name
                let found = preferred.iter().find_map(|&pref| {
                    devices.iter().position(|d| d.name().ok().as_deref() == Some(pref))
                });
                if let Some(idx) = found {
                    devices.into_iter().nth(idx)
                } else {
                    // 2. First device that passes the useful filter (e.g. hw:CARD=...,DEV=0)
                    devices
                        .into_iter()
                        .find(|d| d.name().ok().map(|n| is_useful_device(&n)).unwrap_or(false))
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                if is_input { host.default_input_device() } else { host.default_output_device() }
            }
        }
        Some(n) => {
            // On Linux, reject blocked devices even when explicitly named.
            // Fall back to auto-detect so a stale preference doesn't crash the app.
            #[cfg(target_os = "linux")]
            {
                let blocked: &[&str] = &["vdownmix", "upmix", "speex", "speexrate"];
                if blocked.contains(&n) {
                    eprintln!("[voice] device '{n}' is blocked on Linux — auto-selecting");
                    return get_device(host, None, is_input);
                }
            }
            let iter = if is_input {
                host.input_devices().map_err(|e| anyhow::anyhow!("enumerate devices: {e}"))?
            } else {
                host.output_devices().map_err(|e| anyhow::anyhow!("enumerate devices: {e}"))?
            };
            iter.filter(|d| d.name().ok().as_deref() == Some(n)).next()
        }
    };
    device.ok_or_else(|| anyhow::anyhow!("audio device not found").into())
}

/// Build a cpal input stream that converts mic audio to i16 mono and sends
/// 10ms chunks to `frame_tx`. Downmixes multi-channel input to mono.
pub(crate) fn start_mic_stream(
    device: &cpal::Device,
    frame_tx: tokio::sync::mpsc::UnboundedSender<(Vec<i16>, u32)>,
    is_muted: Arc<AtomicBool>,
) -> Result<(SendableStream, u32)> {
    let config = device
        .default_input_config()
        .map_err(|e| anyhow::anyhow!("input config: {e}"))?;
    let channels = config.channels() as usize;
    let sample_format = config.sample_format();
    // Prefer 48 kHz (WebRTC APM only supports 8/16/32/48 kHz), but fall
    // back to whatever the device actually supports — Bluetooth devices
    // on macOS (e.g. AirPods in SCO) often only advertise 16/24 kHz, and
    // forcing 48 kHz makes build_input_stream fail.
    let preferred_rate: u32 = 48_000;
    let supports_48k = device
        .supported_input_configs()
        .map(|cfgs| {
            cfgs.filter(|c| c.channels() == config.channels() && c.sample_format() == sample_format)
                .any(|c| {
                    c.min_sample_rate().0 <= preferred_rate
                        && c.max_sample_rate().0 >= preferred_rate
                })
        })
        .unwrap_or(false);
    let sample_rate: u32 = if supports_48k {
        preferred_rate
    } else {
        config.sample_rate().0
    };
    let stream_config = cpal::StreamConfig {
        channels: config.channels(),
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    // Build the stream for whatever native format the device reports.
    // Each arm clones the shared state so it can move into the closure.
    macro_rules! build_input {
        ($T:ty, $to_f32:expr) => {{
            let frame_tx = frame_tx.clone();
            let is_muted = Arc::clone(&is_muted);
            device.build_input_stream::<$T, _, _>(
                &stream_config,
                move |data: &[$T], _| {
                    if is_muted.load(Ordering::Relaxed) {
                        return;
                    }
                    let f32s: Vec<f32> = data.iter().copied().map($to_f32).collect();
                    let mono: Vec<i16> = if channels == 1 {
                        f32s.iter()
                            .map(|&s| (s * 32_767.0).clamp(-32_768.0, 32_767.0) as i16)
                            .collect()
                    } else {
                        f32s.chunks(channels)
                            .map(|ch| {
                                let avg = ch.iter().sum::<f32>() / channels as f32;
                                (avg * 32_767.0).clamp(-32_768.0, 32_767.0) as i16
                            })
                            .collect()
                    };
                    let _ = frame_tx.send((mono, sample_rate));
                },
                |e| eprintln!("[voice] mic error: {e}"),
                None,
            )
        }};
    }

    let stream = match sample_format {
        cpal::SampleFormat::F32 => build_input!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => build_input!(i16, |s: i16| s as f32 / 32_768.0),
        cpal::SampleFormat::U16 => build_input!(u16, |s: u16| (s as f32 - 32_768.0) / 32_768.0),
        cpal::SampleFormat::I32 => build_input!(i32, |s: i32| s as f32 / 2_147_483_648.0),
        cpal::SampleFormat::U32 => build_input!(u32, |s: u32| (s as f64 / 2_147_483_648.0 - 1.0) as f32),
        other => return Err(anyhow::anyhow!("unsupported mic format: {other:?}").into()),
    }
    .map_err(|e| anyhow::anyhow!("build mic stream: {e}"))?;

    stream.play().map_err(|e| anyhow::anyhow!("mic stream play: {e}"))?;
    Ok((SendableStream(stream), sample_rate))
}

/// Build a cpal output stream driven by a shared ring buffer.
/// Returns the stream (kept alive by the caller) and the buffer to push into.
///
/// `preferred_rate` is the rate we'd like to run at; we'll honour it when
/// the device supports it and fall back to its default rate otherwise. The
/// caller can compare the returned rate against `preferred_rate` to decide
/// whether the AEC render reference can be tapped without resampling.
pub(crate) fn start_speaker_stream(
    device: &cpal::Device,
    preferred_rate: u32,
) -> Result<(SendableStream, u32, u32, Arc<Mutex<VecDeque<f32>>>)> {
    let config = device
        .default_output_config()
        .map_err(|e| anyhow::anyhow!("output config: {e}"))?;
    let channels = config.channels() as u32;
    let sample_format = config.sample_format();

    let supports_preferred = device
        .supported_output_configs()
        .map(|cfgs| {
            cfgs.filter(|c| c.channels() == config.channels() && c.sample_format() == sample_format)
                .any(|c| {
                    c.min_sample_rate().0 <= preferred_rate
                        && c.max_sample_rate().0 >= preferred_rate
                })
        })
        .unwrap_or(false);
    let sample_rate: u32 = if supports_preferred {
        preferred_rate
    } else {
        config.sample_rate().0
    };
    let stream_config = cpal::StreamConfig {
        channels: config.channels(),
        sample_rate: cpal::SampleRate(sample_rate),
        buffer_size: cpal::BufferSize::Default,
    };

    // 2-second ring buffer (always f32 internally; converted on output)
    let buf: Arc<Mutex<VecDeque<f32>>> =
        Arc::new(Mutex::new(VecDeque::with_capacity((sample_rate * channels * 2) as usize)));

    macro_rules! build_output {
        ($T:ty, $from_f32:expr) => {{
            let buf_cb = Arc::clone(&buf);
            device.build_output_stream::<$T, _, _>(
                &stream_config,
                move |data: &mut [$T], _| {
                    let mut b = buf_cb.lock().unwrap();
                    for s in data.iter_mut() {
                        *s = $from_f32(b.pop_front().unwrap_or(0.0));
                    }
                },
                |e| eprintln!("[voice] speaker error: {e}"),
                None,
            )
        }};
    }

    let stream = match sample_format {
        cpal::SampleFormat::F32 => build_output!(f32, |s: f32| s),
        cpal::SampleFormat::I16 => build_output!(i16, |s: f32| (s * 32_767.0).clamp(-32_768.0, 32_767.0) as i16),
        cpal::SampleFormat::U16 => build_output!(u16, |s: f32| ((s + 1.0) * 32_768.0).clamp(0.0, 65_535.0) as u16),
        cpal::SampleFormat::I32 => build_output!(i32, |s: f32| (s * 2_147_483_647.0).clamp(-2_147_483_648.0, 2_147_483_647.0) as i32),
        other => return Err(anyhow::anyhow!("unsupported speaker format: {other:?}").into()),
    }
    .map_err(|e| anyhow::anyhow!("build speaker stream: {e}"))?;

    stream.play().map_err(|e| anyhow::anyhow!("speaker stream play: {e}"))?;
    Ok((SendableStream(stream), sample_rate, channels, buf))
}

// ── Mixer + per-track drain ───────────────────────────────────────────────

/// Drain a remote track's `NativeAudioStream` into a per-track ring buffer
/// and emit speaking-state transitions. Runs as one tokio task per
/// subscribed remote audio track. The mixer reads from the buffer.
async fn run_drain_task(
    rtc_track: libwebrtc::audio_track::RtcAudioTrack,
    track_key: String,
    track_buffers: TrackBuffers,
    voice_arc: Arc<tokio::sync::Mutex<VoiceState>>,
    participant_identity: String,
    sample_rate: u32,
) {
    let mut audio_stream = NativeAudioStream::new(rtc_track, sample_rate as i32, 1);

    let mut onset_frames: u32 = 0;
    let mut speak_hold: u32 = 0;
    let mut is_speaking = false;

    eprintln!("[voice] remote drain task started for {track_key}");
    while let Some(frame) = audio_stream.next().await {
        let peak = frame.data.iter().map(|&s| s.abs()).max().unwrap_or(0);

        if peak > 1000 {
            onset_frames += 1;
            if onset_frames >= 2 {
                speak_hold = 12;
            }
        } else {
            onset_frames = 0;
            if speak_hold > 0 {
                speak_hold -= 1;
            }
        }
        let now_speaking = speak_hold > 0;
        if now_speaking != is_speaking {
            is_speaking = now_speaking;
            let voice = voice_arc.lock().await;
            if let Some(ch) = &voice.channel {
                if is_speaking {
                    let _ = ch.send(VoiceEvent::SpeakingStarted {
                        identity: participant_identity.clone(),
                    });
                } else {
                    let _ = ch.send(VoiceEvent::SpeakingStopped {
                        identity: participant_identity.clone(),
                    });
                }
            }
        }

        let mut buffers = track_buffers.lock().unwrap();
        let buf = buffers.entry(track_key.clone()).or_insert_with(VecDeque::new);
        buf.extend(frame.data.iter().map(|&s| s as f32 / 32_768.0));
        while buf.len() > TRACK_BUFFER_CAP_SAMPLES {
            buf.pop_front();
        }
    }
    eprintln!("[voice] remote drain task ended for {track_key}");

    // Stream closed — clean up our buffer slot so the mixer doesn't keep
    // reading a dead entry.
    let mut buffers = track_buffers.lock().unwrap();
    buffers.remove(&track_key);
}

/// Mix every active per-track buffer into a single 10 ms frame, send a copy
/// to APM as the AEC render reference, and push the frame (channel-duplicated)
/// onto the cpal output ring. Runs at 100 Hz for the duration of the voice
/// session; aborted on `leave_voice_channel` or output-device switch.
async fn run_mixer_task(
    track_buffers: TrackBuffers,
    output_ring: Arc<Mutex<VecDeque<f32>>>,
    output_channels: u32,
    output_capacity_samples: usize,
    apm_processor: Option<Arc<ApmProcessor>>,
    apm_frame_samples: usize,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(10));
    // Skip catch-up bursts: under sustained load we'd rather lose 10 ms than
    // process several frames back-to-back and inject a click.
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut mix = vec![0.0f32; apm_frame_samples];

    loop {
        interval.tick().await;

        // Reset mix to silence
        for s in mix.iter_mut() {
            *s = 0.0;
        }

        // Sum available samples from each track. Tracks that don't have
        // a full 10 ms ready contribute partial silence — that's a tiny
        // glitch but fixing it would require waiting, which would
        // back-pressure the mixer.
        {
            let mut buffers = track_buffers.lock().unwrap();
            for buf in buffers.values_mut() {
                let take = buf.len().min(apm_frame_samples);
                for slot in mix.iter_mut().take(take) {
                    if let Some(s) = buf.pop_front() {
                        *slot += s;
                    }
                }
            }
        }

        // Soft-clip in case multiple participants stack onto a near-full
        // sample. Hard clipping past ±1.0 sounds harsh; we just hold here.
        for s in mix.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }

        // AEC render reference: APM analyses the about-to-play signal so its
        // echo subtraction has something to subtract on the next capture.
        if let Some(apm) = &apm_processor {
            let _ = voice_apm::analyze_render(apm, &mix, apm_frame_samples);
        }

        // Push to the cpal output ring. The output stream's callback
        // de-interleaves, so we duplicate mono → output_channels here.
        {
            let mut ring = output_ring.lock().unwrap();
            for s in mix.iter() {
                for _ in 0..output_channels {
                    ring.push_back(*s);
                }
            }
            while ring.len() > output_capacity_samples {
                ring.pop_front();
            }
        }
    }
}

/// Open the speaker, spawn the mixer task. Idempotent: if a stream is
/// already running on `output_device_name`, return the existing
/// `(sample_rate, ring)` without rebuilding. Otherwise tear the old one
/// down first.
async fn ensure_playback(
    voice_arc: Arc<tokio::sync::Mutex<VoiceState>>,
    output_device_name: Option<String>,
    apm_handle: Option<Arc<ApmProcessor>>,
    apm_rate: u32,
    apm_frame_samples: usize,
) -> Result<()> {
    // ── Reuse existing stream if already on the requested device. ─────────
    {
        let voice = voice_arc.lock().await;
        let pb = voice.playback.lock().unwrap();
        if pb.output_stream.is_some()
            && pb.output_device_name.as_deref() == output_device_name.as_deref()
        {
            return Ok(());
        }
    }

    // Tear down any existing pipeline first so device-switch is clean.
    {
        let voice = voice_arc.lock().await;
        let mut pb = voice.playback.lock().unwrap();
        pb.stop_all();
    }

    // Open the new cpal output stream on a blocking thread (ALSA syscalls).
    let output_device_name_for_open = output_device_name.clone();
    let (stream, sample_rate, channels, output_ring) = tokio::task::spawn_blocking(move || {
        let host = cpal::default_host();
        let dev = get_device(&host, output_device_name_for_open.as_deref(), false)?;
        start_speaker_stream(&dev, apm_rate)
    })
    .await
    .map_err(|e| anyhow::anyhow!("speaker init panicked: {e}"))??;

    // If the speaker can't run at the APM rate (very rare; e.g. forced
    // device override), the AEC render reference would be at the wrong
    // rate. Disabling the tap is preferable to feeding APM aliased data.
    let apm_for_mixer = if sample_rate == apm_rate {
        apm_handle
    } else {
        eprintln!(
            "[voice] speaker rate {sample_rate} Hz != APM rate {apm_rate} Hz; \
             AEC render reference disabled for this session"
        );
        None
    };

    let (track_buffers, output_capacity_samples) = {
        let voice = voice_arc.lock().await;
        let pb = voice.playback.lock().unwrap();
        let cap = (sample_rate as usize) * (channels as usize) / 5; // 200 ms
        (Arc::clone(&pb.track_buffers), cap)
    };

    let mixer_task = tokio::spawn(run_mixer_task(
        track_buffers,
        Arc::clone(&output_ring),
        channels,
        output_capacity_samples,
        apm_for_mixer,
        apm_frame_samples,
    ));

    let voice = voice_arc.lock().await;
    let mut pb = voice.playback.lock().unwrap();
    pb.output_stream = Some(stream);
    pb.output_ring = Some(output_ring);
    pb.output_sample_rate = sample_rate;
    pb.output_channels = channels;
    pb.output_device_name = output_device_name;
    pb.mixer_task = Some(mixer_task);
    Ok(())
}

/// Subscribe a newly-arrived remote audio track to the playback pipeline:
/// spawn its drain task, register it for mixing. Output stream + mixer are
/// expected to be already running (set up at join time).
async fn register_remote_track(
    rtc_track: libwebrtc::audio_track::RtcAudioTrack,
    track_key: String,
    voice_arc: Arc<tokio::sync::Mutex<VoiceState>>,
    participant_identity: String,
    apm_rate: u32,
) {
    let track_buffers = {
        let voice = voice_arc.lock().await;
        let pb = voice.playback.lock().unwrap();
        Arc::clone(&pb.track_buffers)
    };

    let task_key = track_key.clone();
    let voice_for_task = Arc::clone(&voice_arc);
    let identity_for_task = participant_identity.clone();
    let task = tokio::spawn(run_drain_task(
        rtc_track.clone(),
        task_key,
        track_buffers,
        voice_for_task,
        identity_for_task,
        apm_rate,
    ));

    let voice = voice_arc.lock().await;
    let mut pb = voice.playback.lock().unwrap();
    if let Some(prev) = pb.drain_tasks.insert(track_key.clone(), task) {
        prev.abort();
    }
    pb.rtc_tracks.insert(track_key.clone(), rtc_track);
    pb.identities.insert(track_key, participant_identity);
}

// ── Device name helpers ───────────────────────────────────────────────────

/// Allowlist for Linux: only keep well-known virtual devices and direct
/// hardware interfaces (hw:CARD=X,DEV=0). Everything else — sysdefault,
/// speex, upmix, vdownmix, front:, iec958:, etc. — is filtered out.
/// On macOS/Windows all devices pass through.
fn is_useful_device(_name: &str) -> bool {
    #[cfg(not(target_os = "linux"))]
    return true;
    #[cfg(target_os = "linux")]
    {
        matches!(_name, "default" | "pulse" | "pipewire" | "jack")
            || (_name.starts_with("hw:") && _name.contains("DEV=0"))
    }
}

/// Returns a human-readable label.
/// "hw:CARD=QuadCast,DEV=0" → "QuadCast"
/// "pipewire" → "PipeWire"
fn display_name(raw: &str) -> String {
    // On Linux, extract the card name from hw:CARD=X,DEV=0
    #[cfg(target_os = "linux")]
    if raw.starts_with("hw:") {
        if let Some(card_part) = raw.split(',').next() {
            if let Some((_, card_name)) = card_part.split_once("CARD=") {
                return card_name.to_string();
            }
        }
    }
    match raw {
        "default" => "System Default".to_string(),
        "pulse" => "PulseAudio".to_string(),
        "pipewire" => "PipeWire".to_string(),
        "jack" => "JACK".to_string(),
        other => other.to_string(),
    }
}

// ── Tauri commands ────────────────────────────────────────────────────────

/// Register the Tauri Channel used to push VoiceEvents to the frontend.
/// Call once on app startup, just like subscribe_realtime.
#[tauri::command]
pub async fn subscribe_voice_events(
    on_event: tauri::ipc::Channel<VoiceEvent>,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let mut voice = state.voice.lock().await;
    voice.channel = Some(on_event);
    Ok(())
}

/// Return all available audio input and output devices.
/// Device enumeration makes blocking ALSA syscalls (and produces ALSA warning
/// spam); run it on a blocking thread to avoid stalling the tokio runtime.
#[tauri::command]
pub async fn list_audio_devices() -> Result<Vec<AudioDevice>> {
    tokio::task::spawn_blocking(|| {
        let host = cpal::default_host();
        let mut devices = Vec::new();

        if let Ok(inputs) = host.input_devices() {
            for d in inputs {
                if let Ok(name) = d.name() {
                    if is_useful_device(&name) {
                        devices.push(AudioDevice { id: name.clone(), name: display_name(&name), kind: "input".into() });
                    }
                }
            }
        }
        if let Ok(outputs) = host.output_devices() {
            for d in outputs {
                if let Ok(name) = d.name() {
                    if is_useful_device(&name) {
                        devices.push(AudioDevice { id: name.clone(), name: display_name(&name), kind: "output".into() });
                    }
                }
            }
        }
        devices
    })
    .await
    .map_err(|e| anyhow::anyhow!("device enumeration panicked: {e}").into())
}

/// "Hot mic" warmup: signals user intent to (maybe) join `channel_id` so the
/// client can pre-pay the latency before they actually click Join. Mirrors
/// `room.prepareConnection(url, token)` from the JS livekit-client SDK,
/// which has no equivalent in the `livekit` Rust crate (v0.7) — the Rust
/// `Room::connect` is the only entry point and it commits to the room.
///
/// What we do instead:
///  1. Mint and cache the LiveKit token so the synchronous JWT work is
///     already done by the time the user clicks Join.
///  2. Fire a one-shot HTTPS request to the LiveKit server's RoomService
///     endpoint (`twirp_base/.../ListParticipants`). This warms:
///       - DNS for the LiveKit host in the OS resolver cache,
///       - the TLS session ticket cache used by `rustls` (rustls keeps a
///         per-process cache that the LiveKit WS handshake can reuse),
///       - reqwest's HTTPS connection pool, used for the same host's API.
///     The body of the response is irrelevant; we want the network plumbing
///     to be primed.
///
/// Idempotent + cancel-safe: a second call with the same channel_id while
/// the first warmup is still running is a no-op. Calling with a different
/// channel_id supersedes the pending one (its DNS/TLS work is wasted, but
/// it's a background task — no user impact).
///
/// Frontend should call this on intent points: route entry to a voice
/// channel page, hover/keyboard-select on a voice item in TerminalMenu /
/// SearchPanel, etc. Cheap enough to call eagerly.
#[tauri::command]
pub async fn prepare_voice_connection(
    channel_id: String,
    user_id: String,
    display_name: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let url = state.config.livekit_url.clone();
    if url.is_empty() {
        // No LiveKit configured — nothing to warm. Silent success so the
        // frontend can call this unconditionally.
        return Ok(());
    }

    // ── De-dupe: skip if we already have a fresh warmup for this exact
    // channel + identity. Cheap to mint a token, but firing the HTTPS
    // request again would be wasted work.
    {
        let voice = state.voice.lock().await;
        if let Some(w) = &voice.warmup {
            if w.channel_id == channel_id
                && w.user_id == user_id
                && w.display_name == display_name
                && w.created_at.elapsed() < VOICE_WARMUP_TTL
            {
                return Ok(());
            }
        }
    }

    let token = make_token(
        &state.config,
        &channel_id,
        &format!("voice-{user_id}"),
        &display_name,
    )?;

    // Fire the DNS/TLS warmup in the background. If the user immediately
    // clicks Join, they'll race this — that's fine, the worst case is a
    // single redundant TLS handshake. Errors are non-fatal: this whole
    // command is best-effort.
    let warm_url = url.clone();
    let task = tokio::spawn(async move {
        // Reuse the same twirp transform `livekit::room_service_list_participants`
        // uses; we don't import it to keep the dependency direction clean.
        let twirp = if let Some(rest) = warm_url.strip_prefix("wss://") {
            format!("https://{rest}")
        } else if let Some(rest) = warm_url.strip_prefix("ws://") {
            format!("http://{rest}")
        } else {
            warm_url.clone()
        };
        let probe = format!("{twirp}/rtc/validate");
        // Short timeout — if the server is slow there's nothing to gain by
        // hanging on. The handshake is what we care about, not the response.
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        let started = Instant::now();
        match client.get(&probe).send().await {
            Ok(_resp) => {
                eprintln!(
                    "[voice] warmup probe to {twirp} completed in {:.0}ms",
                    started.elapsed().as_secs_f64() * 1000.0
                );
            }
            Err(e) => {
                eprintln!("[voice] warmup probe failed (non-fatal): {e}");
            }
        }
    });

    // Stash the warm credentials. Replacing any prior entry triggers Drop
    // on the previous warmup, which aborts its still-running background task
    // so a fast hover-flip doesn't pile up redundant probes.
    let mut voice = state.voice.lock().await;
    voice.warmup = Some(VoiceWarmup {
        channel_id,
        token,
        created_at: Instant::now(),
        user_id,
        display_name,
        task: Some(task),
    });

    Ok(())
}

/// Connect to a LiveKit voice room, publish the local microphone, and start
/// the remote-playback pipeline (single shared mixer + cpal output stream).
///
/// `audio_processing` carries the user's APM preferences (AGC, NS, AEC).
/// libwebrtc's internal AudioProcessingModule is disabled at the
/// `AudioSourceOptions` level so we don't double-process: APM is the only
/// stage that touches the mic signal between cpal and `capture_frame`.
///
/// Performance: the LiveKit network handshake (DNS, TLS, WS upgrade) and
/// the cpal mic init (ALSA enumeration + device open) are independent and
/// both block for hundreds of milliseconds on cold starts. We run them
/// concurrently with `tokio::join!` so total join latency is ~max(net, mic)
/// rather than net+mic. If `prepare_voice_connection` was called for this
/// channel, DNS/TLS is already warm and we reuse the precomputed token.
#[tauri::command]
pub async fn join_voice_channel(
    channel_id: String,
    user_id: String,
    display_name: String,
    input_device: Option<String>,
    output_device: Option<String>,
    audio_processing: voice_apm::ApmConfig,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    // Wall-clock anchor for `total_join_ms` and per-phase deltas.
    let join_start = Instant::now();
    let join_started_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    let url = state.config.livekit_url.clone();
    if url.is_empty() {
        return Err(anyhow::anyhow!("LiveKit is not configured on this server").into());
    }

    // Try to consume a fresh warmup for this exact channel + identity. If
    // the warmup is stale or for a different room/user we mint a new token.
    // VoiceWarmup implements Drop (to abort its background task) so we can't
    // simply destructure it; clone the token out before dropping the entry.
    let cached_token: Option<String> = {
        let mut voice = state.voice.lock().await;
        let usable = voice
            .warmup
            .as_ref()
            .map(|w| {
                w.channel_id == channel_id
                    && w.user_id == user_id
                    && w.display_name == display_name
                    && w.created_at.elapsed() < VOICE_WARMUP_TTL
            })
            .unwrap_or(false);
        if usable {
            let cloned = voice.warmup.as_ref().map(|w| w.token.clone());
            // Drop the entry now — its background task is no longer needed.
            voice.warmup = None;
            cloned
        } else {
            // Anything else cached (different channel / stale) is dead weight.
            voice.warmup = None;
            None
        }
    };
    // ── Phase: jwt_mint ────────────────────────────────────────────────────
    // 0 if a warmup-cached token was usable.
    let jwt_mint_ms;
    let token: String = match cached_token {
        Some(t) => {
            jwt_mint_ms = 0;
            t
        }
        None => {
            let jwt_start = Instant::now();
            let t = make_token(
                &state.config,
                &channel_id,
                &format!("voice-{user_id}"),
                &display_name,
            )?;
            jwt_mint_ms = jwt_start.elapsed().as_millis() as u64;
            t
        }
    };

    // ── Run room connect and mic init concurrently ─────────────────────────
    // Both are independent and individually expensive on cold starts. Running
    // them with `tokio::join!` cuts the user-visible delay to ~max(net, mic).
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<i16>, u32)>();
    let is_muted = {
        let voice = state.voice.lock().await;
        voice.is_muted.store(false, Ordering::Relaxed);
        Arc::clone(&voice.is_muted)
    };

    let input_device_clone = input_device.clone();

    let connect_started = Instant::now();
    let mic_started = Instant::now();
    eprintln!("[voice] connecting to room {channel_id} and opening mic in parallel…");

    let connect_fut = async {
        let r = Room::connect(&url, &token, RoomOptions::default()).await;
        let elapsed_ms = connect_started.elapsed().as_millis() as u64;
        (r, elapsed_ms)
    };
    let mic_fut = async {
        tokio::task::spawn_blocking(move || {
            let host = cpal::default_host();
            let dev = get_device(&host, input_device_clone.as_deref(), true)?;
            eprintln!("[voice] mic device: {:?}", dev.name());
            let r = start_mic_stream(&dev, frame_tx, is_muted);
            let elapsed_ms = mic_started.elapsed().as_millis() as u64;
            r.map(|(stream, rate)| (stream, rate, elapsed_ms))
        })
        .await
    };

    let (connect_pair, mic_res) = tokio::join!(connect_fut, mic_fut);
    let (connect_res, room_connect_ms) = connect_pair;
    let (room, mut events) =
        connect_res.map_err(|e| anyhow::anyhow!("LiveKit connect: {e}"))?;
    let (mic_stream, mic_rate, mic_init_ms) = mic_res
        .map_err(|e| anyhow::anyhow!("mic init panicked: {e}"))??;
    eprintln!("[voice] connected to room {channel_id}, mic at {mic_rate} Hz");

    let room = Arc::new(room);

    // ── Build APM at the mic's actual rate ────────────────────────────────
    // WebRTC supports 8/16/32/48 kHz. Anything else (e.g. legacy 44.1) means
    // we can't run APM for this session; we log and proceed without it.
    let apm_stage = match voice_apm::ApmStage::new(mic_rate, audio_processing.clone()) {
        Ok(stage) => Some(stage),
        Err(e) => {
            eprintln!("[voice] APM disabled: {e}");
            None
        }
    };
    let apm_handle = apm_stage.as_ref().map(|s| s.handle());
    let apm_frame_samples = voice_apm::frame_samples(mic_rate);

    // Build the RNNoise denoiser if the user wants click suppression and
    // the mic is at the rate the model was trained on. Other rates pass
    // through to APM unchanged — RNNoise is rate-locked at 48 kHz.
    let denoiser_arc = Arc::clone(&{
        let voice = state.voice.lock().await;
        Arc::clone(&voice.denoiser)
    });
    {
        let mut slot = denoiser_arc.lock().unwrap();
        *slot = if audio_processing.click_suppression && mic_rate == voice_denoiser::REQUIRED_RATE_HZ {
            eprintln!("[voice/rnnoise] engaged @ {mic_rate} Hz");
            Some(voice_denoiser::DenoiserStage::new())
        } else {
            if audio_processing.click_suppression {
                eprintln!(
                    "[voice/rnnoise] requested but mic rate is {mic_rate} Hz; \
                     RNNoise needs 48000 Hz — disabling for this session"
                );
            }
            None
        };
    }

    // Disable libwebrtc's internal AudioProcessingModule — APM is the only
    // stage that touches the mic signal. Leaving libwebrtc's APM on would
    // double-process and produce pumping/swirling artefacts.
    let audio_source = NativeAudioSource::new(
        AudioSourceOptions {
            echo_cancellation: false,
            noise_suppression: false,
            auto_gain_control: false,
        },
        mic_rate,
        1,
        100,
    );

    let local_track = LocalAudioTrack::create_audio_track(
        "microphone",
        RtcAudioSource::Native(audio_source.clone()),
    );

    eprintln!("[voice] publishing track…");
    // ── Phase: first_publish ───────────────────────────────────────────────
    let publish_start = Instant::now();
    room.local_participant()
        .publish_track(
            LocalTrack::Audio(local_track.clone()),
            TrackPublishOptions::default(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("publish track: {e}"))?;
    let first_publish_ms = publish_start.elapsed().as_millis() as u64;
    eprintln!("[voice] track published");

    // ── Mic frame task: rebuffer to exact 10ms, run APM, capture_frame ────
    // Speaking detection runs on the post-APM peak so the indicator follows
    // the user's effective level (after AGC + NS) rather than raw input.
    let audio_source_task = audio_source.clone();
    let voice_arc_frame = Arc::clone(&state.voice);
    let local_identity = format!("voice-{user_id}");
    let apm_for_capture = apm_handle.clone();
    let denoiser_for_capture = Arc::clone(&denoiser_arc);
    let frame_task = tokio::spawn(async move {
        let chunk_size = (mic_rate / 100) as usize;
        let mut buf: Vec<i16> = Vec::new();
        let mut speak_hold: u32 = 0; // counts down after speech stops (12 × 10ms = 120ms hold)
        let mut onset_frames: u32 = 0; // consecutive above-threshold frames; 2 required to (re)trigger
        let mut is_speaking = false;

        while let Some((samples, rate)) = frame_rx.recv().await {
            buf.extend_from_slice(&samples);
            while buf.len() >= chunk_size {
                let mut chunk: Vec<i16> = buf.drain(..chunk_size).collect();

                // RNNoise (if enabled) runs first so APM gets a cleaner signal:
                // its NS / AGC adapt to actual voice energy instead of the
                // typing/click noise floor. Std Mutex; the lock is held for
                // ~0.1 ms per frame (single-writer) and never crosses an await.
                {
                    let mut guard = denoiser_for_capture.lock().unwrap();
                    if let Some(d) = guard.as_mut() {
                        d.process(&mut chunk);
                    }
                }

                // APM mutates the chunk in place (AGC + NS + HPF + AEC capture).
                if let Some(apm) = &apm_for_capture {
                    if let Err(e) = voice_apm::run_capture(apm, &mut chunk, chunk_size) {
                        eprintln!("[voice] APM capture error (frame dropped): {e}");
                        continue;
                    }
                }

                let peak = chunk.iter().map(|&s| s.abs()).max().unwrap_or(0);

                // Speaking detection: require 2 consecutive above-threshold frames to trigger,
                // preventing single trailing spikes from resetting the hold counter.
                if peak > 1000 {
                    onset_frames += 1;
                    if onset_frames >= 2 {
                        speak_hold = 12;
                    }
                } else {
                    onset_frames = 0;
                    if speak_hold > 0 {
                        speak_hold -= 1;
                    }
                }
                let now_speaking = speak_hold > 0;
                if now_speaking != is_speaking {
                    is_speaking = now_speaking;
                    let voice = voice_arc_frame.lock().await;
                    if let Some(ch) = &voice.channel {
                        if is_speaking {
                            let _ = ch.send(VoiceEvent::SpeakingStarted { identity: local_identity.clone() });
                        } else {
                            let _ = ch.send(VoiceEvent::SpeakingStopped { identity: local_identity.clone() });
                        }
                    }
                }

                let frame = AudioFrame {
                    data: chunk.into(),
                    sample_rate: rate,
                    num_channels: 1,
                    samples_per_channel: chunk_size as u32,
                };
                if let Err(e) = audio_source_task.capture_frame(&frame).await {
                    eprintln!("[voice] capture_frame error: {e:?}");
                }
            }
        }
    });

    // ── Open the speaker pipeline: single output stream + mixer task ──────
    // Doing this BEFORE the room event loop starts means the very first
    // TrackSubscribed has somewhere to push its decoded frames.
    if let Err(e) = ensure_playback(
        Arc::clone(&state.voice),
        output_device.clone(),
        apm_handle.clone(),
        mic_rate,
        apm_frame_samples,
    )
    .await
    {
        eprintln!("[voice] playback init failed (speaker disabled this session): {e}");
    }

    // ── Seed participant list ─────────────────────────────────────────────────
    // Emit ParticipantJoined for participants already in the room.
    // Do NOT attach tracks here — TrackSubscribed fires for pre-existing
    // subscribed tracks once the event loop drains buffered events, and
    // attaching twice creates competing draining tasks.
    {
        let voice = state.voice.lock().await;
        if let Some(ch) = &voice.channel {
            let _ = ch.send(VoiceEvent::ParticipantJoined {
                identity: format!("voice-{user_id}"),
                name: display_name.clone(),
                is_muted: false,
            });
            for (_identity, participant) in room.remote_participants() {
                eprintln!("[voice] existing participant: {}", participant.identity());
                let _ = ch.send(VoiceEvent::ParticipantJoined {
                    identity: participant.identity().to_string(),
                    name: participant.name(),
                    is_muted: false,
                });
            }
        }
    }

    let voice_arc = Arc::clone(&state.voice);
    let apm_rate_for_room = mic_rate;
    let room_task = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                RoomEvent::ParticipantConnected(p) => {
                    eprintln!("[voice] participant joined: {}", p.identity());
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::ParticipantJoined {
                            identity: p.identity().to_string(),
                            name: p.name(),
                            is_muted: false,
                        });
                    }
                }
                RoomEvent::ParticipantDisconnected(p) => {
                    eprintln!("[voice] participant left: {}", p.identity());
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::ParticipantLeft {
                            identity: p.identity().to_string(),
                        });
                    }
                }
                RoomEvent::TrackSubscribed { track, publication: _, participant } => {
                    if let RemoteTrack::Audio(audio_track) = track {
                        let track_key = format!("{}-{}", participant.identity(), audio_track.sid());
                        eprintln!("[voice] track subscribed: {track_key}");
                        register_remote_track(
                            audio_track.rtc_track(),
                            track_key,
                            Arc::clone(&voice_arc),
                            participant.identity().to_string(),
                            apm_rate_for_room,
                        )
                        .await;
                    }
                }
                RoomEvent::TrackUnsubscribed { track, publication: _, participant } => {
                    if let RemoteTrack::Audio(audio_track) = track {
                        let track_key = format!("{}-{}", participant.identity(), audio_track.sid());
                        eprintln!("[voice] track unsubscribed: {track_key}");
                        let voice = voice_arc.lock().await;
                        let mut pb = voice.playback.lock().unwrap();
                        if let Some(t) = pb.drain_tasks.remove(&track_key) { t.abort(); }
                        pb.rtc_tracks.remove(&track_key);
                        pb.identities.remove(&track_key);
                        let buffers_arc = Arc::clone(&pb.track_buffers);
                        drop(pb);
                        drop(voice);
                        buffers_arc.lock().unwrap().remove(&track_key);
                    }
                }

                RoomEvent::Disconnected { reason } => {
                    eprintln!("[voice] disconnected: {reason:?}");
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::Disconnected);
                    }
                    break;
                }
                RoomEvent::ConnectionStateChanged(conn_state) => {
                    eprintln!("[voice] connection state: {conn_state:?}");
                }
                _ => {}
            }
        }
    });

    // ── Phase: total_join + record timings ─────────────────────────────────
    let total_join_ms = join_start.elapsed().as_millis() as u64;

    let timings = JoinTimings {
        channel_id: channel_id.clone(),
        jwt_mint_ms,
        room_connect_ms,
        mic_init_ms,
        first_publish_ms,
        total_join_ms,
        join_started_at_ms,
    };
    eprintln!(
        "[voice/timings] channel={} jwt={}ms connect={}ms mic={}ms publish={}ms total={}ms",
        channel_id,
        jwt_mint_ms,
        room_connect_ms,
        mic_init_ms,
        first_publish_ms,
        total_join_ms,
    );

    // ── Store state ───────────────────────────────────────────────────────────
    let mut voice = state.voice.lock().await;
    if let Some(t) = voice.room_task.take() { t.abort(); }
    if let Some(t) = voice.frame_task.take() { t.abort(); }
    voice.room = Some(room);
    voice.local_track = Some(local_track);
    voice.audio_source = Some(audio_source);
    voice.input_stream = Some(mic_stream);
    voice.frame_task = Some(frame_task);
    voice.room_task = Some(room_task);
    voice.current_input_device = input_device;
    voice.apm = apm_stage;
    *voice.last_join_timings.lock().unwrap() = Some(timings);

    Ok(())
}

/// Return the most recent `join_voice_channel` timing record. The frontend
/// calls this immediately after a successful join and dumps the values into
/// the dev console for analysis. Returns `None` if no join has completed
/// since process start.
#[tauri::command]
pub async fn get_last_join_timings(
    state: State<'_, Arc<AppState>>,
) -> Result<Option<JoinTimings>> {
    let voice = state.voice.lock().await;
    let snapshot = voice.last_join_timings.lock().unwrap().clone();
    Ok(snapshot)
}

/// Disconnect from the current voice room and release all audio resources.
#[tauri::command]
pub async fn leave_voice_channel(state: State<'_, Arc<AppState>>) -> Result<()> {
    // Extract everything that needs cleanup while holding the lock, then release
    // the lock before awaiting room.close(). If the network is broken (e.g. VPN
    // dropped), room.close() hangs sending a disconnect signal — holding the lock
    // during that await deadlocks every subsequent command that needs voice state.
    let room = {
        let mut voice = state.voice.lock().await;

        if let Some(t) = voice.room_task.take() { t.abort(); }
        if let Some(t) = voice.frame_task.take() { t.abort(); }

        {
            let mut pb = voice.playback.lock().unwrap();
            pb.stop_all();
            pb.rtc_tracks.clear();
            pb.identities.clear();
            pb.output_device_name = None;
        }

        voice.local_track = None;
        voice.audio_source = None;
        voice.input_stream = None;
        voice.apm = None;
        if let Ok(mut slot) = voice.denoiser.lock() {
            *slot = None;
        }
        voice.is_muted.store(false, Ordering::Relaxed);
        voice.current_input_device = None;

        voice.room.take()
    }; // voice lock released here

    // Close outside the lock with a timeout so a broken connection (dropped VPN,
    // network change) can't stall a reconnect attempt indefinitely.
    if let Some(room) = room {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            room.close(),
        ).await;
    }

    Ok(())
}

/// Toggle the local microphone mute. Returns the new muted state (true = muted).
/// Also signals the muted state to remote participants via the LiveKit publication.
#[tauri::command]
pub async fn toggle_voice_mute(state: State<'_, Arc<AppState>>) -> Result<bool> {
    let voice = state.voice.lock().await;
    let new_muted = !voice.is_muted.load(Ordering::Relaxed);
    voice.is_muted.store(new_muted, Ordering::Relaxed);

    // Hint APM that the output is muted so its AGC/AEC stop adapting to
    // silence frames during the mute window.
    if let Some(apm) = &voice.apm {
        apm.handle().set_output_will_be_muted(new_muted);
    }

    // Signal to remote participants via the LiveKit publication
    if let Some(room) = &voice.room {
        let pubs = room.local_participant().track_publications();
        for (_, pub_) in pubs {
            if pub_.kind() == TrackKind::Audio {
                if new_muted {
                    pub_.mute();
                } else {
                    pub_.unmute();
                }
            }
        }
    }

    Ok(new_muted)
}

/// Switch the microphone device mid-call. Stops the current input stream and
/// restarts it on the new device. Rebuilds APM if the new device's sample
/// rate differs from the current one — APM is rate-locked at construction.
#[tauri::command]
pub async fn set_voice_input_device(
    device_name: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let mut voice = state.voice.lock().await;

    // Only valid while in a call
    if voice.audio_source.is_none() {
        voice.current_input_device = Some(device_name);
        return Ok(());
    }

    // Extract shared atomics + current APM rate / config before dropping the
    // lock — cpal init makes blocking ALSA syscalls and must not hold the
    // async mutex.
    let is_muted_clone = Arc::clone(&voice.is_muted);
    let prev_apm_rate = voice.apm.as_ref().map(|a| a.sample_rate_hz());
    let prev_apm_config = voice.apm.as_ref().map(|a| a.config().clone());
    drop(voice);

    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<i16>, u32)>();
    let device_name_clone = device_name.clone();
    let (new_mic, new_rate) = tokio::task::spawn_blocking(move || {
        let host = cpal::default_host();
        let device = get_device(&host, Some(&device_name_clone), true)?;
        start_mic_stream(&device, frame_tx, is_muted_clone)
    })
    .await
    .map_err(|e| anyhow::anyhow!("audio init panicked: {e}"))??;

    // Rebuild APM if the new mic rate differs from the previous one.
    let rate_changed = prev_apm_rate.map(|r| r != new_rate).unwrap_or(true);
    let new_apm_stage = if rate_changed {
        match (
            prev_apm_config.clone(),
            voice_apm::ApmStage::new(
                new_rate,
                prev_apm_config.unwrap_or_default(),
            ),
        ) {
            (_, Ok(s)) => Some(s),
            (_, Err(e)) => {
                eprintln!("[voice] APM rebuild on mic switch failed: {e}");
                None
            }
        }
    } else {
        None
    };

    let mut voice = state.voice.lock().await;

    // Swap the input stream — dropping the old one stops it
    voice.input_stream = Some(new_mic);
    voice.current_input_device = Some(device_name);
    if rate_changed {
        voice.apm = new_apm_stage;
    }

    // Reset the RNNoise state on every device switch — RNN hidden state is
    // tied to the previous mic's spectrum and isn't useful afterwards.
    // Re-engage it iff the new mic is at the right rate and the user still
    // has click suppression enabled.
    let want_denoiser = voice
        .apm
        .as_ref()
        .map(|a| a.config().click_suppression)
        .unwrap_or(false);
    {
        let mut slot = voice.denoiser.lock().unwrap();
        *slot = if want_denoiser && new_rate == voice_denoiser::REQUIRED_RATE_HZ {
            Some(voice_denoiser::DenoiserStage::new())
        } else {
            None
        };
    }

    // Abort the old frame-feed task and start a new one on the new channel.
    if let Some(t) = voice.frame_task.take() { t.abort(); }
    let source = voice.audio_source.clone().unwrap();
    let apm_for_capture = voice.apm.as_ref().map(|a| a.handle());
    let denoiser_for_capture = Arc::clone(&voice.denoiser);
    let task = tokio::spawn(async move {
        let mut buf: Vec<i16> = Vec::new();
        while let Some((samples, rate)) = frame_rx.recv().await {
            buf.extend_from_slice(&samples);
            let chunk_size = (rate / 100) as usize;
            while buf.len() >= chunk_size {
                let mut chunk: Vec<i16> = buf.drain(..chunk_size).collect();
                {
                    let mut guard = denoiser_for_capture.lock().unwrap();
                    if let Some(d) = guard.as_mut() {
                        d.process(&mut chunk);
                    }
                }
                if let Some(apm) = &apm_for_capture {
                    if let Err(e) = voice_apm::run_capture(apm, &mut chunk, chunk_size) {
                        eprintln!("[voice] APM capture error (frame dropped): {e}");
                        continue;
                    }
                }
                let frame = AudioFrame {
                    data: chunk.into(),
                    sample_rate: rate,
                    num_channels: 1,
                    samples_per_channel: chunk_size as u32,
                };
                let _ = source.capture_frame(&frame).await;
            }
        }
    });
    voice.frame_task = Some(task);

    Ok(())
}

/// Switch the speaker device mid-call. Tears down the current cpal output
/// stream + mixer task and rebuilds them on the new device. Per-track drain
/// tasks keep running — `track_buffers` is preserved across the switch, so
/// the new mixer picks up where the old one left off without re-subscribing.
#[tauri::command]
pub async fn set_voice_output_device(
    device_name: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let voice_arc = Arc::clone(&state.voice);

    let (apm_handle, apm_rate, apm_frame_samples) = {
        let voice = voice_arc.lock().await;
        let (handle, rate, samples) = match &voice.apm {
            Some(stage) => (
                Some(stage.handle()),
                stage.sample_rate_hz(),
                stage.frame_samples(),
            ),
            None => (None, voice_apm::DEFAULT_APM_RATE_HZ, voice_apm::frame_samples(voice_apm::DEFAULT_APM_RATE_HZ)),
        };
        (handle, rate, samples)
    };

    ensure_playback(
        voice_arc,
        Some(device_name),
        apm_handle,
        apm_rate,
        apm_frame_samples,
    )
    .await
}

/// Update the live APM configuration without rejoining. Internal AEC / AGC /
/// NS state is preserved across config changes — only the changed submodule
/// re-initialises. The RNNoise denoiser is created or dropped to match the
/// new `click_suppression` flag (its RNN state isn't worth preserving across
/// toggles). No-op when no voice session is active.
#[tauri::command]
pub async fn set_voice_audio_processing(
    config: voice_apm::ApmConfig,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let mut voice = state.voice.lock().await;

    // Mic rate is fixed at session start; reuse it for the denoiser-rate check.
    let mic_rate = voice.apm.as_ref().map(|a| a.sample_rate_hz());

    if let Some(stage) = voice.apm.as_mut() {
        stage.set_config(config.clone());
    }

    // Reconcile the RNNoise slot with the new flag.
    if let Some(rate) = mic_rate {
        let mut slot = voice.denoiser.lock().unwrap();
        let want = config.click_suppression && rate == voice_denoiser::REQUIRED_RATE_HZ;
        match (want, slot.is_some()) {
            (true, false) => {
                eprintln!("[voice/rnnoise] enabled mid-call");
                *slot = Some(voice_denoiser::DenoiserStage::new());
            }
            (false, true) => {
                eprintln!("[voice/rnnoise] disabled mid-call");
                *slot = None;
            }
            _ => { /* already in the desired state */ }
        }
    }

    Ok(())
}
