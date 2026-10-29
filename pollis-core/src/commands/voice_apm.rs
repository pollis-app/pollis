//! WebRTC AudioProcessing Module (APM) wrapper.
//!
//! The only mic-side processing stage (AGC/NS/HPF/AEC); libwebrtc's APM is off.
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
//! APM rate = the cpal mic rate (8/16/32/48 kHz).
//!
//! Windows: the sys crate has no MSVC build, so these are no-op stubs.

use serde::{Deserialize, Serialize};
use std::sync::Arc;

#[cfg(not(target_os = "windows"))]
use webrtc_audio_processing::{
    config::{
        AdaptiveDigital, CaptureAmplifier, EchoCanceller, GainController, GainController2,
        HighPassFilter, NoiseSuppression, NoiseSuppressionLevel, PreAmplifier,
    },
    Config,
};

/// The APM processor; a unit stub on Windows.
#[cfg(not(target_os = "windows"))]
pub use webrtc_audio_processing::Processor;

#[cfg(target_os = "windows")]
pub struct Processor;

#[cfg(target_os = "windows")]
impl Processor {
    pub fn set_output_will_be_muted(&self, _muted: bool) {}
}

/// Number of mono samples in a 10ms APM frame at `sample_rate_hz`.
pub const fn frame_samples(sample_rate_hz: u32) -> usize {
    (sample_rate_hz / 100) as usize
}

/// Preferred APM rate: matches LiveKit's encoding and aligns AEC frames.
pub const DEFAULT_APM_RATE_HZ: u32 = 48_000;

/// User audio-processing settings, passed at join; live changes via [`ApmStage::set_config`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ApmConfig {
    /// Pre-AGC mic boost in dB (0..=20, 0 = off), so AGC engages on quiet talkers.
    pub mic_boost_db: u8,
    /// AGC on/off. Mirrors the existing `auto_gain_control` preference.
    pub agc_enabled: bool,
    /// AGC2 `headroom_db` (smaller = louder; UI clamps 3..=15).
    pub agc_target_dbfs: u8,
    /// Noise suppression aggressiveness.
    pub ns_level: NsLevel,
    /// Echo cancellation on/off.
    pub aec_enabled: bool,
    /// RNNoise pre-APM denoiser for transients (clicks); 48 kHz only, ~5% of a core.
    pub click_suppression: bool,
}

impl Default for ApmConfig {
    fn default() -> Self {
        Self {
            mic_boost_db: 0,
            agc_enabled: true,
            // 6 dB headroom: AGC2's canonical default.
            agc_target_dbfs: 6,
            ns_level: NsLevel::High,
            aec_enabled: true,
            click_suppression: false,
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

#[cfg(not(target_os = "windows"))]
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
            // Let AEC3 estimate the delay; we have no calibrated per-host number.
            Some(EchoCanceller::Full { stream_delay_ms: None })
        } else {
            None
        };

        let gain_controller = if self.agc_enabled {
            // AGC2 AdaptiveDigital: AGC1's conservative target made voices too quiet.
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
                // No static post-gain; the adaptive stage does the work.
                fixed_digital: webrtc_audio_processing::config::FixedDigital { gain_db: 0.0 },
            }))
        } else {
            None
        };

        // Fixed pre-amp before all stages; bypassed at mic_boost_db == 0.
        let capture_amplifier = if self.mic_boost_db > 0 {
            // dB → linear: 10^(dB/20). +6 dB = ×2, +20 dB = ×10.
            let gain = 10f32.powf(f32::from(self.mic_boost_db.min(20)) / 20.0);
            Some(CaptureAmplifier::PreAmplifier(PreAmplifier {
                fixed_gain_factor: gain,
            }))
        } else {
            None
        };

        Config {
            capture_amplifier,
            high_pass_filter: Some(HighPassFilter::default()),
            echo_canceller,
            noise_suppression,
            gain_controller,
            ..Config::default()
        }
    }
}

/// Owns one session's APM `Processor` (Arc-shared, internally serialised).
pub struct ApmStage {
    processor: Arc<Processor>,
    sample_rate_hz: u32,
    config: ApmConfig,
}

impl ApmStage {
    /// Build an APM at `sample_rate_hz`; it must match the mic and render rates.
    #[cfg(not(target_os = "windows"))]
    pub fn new(sample_rate_hz: u32, config: ApmConfig) -> Result<Self, String> {
        if !matches!(sample_rate_hz, 8_000 | 16_000 | 32_000 | 48_000) {
            return Err(format!(
                "APM only supports 8/16/32/48 kHz, got {sample_rate_hz} Hz"
            ));
        }
        let processor = Processor::new(sample_rate_hz)
            .map_err(|e| format!("APM init failed: {e}"))?;
        processor.set_config(config.to_processor_config());
        crate::diag!(
            "[voice/apm] engaged @ {sample_rate_hz} Hz: boost={} dB, AGC2={} (headroom={} dB), \
             NS={:?}, AEC={}, RNNoise={}",
            config.mic_boost_db,
            config.agc_enabled,
            config.agc_target_dbfs,
            config.ns_level,
            config.aec_enabled,
            config.click_suppression,
        );
        Ok(Self {
            processor: Arc::new(processor),
            sample_rate_hz,
            config,
        })
    }

    #[cfg(target_os = "windows")]
    pub fn new(sample_rate_hz: u32, config: ApmConfig) -> Result<Self, String> {
        if !matches!(sample_rate_hz, 8_000 | 16_000 | 32_000 | 48_000) {
            return Err(format!(
                "APM only supports 8/16/32/48 kHz, got {sample_rate_hz} Hz"
            ));
        }
        crate::diag!(
            "[voice/apm] disabled on Windows (webrtc-audio-processing-sys has no MSVC \
             support upstream); requested config is stored but not applied: \
             boost={} dB, AGC2={} (headroom={} dB), NS={:?}, AEC={}, RNNoise={}",
            config.mic_boost_db,
            config.agc_enabled,
            config.agc_target_dbfs,
            config.ns_level,
            config.aec_enabled,
            config.click_suppression,
        );
        Ok(Self {
            processor: Arc::new(Processor),
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

    /// Apply a new config, preserving internal state.
    #[cfg(not(target_os = "windows"))]
    pub fn set_config(&mut self, config: ApmConfig) {
        self.processor.set_config(config.to_processor_config());
        crate::diag!(
            "[voice/apm] reconfigured: boost={} dB, AGC2={} (headroom={} dB), NS={:?}, AEC={}, RNNoise={}",
            config.mic_boost_db,
            config.agc_enabled,
            config.agc_target_dbfs,
            config.ns_level,
            config.aec_enabled,
            config.click_suppression,
        );
        self.config = config;
    }

    #[cfg(target_os = "windows")]
    pub fn set_config(&mut self, config: ApmConfig) {
        // No-op on Windows, but keep the config so reads reflect user prefs.
        self.config = config;
    }
}

/// Run APM in place on a 10 ms i16 mono capture frame of `frame_samples` length.
#[cfg(not(target_os = "windows"))]
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

/// Windows no-op passthrough (`Infallible` error keeps call sites typed).
#[cfg(target_os = "windows")]
pub fn run_capture(
    _processor: &Processor,
    _samples: &mut [i16],
    _expected_len: usize,
) -> Result<(), std::convert::Infallible> {
    Ok(())
}

/// Feed a 10 ms f32 mono render frame to APM as the AEC reference.
#[cfg(not(target_os = "windows"))]
pub fn analyze_render(
    processor: &Processor,
    samples: &[f32],
    expected_len: usize,
) -> Result<(), webrtc_audio_processing::Error> {
    debug_assert_eq!(samples.len(), expected_len, "render frame size mismatch");
    processor.analyze_render_frame([samples])
}

/// Windows no-op: APM has no AEC reference to feed.
#[cfg(target_os = "windows")]
pub fn analyze_render(
    _processor: &Processor,
    _samples: &[f32],
    _expected_len: usize,
) -> Result<(), std::convert::Infallible> {
    Ok(())
}
