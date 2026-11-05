//! Soft-knee peak limiter for the playback mix.
//!
//! Stacked participants can sum past full scale; a hard clamp turns that into
//! a buzz. This tracks the peak envelope (instant attack, exponential release)
//! and applies a gain that bends in smoothly over a knee around the threshold,
//! so overs are tamed without a waveform crease. Below the knee it is exactly
//! unity, bit for bit.

/// Threshold: -1 dBFS.
const THRESHOLD: f32 = 0.891_25;
/// Knee width in dB; the gain curve bends from `-KNEE_DB/2` to `+KNEE_DB/2`
/// around the threshold.
const KNEE_DB: f32 = 6.0;
/// Release time constant.
const RELEASE_SECS: f32 = 0.08;

pub(crate) struct SoftLimiter {
    env: f32,
    release: f32,
    /// Envelope below which the gain is exactly 1.0 (knee start), so the
    /// per-sample log/pow only runs on actual overs.
    knee_start: f32,
}

impl SoftLimiter {
    pub(crate) fn new(sample_rate: u32) -> Self {
        Self {
            env: 0.0,
            release: (-1.0 / (sample_rate as f32 * RELEASE_SECS)).exp(),
            knee_start: THRESHOLD * 10f32.powf(-KNEE_DB / 40.0),
        }
    }

    /// Gain (linear) for an envelope level, with the soft knee in dB.
    fn gain_for(&self, env: f32) -> f32 {
        if env <= self.knee_start {
            return 1.0;
        }
        let over_db = 20.0 * (env / THRESHOLD).log10();
        let half = KNEE_DB / 2.0;
        let reduction_db = if over_db >= half {
            over_db
        } else {
            let t = over_db + half;
            (t * t) / (2.0 * KNEE_DB)
        };
        10f32.powf(-reduction_db / 20.0)
    }

    /// Limit `frame` in place.
    pub(crate) fn process(&mut self, frame: &mut [f32]) {
        for s in frame.iter_mut() {
            let a = s.abs();
            self.env = if a > self.env {
                a
            } else {
                self.env * self.release
            };
            let g = self.gain_for(self.env);
            // Instant attack means no lookahead overshoot beyond the knee
            // curve itself; the clamp is a last-resort guard for NaN/inf input.
            *s = (*s * g).clamp(-1.0, 1.0);
        }
    }

    #[cfg(test)]
    fn envelope(&self) -> f32 {
        self.env
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(amp: f32, hz: f32, rate: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * hz * i as f32 / rate as f32).sin())
            .collect()
    }

    #[test]
    fn below_the_knee_is_bit_exact_unity() {
        let mut lim = SoftLimiter::new(48_000);
        let input = sine(0.5, 440.0, 48_000, 4_800);
        let mut out = input.clone();
        lim.process(&mut out);
        assert_eq!(input, out);
    }

    #[test]
    fn hot_signal_never_exceeds_full_scale_and_is_not_crushed() {
        let mut lim = SoftLimiter::new(48_000);
        let mut out = sine(2.0, 440.0, 48_000, 9_600);
        lim.process(&mut out);
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak <= 1.0, "peak {peak}");
        // Steady state on a 2× over: gain = T/env, so the peak lands on the threshold.
        let tail_peak = out[4_800..].iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(
            (tail_peak - THRESHOLD).abs() < 0.02,
            "tail peak {tail_peak}"
        );
    }

    #[test]
    fn gain_curve_is_monotonic_and_continuous_across_the_knee() {
        let lim = SoftLimiter::new(48_000);
        let mut prev_out = 0.0f32;
        let mut env = 0.3f32;
        while env < 3.0 {
            let out = env * lim.gain_for(env);
            assert!(
                out + 1e-4 >= prev_out,
                "output level must not fall as input rises ({env})"
            );
            prev_out = out;
            env += 0.005;
        }
        // At the top of the knee the reduction equals the hard curve exactly.
        let top = THRESHOLD * 10f32.powf(KNEE_DB / 40.0);
        assert!(((top * lim.gain_for(top)) - THRESHOLD).abs() < 1e-3);
    }

    #[test]
    fn releases_back_to_unity_after_a_burst() {
        let mut lim = SoftLimiter::new(48_000);
        let mut burst = sine(2.0, 440.0, 48_000, 480);
        lim.process(&mut burst);
        assert!(lim.envelope() > THRESHOLD);
        // 400 ms of quiet is five release time constants.
        let quiet = sine(0.1, 440.0, 48_000, 19_200);
        let mut out = quiet.clone();
        lim.process(&mut out);
        assert!(lim.envelope() < lim.knee_start);
        assert_eq!(out[out.len() - 480..], quiet[quiet.len() - 480..]);
    }
}
