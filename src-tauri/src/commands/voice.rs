use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        Arc, Mutex,
    },
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

use crate::{commands::livekit::make_token, error::Result, state::AppState};

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

// ── Playback state shared between VoiceState and the room event task ─────

pub struct PlaybackState {
    // cpal output streams keyed by track key ("identity-sid")
    pub streams: HashMap<String, SendableStream>,
    // tasks draining NativeAudioStream → ring buffer
    pub tasks: HashMap<String, tokio::task::JoinHandle<()>>,
    // raw RtcAudioTrack refs kept so output device switching can rebuild streams
    pub rtc_tracks: HashMap<String, libwebrtc::audio_track::RtcAudioTrack>,
    // participant identity keyed by track key — needed to re-attach on device switch
    pub identities: HashMap<String, String>,
    pub output_device_name: Option<String>,
}

impl PlaybackState {
    fn new() -> Self {
        Self {
            streams: HashMap::new(),
            tasks: HashMap::new(),
            rtc_tracks: HashMap::new(),
            identities: HashMap::new(),
            output_device_name: None,
        }
    }

    /// Stop and drop all active playback for every remote track.
    fn stop_all(&mut self) {
        for (_, task) in self.tasks.drain() {
            task.abort();
        }
        self.streams.clear();
    }
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
    /// Noise gate threshold stored as f32 bits. 0.0 = off. Persists across calls.
    pub noise_floor: Arc<AtomicU32>,
    pub current_input_device: Option<String>,
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
            noise_floor: Arc::new(AtomicU32::new(0u32)),
            current_input_device: None,
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────

pub(crate) fn get_device(host: &cpal::Host, name: Option<&str>, is_input: bool) -> Result<cpal::Device> {
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
    noise_floor: Arc<AtomicU32>,
) -> Result<(SendableStream, u32)> {
    let config = device
        .default_input_config()
        .map_err(|e| anyhow::anyhow!("input config: {e}"))?;
    let channels = config.channels() as usize;
    let sample_format = config.sample_format();
    // Force 48000 Hz: WebRTC APM only supports 8/16/32/48 kHz.
    // PipeWire (and most modern audio servers) resample transparently.
    let sample_rate: u32 = 48000;
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
            let noise_floor = Arc::clone(&noise_floor);
            device.build_input_stream::<$T, _, _>(
                &stream_config,
                move |data: &[$T], _| {
                    if is_muted.load(Ordering::Relaxed) {
                        return;
                    }
                    let threshold = f32::from_bits(noise_floor.load(Ordering::Relaxed));
                    let f32s: Vec<f32> = data.iter().copied().map($to_f32).collect();
                    if threshold > 0.0 {
                        let peak = f32s.iter().map(|s| s.abs()).fold(0.0f32, f32::max);
                        if peak < threshold {
                            return;
                        }
                    }
                    let mono: Vec<i16> = if channels == 1 {
                        f32s.iter()
                            .map(|&s| (s * 32767.0).clamp(-32768.0, 32767.0) as i16)
                            .collect()
                    } else {
                        f32s.chunks(channels)
                            .map(|ch| {
                                let avg = ch.iter().sum::<f32>() / channels as f32;
                                (avg * 32767.0).clamp(-32768.0, 32767.0) as i16
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
pub(crate) fn start_speaker_stream(
    device: &cpal::Device,
) -> Result<(SendableStream, u32, u32, Arc<Mutex<VecDeque<f32>>>)> {
    let config = device
        .default_output_config()
        .map_err(|e| anyhow::anyhow!("output config: {e}"))?;
    let channels = config.channels() as u32;
    let sample_format = config.sample_format();
    // Force 48000 Hz to match the NativeAudioSource encoding rate.
    let sample_rate: u32 = 48000;
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

/// Attach a NativeAudioStream to a speaker output device. Async: blocking ALSA
/// device setup runs in spawn_blocking, then a tokio task drains frames into
/// the ring buffer. Call via tokio::spawn so the room event loop isn't blocked.
async fn attach_remote_track(
    output_device_name: Option<String>,
    rtc_track: libwebrtc::audio_track::RtcAudioTrack,
    playback: Arc<Mutex<PlaybackState>>,
    track_key: String,
    voice_arc: Arc<tokio::sync::Mutex<VoiceState>>,
    participant_identity: String,
) {
    // Build the cpal output stream on a blocking thread (ALSA syscalls).
    let result = tokio::task::spawn_blocking(move || {
        let host = cpal::default_host();
        let output_dev = get_device(&host, output_device_name.as_deref(), false)?;
        start_speaker_stream(&output_dev)
    })
    .await;

    let (stream, sample_rate, channels, buf) = match result {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            eprintln!("[voice] speaker stream error for {track_key}: {e}");
            return;
        }
        Err(e) => {
            eprintln!("[voice] speaker stream panicked for {track_key}: {e}");
            return;
        }
    };

    let mut audio_stream =
        NativeAudioStream::new(rtc_track.clone(), sample_rate as i32, channels as i32);

    // Cap the ring buffer at 200ms to keep audio fresh and latency low.
    let max_buf = (sample_rate * channels / 5) as usize;
    let task_key = track_key.clone();
    let voice_arc_task = voice_arc.clone();
    let identity_task = participant_identity.clone();
    let task = tokio::spawn(async move {
        eprintln!("[voice] remote drain task started for {task_key}");
        let mut onset_frames: u32 = 0;
        let mut speak_hold: u32 = 0;
        let mut is_speaking = false;
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
                let voice = voice_arc_task.lock().await;
                if let Some(ch) = &voice.channel {
                    if is_speaking {
                        let _ = ch.send(VoiceEvent::SpeakingStarted { identity: identity_task.clone() });
                    } else {
                        let _ = ch.send(VoiceEvent::SpeakingStopped { identity: identity_task.clone() });
                    }
                }
            }

            let samples: Vec<f32> =
                frame.data.iter().map(|&s| s as f32 / 32768.0).collect();
            let mut b = buf.lock().unwrap();
            b.extend(samples);
            while b.len() > max_buf {
                b.pop_front();
            }
        }
        eprintln!("[voice] remote drain task ended for {task_key}");
    });

    let mut pb = playback.lock().unwrap();
    pb.streams.insert(track_key.clone(), stream);
    pb.tasks.insert(track_key.clone(), task);
    pb.rtc_tracks.insert(track_key.clone(), rtc_track);
    pb.identities.insert(track_key, participant_identity);
}

// ── Device name helpers ───────────────────────────────────────────────────

/// Allowlist for Linux: only keep well-known virtual devices and direct
/// hardware interfaces (hw:CARD=X,DEV=0). Everything else — sysdefault,
/// speex, upmix, vdownmix, front:, iec958:, etc. — is filtered out.
/// On macOS/Windows all devices pass through.
fn is_useful_device(name: &str) -> bool {
    #[cfg(not(target_os = "linux"))]
    return true;
    #[cfg(target_os = "linux")]
    {
        matches!(name, "default" | "pulse" | "pipewire" | "jack")
            || (name.starts_with("hw:") && name.contains("DEV=0"))
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

/// Connect to a LiveKit voice room and publish the local microphone.
/// Step 2: mic input added. Playback of remote tracks comes next.
#[tauri::command]
pub async fn join_voice_channel(
    channel_id: String,
    user_id: String,
    display_name: String,
    input_device: Option<String>,
    _output_device: Option<String>,
    auto_gain_control: bool,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let url = state.config.livekit_url.clone();
    if url.is_empty() {
        return Err(anyhow::anyhow!("LiveKit is not configured on this server").into());
    }

    let token = make_token(
        &state.config,
        &channel_id,
        &format!("voice-{user_id}"),
        &display_name,
    )?;

    eprintln!("[voice] connecting to room {channel_id}…");
    let (room, mut events) = Room::connect(&url, &token, RoomOptions::default())
        .await
        .map_err(|e| anyhow::anyhow!("LiveKit connect: {e}"))?;
    eprintln!("[voice] connected to room {channel_id}");

    let room = Arc::new(room);

    // ── Mic open, frames dropped (diagnostic step) ───────────────────────────
    // Open the mic and receive frames but don't call capture_frame yet.
    // If this crashes, the bug is in cpal. If stable, it's in capture_frame.
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<i16>, u32)>();
    let is_muted = {
        let voice = state.voice.lock().await;
        voice.is_muted.store(false, Ordering::Relaxed);
        Arc::clone(&voice.is_muted)
    };
    let noise_floor = {
        let voice = state.voice.lock().await;
        Arc::clone(&voice.noise_floor)
    };

    let input_device_clone = input_device.clone();
    eprintln!("[voice] opening mic…");
    let (mic_stream, mic_rate) = tokio::task::spawn_blocking(move || {
        let host = cpal::default_host();
        let dev = get_device(&host, input_device_clone.as_deref(), true)?;
        eprintln!("[voice] mic device: {:?}", dev.name());
        start_mic_stream(&dev, frame_tx, is_muted, noise_floor)
    })
    .await
    .map_err(|e| anyhow::anyhow!("mic init panicked: {e}"))??;
    eprintln!("[voice] mic opened at {mic_rate} Hz");

    let audio_source = NativeAudioSource::new(
        AudioSourceOptions {
            echo_cancellation: true,
            noise_suppression: true,
            auto_gain_control,
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
    room.local_participant()
        .publish_track(
            LocalTrack::Audio(local_track.clone()),
            TrackPublishOptions::default(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("publish track: {e}"))?;
    eprintln!("[voice] track published");

    // Buffer mic frames into exact 10ms chunks, detect local speaking, and
    // feed to capture_frame. Speaking is detected client-side from audio peaks
    // so the indicator works without waiting for the LiveKit server.
    let audio_source_task = audio_source.clone();
    let voice_arc_frame = Arc::clone(&state.voice);
    let local_identity = format!("voice-{user_id}");
    let frame_task = tokio::spawn(async move {
        let chunk_size = (mic_rate / 100) as usize;
        let mut buf: Vec<i16> = Vec::new();
        let mut speak_hold: u32 = 0; // counts down after speech stops (12 × 10ms = 120ms hold)
        let mut onset_frames: u32 = 0; // consecutive above-threshold frames; 2 required to (re)trigger
        let mut is_speaking = false;

        while let Some((samples, rate)) = frame_rx.recv().await {
            buf.extend_from_slice(&samples);
            while buf.len() >= chunk_size {
                let chunk: Vec<i16> = buf.drain(..chunk_size).collect();
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

    // ── Seed participant list ─────────────────────────────────────────────────
    // Emit ParticipantJoined for participants already in the room.
    // Do NOT attach tracks here — TrackSubscribed fires for pre-existing
    // subscribed tracks once the event loop drains buffered events, and
    // attaching twice creates competing NativeAudioStream sinks.
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
    let playback_arc = {
        let v = state.voice.lock().await;
        Arc::clone(&v.playback)
    };
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
                        let output_device_name = {
                            let pb = playback_arc.lock().unwrap();
                            pb.output_device_name.clone()
                        };
                        tokio::spawn(attach_remote_track(
                            output_device_name,
                            audio_track.rtc_track(),
                            Arc::clone(&playback_arc),
                            track_key,
                            Arc::clone(&voice_arc),
                            participant.identity().to_string(),
                        ));
                    }
                }
                RoomEvent::TrackUnsubscribed { track, publication: _, participant } => {
                    if let RemoteTrack::Audio(audio_track) = track {
                        let track_key = format!("{}-{}", participant.identity(), audio_track.sid());
                        eprintln!("[voice] track unsubscribed: {track_key}");
                        let mut pb = playback_arc.lock().unwrap();
                        if let Some(t) = pb.tasks.remove(&track_key) { t.abort(); }
                        pb.streams.remove(&track_key);
                        pb.rtc_tracks.remove(&track_key);
                        pb.identities.remove(&track_key);
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
                RoomEvent::ConnectionStateChanged(state) => {
                    eprintln!("[voice] connection state: {state:?}");
                }
                _ => {}
            }
        }
    });

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

    Ok(())
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
        }

        voice.local_track = None;
        voice.audio_source = None;
        voice.input_stream = None;
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
/// restarts it on the new device without disconnecting from the room.
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

    // Extract shared atomics then drop the lock — cpal init makes blocking ALSA
    // syscalls and must not hold the async mutex.
    let is_muted_clone = Arc::clone(&voice.is_muted);
    let noise_floor_clone = Arc::clone(&voice.noise_floor);
    drop(voice);

    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<i16>, u32)>();
    let device_name_clone = device_name.clone();
    let (new_mic, _) = tokio::task::spawn_blocking(move || {
        let host = cpal::default_host();
        let device = get_device(&host, Some(&device_name_clone), true)?;
        start_mic_stream(&device, frame_tx, is_muted_clone, noise_floor_clone)
    })
    .await
    .map_err(|e| anyhow::anyhow!("audio init panicked: {e}"))??;

    let mut voice = state.voice.lock().await;

    // Swap the input stream — dropping the old one stops it
    voice.input_stream = Some(new_mic);
    voice.current_input_device = Some(device_name);

    // Abort the old frame-feed task and start a new one on the new channel.
    if let Some(t) = voice.frame_task.take() { t.abort(); }
    let source = voice.audio_source.clone().unwrap();
    let task = tokio::spawn(async move {
        let mut buf: Vec<i16> = Vec::new();
        while let Some((samples, rate)) = frame_rx.recv().await {
            buf.extend_from_slice(&samples);
            let chunk_size = (rate / 100) as usize;
            while buf.len() >= chunk_size {
                let chunk: Vec<i16> = buf.drain(..chunk_size).collect();
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

/// Switch the speaker device mid-call. Rebuilds output streams for all
/// currently-subscribed remote audio tracks.
#[tauri::command]
pub async fn set_voice_output_device(
    device_name: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let voice_arc = Arc::clone(&state.voice);
    let voice = voice_arc.lock().await;
    let playback_arc = Arc::clone(&voice.playback);
    drop(voice);

    let rtc_tracks: Vec<(String, libwebrtc::audio_track::RtcAudioTrack, String)> = {
        let mut pb = playback_arc.lock().unwrap();
        pb.stop_all();
        pb.output_device_name = Some(device_name.clone());
        pb.rtc_tracks.iter().map(|(k, t)| {
            let identity = pb.identities.get(k).cloned().unwrap_or_default();
            (k.clone(), t.clone(), identity)
        }).collect()
    };

    // Re-attach each remote track to the new output device.
    for (key, rtc_track, identity) in rtc_tracks {
        tokio::spawn(attach_remote_track(
            Some(device_name.clone()),
            rtc_track,
            Arc::clone(&playback_arc),
            key,
            Arc::clone(&voice_arc),
            identity,
        ));
    }

    Ok(())
}

/// Set the noise gate threshold for the local microphone. Frames whose peak
/// amplitude is below this value are silenced. Pass 0.0 to disable the gate.
/// Range: 0.0 (off) to ~0.1 (aggressive). Persists across join/leave.
#[tauri::command]
pub async fn set_noise_floor(threshold: f32, state: State<'_, Arc<AppState>>) -> Result<()> {
    let voice = state.voice.lock().await;
    voice.noise_floor.store(threshold.to_bits(), Ordering::Relaxed);
    Ok(())
}
