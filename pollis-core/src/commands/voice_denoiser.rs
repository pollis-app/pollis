//! RNNoise (via [`nnnoiseless`]) denoiser, run before APM in the capture path
//! when Click Suppression is on: APM's NS misses fast transients like typing,
//! which RNNoise was trained on.
//!
//! ```text
//!   cpal mic ─→ rebuffer to 10ms i16 mono
//!     ─→ DenoiserStage::process       // ← this module (when enabled)
//!     ─→ APM::run_capture             // HPF + AGC + AEC, NS user-configurable
//!     ─→ LiveKit capture_frame
//! ```
//!
//! Constraints: 48 kHz only (the stage is disabled at any other rate); 480-sample
//! frames, which equal APM's 10 ms frame at 48 kHz; samples are i16-magnitude
//! f32, not normalised to [-1, 1].

use nnnoiseless::DenoiseState;

pub const FRAME_SAMPLES: usize = DenoiseState::FRAME_SIZE;

pub const REQUIRED_RATE_HZ: u32 = 48_000;

/// RNNoise state for one capture stream. Recreate on rejoin or device switch so
/// noise estimates don't carry over from a previous mic.
pub struct DenoiserStage {
    state: Box<DenoiseState<'static>>,
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

    /// Denoise a 10 ms i16 mono frame in place. A frame of the wrong size passes
    /// through untouched so a buffer-size bug elsewhere can't take voice down.
    pub fn process(&mut self, frame: &mut [i16]) {
        if frame.len() != FRAME_SAMPLES {
            debug_assert!(false, "denoiser frame size mismatch: {}", frame.len());
            return;
        }
        for (dst, &src) in self.in_buf.iter_mut().zip(frame.iter()) {
            *dst = src as f32;
        }
        // The VAD probability is unused; APM's AGC has its own.
        let _vad = self.state.process_frame(&mut self.out_buf, &self.in_buf);
        for (dst, &src) in frame.iter_mut().zip(self.out_buf.iter()) {
            *dst = src.clamp(-32_768.0, 32_767.0) as i16;
        }
    }
}
