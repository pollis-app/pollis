//! 10 ms-frame resampler at the two device edges of the voice pipeline.
//!
//! The pipeline runs at a fixed 48 kHz (`MIX_RATE_HZ`); only the cpal mic and
//! speaker streams are at whatever rate the device wants. This wraps
//! libwebrtc's sinc `PushResampler` (already linked for LiveKit), which works
//! on exact 10 ms mono frames and allocates only on first use — the primed
//! instance is safe to drive from the output callback.

use libwebrtc::native::audio_resampler::AudioResampler;

/// The internal pipeline rate: mic processing, RNNoise, APM, the mix and the
/// AEC render reference all run here, whatever the devices do.
pub(crate) const MIX_RATE_HZ: u32 = 48_000;

/// Samples in one 10 ms mono frame at [`MIX_RATE_HZ`].
pub(crate) const MIX_FRAME_SAMPLES: usize = (MIX_RATE_HZ / 100) as usize;

/// Samples in one 10 ms mono frame at `rate`.
pub(crate) const fn frame_samples(rate: u32) -> usize {
    (rate / 100) as usize
}

pub(crate) struct FrameResampler {
    /// `None` when `src_rate == dst_rate`: frames pass through untouched.
    inner: Option<AudioResampler>,
    src_rate: u32,
    dst_rate: u32,
    src_len: usize,
    dst_len: usize,
    /// i16 scratch for the f32 entry point.
    i16_in: Vec<i16>,
    /// Owned copy of the last output, so the f32 entry point can borrow it
    /// after the FFI slice's lifetime ends.
    out: Vec<i16>,
}

impl FrameResampler {
    pub(crate) fn new(src_rate: u32, dst_rate: u32) -> Self {
        let src_len = frame_samples(src_rate);
        let dst_len = frame_samples(dst_rate);
        let mut this = Self {
            inner: (src_rate != dst_rate).then(AudioResampler::default),
            src_rate,
            dst_rate,
            src_len,
            dst_len,
            i16_in: vec![0; src_len],
            out: vec![0; dst_len],
        };
        // First call initialises the sinc tables; do it here, not in the callback.
        let zeros = vec![0i16; src_len];
        let _ = this.resample(&zeros);
        this
    }

    pub(crate) fn src_frame_samples(&self) -> usize {
        self.src_len
    }

    pub(crate) fn dst_frame_samples(&self) -> usize {
        self.dst_len
    }

    /// Resample one 10 ms mono frame (`src.len() == src_frame_samples()`).
    ///
    /// Returns the 10 ms frame at the destination rate; the input itself when
    /// the rates match.
    pub(crate) fn resample<'a>(&'a mut self, src: &'a [i16]) -> &'a [i16] {
        debug_assert_eq!(src.len(), self.src_len, "resampler frame size mismatch");
        match self.inner.as_mut() {
            None => src,
            Some(r) => {
                let out = r.remix_and_resample(
                    src,
                    self.src_len as u32,
                    1,
                    self.src_rate,
                    1,
                    self.dst_rate,
                );
                let n = out.len().min(self.dst_len);
                self.out[..n].copy_from_slice(&out[..n]);
                for s in self.out[n..].iter_mut() {
                    *s = 0;
                }
                &self.out
            }
        }
    }

    /// f32 variant for the playback edge: `src.len() == src_frame_samples()`,
    /// `dst.len() == dst_frame_samples()`.
    pub(crate) fn resample_f32(&mut self, src: &[f32], dst: &mut [f32]) {
        debug_assert_eq!(dst.len(), self.dst_len, "resampler output size mismatch");
        if self.inner.is_none() {
            dst.copy_from_slice(src);
            return;
        }
        for (d, s) in self.i16_in.iter_mut().zip(src.iter()) {
            *d = (*s * 32_767.0).clamp(-32_768.0, 32_767.0) as i16;
        }
        let mut scratch = std::mem::take(&mut self.i16_in);
        let out = self.resample(&scratch);
        for (d, s) in dst.iter_mut().zip(out.iter()) {
            *d = f32::from(*s) / 32_768.0;
        }
        std::mem::swap(&mut self.i16_in, &mut scratch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine_i16(hz: f32, rate: u32, n: usize, phase0: usize) -> Vec<i16> {
        (0..n)
            .map(|i| {
                let t = (i + phase0) as f32 / rate as f32;
                (0.5 * (2.0 * std::f32::consts::PI * hz * t).sin() * 32_767.0) as i16
            })
            .collect()
    }

    fn zero_crossings(x: &[i16]) -> usize {
        x.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count()
    }

    fn rms(x: &[i16]) -> f32 {
        (x.iter().map(|s| f32::from(*s).powi(2)).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn same_rate_is_a_passthrough() {
        let mut r = FrameResampler::new(48_000, 48_000);
        let frame = sine_i16(1_000.0, 48_000, 480, 0);
        let out = r.resample(&frame);
        assert_eq!(out, &frame[..]);
        assert_eq!(r.dst_frame_samples(), 480);
    }

    #[test]
    fn upsamples_44100_to_48000_preserving_pitch_and_level() {
        let mut r = FrameResampler::new(44_100, 48_000);
        assert_eq!(r.src_frame_samples(), 441);
        let mut last = Vec::new();
        // Several frames so the sinc delay line is full of signal.
        for k in 0..20 {
            let frame = sine_i16(1_000.0, 44_100, 441, k * 441);
            last = r.resample(&frame).to_vec();
        }
        assert_eq!(last.len(), 480);
        // 1 kHz over 10 ms crosses zero 20 times.
        let zc = zero_crossings(&last);
        assert!((18..=22).contains(&zc), "zero crossings {zc}");
        let level = rms(&last) / (0.5 * 32_767.0 / 2f32.sqrt());
        assert!((0.9..=1.1).contains(&level), "level ratio {level}");
    }

    #[test]
    fn downsamples_48000_to_16000() {
        let mut r = FrameResampler::new(48_000, 16_000);
        let mut last = Vec::new();
        for k in 0..20 {
            let frame = sine_i16(500.0, 48_000, 480, k * 480);
            last = r.resample(&frame).to_vec();
        }
        assert_eq!(last.len(), 160);
        let zc = zero_crossings(&last);
        assert!((8..=12).contains(&zc), "zero crossings {zc}");
    }

    #[test]
    fn f32_entry_point_round_trips() {
        let mut r = FrameResampler::new(48_000, 44_100);
        let src: Vec<f32> = (0..480)
            .map(|i| 0.25 * (2.0 * std::f32::consts::PI * 1_000.0 * i as f32 / 48_000.0).sin())
            .collect();
        let mut dst = vec![0.0f32; 441];
        for _ in 0..10 {
            r.resample_f32(&src, &mut dst);
        }
        let peak = dst.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!((0.2..=0.3).contains(&peak), "peak {peak}");
    }
}
