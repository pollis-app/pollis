//! The mic DSP thread: cpal period → ring → 48 kHz → RNNoise → APM → LiveKit.
//!
//! One `spawn_capture_task` serves both the join and a mid-call device
//! switch, so the two paths cannot drift apart again (#1540's lesson in
//! this file: the switch path once lost speaking detection and the meter).
//!
//! Gating is applied here, at the very end: a muted or PTT-idle session
//! still runs every stage and feeds the local meter, then hands LiveKit a
//! silent frame instead of nothing. Frames must keep flowing — libwebrtc's
//! encoder timeline is driven by captured frames, and with the source queue
//! at zero there is no silence padder any more — and sending zeros is what
//! lets Opus DTX collapse the stream to one packet every 400 ms.

use std::{
    borrow::Cow,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread::{self, Thread},
    time::{Duration, Instant},
};

use futures_util::FutureExt;
use libwebrtc::{audio_source::native::NativeAudioSource, prelude::AudioFrame};

use crate::{
    commands::{voice_apm, voice_apm::Processor as ApmProcessor, voice_denoiser},
    error::Result,
    sink::EventSink,
};

use super::levels::{BandAnalyzer, LevelsHub};
use super::resampler::{FrameResampler, MIX_FRAME_SAMPLES, MIX_RATE_HZ};
use super::streams::build_mic_stream;
use super::types::{SendableStream, VoiceEvent};

/// Ring capacity in mono samples; 400 ms at 48 kHz, more at lower rates.
const MIC_RING_SAMPLES: usize = 19_200;
/// Backlog above which the DSP thread discards down to `MIC_TRIM_MS`: the
/// thread stalled (or the machine did), and old audio is worth less than
/// low latency.
const MIC_MAX_BACKLOG_MS: usize = 100;
const MIC_TRIM_MS: usize = 20;
/// Speaking: two consecutive loud frames to trigger, 120 ms hold.
const SPEAK_PEAK: u16 = 1_000;
const SPEAK_HOLD_FRAMES: u32 = 12;

struct MicShared {
    /// Set once the DSP thread exists; the audio thread unparks it per period.
    waker: Mutex<Option<Thread>>,
    /// Samples that didn't fit the ring (consumer stalled).
    dropped: AtomicU64,
}

/// Producer side; lives inside the cpal input callback.
struct MicFeed {
    producer: rtrb::Producer<i16>,
    shared: Arc<MicShared>,
}

impl MicFeed {
    fn push(&mut self, mono: &[i16]) {
        let (_, remainder) = self.producer.push_partial_slice(mono);
        if !remainder.is_empty() {
            self.shared
                .dropped
                .fetch_add(remainder.len() as u64, Ordering::Relaxed);
        }
        if let Ok(guard) = self.shared.waker.try_lock() {
            if let Some(t) = guard.as_ref() {
                t.unpark();
            }
        }
    }
}

/// Consumer side; handed to `spawn_capture_task`.
pub(crate) struct MicSource {
    consumer: rtrb::Consumer<i16>,
    /// Device rate the ring carries.
    pub(crate) sample_rate: u32,
    shared: Arc<MicShared>,
}

/// Open the mic on `device` and return its stream plus the ring's consumer.
pub(crate) fn open_mic(device: &cpal::Device) -> Result<(SendableStream, MicSource)> {
    let (producer, consumer) = rtrb::RingBuffer::<i16>::new(MIC_RING_SAMPLES);
    let shared = Arc::new(MicShared {
        waker: Mutex::new(None),
        dropped: AtomicU64::new(0),
    });
    let mut feed = MicFeed {
        producer,
        shared: Arc::clone(&shared),
    };
    let (stream, sample_rate) = build_mic_stream(device, move |mono, _rate| feed.push(mono))?;
    Ok((
        stream,
        MicSource {
            consumer,
            sample_rate,
            shared,
        },
    ))
}

/// Handle to the DSP thread; dropping or `abort`ing stops it.
pub struct CaptureTask {
    stop: Arc<AtomicBool>,
    thread: Thread,
    local_identity: String,
}

impl CaptureTask {
    pub fn abort(self) {
        self.stop.store(true, Ordering::Release);
        self.thread.unpark();
    }

    /// The identity this thread's speaking/meter events carry, so a device
    /// switch can start its successor under the same one.
    pub(crate) fn local_identity(&self) -> &str {
        &self.local_identity
    }
}

impl Drop for CaptureTask {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.thread.unpark();
    }
}

/// Everything the DSP thread needs; cloned out of `VoiceState` by the caller
/// so the thread never takes the voice lock.
pub(crate) struct CaptureParams {
    pub(crate) source: MicSource,
    pub(crate) audio_source: NativeAudioSource,
    pub(crate) apm: Option<Arc<ApmProcessor>>,
    pub(crate) denoiser: Arc<Mutex<Option<voice_denoiser::DenoiserStage>>>,
    pub(crate) transmit_muted: Arc<AtomicBool>,
    pub(crate) levels: Arc<LevelsHub>,
    pub(crate) sink: Option<Arc<dyn EventSink<VoiceEvent>>>,
    pub(crate) local_identity: String,
}

pub(crate) fn spawn_capture_task(params: CaptureParams) -> Result<CaptureTask> {
    let stop = Arc::new(AtomicBool::new(false));
    let stop_for_thread = Arc::clone(&stop);
    let shared = Arc::clone(&params.source.shared);
    let local_identity = params.local_identity.clone();
    let handle = thread::Builder::new()
        .name("pollis-mic-dsp".into())
        .spawn(move || run(params, stop_for_thread))
        .map_err(|e| anyhow::anyhow!("spawn mic DSP thread: {e}"))?;
    let thread = handle.thread().clone();
    *shared.waker.lock().unwrap_or_else(|p| p.into_inner()) = Some(thread.clone());
    // The thread outlives the handle; stop is signalled through `stop`.
    Ok(CaptureTask {
        stop,
        thread,
        local_identity,
    })
}

fn run(mut p: CaptureParams, stop: Arc<AtomicBool>) {
    let src_rate = p.source.sample_rate;
    let mut resampler = FrameResampler::new(src_rate, MIX_RATE_HZ);
    let src_len = resampler.src_frame_samples();
    let mut src_buf = vec![0i16; src_len];
    let mut chunk = [0i16; MIX_FRAME_SAMPLES];
    let mut apm_scratch: Vec<f32> = Vec::with_capacity(MIX_FRAME_SAMPLES);
    let mut analyzer = BandAnalyzer::new(MIX_RATE_HZ);
    let max_backlog = src_rate as usize * MIC_MAX_BACKLOG_MS / 1_000;
    let trim_to = src_rate as usize * MIC_TRIM_MS / 1_000;

    let mut onset_frames: u32 = 0;
    let mut speak_hold: u32 = 0;
    let mut is_speaking = false;
    let mut trimmed_total: u64 = 0;
    let mut last_report = Instant::now();
    let mut capture_errors: u64 = 0;

    crate::diag!("[voice] mic DSP thread up: {src_rate} Hz → {MIX_RATE_HZ} Hz");

    while !stop.load(Ordering::Acquire) {
        let backlog = p.source.consumer.slots();
        if backlog > max_backlog {
            let drop = backlog - trim_to;
            if let Ok(chunk) = p.source.consumer.read_chunk(drop) {
                chunk.commit_all();
            }
            trimmed_total += drop as u64;
        }
        while p.source.consumer.slots() >= src_len && !stop.load(Ordering::Acquire) {
            match p.source.consumer.read_chunk(src_len) {
                Ok(c) => {
                    let (a, b) = c.as_slices();
                    src_buf[..a.len()].copy_from_slice(a);
                    src_buf[a.len()..a.len() + b.len()].copy_from_slice(b);
                    c.commit_all();
                }
                Err(_) => break,
            }
            chunk.copy_from_slice(resampler.resample(&src_buf));

            // RNNoise first so APM's NS/AGC adapt to voice, not clicks.
            if let Ok(mut guard) = p.denoiser.lock() {
                if let Some(d) = guard.as_mut() {
                    d.process(&mut chunk);
                }
            }
            if let Some(apm) = &p.apm {
                if let Err(e) = voice_apm::run_capture_into(apm, &mut chunk, &mut apm_scratch) {
                    capture_errors += 1;
                    if capture_errors == 1 {
                        crate::diag!("[voice] APM capture error (frame dropped): {e}");
                    }
                    continue;
                }
            }

            let peak = chunk.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
            let transmitting = !p.transmit_muted.load(Ordering::Relaxed);

            // Meter: always, muted or not — it is a local-only signal.
            analyzer.process(&chunk);
            if let Some(ev) = p
                .levels
                .update(&p.local_identity, analyzer.levels(), Instant::now())
            {
                if let Some(ch) = &p.sink {
                    let _ = ch.send(ev);
                }
            }

            if peak > SPEAK_PEAK {
                onset_frames += 1;
                if onset_frames >= 2 {
                    speak_hold = SPEAK_HOLD_FRAMES;
                }
            } else {
                onset_frames = 0;
                speak_hold = speak_hold.saturating_sub(1);
            }
            // The ring only lights while others can hear you.
            let now_speaking = transmitting && speak_hold > 0;
            if now_speaking != is_speaking {
                is_speaking = now_speaking;
                if let Some(ch) = &p.sink {
                    let ev = if is_speaking {
                        VoiceEvent::SpeakingStarted {
                            identity: p.local_identity.clone(),
                        }
                    } else {
                        VoiceEvent::SpeakingStopped {
                            identity: p.local_identity.clone(),
                        }
                    };
                    let _ = ch.send(ev);
                }
            }

            if !transmitting {
                chunk.fill(0);
            }
            let frame = AudioFrame {
                data: Cow::Borrowed(&chunk[..]),
                sample_rate: MIX_RATE_HZ,
                num_channels: 1,
                samples_per_channel: MIX_FRAME_SAMPLES as u32,
            };
            // The unbuffered source completes synchronously; `None` would
            // mean it started pending, which this configuration cannot.
            match p.audio_source.capture_frame(&frame).now_or_never() {
                Some(Ok(())) => {}
                Some(Err(e)) => {
                    capture_errors += 1;
                    if capture_errors == 1 {
                        crate::diag!("[voice] capture_frame error: {e:?}");
                    }
                }
                None => {
                    capture_errors += 1;
                    if capture_errors == 1 {
                        crate::diag!("[voice] capture_frame unexpectedly pending; frame dropped");
                    }
                }
            }
        }

        if last_report.elapsed() >= Duration::from_secs(30) {
            last_report = Instant::now();
            let dropped = p.source.shared.dropped.load(Ordering::Relaxed);
            if dropped > 0 || trimmed_total > 0 || capture_errors > 0 {
                crate::diag!(
                    "[voice] mic ring: {dropped} samples dropped at the device, \
                     {trimmed_total} trimmed as backlog, {capture_errors} capture errors"
                );
            }
        }
        thread::park_timeout(Duration::from_millis(20));
    }

    p.levels.remove(&p.local_identity);
    if is_speaking {
        if let Some(ch) = &p.sink {
            let _ = ch.send(VoiceEvent::SpeakingStopped {
                identity: p.local_identity.clone(),
            });
        }
    }
    crate::diag!("[voice] mic DSP thread down");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_scale_negative_sample_does_not_overflow_the_peak() {
        let chunk = [i16::MIN, 0, 12, -32_767];
        let peak = chunk.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        assert_eq!(peak, 32_768);
        assert!(peak > SPEAK_PEAK);
    }
}
