//! WebRTC AudioProcessing Module (APM) wrapper.
//!
//! This is the single source of mic-side audio processing for voice channels:
//! AGC, noise suppression, high-pass filter, transient suppression, and
//! acoustic echo cancellation. We own it end-to-end so we can tune the AGC
//! target, NS aggressiveness, and AEC mode from user preferences. libwebrtc's
//! internal APM is disabled at the LiveKit `AudioSourceOptions` level so we
//! don't double-process the signal.
//!
//! Pipeline (per voice session, one [`ApmStage`] for the lifetime of the join):
//!
//! ```text
//!   cpal mic capture (10ms i16 mono @ APM rate)
//!     ─→ run_capture(processor, frame)        // AGC + NS + HPF + AEC capture side
//!     ─→ LiveKit NativeAudioSource.capture_frame
//!
//!   mixed remote playback (10ms f32 mono @ APM rate, what's hitting the speaker)
//!     ─→ analyze_render(processor, frame)     // AEC render reference
//! ```
//!
//! APM rate is locked to the cpal mic input rate. WebRTC supports
//! 8/16/32/48 kHz, and the rest of the pipeline (mic stream, speaker stream,
//! mixer) is configured to match.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use webrtc_audio_processing::{
    config::{
        AdaptiveDigital, EchoCanceller, GainController, GainController2, HighPassFilter,
        NoiseSuppression, NoiseSuppressionLevel,
    },
    Config, Processor,
};

/// Number of mono samples in a 10ms APM frame at `sample_rate_hz`.
pub const fn frame_samples(sample_rate_hz: u32) -> usize {
    (sample_rate_hz / 100) as usize
}

/// Default APM rate when the mic device cooperates. WebRTC supports 8/16/32/48
/// kHz; we prefer 48 kHz because it matches LiveKit's encoding rate and keeps
/// the AEC reference frame size aligned across the whole pipeline.
pub const DEFAULT_APM_RATE_HZ: u32 = 48_000;

/// User-facing audio-processing settings. Persisted via the existing
/// preferences flow (see `frontend/src/hooks/queries/usePreferences.ts`) and
/// passed in at every join. Mid-call changes hit `set_voice_audio_processing`,
/// which calls [`ApmStage::set_config`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ApmConfig {
    /// AGC on/off. Mirrors the existing `auto_gain_control` preference.
    pub agc_enabled: bool,
    /// AGC target loudness expressed as headroom from full scale in dB.
    /// Smaller magnitude = louder; clamped to 3..=15 in the UI. Maps to
    /// WebRTC AGC2's `headroom_db`. Default 6 dB matches the canonical
    /// WebRTC default for AdaptiveDigital and is meaningfully louder than
    /// AGC1's old defaults.
    pub agc_target_dbfs: u8,
    /// Noise suppression aggressiveness.
    pub ns_level: NsLevel,
    /// Echo cancellation on/off. Off is occasionally useful for headset users
    /// who want raw mic-only processing, but most voice setups want this on.
    pub aec_enabled: bool,
}

impl Default for ApmConfig {
    fn default() -> Self {
        Self {
            agc_enabled: true,
            // 6 dB headroom: WebRTC AGC2 canonical default. Lower values
            // (3) clip easily on hot mics; higher (12+) sound quiet.
            agc_target_dbfs: 6,
            // High by default — Moderate matched libwebrtc's old default
            // and produced no audible change for users upgrading.
            ns_level: NsLevel::High,
            aec_enabled: true,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum NsLevel {
    Off,
    Low,
    Moderate,
    High,
}

impl ApmConfig {
    fn to_processor_config(&self) -> Config {
        let noise_suppression = match self.ns_level {
            NsLevel::Off => None,
            level => Some(NoiseSuppression {
                level: match level {
                    NsLevel::Low => NoiseSuppressionLevel::Low,
                    NsLevel::Moderate => NoiseSuppressionLevel::Moderate,
                    NsLevel::High => NoiseSuppressionLevel::High,
                    NsLevel::Off => unreachable!(),
                },
                analyze_linear_aec_output: false,
            }),
        };

        let echo_canceller = if self.aec_enabled {
            // Full AEC3 with `stream_delay_ms: None` lets APM3's internal
            // delay estimator run. That's the right default for a desktop
            // app where mic→speaker latency is dominated by the OS audio
            // stack and varies device-to-device. Manually setting a delay
            // is only worth it once we have a calibrated number per host.
            Some(EchoCanceller::Full { stream_delay_ms: None })
        } else {
            None
        };

        let gain_controller = if self.agc_enabled {
            // AGC2 with AdaptiveDigital — modern WebRTC AGC. Speech-presence
            // gated, up to 30 dB of boost for quiet talkers, with built-in
            // noise-floor limiter. AGC1 (the older controller) is what
            // libwebrtc internally enables when `auto_gain_control: true`,
            // and its hardcoded conservative target is what made the issue's
            // "voice too quiet" complaint show up — switching to AGC2 is
            // the whole point of owning APM ourselves.
            Some(GainController::GainController2(GainController2 {
                // No hardware analog control on a desktop app — the OS owns mic gain.
                input_volume_controller_enabled: false,
                adaptive_digital: Some(AdaptiveDigital {
                    headroom_db: f32::from(self.agc_target_dbfs.clamp(3, 15)),
                    // WebRTC defaults: max +30 dB total, +8 dB initial,
                    // 3 dB/s max change rate, output noise floor -50 dBFS.
                    max_gain_db: 30.0,
                    initial_gain_db: 8.0,
                    max_gain_change_db_per_second: 3.0,
                    max_output_noise_level_dbfs: -50.0,
                }),
                // FixedDigital is a static post-gain on top of AdaptiveDigital.
                // 0 dB = no extra static gain; the adaptive stage does the work.
                fixed_digital: webrtc_audio_processing::config::FixedDigital { gain_db: 0.0 },
            }))
        } else {
            None
        };

        Config {
            high_pass_filter: Some(HighPassFilter::default()),
            echo_canceller,
            noise_suppression,
            gain_controller,
            ..Config::default()
        }
    }
}

/// Owns the APM `Processor` for one voice session. Cheap to clone (the
/// processor itself is shared via `Arc`); the underlying C++ object is
/// `Send + Sync` and serialises capture/render internally.
pub struct ApmStage {
    processor: Arc<Processor>,
    sample_rate_hz: u32,
    config: ApmConfig,
}

impl ApmStage {
    /// Build an APM at `sample_rate_hz` and apply `config`. The rate must
    /// match the mic stream rate (and the render reference rate); otherwise
    /// `process_capture_frame` / `analyze_render_frame` will panic on frame
    /// size mismatches.
    pub fn new(sample_rate_hz: u32, config: ApmConfig) -> Result<Self, String> {
        if !matches!(sample_rate_hz, 8_000 | 16_000 | 32_000 | 48_000) {
            return Err(format!(
                "APM only supports 8/16/32/48 kHz, got {sample_rate_hz} Hz"
            ));
        }
        let processor = Processor::new(sample_rate_hz)
            .map_err(|e| format!("APM init failed: {e}"))?;
        processor.set_config(config.to_processor_config());
        eprintln!(
            "[voice/apm] engaged @ {sample_rate_hz} Hz: AGC2={} (headroom={} dB), \
             NS={:?}, AEC={}",
            config.agc_enabled, config.agc_target_dbfs, config.ns_level, config.aec_enabled,
        );
        Ok(Self {
            processor: Arc::new(processor),
            sample_rate_hz,
            config,
        })
    }

    pub fn handle(&self) -> Arc<Processor> {
        Arc::clone(&self.processor)
    }

    pub fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    pub fn frame_samples(&self) -> usize {
        frame_samples(self.sample_rate_hz)
    }

    pub fn config(&self) -> &ApmConfig {
        &self.config
    }

    /// Apply a new config without recreating the processor. Internal state
    /// (echo estimate, noise estimate, AGC envelope) is preserved; only
    /// changed submodules are re-initialised.
    pub fn set_config(&mut self, config: ApmConfig) {
        self.processor.set_config(config.to_processor_config());
        eprintln!(
            "[voice/apm] reconfigured: AGC2={} (headroom={} dB), NS={:?}, AEC={}",
            config.agc_enabled, config.agc_target_dbfs, config.ns_level, config.aec_enabled,
        );
        self.config = config;
    }
}

/// Run APM on a 10ms i16 mono capture frame, in place. `samples.len()` must
/// equal [`ApmStage::frame_samples`]. Converts to non-interleaved f32 for the
/// FFI call and converts back; the round-trip is the same precision loss
/// libwebrtc's internal pipeline already incurs.
pub fn run_capture(
    processor: &Processor,
    samples: &mut [i16],
    expected_len: usize,
) -> Result<(), webrtc_audio_processing::Error> {
    debug_assert_eq!(samples.len(), expected_len, "capture frame size mismatch");
    let mut channel: Vec<f32> = samples.iter().map(|s| *s as f32 / 32_768.0).collect();
    processor.process_capture_frame([channel.as_mut_slice()])?;
    for (dst, src) in samples.iter_mut().zip(channel.iter()) {
        *dst = (*src * 32_767.0).clamp(-32_768.0, 32_767.0) as i16;
    }
    Ok(())
}

/// Feed a 10ms f32 mono render frame (what's about to hit the speaker) into
/// APM as the AEC reference. Doesn't modify the frame; APM only inspects.
/// `samples.len()` must equal [`ApmStage::frame_samples`].
pub fn analyze_render(
    processor: &Processor,
    samples: &[f32],
    expected_len: usize,
) -> Result<(), webrtc_audio_processing::Error> {
    debug_assert_eq!(samples.len(), expected_len, "render frame size mismatch");
    processor.analyze_render_frame([samples])
}
