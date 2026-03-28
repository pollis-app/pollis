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
struct SendableStream(cpal::Stream);
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
    ActiveSpeakers { identities: Vec<String> },
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
    pub output_device_name: Option<String>,
}

impl PlaybackState {
    fn new() -> Self {
        Self {
            streams: HashMap::new(),
            tasks: HashMap::new(),
            rtc_tracks: HashMap::new(),
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

fn get_device(host: &cpal::Host, name: Option<&str>, is_input: bool) -> Result<cpal::Device> {
    let device = match name {
        None => {
            if is_input {
                host.default_input_device()
            } else {
                host.default_output_device()
            }
        }
        Some(n) => {
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
fn start_mic_stream(
    device: &cpal::Device,
    frame_tx: tokio::sync::mpsc::UnboundedSender<(Vec<i16>, u32)>,
    is_muted: Arc<AtomicBool>,
    noise_floor: Arc<AtomicU32>,
) -> Result<(SendableStream, u32)> {
    let config = device
        .default_input_config()
        .map_err(|e| anyhow::anyhow!("input config: {e}"))?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as usize;
    let sample_format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();

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
fn start_speaker_stream(
    device: &cpal::Device,
) -> Result<(SendableStream, u32, u32, Arc<Mutex<VecDeque<f32>>>)> {
    let config = device
        .default_output_config()
        .map_err(|e| anyhow::anyhow!("output config: {e}"))?;
    let sample_rate = config.sample_rate().0;
    let channels = config.channels() as u32;
    let sample_format = config.sample_format();
    let stream_config: cpal::StreamConfig = config.into();

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

/// Attach a NativeAudioStream to a speaker output device. Spawns a task that
/// drains audio frames from LiveKit and writes them into the ring buffer.
fn attach_remote_track(
    host: &cpal::Host,
    output_device_name: Option<&str>,
    rtc_track: libwebrtc::audio_track::RtcAudioTrack,
    playback: &Arc<Mutex<PlaybackState>>,
    track_key: &str,
) {
    let output_dev = match get_device(host, output_device_name, false) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("[voice] output device error for {track_key}: {e}");
            return;
        }
    };

    let (stream, sample_rate, channels, buf) = match start_speaker_stream(&output_dev) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("[voice] speaker stream error for {track_key}: {e}");
            return;
        }
    };

    let mut audio_stream =
        NativeAudioStream::new(rtc_track.clone(), sample_rate as i32, channels as i32);

    let max_buf = (sample_rate * channels * 2) as usize;
    let task = tokio::spawn(async move {
        while let Some(frame) = audio_stream.next().await {
            let samples: Vec<f32> =
                frame.data.iter().map(|&s| s as f32 / 32768.0).collect();
            let mut b = buf.lock().unwrap();
            b.extend(samples);
            // Cap at 2 seconds to prevent unbounded growth on a stalled output
            while b.len() > max_buf {
                b.pop_front();
            }
        }
    });

    let mut pb = playback.lock().unwrap();
    pb.streams.insert(track_key.to_string(), stream);
    pb.tasks.insert(track_key.to_string(), task);
    pb.rtc_tracks.insert(track_key.to_string(), rtc_track);
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
#[tauri::command]
pub async fn list_audio_devices() -> Result<Vec<AudioDevice>> {
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
    Ok(devices)
}

/// Connect to a LiveKit voice room and start publishing the local microphone.
/// `input_device` and `output_device` are device names from `list_audio_devices`
/// (or null/undefined to use the system default).
#[tauri::command]
pub async fn join_voice_channel(
    channel_id: String,
    user_id: String,
    display_name: String,
    input_device: Option<String>,
    output_device: Option<String>,
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

    // Extract the shared atomics from voice state so toggle_voice_mute and
    // set_noise_floor affect the live mic callback. Reset mute on every join.
    let (is_muted, noise_floor) = {
        let voice = state.voice.lock().await;
        voice.is_muted.store(false, Ordering::Relaxed);
        (Arc::clone(&voice.is_muted), Arc::clone(&voice.noise_floor))
    };

    // Connect to the LiveKit room
    let (room, mut events) = Room::connect(&url, &token, RoomOptions::default())
        .await
        .map_err(|e| anyhow::anyhow!("LiveKit connect: {e}"))?;
    let room = Arc::new(room);

    // Create audio source (LiveKit encodes and transmits this as an audio track)
    let host = cpal::default_host();
    let input_dev = get_device(&host, input_device.as_deref(), true)?;
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<i16>, u32)>();

    let (mic_stream, mic_rate) =
        start_mic_stream(&input_dev, frame_tx, Arc::clone(&is_muted), Arc::clone(&noise_floor))?;

    let audio_source =
        NativeAudioSource::new(AudioSourceOptions::default(), mic_rate, 1, 100);

    let local_track = LocalAudioTrack::create_audio_track(
        "microphone",
        RtcAudioSource::Native(audio_source.clone()),
    );

    room.local_participant()
        .publish_track(
            LocalTrack::Audio(local_track.clone()),
            TrackPublishOptions::default(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("publish track: {e}"))?;

    // Task: feed i16 frames from cpal callback into the LiveKit audio source
    let source_clone = audio_source.clone();
    let frame_task = tokio::spawn(async move {
        while let Some((samples, rate)) = frame_rx.recv().await {
            let n = samples.len() as u32;
            let frame = AudioFrame {
                data: samples.into(),
                sample_rate: rate,
                num_channels: 1,
                samples_per_channel: n,
            };
            let _ = source_clone.capture_frame(&frame).await;
        }
    });

    // Task: handle incoming room events (participants, tracks, speakers)
    let voice_arc = Arc::clone(&state.voice);
    let playback_arc = {
        let voice = state.voice.lock().await;
        Arc::clone(&voice.playback)
    };
    let room_task = tokio::spawn(async move {
        let host = cpal::default_host();

        while let Some(event) = events.recv().await {
            match event {
                RoomEvent::ParticipantConnected(p) => {
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
                    let identity = p.identity().to_string();
                    // Clean up all playback streams for this participant
                    {
                        let mut pb = playback_arc.lock().unwrap();
                        let keys: Vec<String> = pb
                            .streams
                            .keys()
                            .filter(|k| k.starts_with(&identity))
                            .cloned()
                            .collect();
                        for k in keys {
                            if let Some(t) = pb.tasks.remove(&k) { t.abort(); }
                            pb.streams.remove(&k);
                            pb.rtc_tracks.remove(&k);
                        }
                    }
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::ParticipantLeft { identity });
                    }
                }

                RoomEvent::TrackSubscribed { track, participant, .. } => {
                    if let RemoteTrack::Audio(audio_track) = track {
                        let output_dev_name = {
                            let pb = playback_arc.lock().unwrap();
                            pb.output_device_name.clone()
                        };
                        let key = format!(
                            "{}-{}",
                            participant.identity(),
                            audio_track.sid()
                        );
                        attach_remote_track(
                            &host,
                            output_dev_name.as_deref(),
                            audio_track.rtc_track(),
                            &playback_arc,
                            &key,
                        );
                    }
                }

                RoomEvent::TrackUnsubscribed { track, participant, .. } => {
                    if let RemoteTrack::Audio(audio_track) = track {
                        let key =
                            format!("{}-{}", participant.identity(), audio_track.sid());
                        let mut pb = playback_arc.lock().unwrap();
                        if let Some(t) = pb.tasks.remove(&key) { t.abort(); }
                        pb.streams.remove(&key);
                        pb.rtc_tracks.remove(&key);
                    }
                }

                RoomEvent::TrackMuted { participant, .. } => {
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::Muted {
                            identity: participant.identity().to_string(),
                        });
                    }
                }

                RoomEvent::TrackUnmuted { participant, .. } => {
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::Unmuted {
                            identity: participant.identity().to_string(),
                        });
                    }
                }

                RoomEvent::ActiveSpeakersChanged { speakers } => {
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let identities =
                            speakers.iter().map(|s| s.identity().to_string()).collect();
                        let _ = ch.send(VoiceEvent::ActiveSpeakers { identities });
                    }
                }

                RoomEvent::Disconnected { .. } => {
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::Disconnected);
                    }
                    break;
                }

                _ => {}
            }
        }
    });

    // Store everything
    let mut voice = state.voice.lock().await;
    // Abort any previous call that wasn't cleanly left
    if let Some(t) = voice.room_task.take() { t.abort(); }
    if let Some(t) = voice.frame_task.take() { t.abort(); }
    {
        let mut pb = voice.playback.lock().unwrap();
        pb.stop_all();
        pb.output_device_name = output_device;
    }

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
    let mut voice = state.voice.lock().await;

    if let Some(t) = voice.room_task.take() { t.abort(); }
    if let Some(t) = voice.frame_task.take() { t.abort(); }

    {
        let mut pb = voice.playback.lock().unwrap();
        pb.stop_all();
        pb.rtc_tracks.clear();
    }

    if let Some(room) = voice.room.take() {
        let _ = room.close().await;
    }

    voice.local_track = None;
    voice.audio_source = None;
    voice.input_stream = None;
    voice.is_muted.store(false, Ordering::Relaxed);
    voice.current_input_device = None;

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

    let host = cpal::default_host();
    let device = get_device(&host, Some(&device_name), true)?;

    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<i16>, u32)>();
    let (new_mic, _) = start_mic_stream(&device, frame_tx, Arc::clone(&voice.is_muted), Arc::clone(&voice.noise_floor))?;

    // Swap the input stream — dropping the old one stops it
    voice.input_stream = Some(new_mic);
    voice.current_input_device = Some(device_name);

    // Abort the old frame-feed task and start a new one on the new channel
    if let Some(t) = voice.frame_task.take() { t.abort(); }
    let source = voice.audio_source.clone().unwrap();
    let task = tokio::spawn(async move {
        while let Some((samples, rate)) = frame_rx.recv().await {
            let n = samples.len() as u32;
            let frame = AudioFrame {
                data: samples.into(),
                sample_rate: rate,
                num_channels: 1,
                samples_per_channel: n,
            };
            let _ = source.capture_frame(&frame).await;
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
    let voice = state.voice.lock().await;
    let playback_arc = Arc::clone(&voice.playback);
    drop(voice);

    let host = cpal::default_host();
    let rtc_tracks: Vec<(String, libwebrtc::audio_track::RtcAudioTrack)> = {
        let mut pb = playback_arc.lock().unwrap();
        pb.stop_all();
        pb.output_device_name = Some(device_name.clone());
        pb.rtc_tracks.iter().map(|(k, t)| (k.clone(), t.clone())).collect()
    };

    // Re-attach each remote track to the new device
    for (key, rtc_track) in rtc_tracks {
        attach_remote_track(&host, Some(&device_name), rtc_track, &playback_arc, &key);
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
