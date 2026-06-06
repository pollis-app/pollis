//! Per-source multi-band audio level analysis for the participant-tile
//! "live waveform" meter. We already compute a per-frame peak for speaking
//! detection (see `playback.rs` / `lifecycle.rs`) and throw it away; this
//! turns the same PCM into a small bank of band envelopes so the UI can
//! show a real, per-source meter instead of a static glyph.
//!
//! Deliberately cheap: a fixed bank of 2nd-order bandpass biquads (RBJ
//! cookbook, constant 0 dB peak gain) with a peak-hold envelope follower.
//! No FFT, no allocation in the hot path. For mono voice at 48 kHz this is
//! a few million flops/sec across all bands — negligible next to the codec
//! and APM. The result is decimated to ~20 Hz before it leaves Rust.

use std::f32::consts::PI;

/// Number of frequency bands. MUST stay in sync with the renderer
/// (`frontend/src/voice/audioLevels.ts` `BAND_COUNT`). Voice energy is
/// concentrated low/low-mid, so three wide bands capture it well — five
/// narrow bands left the top two near-dead for normal speech.
pub const BAND_COUNT: usize = 3;

/// Band centers across the voice-relevant range (Hz): fundamentals/low
/// formants, the main formant region, and consonant/sibilance energy.
const CENTERS_HZ: [f32; BAND_COUNT] = [250.0, 800.0, 2000.0];

/// Bandpass Q. Low Q = wide bands that integrate more energy → a meter
/// that actually moves at conversational levels.
const Q: f32 = 1.0;

/// i16 band-envelope magnitude that maps to a full bar. Each bandpass only
/// passes a slice of the signal, so per-band envelopes are a fraction of
/// the full-band peak — this reference is set low (with a perceptual curve
/// in `levels()`) so normal speech a foot from the mic fills the bars.
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

    /// Current per-band levels, normalized to 0..1 with a perceptual
    /// (sqrt) curve so quiet-but-audible speech reads as real movement
    /// instead of a barely-lifted floor.
    pub fn levels(&self) -> Vec<f32> {
        self.env
            .iter()
            .map(|&e| (e / REF).clamp(0.0, 1.0).sqrt())
            .collect()
    }
}
