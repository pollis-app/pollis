use std::collections::VecDeque;
use std::f32::consts::PI;
use std::sync::{
    atomic::{AtomicBool, AtomicU32, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;

use crate::commands::voice::{
    get_device, start_mic_stream, start_speaker_stream, SendableStream,
};
use crate::error::Result;
use crate::state::AppState;

// ── Events pushed to the frontend ─────────────────────────────────────────

/// Lifecycle + meter events for the Voice Settings test harness.
/// `Frame` is emitted at ~30 Hz while a mic test is running. The record /
/// tone commands emit the Recording*/Playback* markers so the UI can
/// reflect state transitions without the frontend having to poll.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VoiceTestEvent {
    /// Mic level sample. `peak` and `rms` are normalized 0.0..1.0 and are
    /// computed on the raw (ungated) signal so the meter still moves when
    /// the user speaks below the current noise floor. `gated` reports
    /// whether the same sample would be dropped by the real gate.
    Frame { peak: f32, rms: f32, gated: bool },
    RecordingStarted,
    RecordingFinished,
    PlaybackStarted,
    PlaybackFinished,
}

// ── Test session state ────────────────────────────────────────────────────

/// All state owned by the test harness. Held in a tokio Mutex inside
/// `AppState`. There is at most one active test at a time — starting a
/// new one (mic test, tone, or record+play) tears the previous one down.
pub struct VoiceTestState {
    pub channel: Option<tauri::ipc::Channel<VoiceTestEvent>>,
    // ── Mic test ──
    pub mic_stream: Option<SendableStream>,
    pub mic_task: Option<tokio::task::JoinHandle<()>>,
    pub monitor_enabled: Arc<AtomicBool>,
    /// Always zero — `start_mic_stream` gates before forwarding, and we
    /// want the meter to see every frame. Real gate status is computed in
    /// the emit task against the live voice `noise_floor`.
    pub test_noise_floor: Arc<AtomicU32>,
    // ── Output (shared between monitor / tone / record-playback) ──
    pub output_stream: Option<SendableStream>,
    pub output_buf: Option<Arc<Mutex<VecDeque<f32>>>>,
    pub output_channels: u32,
    pub output_sample_rate: u32,
    pub output_device_name: Option<String>,
    pub playback_task: Option<tokio::task::JoinHandle<()>>,
}

impl VoiceTestState {
    pub fn new() -> Self {
        Self {
            channel: None,
            mic_stream: None,
            mic_task: None,
            monitor_enabled: Arc::new(AtomicBool::new(false)),
            test_noise_floor: Arc::new(AtomicU32::new(0u32)),
            output_stream: None,
            output_buf: None,
            output_channels: 2,
            output_sample_rate: 48_000,
            output_device_name: None,
            playback_task: None,
        }
    }
}

// ── Internal helpers ──────────────────────────────────────────────────────

/// Abort any running playback / monitor / mic tasks and drop their streams.
/// Called at the start of every `start_*` command so tests are mutually
/// exclusive and idempotent.
fn stop_everything(state: &mut VoiceTestState) {
    if let Some(t) = state.playback_task.take() {
        t.abort();
    }
    if let Some(t) = state.mic_task.take() {
        t.abort();
    }
    // Dropping the cpal streams stops them.
    state.mic_stream = None;
    state.output_stream = None;
    state.output_buf = None;
    state.output_device_name = None;
    state.monitor_enabled.store(false, Ordering::Relaxed);
}

/// Ensure `state.output_*` is populated for `device_name`. If an output
/// stream is already open on a different device, tear it down first.
/// Runs the blocking cpal init on a dedicated thread so the async mutex
/// stays responsive.
async fn ensure_output(
    state_arc: &Arc<tokio::sync::Mutex<VoiceTestState>>,
    device_name: &str,
) -> Result<(Arc<Mutex<VecDeque<f32>>>, u32, u32)> {
    {
        let s = state_arc.lock().await;
        if s.output_device_name.as_deref() == Some(device_name) {
            if let (Some(buf), sr, ch) = (
                s.output_buf.clone(),
                s.output_sample_rate,
                s.output_channels,
            ) {
                return Ok((buf, sr, ch));
            }
        }
    }

    let dev_name = device_name.to_string();
    let (stream, sample_rate, channels, buf) = tokio::task::spawn_blocking(move || {
        let host = cpal::default_host();
        let dev = get_device(&host, Some(&dev_name), false)?;
        start_speaker_stream(&dev)
    })
    .await
    .map_err(|e| anyhow::anyhow!("speaker init panicked: {e}"))??;

    let mut s = state_arc.lock().await;
    // If a previous stream was opened on another device, drop it so the
    // new one takes over cleanly.
    s.output_stream = None;
    s.output_stream = Some(stream);
    s.output_buf = Some(buf.clone());
    s.output_sample_rate = sample_rate;
    s.output_channels = channels;
    s.output_device_name = Some(device_name.to_string());
    Ok((buf, sample_rate, channels))
}

/// Fire an event on the registered channel, if any. No-op when the frontend
/// hasn't called `subscribe_voice_test_events` yet.
async fn emit(state_arc: &Arc<tokio::sync::Mutex<VoiceTestState>>, ev: VoiceTestEvent) {
    let s = state_arc.lock().await;
    if let Some(ch) = &s.channel {
        let _ = ch.send(ev);
    }
}

// ── Tauri commands ────────────────────────────────────────────────────────

/// Register the Tauri Channel used to push VoiceTestEvents.
/// Call once when the Voice Settings page mounts.
#[tauri::command]
pub async fn subscribe_voice_test_events(
    on_event: tauri::ipc::Channel<VoiceTestEvent>,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let mut s = state.voice_test.lock().await;
    s.channel = Some(on_event);
    Ok(())
}

/// Start a mic self-test against `input_device_id`. Opens the same cpal
/// input stream the voice chat uses, computes peak/RMS on every frame, and
/// pushes a `Frame` event to the frontend at ~30 Hz. When `monitor` is
/// true, also opens the output device and loops the mic back through it
/// (feedback warning).
///
/// Cancels any previous test.
#[tauri::command]
pub async fn start_mic_test(
    input_device_id: String,
    output_device_id: String,
    monitor: bool,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let state_arc = Arc::clone(&state.voice_test);

    // Cancel any previous test atomically.
    {
        let mut s = state_arc.lock().await;
        stop_everything(&mut s);
    }

    // Open the output first if we're monitoring — makes feedback obvious
    // from the first mic frame rather than after a one-way warmup gap.
    let output = if monitor {
        Some(ensure_output(&state_arc, &output_device_id).await?)
    } else {
        None
    };

    // Pull the shared atomics we need before spawning the blocking init.
    let (is_muted, test_gate, monitor_flag, real_noise_floor) = {
        let s = state_arc.lock().await;
        let voice = state.voice.lock().await;
        (
            Arc::clone(&voice.is_muted),
            Arc::clone(&s.test_noise_floor),
            Arc::clone(&s.monitor_enabled),
            Arc::clone(&voice.noise_floor),
        )
    };
    monitor_flag.store(monitor, Ordering::Relaxed);

    let (frame_tx, mut frame_rx) =
        tokio::sync::mpsc::unbounded_channel::<(Vec<i16>, u32)>();

    let input_id = input_device_id.clone();
    let (mic_stream, _mic_rate) = tokio::task::spawn_blocking(move || {
        let host = cpal::default_host();
        let dev = get_device(&host, Some(&input_id), true)?;
        start_mic_stream(&dev, frame_tx, is_muted, test_gate)
    })
    .await
    .map_err(|e| anyhow::anyhow!("mic init panicked: {e}"))??;

    let state_task = Arc::clone(&state_arc);
    let monitor_flag_task = Arc::clone(&monitor_flag);
    let task = tokio::spawn(async move {
        // Emit one frame every 3 mic chunks (≈30ms ≈ 33Hz). Keeps the
        // IPC volume reasonable while still feeling responsive.
        const EMIT_EVERY: u32 = 3;
        let mut chunks_seen: u32 = 0;
        let mut acc_peak: i16 = 0;
        let mut acc_sq: f64 = 0.0;
        let mut acc_count: usize = 0;

        while let Some((samples, _rate)) = frame_rx.recv().await {
            // Level math on the raw chunk.
            let mut chunk_peak: i16 = 0;
            let mut chunk_sq: f64 = 0.0;
            for &s in &samples {
                let a = s.saturating_abs();
                if a > chunk_peak {
                    chunk_peak = a;
                }
                let f = s as f64;
                chunk_sq += f * f;
            }
            if chunk_peak > acc_peak {
                acc_peak = chunk_peak;
            }
            acc_sq += chunk_sq;
            acc_count += samples.len();

            // Feed monitor output if enabled. Mono → output_channels by
            // duplication. Use the same ring buffer cap strategy as the
            // call-path: trim to 200ms so it doesn't drift.
            if monitor_flag_task.load(Ordering::Relaxed) {
                let (buf_opt, ch) = {
                    let s = state_task.lock().await;
                    (s.output_buf.clone(), s.output_channels)
                };
                if let Some(buf) = buf_opt {
                    let mut b = buf.lock().unwrap();
                    for &sample in &samples {
                        let f = sample as f32 / 32_768.0;
                        for _ in 0..ch {
                            b.push_back(f);
                        }
                    }
                    let cap = (48_000 * ch / 5) as usize;
                    while b.len() > cap {
                        b.pop_front();
                    }
                }
            }

            chunks_seen += 1;
            if chunks_seen >= EMIT_EVERY {
                let peak_norm = (acc_peak as f32 / 32_768.0).clamp(0.0, 1.0);
                let rms_norm = if acc_count > 0 {
                    ((acc_sq / acc_count as f64).sqrt() as f32 / 32_768.0).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let gate = f32::from_bits(real_noise_floor.load(Ordering::Relaxed));
                let gated = gate > 0.0 && peak_norm < gate;

                let ev = VoiceTestEvent::Frame {
                    peak: peak_norm,
                    rms: rms_norm,
                    gated,
                };
                {
                    let s = state_task.lock().await;
                    if let Some(ch_) = &s.channel {
                        let _ = ch_.send(ev);
                    }
                }

                chunks_seen = 0;
                acc_peak = 0;
                acc_sq = 0.0;
                acc_count = 0;
            }
        }
    });

    let _ = output; // suppress unused in monitor=false path
    let mut s = state_arc.lock().await;
    s.mic_stream = Some(mic_stream);
    s.mic_task = Some(task);
    Ok(())
}

/// Toggle the monitor (loopback) path on or off while a mic test is
/// running. No-op if no test is active.
#[tauri::command]
pub async fn set_mic_test_monitor(
    enabled: bool,
    output_device_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let state_arc = Arc::clone(&state.voice_test);

    // When enabling, make sure the output is open on the requested device
    // before flipping the flag so we don't drop the first 100ms of monitor
    // audio into a silent ring.
    if enabled {
        ensure_output(&state_arc, &output_device_id).await?;
    }

    let s = state_arc.lock().await;
    s.monitor_enabled.store(enabled, Ordering::Relaxed);
    Ok(())
}

/// Stop any running mic test (monitor + meter). Idempotent.
#[tauri::command]
pub async fn stop_mic_test(state: State<'_, Arc<AppState>>) -> Result<()> {
    let mut s = state.voice_test.lock().await;
    stop_everything(&mut s);
    Ok(())
}

/// Record `duration_ms` of microphone audio into memory, then play it back
/// through the output device. Cancels any other running test. Emits
/// RecordingStarted → RecordingFinished → PlaybackStarted → PlaybackFinished.
#[tauri::command]
pub async fn record_and_play_back(
    input_device_id: String,
    output_device_id: String,
    duration_ms: u32,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let state_arc = Arc::clone(&state.voice_test);
    let voice_arc = Arc::clone(&state.voice);

    {
        let mut s = state_arc.lock().await;
        stop_everything(&mut s);
    }

    // Set up mic (gated with zero floor so we record the whole signal,
    // not post-gate silence).
    let (is_muted, test_gate, real_noise_floor) = {
        let s = state_arc.lock().await;
        let v = voice_arc.lock().await;
        (
            Arc::clone(&v.is_muted),
            Arc::clone(&s.test_noise_floor),
            Arc::clone(&v.noise_floor),
        )
    };
    let (frame_tx, mut frame_rx) =
        tokio::sync::mpsc::unbounded_channel::<(Vec<i16>, u32)>();

    let input_id = input_device_id.clone();
    let (mic_stream, mic_rate) = tokio::task::spawn_blocking(move || {
        let host = cpal::default_host();
        let dev = get_device(&host, Some(&input_id), true)?;
        start_mic_stream(&dev, frame_tx, is_muted, test_gate)
    })
    .await
    .map_err(|e| anyhow::anyhow!("mic init panicked: {e}"))??;

    // Store mic stream so cancellation can drop it.
    {
        let mut s = state_arc.lock().await;
        s.mic_stream = Some(mic_stream);
    }

    let state_task = Arc::clone(&state_arc);
    let task = tokio::spawn(async move {
        emit(&state_task, VoiceTestEvent::RecordingStarted).await;

        // Accumulate samples for `duration_ms`.
        let target_samples = (mic_rate as u64 * duration_ms as u64 / 1_000) as usize;
        let mut recorded: Vec<i16> = Vec::with_capacity(target_samples + 4_800);
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(duration_ms as u64);

        // Same level-meter emission as start_mic_test — emit every 3 chunks
        // (~33 Hz) so users see input feedback while recording.
        const EMIT_EVERY: u32 = 3;
        let mut chunks_seen: u32 = 0;
        let mut acc_peak: i16 = 0;
        let mut acc_sq: f64 = 0.0;
        let mut acc_count: usize = 0;

        loop {
            let timeout = deadline.saturating_duration_since(tokio::time::Instant::now());
            if timeout.is_zero() {
                break;
            }
            match tokio::time::timeout(timeout, frame_rx.recv()).await {
                Ok(Some((samples, _rate))) => {
                    let mut chunk_peak: i16 = 0;
                    let mut chunk_sq: f64 = 0.0;
                    for &s in &samples {
                        let a = s.saturating_abs();
                        if a > chunk_peak {
                            chunk_peak = a;
                        }
                        let f = s as f64;
                        chunk_sq += f * f;
                    }
                    if chunk_peak > acc_peak {
                        acc_peak = chunk_peak;
                    }
                    acc_sq += chunk_sq;
                    acc_count += samples.len();

                    recorded.extend_from_slice(&samples);

                    chunks_seen += 1;
                    if chunks_seen >= EMIT_EVERY {
                        let peak_norm = (acc_peak as f32 / 32_768.0).clamp(0.0, 1.0);
                        let rms_norm = if acc_count > 0 {
                            ((acc_sq / acc_count as f64).sqrt() as f32 / 32_768.0).clamp(0.0, 1.0)
                        } else {
                            0.0
                        };
                        let gate = f32::from_bits(real_noise_floor.load(Ordering::Relaxed));
                        let gated = gate > 0.0 && peak_norm < gate;
                        let ev = VoiceTestEvent::Frame {
                            peak: peak_norm,
                            rms: rms_norm,
                            gated,
                        };
                        {
                            let s = state_task.lock().await;
                            if let Some(ch_) = &s.channel {
                                let _ = ch_.send(ev);
                            }
                        }
                        chunks_seen = 0;
                        acc_peak = 0;
                        acc_sq = 0.0;
                        acc_count = 0;
                    }

                    if recorded.len() >= target_samples {
                        break;
                    }
                }
                _ => break, // channel closed or deadline hit
            }
        }

        // Stop the mic — we have what we need.
        {
            let mut s = state_task.lock().await;
            s.mic_stream = None;
        }
        emit(&state_task, VoiceTestEvent::RecordingFinished).await;

        // Playback phase.
        let output = match ensure_output(&state_task, &output_device_id).await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[voice-test] record-playback output init failed: {e}");
                return;
            }
        };
        let (buf, out_rate, out_channels) = output;

        emit(&state_task, VoiceTestEvent::PlaybackStarted).await;

        // Push recorded mono i16 into the f32 ring, duplicated per channel.
        // Assumes mic_rate == out_rate (both forced to 48 kHz elsewhere).
        {
            let mut b = buf.lock().unwrap();
            for &sample in &recorded {
                let f = sample as f32 / 32_768.0;
                for _ in 0..out_channels {
                    b.push_back(f);
                }
            }
        }

        // Wait for drain. Poll at 50ms; covers ~20 ticks/sec.
        let drained_at_wait = {
            let total_samples = recorded.len() as u64 * out_channels as u64;
            Duration::from_millis(total_samples * 1_000 / out_rate as u64 + 250)
        };
        let start = tokio::time::Instant::now();
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            let empty = {
                let b = buf.lock().unwrap();
                b.is_empty()
            };
            if empty || start.elapsed() > drained_at_wait {
                break;
            }
        }

        // Tear down the output now that playback is done.
        {
            let mut s = state_task.lock().await;
            s.output_stream = None;
            s.output_buf = None;
            s.output_device_name = None;
        }
        emit(&state_task, VoiceTestEvent::PlaybackFinished).await;
    });

    let mut s = state_arc.lock().await;
    s.playback_task = Some(task);
    Ok(())
}

/// Play a test tone through `output_device_id`.
/// `kind` is `"sweep"` (200→2000 Hz linear sine over 1.5s) or `"chime"`
/// (C5-E5-G5 arpeggio, 200ms each). Cancels any other running test.
#[tauri::command]
pub async fn play_test_tone(
    output_device_id: String,
    kind: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let state_arc = Arc::clone(&state.voice_test);

    {
        let mut s = state_arc.lock().await;
        stop_everything(&mut s);
    }

    let (buf, sample_rate, channels) =
        ensure_output(&state_arc, &output_device_id).await?;

    let samples: Vec<f32> = match kind.as_str() {
        "sweep" => generate_sweep(sample_rate, 1_500, 200.0, 2_000.0),
        "chime" => generate_chime(sample_rate),
        other => {
            return Err(anyhow::anyhow!("unknown tone kind: {other}").into());
        }
    };

    let total_duration_ms =
        (samples.len() as u64 * 1_000 / sample_rate as u64) as u64;

    let state_task = Arc::clone(&state_arc);
    let task = tokio::spawn(async move {
        emit(&state_task, VoiceTestEvent::PlaybackStarted).await;

        // Push mono samples duplicated to output channels.
        {
            let mut b = buf.lock().unwrap();
            for &s in &samples {
                for _ in 0..channels {
                    b.push_back(s);
                }
            }
        }

        // Wait for the ring to drain, with a small grace period.
        tokio::time::sleep(Duration::from_millis(total_duration_ms + 250)).await;

        // Tear down so the next test starts clean.
        {
            let mut s = state_task.lock().await;
            s.output_stream = None;
            s.output_buf = None;
            s.output_device_name = None;
        }
        emit(&state_task, VoiceTestEvent::PlaybackFinished).await;
    });

    let mut s = state_arc.lock().await;
    s.playback_task = Some(task);
    Ok(())
}

/// Stop any running tone / record-playback. Mic test (if separately running)
/// is untouched. Idempotent.
#[tauri::command]
pub async fn stop_test_playback(state: State<'_, Arc<AppState>>) -> Result<()> {
    let mut s = state.voice_test.lock().await;
    if let Some(t) = s.playback_task.take() {
        t.abort();
    }
    s.output_stream = None;
    s.output_buf = None;
    s.output_device_name = None;
    Ok(())
}

// ── Tone generators ───────────────────────────────────────────────────────

/// Linear-frequency sine sweep with a short attack/release envelope to
/// avoid clicks at the edges.
fn generate_sweep(sample_rate: u32, duration_ms: u32, f0: f32, f1: f32) -> Vec<f32> {
    let total = (sample_rate as u64 * duration_ms as u64 / 1_000) as usize;
    let mut out = Vec::with_capacity(total);
    let env_samples = sample_rate as usize / 50; // 20ms fade
    let mut phase = 0.0f32;
    for i in 0..total {
        let t = i as f32 / total as f32;
        let f = f0 + (f1 - f0) * t;
        phase += 2.0 * PI * f / sample_rate as f32;
        if phase > 2.0 * PI {
            phase -= 2.0 * PI;
        }
        let env = if i < env_samples {
            i as f32 / env_samples as f32
        } else if i > total - env_samples {
            (total - i) as f32 / env_samples as f32
        } else {
            1.0
        };
        out.push(phase.sin() * 0.15 * env);
    }
    out
}

/// C5–E5–G5 arpeggio, 200ms per note, with envelopes. Just enough of a
/// "voice ready" jingle to confirm the output path is wired up end-to-end.
fn generate_chime(sample_rate: u32) -> Vec<f32> {
    let notes: [f32; 3] = [523.25, 659.25, 783.99];
    let per_note_ms = 200u32;
    let note_samples = (sample_rate as u64 * per_note_ms as u64 / 1_000) as usize;
    let mut out = Vec::with_capacity(note_samples * notes.len());
    for freq in notes {
        let mut phase = 0.0f32;
        for i in 0..note_samples {
            phase += 2.0 * PI * freq / sample_rate as f32;
            if phase > 2.0 * PI {
                phase -= 2.0 * PI;
            }
            // Exponential decay inside each note so it sounds plucked
            // rather than held.
            let env = (-4.0 * i as f32 / note_samples as f32).exp();
            out.push(phase.sin() * 0.15 * env);
        }
    }
    out
}
