//! WebRTC AudioProcessing Module (APM): the single source of mic-side processing
//! for voice (AGC, NS, HPF, AEC), owned so it can be tuned from preferences.
//! libwebrtc's internal APM is disabled in `AudioSourceOptions` so the signal
//! isn't processed twice.
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
//! The APM rate is locked to the cpal mic rate.
//!
//! Windows: `webrtc-audio-processing-sys` doesn't build with MSVC, so the API
//! here is no-op stubs there; voice works without AEC/AGC/NS.

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

/// The APM processor; a unit stub on Windows so the pipeline keeps one set of
/// types.
#[cfg(not(target_os = "windows"))]
pub use webrtc_audio_processing::Processor;

#[cfg(target_os = "windows")]
pub struct Processor;

#[cfg(target_os = "windows")]
impl Processor {
    pub fn set_output_will_be_muted(&self, _muted: bool) {}
}

pub const fn frame_samples(sample_rate_hz: u32) -> usize {
    (sample_rate_hz / 100) as usize
}

/// Default APM rate: 48 kHz matches LiveKit's encoding rate and keeps the AEC
/// reference frame size aligned across the pipeline.
pub const DEFAULT_APM_RATE_HZ: u32 = 48_000;

/// User-facing audio-processing settings, passed at every join; mid-call changes
/// go through [`ApmStage::set_config`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ApmConfig {
    /// Fixed pre-AGC boost in dB (0..=20, 0 = off), lifting quiet input into the
    /// range where AGC's speech detector engages.
    pub mic_boost_db: u8,
    pub agc_enabled: bool,
    /// AGC target as headroom from full scale in dB (3..=15); smaller is louder. Maps
    /// to AGC2's `headroom_db`.
    pub agc_target_dbfs: u8,
    pub ns_level: NsLevel,
    pub aec_enabled: bool,
    /// Run RNNoise as a pre-APM denoiser for transients (clicks, typing) APM's NS
    /// misses. Off by default: ~5% of a core, and 48 kHz only.
    pub click_suppression: bool,
}

impl Default for ApmConfig {
    fn default() -> Self {
        Self {
            mic_boost_db: 0,
            agc_enabled: true,
            // WebRTC AGC2's default; 3 clips on hot mics, 12+ sounds quiet.
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
            // `stream_delay_ms: None` lets AEC3 estimate the delay, which varies with the
            // OS audio stack per device.
            Some(EchoCanceller::Full { stream_delay_ms: None })
        } else {
            None
        };

        let gain_controller = if self.agc_enabled {
            // AGC2 AdaptiveDigital: libwebrtc's built-in AGC1 has a conservative target
            // that left voices too quiet, which is why we own APM.
            Some(GainController::GainController2(GainController2 {
                // The OS owns mic gain; no analog control.
                input_volume_controller_enabled: false,
                adaptive_digital: Some(AdaptiveDigital {
                    headroom_db: f32::from(self.agc_target_dbfs.clamp(3, 15)),
                    max_gain_db: 30.0,
                    initial_gain_db: 8.0,
                    max_gain_change_db_per_second: 3.0,
                    max_output_noise_level_dbfs: -50.0,
                }),
                fixed_digital: webrtc_audio_processing::config::FixedDigital { gain_db: 0.0 },
            }))
        } else {
            None
        };

        // Bypassed at 0 dB so the signal path is unchanged without a boost.
        let capture_amplifier = if self.mic_boost_db > 0 {
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

/// Owns the APM `Processor` for one voice session. The C++ object is
/// `Send + Sync` and serialises capture/render internally.
pub struct ApmStage {
    processor: Arc<Processor>,
    sample_rate_hz: u32,
    config: ApmConfig,
}

impl ApmStage {
    /// Build an APM at `sample_rate_hz`, which must match the mic and render
    /// reference rates or frame-size mismatches panic.
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

    /// Apply a new config without recreating the processor; echo/noise/AGC state is
    /// kept.
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
        // Stored even though APM isn't running, so reads reflect user prefs.
        self.config = config;
    }
}

/// Run APM on a 10 ms i16 mono capture frame in place. `samples.len()` must equal
/// [`ApmStage::frame_samples`].
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

/// Windows no-op. `Infallible` keeps the call site's `if let Err(e)` compiling.
#[cfg(target_os = "windows")]
pub fn run_capture(
    _processor: &Processor,
    _samples: &mut [i16],
    _expected_len: usize,
) -> Result<(), std::convert::Infallible> {
    Ok(())
}

/// Feed a 10 ms f32 mono render frame to APM as the AEC reference (read-only).
#[cfg(not(target_os = "windows"))]
pub fn analyze_render(
    processor: &Processor,
    samples: &[f32],
    expected_len: usize,
) -> Result<(), webrtc_audio_processing::Error> {
    debug_assert_eq!(samples.len(), expected_len, "render frame size mismatch");
    processor.analyze_render_frame([samples])
}

#[cfg(target_os = "windows")]
pub fn analyze_render(
    _processor: &Processor,
    _samples: &[f32],
    _expected_len: usize,
) -> Result<(), std::convert::Infallible> {
    Ok(())
}
