//! RNNoise (via [`nnnoiseless`]) ML denoiser stage.
//!
//! Runs before APM to catch transients (typing, clicks) APM's NS misses.
//!
//! Pipeline position:
//!
//! ```text
//!   cpal mic ─→ rebuffer to 10ms i16 mono
//!     ─→ DenoiserStage::process       // ← this module (when enabled)
//!     ─→ APM::run_capture             // HPF + AGC + AEC, NS user-configurable
//!     ─→ LiveKit capture_frame
//! ```
//!
//! - **48 kHz only** (as trained); disabled at other rates.
//! - **480-sample frames**, equal to APM's 10 ms frame at 48 kHz.
//! - Samples are `i16`-magnitude f32, not normalised.

use nnnoiseless::DenoiseState;

/// Number of mono samples in a single denoiser frame. Hardcoded by RNNoise.
pub const FRAME_SAMPLES: usize = DenoiseState::FRAME_SIZE;

/// Sample rate the denoiser model was trained on. Cannot be changed.
pub const REQUIRED_RATE_HZ: u32 = 48_000;

/// Stateful RNNoise; recreate per stream so old mic estimates don't carry over.
pub struct DenoiserStage {
    state: Box<DenoiseState<'static>>,
    /// Pre-allocated buffers so the per-frame hot path doesn't allocate.
    in_buf: Vec<f32>,
    out_buf: Vec<f32>,
}

impl Default for DenoiserStage {
    fn default() -> Self {
        Self::new()
    }
}

impl DenoiserStage {
    pub fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            in_buf: vec![0.0; FRAME_SAMPLES],
            out_buf: vec![0.0; FRAME_SAMPLES],
        }
    }

    /// Denoise a 10 ms frame in place; wrong-size frames pass through untouched.
    pub fn process(&mut self, frame: &mut [i16]) {
        if frame.len() != FRAME_SAMPLES {
            debug_assert!(false, "denoiser frame size mismatch: {}", frame.len());
            return;
        }
        // i16 → f32 (no normalisation — RNNoise expects i16 magnitude).
        for (dst, &src) in self.in_buf.iter_mut().zip(frame.iter()) {
            *dst = src as f32;
        }
        // VAD probability unused (APM's AGC has its own).
        let _vad = self.state.process_frame(&mut self.out_buf, &self.in_buf);
        // f32 → i16, clamped.
        for (dst, &src) in frame.iter_mut().zip(self.out_buf.iter()) {
            *dst = src.clamp(-32_768.0, 32_767.0) as i16;
        }
    }
}
