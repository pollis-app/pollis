use std::collections::VecDeque;
use std::f32::consts::PI;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;

use serde::{Deserialize, Serialize};


use crate::commands::voice::{
    get_device, start_mic_stream, start_speaker_stream, SendableStream,
};
use crate::error::Result;
use crate::state::AppState;

/// Speaker rate requested for the test; `start_speaker_stream` falls back to the
/// device's native rate if it can't be honoured.
const TEST_SPEAKER_RATE_HZ: u32 = 48_000;

/// Lifecycle and meter events for the Voice Settings test harness.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VoiceTestEvent {
    /// Mic level, 0.0..1.0, on the raw pre-APM signal so the meter moves even under
    /// quiet input.
    Frame { peak: f32, rms: f32 },
    RecordingStarted,
    RecordingFinished,
    PlaybackStarted,
    PlaybackFinished,
}

/// Test-harness state. At most one test runs; starting one tears down the last.
pub struct VoiceTestState {
    pub channel: Option<std::sync::Arc<dyn crate::sink::EventSink<VoiceTestEvent>>>,
    pub mic_stream: Option<SendableStream>,
    pub mic_task: Option<tokio::task::JoinHandle<()>>,
    pub monitor_enabled: Arc<AtomicBool>,
    // Output, shared by monitor, tone and record-playback.
    pub output_stream: Option<SendableStream>,
    pub output_buf: Option<Arc<Mutex<VecDeque<f32>>>>,
    pub output_channels: u32,
    pub output_sample_rate: u32,
    pub output_device_name: Option<String>,
    pub playback_task: Option<tokio::task::JoinHandle<()>>,
}

impl Default for VoiceTestState {
    fn default() -> Self {
        Self::new()
    }
}

impl VoiceTestState {
    pub fn new() -> Self {
        Self {
            channel: None,
            mic_stream: None,
            mic_task: None,
            monitor_enabled: Arc::new(AtomicBool::new(false)),
            output_stream: None,
            output_buf: None,
            output_channels: 2,
            output_sample_rate: 48_000,
            output_device_name: None,
            playback_task: None,
        }
    }
}

/// Abort running tasks and drop their streams; every `start_*` calls this first.
fn stop_everything(state: &mut VoiceTestState) {
    if let Some(t) = state.playback_task.take() {
        t.abort();
    }
    if let Some(t) = state.mic_task.take() {
        t.abort();
    }
    state.mic_stream = None;
    state.output_stream = None;
    state.output_buf = None;
    state.output_device_name = None;
    state.monitor_enabled.store(false, Ordering::Relaxed);
}

/// Ensure an output stream is open on `device_name`, replacing one on another
/// device. cpal init runs on a blocking thread.
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

    // If the requested device can't open (e.g. Bluetooth in HFP, stale pref), fall
    // back to the OS default so the user still hears the test.
    let dev_name = device_name.to_string();
    let (stream, sample_rate, channels, buf, opened_name) = tokio::task::spawn_blocking(move || -> Result<_> {
        let host = cpal::default_host();
        let try_open = |name: Option<&str>| -> Result<_> {
            let dev = get_device(&host, name, false)?;
            start_speaker_stream(&dev, TEST_SPEAKER_RATE_HZ)
        };
        match try_open(Some(&dev_name)) {
            Ok((s, sr, ch, buf)) => Ok((s, sr, ch, buf, dev_name)),
            Err(e) => {
                let was_explicit = !dev_name.is_empty() && dev_name != "default";
                if !was_explicit {
                    return Err(e);
                }
                crate::diag!(
                    "[voice-test] speaker open failed for the selected device: {e} — falling back to OS default"
                );
                let (s, sr, ch, buf) = try_open(None)?;
                Ok((s, sr, ch, buf, String::from("default")))
            }
        }
    })
    .await
    .map_err(|e| anyhow::anyhow!("speaker init panicked: {e}"))??;

    let mut s = state_arc.lock().await;
    s.output_stream = None;
    s.output_stream = Some(stream);
    s.output_buf = Some(buf.clone());
    s.output_sample_rate = sample_rate;
    s.output_channels = channels;
    s.output_device_name = Some(opened_name);
    Ok((buf, sample_rate, channels))
}

async fn emit(state_arc: &Arc<tokio::sync::Mutex<VoiceTestState>>, ev: VoiceTestEvent) {
    let s = state_arc.lock().await;
    if let Some(ch) = &s.channel {
        let _ = ch.send(ev);
    }
}

pub async fn subscribe_voice_test_events(
    on_event: std::sync::Arc<dyn crate::sink::EventSink<VoiceTestEvent>>,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut s = state.voice_test.lock().await;
    s.channel = Some(on_event);
    Ok(())
}

/// Start a mic self-test: the voice path's cpal input, with peak/RMS `Frame`
/// events at ~30 Hz. With `monitor`, also loops the mic to the output. Cancels any
/// previous test.
pub async fn start_mic_test(
    input_device_id: String,
    output_device_id: String,
    monitor: bool,
    state: &Arc<AppState>,
) -> Result<()> {
    let state_arc = Arc::clone(&state.voice_test);

    {
        let mut s = state_arc.lock().await;
        stop_everything(&mut s);
    }

    // Open the output first so monitoring is audible from the first mic frame.
    let output = if monitor {
        Some(ensure_output(&state_arc, &output_device_id).await?)
    } else {
        None
    };

    let (is_muted, monitor_flag) = {
        let s = state_arc.lock().await;
        let voice = state.voice.lock().await;
        (Arc::clone(&voice.self_muted), Arc::clone(&s.monitor_enabled))
    };
    monitor_flag.store(monitor, Ordering::Relaxed);

    let (frame_tx, mut frame_rx) =
        tokio::sync::mpsc::unbounded_channel::<(Vec<i16>, u32)>();

    let input_id = input_device_id.clone();
    let (mic_stream, _mic_rate) = tokio::task::spawn_blocking(move || {
        let host = cpal::default_host();
        let dev = get_device(&host, Some(&input_id), true)?;
        start_mic_stream(&dev, frame_tx, is_muted)
    })
    .await
    .map_err(|e| anyhow::anyhow!("mic init panicked: {e}"))??;

    let state_task = Arc::clone(&state_arc);
    let monitor_flag_task = Arc::clone(&monitor_flag);
    let task = tokio::spawn(async move {
        // One meter event per 3 mic chunks (~33 Hz) to bound IPC volume.
        const EMIT_EVERY: u32 = 3;
        let mut chunks_seen: u32 = 0;
        let mut acc_peak: i16 = 0;
        let mut acc_sq: f64 = 0.0;
        let mut acc_count: usize = 0;

        while let Some((samples, _rate)) = frame_rx.recv().await {
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

            // Mono to `output_channels` by duplication, trimmed to 200 ms like the call
            // path so it doesn't drift.
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

                let ev = VoiceTestEvent::Frame { peak: peak_norm, rms: rms_norm };
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

/// Toggle monitor loopback during a mic test. No-op if none is running.
pub async fn set_mic_test_monitor(
    enabled: bool,
    output_device_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let state_arc = Arc::clone(&state.voice_test);

    // Open the output before flipping the flag so early monitor audio isn't lost.
    if enabled {
        ensure_output(&state_arc, &output_device_id).await?;
    }

    let s = state_arc.lock().await;
    s.monitor_enabled.store(enabled, Ordering::Relaxed);
    Ok(())
}

pub async fn stop_mic_test(state: &Arc<AppState>) -> Result<()> {
    let mut s = state.voice_test.lock().await;
    stop_everything(&mut s);
    Ok(())
}

/// Record `duration_ms` of mic audio, then play it back. Cancels any other test.
pub async fn record_and_play_back(
    input_device_id: String,
    output_device_id: String,
    duration_ms: u32,
    state: &Arc<AppState>,
) -> Result<()> {
    let state_arc = Arc::clone(&state.voice_test);
    let voice_arc = Arc::clone(&state.voice);

    {
        let mut s = state_arc.lock().await;
        stop_everything(&mut s);
    }

    // Raw input, no APM, so users hear what their device actually delivers.
    let is_muted = {
        let _s = state_arc.lock().await;
        let v = voice_arc.lock().await;
        Arc::clone(&v.self_muted)
    };
    let (frame_tx, mut frame_rx) =
        tokio::sync::mpsc::unbounded_channel::<(Vec<i16>, u32)>();

    let input_id = input_device_id.clone();
    let (mic_stream, mic_rate) = tokio::task::spawn_blocking(move || {
        let host = cpal::default_host();
        let dev = get_device(&host, Some(&input_id), true)?;
        start_mic_stream(&dev, frame_tx, is_muted)
    })
    .await
    .map_err(|e| anyhow::anyhow!("mic init panicked: {e}"))??;

    {
        let mut s = state_arc.lock().await;
        s.mic_stream = Some(mic_stream);
    }

    let state_task = Arc::clone(&state_arc);
    let task = tokio::spawn(async move {
        emit(&state_task, VoiceTestEvent::RecordingStarted).await;

        let target_samples = (mic_rate as u64 * duration_ms as u64 / 1_000) as usize;
        let mut recorded: Vec<i16> = Vec::with_capacity(target_samples + 4_800);
        let deadline = tokio::time::Instant::now()
            + Duration::from_millis(duration_ms as u64);

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
                        let ev = VoiceTestEvent::Frame { peak: peak_norm, rms: rms_norm };
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

        {
            let mut s = state_task.lock().await;
            s.mic_stream = None;
        }
        emit(&state_task, VoiceTestEvent::RecordingFinished).await;

        let output = match ensure_output(&state_task, &output_device_id).await {
            Ok(v) => v,
            Err(e) => {
                crate::diag!("[voice-test] record-playback output init failed: {e}");
                return;
            }
        };
        let (buf, out_rate, out_channels) = output;

        emit(&state_task, VoiceTestEvent::PlaybackStarted).await;

        // Assumes mic_rate == out_rate (both 48 kHz).
        {
            let mut b = buf.lock().unwrap();
            for &sample in &recorded {
                let f = sample as f32 / 32_768.0;
                for _ in 0..out_channels {
                    b.push_back(f);
                }
            }
        }

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

/// Play a test tone: `"sweep"` (200 to 2000 Hz over 1.5 s) or `"chime"` (C5-E5-G5
/// arpeggio). Cancels any other test.
pub async fn play_test_tone(
    output_device_id: String,
    kind: String,
    state: &Arc<AppState>,
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

        {
            let mut b = buf.lock().unwrap();
            for &s in &samples {
                for _ in 0..channels {
                    b.push_back(s);
                }
            }
        }

        tokio::time::sleep(Duration::from_millis(total_duration_ms + 250)).await;

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

/// Stop any tone or record-playback; a mic test is untouched. Idempotent.
pub async fn stop_test_playback(state: &Arc<AppState>) -> Result<()> {
    let mut s = state.voice_test.lock().await;
    if let Some(t) = s.playback_task.take() {
        t.abort();
    }
    s.output_stream = None;
    s.output_buf = None;
    s.output_device_name = None;
    Ok(())
}

/// Linear sine sweep with a short envelope to avoid edge clicks.
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

/// C5-E5-G5 arpeggio, 200 ms per note.
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
            // Exponential decay per note so it sounds plucked.
            let env = (-4.0 * i as f32 / note_samples as f32).exp();
            out.push(phase.sin() * 0.15 * env);
        }
    }
    out
}
