//! Pull-model playback mixer.
//!
//! Each remote track's drain task writes 48 kHz mono into its own lock-free
//! SPSC ring; the cpal output callback pulls whole 10 ms frames from every
//! ring, sums them, limits, taps the mix for AEC and resamples to the device
//! rate. There is no intermediate clock: the sound card's own callback is the
//! only consumer, so there is nothing to drift against except each track's
//! source, and that drift is corrected a frame at a time (see `TrackSlot`).

use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
};

use crate::commands::{
    screenshare::self_echo::SelfEchoSlot,
    voice_apm::{self, Processor as ApmProcessor},
};

use super::limiter::SoftLimiter;
use super::resampler::{FrameResampler, MIX_FRAME_SAMPLES, MIX_RATE_HZ};

/// Frames a track must have queued before it starts (or restarts) playing.
pub(crate) const PREROLL_FRAMES: usize = 2;
/// Above this many queued frames a *quiet* frame is dropped to pull the
/// track back toward the pre-roll; the source clock is running fast.
const HIGH_WATER_FRAMES: usize = 6;
/// Above this a frame is dropped whatever its level.
const HARD_CAP_FRAMES: usize = 12;
/// Ring capacity per track; the drain task counts what doesn't fit.
pub(crate) const RING_FRAMES: usize = 30;
/// Length of the ramp applied when a track (re)starts after a pre-roll, so a
/// mid-waveform resume doesn't click.
const FADE_IN_SAMPLES: usize = 48;
/// Peak below which a frame counts as quiet for drift dropping (-40 dBFS).
const QUIET_PEAK: f32 = 0.01;

/// The producing end of one track's ring, owned by its drain task.
pub(crate) struct TrackFeed {
    producer: rtrb::Producer<f32>,
    /// Samples that didn't fit because the output isn't consuming.
    dropped: Arc<AtomicU64>,
}

impl TrackFeed {
    /// Queue one decoded frame; whatever doesn't fit is dropped and counted.
    pub(crate) fn push(&mut self, frame: &[f32]) {
        let (_, remainder) = self.producer.push_partial_slice(frame);
        if !remainder.is_empty() {
            self.dropped
                .fetch_add(remainder.len() as u64, Ordering::Relaxed);
        }
    }

    pub(crate) fn dropped_samples(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TrackStats {
    /// Frames concealed because the ring ran dry mid-playback.
    pub underruns: u32,
    /// Frames discarded by drift control.
    pub drift_drops: u32,
}

/// The consuming end of one track's ring plus its playback state; lives in
/// `MixerShared::tracks` and is only touched from the output callback.
pub(crate) struct TrackSlot {
    pub(crate) key: String,
    pub(crate) user_id: String,
    consumer: rtrb::Consumer<f32>,
    /// f32 bits; written by `set_remote_user_volume`.
    gain: Arc<AtomicU32>,
    primed: bool,
    /// Last frame played, for whole-frame concealment.
    last: [f32; MIX_FRAME_SAMPLES],
    has_last: bool,
    /// Samples of fade-in still owed after a (re)start.
    fade_in_left: usize,
    pub(crate) stats: TrackStats,
    scratch: [f32; MIX_FRAME_SAMPLES],
}

impl TrackSlot {
    pub(crate) fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }

    pub(crate) fn set_gain(&self, gain: f32) {
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
    }

    /// Discard queued audio beyond the pre-roll so a track doesn't resume
    /// with a stale backlog (output stream restarted, or first callback).
    fn trim_to_preroll(&mut self) {
        let keep = PREROLL_FRAMES * MIX_FRAME_SAMPLES;
        let avail = self.consumer.slots();
        if avail > keep {
            let drop = avail - keep;
            if let Ok(chunk) = self.consumer.read_chunk(drop) {
                chunk.commit_all();
            }
        }
    }

    fn discard_frame(&mut self) {
        if let Ok(chunk) = self.consumer.read_chunk(MIX_FRAME_SAMPLES) {
            chunk.commit_all();
        }
        self.stats.drift_drops += 1;
    }

    /// Peak of the next queued frame without consuming it (an uncommitted
    /// chunk leaves the ring untouched).
    fn peek_peak(&mut self) -> f32 {
        match self.consumer.read_chunk(MIX_FRAME_SAMPLES) {
            Ok(chunk) => {
                let (a, b) = chunk.as_slices();
                a.iter().chain(b.iter()).fold(0.0f32, |m, s| m.max(s.abs()))
            }
            Err(_) => 0.0,
        }
    }

    /// Add this track's next 10 ms to `mix`.
    fn mix_into(&mut self, mix: &mut [f32; MIX_FRAME_SAMPLES]) {
        let avail = self.consumer.slots();
        if !self.primed {
            if avail < PREROLL_FRAMES * MIX_FRAME_SAMPLES {
                return;
            }
            self.primed = true;
            self.fade_in_left = FADE_IN_SAMPLES;
        }
        let gain = self.gain();
        if avail < MIX_FRAME_SAMPLES {
            // Underrun: play the last frame once, fading to silence, then
            // wait for a fresh pre-roll. Never a partial frame of zeros.
            self.stats.underruns += 1;
            self.primed = false;
            if self.has_last {
                self.has_last = false;
                let n = MIX_FRAME_SAMPLES as f32;
                for (i, (m, s)) in mix.iter_mut().zip(self.last.iter()).enumerate() {
                    let fade = 1.0 - (i as f32 / n);
                    *m += s * fade * gain;
                }
            }
            return;
        }
        if avail >= HARD_CAP_FRAMES * MIX_FRAME_SAMPLES
            || (avail >= HIGH_WATER_FRAMES * MIX_FRAME_SAMPLES && self.peek_peak() < QUIET_PEAK)
        {
            self.discard_frame();
        }
        let Ok(chunk) = self.consumer.read_chunk(MIX_FRAME_SAMPLES) else {
            return;
        };
        let (a, b) = chunk.as_slices();
        self.scratch[..a.len()].copy_from_slice(a);
        self.scratch[a.len()..a.len() + b.len()].copy_from_slice(b);
        chunk.commit_all();
        if self.fade_in_left > 0 {
            let total = FADE_IN_SAMPLES as f32;
            for s in self.scratch.iter_mut() {
                if self.fade_in_left == 0 {
                    break;
                }
                let done = FADE_IN_SAMPLES - self.fade_in_left;
                *s *= done as f32 / total;
                self.fade_in_left -= 1;
            }
        }
        for (m, s) in mix.iter_mut().zip(self.scratch.iter()) {
            *m += s * gain;
        }
        self.last.copy_from_slice(&self.scratch);
        self.has_last = true;
    }
}

/// Everything the output callback and the control plane share. Outlives any
/// one cpal stream, so switching the speaker never touches the tracks.
pub(crate) struct MixerShared {
    pub(crate) tracks: Mutex<Vec<TrackSlot>>,
    pub(crate) deafened: Arc<AtomicBool>,
    /// The session's APM, for the AEC render reference; `None` outside a
    /// call or on a listen-only join.
    apm: Mutex<Option<Arc<ApmProcessor>>>,
    pub(crate) self_echo: SelfEchoSlot,
    /// Set by a fresh output stream so its first callback trims every ring.
    restart_pending: AtomicBool,
}

impl MixerShared {
    pub(crate) fn new(deafened: Arc<AtomicBool>, self_echo: SelfEchoSlot) -> Self {
        Self {
            tracks: Mutex::new(Vec::with_capacity(16)),
            deafened,
            apm: Mutex::new(None),
            self_echo,
            restart_pending: AtomicBool::new(true),
        }
    }

    pub(crate) fn set_apm(&self, apm: Option<Arc<ApmProcessor>>) {
        *self.apm.lock().unwrap() = apm;
    }

    /// Create a track's ring, park its consumer with the mixer and hand the
    /// producer to the drain task.
    pub(crate) fn add_track(&self, key: String, user_id: String, gain: f32) -> TrackFeed {
        let (producer, consumer) = rtrb::RingBuffer::<f32>::new(RING_FRAMES * MIX_FRAME_SAMPLES);
        let dropped = Arc::new(AtomicU64::new(0));
        let slot = TrackSlot {
            key: key.clone(),
            user_id,
            consumer,
            gain: Arc::new(AtomicU32::new(gain.to_bits())),
            primed: false,
            last: [0.0; MIX_FRAME_SAMPLES],
            has_last: false,
            fade_in_left: 0,
            stats: TrackStats::default(),
            scratch: [0.0; MIX_FRAME_SAMPLES],
        };
        let mut tracks = self.tracks.lock().unwrap();
        tracks.retain(|t| t.key != key);
        tracks.push(slot);
        TrackFeed { producer, dropped }
    }

    /// Drop a track's slot; returns its stats for the leave log.
    pub(crate) fn remove_track(&self, key: &str) -> Option<TrackStats> {
        let mut tracks = self.tracks.lock().unwrap();
        let idx = tracks.iter().position(|t| t.key == key)?;
        Some(tracks.swap_remove(idx).stats)
    }

    pub(crate) fn clear_tracks(&self) {
        self.tracks.lock().unwrap().clear();
    }

    /// Apply a gain to every track of `user_id` (new tracks inherit it via
    /// `add_track`).
    pub(crate) fn set_user_gain(&self, user_id: &str, gain: f32) {
        for t in self.tracks.lock().unwrap().iter() {
            if t.user_id == user_id {
                t.set_gain(gain);
            }
        }
    }

    /// Mark that the next callback belongs to a freshly opened stream.
    pub(crate) fn mark_restart(&self) {
        self.restart_pending.store(true, Ordering::Release);
    }

    pub(crate) fn lock_tracks(&self) -> MutexGuard<'_, Vec<TrackSlot>> {
        self.tracks.lock().unwrap_or_else(|p| p.into_inner())
    }
}

/// Per-output-stream mixing state; one is created for each cpal stream and
/// driven from its callback.
pub(crate) struct OutputMixer {
    shared: Arc<MixerShared>,
    channels: usize,
    mix: [f32; MIX_FRAME_SAMPLES],
    limiter: SoftLimiter,
    resampler: FrameResampler,
    /// One 10 ms frame at the device rate.
    device_frame: Vec<f32>,
    /// Device-rate mono samples rendered but not yet handed to the device.
    leftover: VecDeque<f32>,
}

impl OutputMixer {
    pub(crate) fn new(shared: Arc<MixerShared>, device_rate: u32, channels: u32) -> Self {
        let resampler = FrameResampler::new(MIX_RATE_HZ, device_rate);
        let device_len = resampler.dst_frame_samples();
        shared.mark_restart();
        Self {
            shared,
            channels: channels.max(1) as usize,
            mix: [0.0; MIX_FRAME_SAMPLES],
            limiter: SoftLimiter::new(MIX_RATE_HZ),
            resampler,
            device_frame: vec![0.0; device_len],
            leftover: VecDeque::with_capacity(device_len * 2),
        }
    }

    /// Render one 10 ms mix frame into `self.mix`: sum, limit, deafen, tap.
    fn render_mix_frame(&mut self, tracks: &mut [TrackSlot]) {
        for s in self.mix.iter_mut() {
            *s = 0.0;
        }
        for t in tracks.iter_mut() {
            t.mix_into(&mut self.mix);
        }
        self.limiter.process(&mut self.mix);
        if self.shared.deafened.load(Ordering::Relaxed) {
            for s in self.mix.iter_mut() {
                *s = 0.0;
            }
        }
        // What is about to hit the speaker is the AEC reference, deafened or not.
        if let Ok(apm) = self.shared.apm.try_lock() {
            if let Some(apm) = apm.as_ref() {
                let _ = voice_apm::analyze_render(apm, &self.mix, MIX_FRAME_SAMPLES);
            }
        }
        crate::commands::screenshare::self_echo::analyze_render(
            &self.shared.self_echo,
            &self.mix,
            MIX_RATE_HZ,
        );
    }

    /// Fill an interleaved device buffer; called from the cpal callback.
    pub(crate) fn fill(&mut self, out: &mut [f32]) {
        let shared = Arc::clone(&self.shared);
        let mut tracks = shared.lock_tracks();
        if shared.restart_pending.swap(false, Ordering::AcqRel) {
            for t in tracks.iter_mut() {
                t.trim_to_preroll();
                t.primed = false;
            }
            self.leftover.clear();
        }
        let channels = self.channels;
        for frame in out.chunks_mut(channels) {
            if self.leftover.is_empty() {
                self.render_mix_frame(&mut tracks);
                let mut device_frame = std::mem::take(&mut self.device_frame);
                self.resampler.resample_f32(&self.mix, &mut device_frame);
                self.leftover.extend(device_frame.iter().copied());
                self.device_frame = device_frame;
            }
            let s = self.leftover.pop_front().unwrap_or(0.0);
            for ch in frame.iter_mut() {
                *ch = s;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Arc<MixerShared> {
        Arc::new(MixerShared::new(
            Arc::new(AtomicBool::new(false)),
            Arc::new(Mutex::new(None)),
        ))
    }

    fn frame_of(v: f32) -> Vec<f32> {
        vec![v; MIX_FRAME_SAMPLES]
    }

    /// Render `frames` × 10 ms of mono 48 kHz output.
    fn render(mixer: &mut OutputMixer, frames: usize) -> Vec<f32> {
        let mut out = vec![0.0f32; frames * MIX_FRAME_SAMPLES];
        mixer.fill(&mut out);
        out
    }

    #[test]
    fn track_waits_for_preroll_then_plays_at_its_gain() {
        let sh = shared();
        let mut feed = sh.add_track("a-1".into(), "a".into(), 0.5);
        let mut m = OutputMixer::new(Arc::clone(&sh), MIX_RATE_HZ, 1);
        feed.push(&frame_of(0.4));
        // One frame queued < pre-roll: silence, nothing consumed.
        assert!(render(&mut m, 1).iter().all(|s| *s == 0.0));
        feed.push(&frame_of(0.4));
        let out = render(&mut m, 2);
        // First FADE_IN_SAMPLES ramp up; after that it's 0.4 × 0.5.
        assert!((out[FADE_IN_SAMPLES] - 0.2).abs() < 1e-6);
        assert!((out[MIX_FRAME_SAMPLES + 10] - 0.2).abs() < 1e-6);
        assert_eq!(out[0], 0.0);
    }

    #[test]
    fn two_tracks_sum_and_the_limiter_holds_full_scale() {
        let sh = shared();
        let mut a = sh.add_track("a-1".into(), "a".into(), 1.0);
        let mut b = sh.add_track("b-1".into(), "b".into(), 1.0);
        let mut m = OutputMixer::new(Arc::clone(&sh), MIX_RATE_HZ, 1);
        for _ in 0..4 {
            a.push(&frame_of(0.3));
            b.push(&frame_of(0.2));
        }
        let out = render(&mut m, 2);
        assert!((out[MIX_FRAME_SAMPLES + 10] - 0.5).abs() < 1e-6);
        for _ in 0..4 {
            a.push(&frame_of(0.9));
            b.push(&frame_of(0.9));
        }
        let out = render(&mut m, 4);
        assert!(out.iter().all(|s| s.abs() <= 1.0));
        assert!(out[3 * MIX_FRAME_SAMPLES + 10] > 0.8);
    }

    #[test]
    fn underrun_conceals_with_a_faded_repeat_never_partial_zeros() {
        let sh = shared();
        let mut feed = sh.add_track("a-1".into(), "a".into(), 1.0);
        let mut m = OutputMixer::new(Arc::clone(&sh), MIX_RATE_HZ, 1);
        feed.push(&frame_of(0.5));
        feed.push(&frame_of(0.5));
        // Leave half a frame queued: a partial-fill design would emit it.
        feed.push(&frame_of(0.5)[..MIX_FRAME_SAMPLES / 2]);
        let out = render(&mut m, 4);
        let f = |i: usize| &out[i * MIX_FRAME_SAMPLES..(i + 1) * MIX_FRAME_SAMPLES];
        assert!((f(1)[10] - 0.5).abs() < 1e-6);
        // Frame 2 is the concealment: a linear fade from the last frame to 0.
        assert!((f(2)[0] - 0.5).abs() < 1e-6);
        assert!((f(2)[MIX_FRAME_SAMPLES / 2] - 0.25).abs() < 2e-3);
        assert!(f(2)[MIX_FRAME_SAMPLES - 1] < 0.01);
        // Frame 3: unprimed again, so silence even though 240 samples wait.
        assert!(f(3).iter().all(|s| *s == 0.0));
        let stats = sh.remove_track("a-1").unwrap();
        assert_eq!(stats.underruns, 1);
    }

    #[test]
    fn drift_control_drops_a_quiet_frame_above_the_high_water_mark() {
        let sh = shared();
        let mut feed = sh.add_track("a-1".into(), "a".into(), 1.0);
        let mut m = OutputMixer::new(Arc::clone(&sh), MIX_RATE_HZ, 1);
        // Loud frames keep queueing past the high-water mark: never dropped.
        for _ in 0..HIGH_WATER_FRAMES + 2 {
            feed.push(&frame_of(0.3));
        }
        render(&mut m, 1);
        assert_eq!(sh.lock_tracks()[0].stats.drift_drops, 0);
        // A quiet frame at the head with the backlog still high is dropped.
        let _ = render(&mut m, HIGH_WATER_FRAMES + 1);
        for _ in 0..HIGH_WATER_FRAMES + 1 {
            feed.push(&frame_of(0.0));
        }
        render(&mut m, 1);
        assert_eq!(sh.lock_tracks()[0].stats.drift_drops, 1);
        // Past the hard cap even loud frames go.
        for _ in 0..HARD_CAP_FRAMES + 1 {
            feed.push(&frame_of(0.3));
        }
        render(&mut m, 1);
        assert_eq!(sh.lock_tracks()[0].stats.drift_drops, 2);
    }

    #[test]
    fn deafen_silences_the_output() {
        let sh = shared();
        let mut feed = sh.add_track("a-1".into(), "a".into(), 1.0);
        let mut m = OutputMixer::new(Arc::clone(&sh), MIX_RATE_HZ, 2);
        for _ in 0..4 {
            feed.push(&frame_of(0.3));
        }
        sh.deafened.store(true, Ordering::Relaxed);
        let mut out = vec![0.0f32; 2 * 2 * MIX_FRAME_SAMPLES];
        m.fill(&mut out);
        assert!(out.iter().all(|s| *s == 0.0));
    }

    #[test]
    fn a_new_output_stream_trims_the_backlog_to_the_preroll() {
        let sh = shared();
        let mut feed = sh.add_track("a-1".into(), "a".into(), 1.0);
        for _ in 0..20 {
            feed.push(&frame_of(0.3));
        }
        let mut m = OutputMixer::new(Arc::clone(&sh), MIX_RATE_HZ, 1);
        render(&mut m, 1);
        let queued = sh.lock_tracks()[0].consumer.slots();
        assert_eq!(queued, (PREROLL_FRAMES - 1) * MIX_FRAME_SAMPLES);
    }

    #[test]
    fn resamples_to_a_44100_device_and_spans_odd_period_sizes() {
        let sh = shared();
        let mut feed = sh.add_track("a-1".into(), "a".into(), 1.0);
        let mut m = OutputMixer::new(Arc::clone(&sh), 44_100, 2);
        // The first callback of a new stream trims backlogs; get it over with.
        m.fill(&mut []);
        for _ in 0..10 {
            feed.push(&frame_of(0.25));
        }
        // 1000 stereo frames: not a multiple of 441, so leftover carries over.
        let mut out = vec![0.0f32; 2 * 1000];
        m.fill(&mut out);
        let mut out2 = vec![0.0f32; 2 * 1000];
        m.fill(&mut out2);
        let tail = &out2[2 * 500..];
        let mean = tail.iter().sum::<f32>() / tail.len() as f32;
        assert!((mean - 0.25).abs() < 0.02, "mean {mean}");
        assert_eq!(out2[2 * 700], out2[2 * 700 + 1]);
    }

    #[test]
    fn feed_counts_what_a_full_ring_drops() {
        let sh = shared();
        let mut feed = sh.add_track("a-1".into(), "a".into(), 1.0);
        for _ in 0..RING_FRAMES + 3 {
            feed.push(&frame_of(0.1));
        }
        assert_eq!(feed.dropped_samples(), 3 * MIX_FRAME_SAMPLES as u64);
    }

    #[test]
    fn user_gain_applies_to_every_track_of_that_user() {
        let sh = shared();
        let _a = sh.add_track("u-1".into(), "u".into(), 1.0);
        let _b = sh.add_track("u-2".into(), "u".into(), 1.0);
        let _c = sh.add_track("v-1".into(), "v".into(), 1.0);
        sh.set_user_gain("u", 0.25);
        let tracks = sh.lock_tracks();
        assert_eq!(tracks.iter().filter(|t| t.gain() == 0.25).count(), 2);
        assert_eq!(tracks.iter().find(|t| t.key == "v-1").unwrap().gain(), 1.0);
    }
}
