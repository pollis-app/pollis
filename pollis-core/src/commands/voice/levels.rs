//! Per-source multi-band level meter for participant tiles.
//!
//! Cheap: a few RBJ bandpass biquads + peak-hold envelopes; no FFT or allocation.

use std::{
    f32::consts::PI,
    sync::Mutex,
    time::{Duration, Instant},
};

use super::types::{AudioBandsEntry, VoiceEvent};

/// Number of bands; MUST match `BAND_COUNT` in `frontend/src/voice/audioLevels.ts`.
pub const BAND_COUNT: usize = 3;

/// Band centers (Hz): low formants, main formants, sibilance.
const CENTERS_HZ: [f32; BAND_COUNT] = [250.0, 800.0, 2000.0];

/// Bandpass Q; low = wide bands, so the meter moves at speaking levels.
const Q: f32 = 1.0;

/// Band-envelope magnitude for a full bar; low because bands pass a slice.
const REF: f32 = 2000.0;

/// Direct-Form-I biquad. Coefficients are pre-normalized by a0.
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    /// RBJ bandpass with constant 0 dB peak gain.
    fn bandpass(sample_rate: u32, center_hz: f32, q: f32) -> Self {
        let w0 = 2.0 * PI * center_hz / sample_rate as f32;
        let (sin_w0, cos_w0) = w0.sin_cos();
        let alpha = sin_w0 / (2.0 * q);

        let b0 = alpha;
        let b1 = 0.0;
        let b2 = -alpha;
        let a0 = 1.0 + alpha;
        let a1 = -2.0 * cos_w0;
        let a2 = 1.0 - alpha;

        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }

    #[inline]
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// A bank of bandpass filters + per-band peak-hold envelopes.
pub struct BandAnalyzer {
    bands: Vec<Biquad>,
    env: [f32; BAND_COUNT],
    /// Per-sample release coefficient (~60 ms decay).
    decay: f32,
}

impl BandAnalyzer {
    pub fn new(sample_rate: u32) -> Self {
        let bands = CENTERS_HZ
            .iter()
            .map(|&c| Biquad::bandpass(sample_rate, c, Q))
            .collect();
        // exp(-1 / (fs * 0.06)) — ~60 ms to decay by 1/e.
        let decay = (-1.0 / (sample_rate as f32 * 0.06)).exp();
        Self {
            bands,
            env: [0.0; BAND_COUNT],
            decay,
        }
    }

    /// Feed one frame of mono i16 samples through the bank, updating the
    /// per-band envelopes (instant attack, slow release).
    pub fn process(&mut self, samples: &[i16]) {
        for &s in samples {
            let x = s as f32;
            for i in 0..BAND_COUNT {
                let mag = self.bands[i].process(x).abs();
                if mag > self.env[i] {
                    self.env[i] = mag;
                } else {
                    self.env[i] *= self.decay;
                }
            }
        }
    }

    /// Per-band levels in 0..1, sqrt-curved so quiet speech still moves.
    pub fn levels(&self) -> [f32; BAND_COUNT] {
        let mut out = [0.0; BAND_COUNT];
        for (o, &e) in out.iter_mut().zip(self.env.iter()) {
            *o = (e / REF).clamp(0.0, 1.0).sqrt();
        }
        out
    }
}

/// Peak magnitude of a frame. `unsigned_abs`, because `i16::abs` panics on a
/// full-scale negative sample and that killed drain tasks in the field.
pub(crate) fn frame_peak(samples: &[i16]) -> u16 {
    samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0)
}

/// Interval between `AudioBands` events.
pub(crate) const BANDS_EMIT_INTERVAL: Duration = Duration::from_millis(50);

/// Collects every source's latest band levels and coalesces them into one
/// `AudioBands` event per [`BANDS_EMIT_INTERVAL`], so N participants cost one
/// IPC message per tick instead of N × 20/s. A tick in which everyone is
/// silent is sent once (so meters fall to zero) and then suppressed until
/// something moves.
pub(crate) struct LevelsHub {
    inner: Mutex<HubInner>,
}

struct HubInner {
    entries: Vec<(String, [f32; BAND_COUNT])>,
    last_emit: Option<Instant>,
    prev_all_silent: bool,
}

impl Default for LevelsHub {
    fn default() -> Self {
        Self::new()
    }
}

impl LevelsHub {
    pub(crate) fn new() -> Self {
        Self {
            inner: Mutex::new(HubInner {
                entries: Vec::new(),
                last_emit: None,
                prev_all_silent: true,
            }),
        }
    }

    /// Record `identity`'s latest levels; returns the batched event when a
    /// tick is due (the caller sends it on the sink it already holds).
    pub(crate) fn update(
        &self,
        identity: &str,
        bands: [f32; BAND_COUNT],
        now: Instant,
    ) -> Option<VoiceEvent> {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        let rounded = bands.map(|v| (v * 100.0).round() / 100.0);
        match inner.entries.iter_mut().find(|(id, _)| id == identity) {
            Some((_, b)) => *b = rounded,
            None => inner.entries.push((identity.to_string(), rounded)),
        }
        let due = inner
            .last_emit
            .map(|t| now.duration_since(t) >= BANDS_EMIT_INTERVAL)
            .unwrap_or(true);
        if !due {
            return None;
        }
        let all_silent = inner
            .entries
            .iter()
            .all(|(_, b)| b.iter().all(|v| *v == 0.0));
        if all_silent && inner.prev_all_silent {
            return None;
        }
        inner.last_emit = Some(now);
        inner.prev_all_silent = all_silent;
        Some(VoiceEvent::AudioBands {
            levels: inner
                .entries
                .iter()
                .map(|(id, b)| AudioBandsEntry {
                    identity: id.clone(),
                    bands: b.to_vec(),
                })
                .collect(),
        })
    }

    /// Forget a source (its drain or capture task ended).
    pub(crate) fn remove(&self, identity: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.entries.retain(|(id, _)| id != identity);
        // A departure changes the picture; let the next update send it.
        inner.prev_all_silent = false;
    }

    pub(crate) fn clear(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.entries.clear();
        inner.last_emit = None;
        inner.prev_all_silent = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(ev: Option<VoiceEvent>) -> Vec<(String, Vec<f32>)> {
        match ev {
            Some(VoiceEvent::AudioBands { levels }) => {
                levels.into_iter().map(|e| (e.identity, e.bands)).collect()
            }
            Some(_) => panic!("wrong event"),
            None => panic!("no event"),
        }
    }

    #[test]
    fn frame_peak_survives_a_full_scale_negative_sample() {
        assert_eq!(frame_peak(&[0, i16::MIN, 12, -32_767]), 32_768);
        assert_eq!(frame_peak(&[]), 0);
        assert_eq!(frame_peak(&[-5, 3]), 5);
    }

    #[test]
    fn coalesces_sources_into_one_event_per_tick() {
        let hub = LevelsHub::new();
        let t0 = Instant::now();
        // First update emits immediately (nothing sent yet).
        let ev = hub.update("a", [0.5, 0.25, 0.0], t0);
        assert_eq!(entries(ev), vec![("a".to_string(), vec![0.5, 0.25, 0.0])]);
        // Within the interval: absorbed.
        assert!(hub
            .update("b", [0.1, 0.1, 0.1], t0 + Duration::from_millis(10))
            .is_none());
        assert!(hub
            .update("a", [0.6, 0.3, 0.1], t0 + Duration::from_millis(20))
            .is_none());
        // Next tick carries the latest of both.
        let ev = hub.update("b", [0.2, 0.2, 0.2], t0 + Duration::from_millis(50));
        let got = entries(ev);
        assert_eq!(got.len(), 2);
        assert!(got.contains(&("a".to_string(), vec![0.6, 0.3, 0.1])));
        assert!(got.contains(&("b".to_string(), vec![0.2, 0.2, 0.2])));
    }

    #[test]
    fn rounds_to_two_decimals() {
        let hub = LevelsHub::new();
        let ev = hub.update("a", [0.123_456, 0.999_9, 0.004], Instant::now());
        assert_eq!(entries(ev)[0].1, vec![0.12, 1.0, 0.0]);
    }

    #[test]
    fn all_silent_is_sent_once_then_suppressed_until_something_moves() {
        let hub = LevelsHub::new();
        let t0 = Instant::now();
        assert!(hub.update("a", [0.3, 0.0, 0.0], t0).is_some());
        // Silence after sound: one event so the meter falls.
        assert!(hub
            .update("a", [0.0; BAND_COUNT], t0 + Duration::from_millis(50))
            .is_some());
        // Still silent on later ticks: nothing.
        assert!(hub
            .update("a", [0.0; BAND_COUNT], t0 + Duration::from_millis(100))
            .is_none());
        assert!(hub
            .update("a", [0.0; BAND_COUNT], t0 + Duration::from_millis(500))
            .is_none());
        // Sound again: sent.
        assert!(hub
            .update("a", [0.2, 0.0, 0.0], t0 + Duration::from_millis(550))
            .is_some());
    }

    #[test]
    fn removed_sources_leave_the_batch() {
        let hub = LevelsHub::new();
        let t0 = Instant::now();
        hub.update("a", [0.3, 0.0, 0.0], t0);
        hub.update("b", [0.3, 0.0, 0.0], t0 + Duration::from_millis(1));
        hub.remove("a");
        let got = entries(hub.update("b", [0.3, 0.0, 0.0], t0 + Duration::from_millis(60)));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].0, "b");
    }
}
