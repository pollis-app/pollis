//! RNNoise (via [`nnnoiseless`]) ML denoiser stage.
//!
//! Runs *before* APM in the capture path when the user enables Click
//! Suppression. WebRTC APM's spectral NS handles stationary noise (fans,
//! hum) but misses fast transients — keyboard typing, mouse clicks, hard
//! consonants slamming the mic. RNNoise was specifically trained on
//! keyboard-typing samples and is the de-facto open-source answer to that
//! gap. (Discord uses Krisp, which is the proprietary tier above this.)
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
//! ## Constraints
//!
//! - **48 kHz only.** RNNoise's RNN was trained on 48 kHz; nnnoiseless
//!   inherits that. We disable the stage entirely when the mic comes up
//!   at any other rate (rare; Bluetooth SCO).
//! - **Frame size is fixed at 480 samples** (`DenoiseState::FRAME_SIZE`),
//!   which happens to equal APM's 10 ms frame at 48 kHz — no rebuffer.
//! - Sample magnitude convention is `i16-as-f32` (range −32768..=32767),
//!   not normalised [-1, 1]. We cast directly between i16 and f32.

use nnnoiseless::DenoiseState;

/// Number of mono samples in a single denoiser frame. Hardcoded by RNNoise.
pub const FRAME_SAMPLES: usize = DenoiseState::FRAME_SIZE;

/// Sample rate the denoiser model was trained on. Cannot be changed.
pub const REQUIRED_RATE_HZ: u32 = 48_000;

/// Stateful RNNoise instance. Owns the RNN hidden state for one capture
/// stream — drop and recreate on rejoin or device switch so the model
/// doesn't carry over noise estimates from a previous mic.
pub struct DenoiserStage {
    state: Box<DenoiseState<'static>>,
    /// Pre-allocated buffers so the per-frame hot path doesn't allocate.
    in_buf: Vec<f32>,
    out_buf: Vec<f32>,
}

impl DenoiserStage {
    pub fn new() -> Self {
        Self {
            state: DenoiseState::new(),
            in_buf: vec![0.0; FRAME_SAMPLES],
            out_buf: vec![0.0; FRAME_SAMPLES],
        }
    }

    /// Run RNNoise on a 10 ms i16 mono frame, in place. `frame.len()` must
    /// equal [`FRAME_SAMPLES`]; mismatched frames are silently passed
    /// through so a stray buffer-size bug elsewhere can't take voice down.
    pub fn process(&mut self, frame: &mut [i16]) {
        if frame.len() != FRAME_SAMPLES {
            debug_assert!(false, "denoiser frame size mismatch: {}", frame.len());
            return;
        }
        // i16 → f32 (no normalisation — RNNoise expects i16 magnitude).
        for (dst, &src) in self.in_buf.iter_mut().zip(frame.iter()) {
            *dst = src as f32;
        }
        // Returns voice activity probability; we don't use it (APM's AGC
        // has its own VAD).
        let _vad = self.state.process_frame(&mut self.out_buf, &self.in_buf);
        // f32 → i16, clamped.
        for (dst, &src) in frame.iter_mut().zip(self.out_buf.iter()) {
            *dst = src.clamp(-32_768.0, 32_767.0) as i16;
        }
    }
}
