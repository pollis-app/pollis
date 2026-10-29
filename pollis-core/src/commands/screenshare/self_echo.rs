//! Keeping the call out of the shared audio.
//!
//! On Linux the sink monitor contains the call, so capture would re-publish it.
//!
//! macOS/Windows exclude our process at the OS; PipeWire's monitor can't.
//!
//! So we subtract it: the voice mixer's output is the AEC render reference.
//!
//! ```text
//!   voice mixer 10 ms mix ──→ analyze_render ─┐
//!                                             ├─→ AEC ──→ published
//!   sink-monitor loopback ──→ run_capture ────┘           shared audio
//! ```
//!
//! AEC only: NS/AGC/HPF would wreck music.

use std::sync::{Arc, Mutex};

use crate::commands::voice_apm::{self, ApmConfig, ApmStage, NsLevel, Processor};

use super::audio::{SharedAudioResampler, SHARED_AUDIO_FRAME_SAMPLES, SHARED_AUDIO_RATE_HZ};

/// The slot the voice mixer publishes its render reference into.
///
/// On `VoiceState` so it outlives shares and mixer rebuilds.
pub type SelfEchoSlot = Arc<Mutex<Option<RenderTap>>>;

/// Armed slot: the processor plus a resampler that must persist across ticks.
pub struct RenderTap {
    processor: Arc<Processor>,
    resampler: SharedAudioResampler,
}

/// Feed the mixer's 10 ms mono frame to the armed canceller, at 48 kHz.
///
/// Never fails loudly: an unusable frame is dropped.
pub fn analyze_render(slot: &SelfEchoSlot, mix: &[f32], mix_rate: u32) {
    let mut guard = match slot.lock() {
        Ok(g) => g,
        Err(_) => return,
    };
    let tap = match guard.as_mut() {
        Some(t) => t,
        None => return,
    };
    if mix_rate == SHARED_AUDIO_RATE_HZ {
        // Common case: already 48 kHz 10 ms, so it stays sample-aligned.
        let _ = voice_apm::analyze_render(&tap.processor, mix, mix.len());
        return;
    }
    // Off-rate (Bluetooth SCO): an imperfect reference still removes most of it.
    tap.resampler.set_src_rate(mix_rate);
    let pcm: Vec<i16> = mix
        .iter()
        .map(|s| (s * 32_767.0).clamp(-32_768.0, 32_767.0) as i16)
        .collect();
    for frame in tap.resampler.push(&pcm, 1) {
        let f32s: Vec<f32> = frame.iter().map(|s| f32::from(*s) / 32_768.0).collect();
        let _ = voice_apm::analyze_render(&tap.processor, &f32s, f32s.len());
    }
}

/// The capture half: an AEC-only APM at 48 kHz that the shared-audio
/// frames pass through on their way to LiveKit.
pub struct SelfEchoCanceller {
    stage: ApmStage,
}

// Only the Linux capture path constructs one — macOS and Windows exclude
// our process at the OS layer, so there is nothing left to subtract. The
// tests below still build and run it on every platform, which is why the
// allow is conditional rather than blanket.
#[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
impl SelfEchoCanceller {
    /// Build the canceller and arm `slot`; `None` if APM is unavailable.
    pub fn new(slot: &SelfEchoSlot) -> Option<Self> {
        let config = ApmConfig {
            mic_boost_db: 0,
            agc_enabled: false,
            // Ignored while `agc_enabled` is false.
            agc_target_dbfs: 6,
            ns_level: NsLevel::Off,
            aec_enabled: true,
            click_suppression: false,
        };
        let stage = match ApmStage::new(SHARED_AUDIO_RATE_HZ, config) {
            Ok(s) => s,
            Err(e) => {
                crate::diag!("[screenshare/audio] self-echo canceller unavailable: {e}");
                return None;
            }
        };
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(RenderTap {
                processor: stage.handle(),
                resampler: SharedAudioResampler::new(SHARED_AUDIO_RATE_HZ),
            });
        } else {
            crate::diag!("[screenshare/audio] render slot poisoned; self-echo cancellation off");
            return None;
        }
        crate::diag!("[screenshare/audio] self-echo cancellation armed @ 48 kHz (AEC only)");
        Some(Self { stage })
    }

    /// Remove the call from one 10 ms shared-audio frame, in place.
    pub fn process(&self, frame: &mut [i16]) {
        if frame.len() != SHARED_AUDIO_FRAME_SAMPLES {
            return;
        }
        if let Err(e) = voice_apm::run_capture(
            &self.stage.handle(),
            frame,
            SHARED_AUDIO_FRAME_SAMPLES,
        ) {
            crate::diag!("[screenshare/audio] self-echo capture error: {e}");
        }
    }
}

/// Disarm the mixer's render tap. Idempotent.
pub fn disarm(slot: &SelfEchoSlot) {
    if let Ok(mut guard) = slot.lock() {
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot() -> SelfEchoSlot {
        Arc::new(Mutex::new(None))
    }

    #[test]
    fn arming_publishes_a_processor_and_disarm_clears_it() {
        let s = slot();
        assert!(s.lock().unwrap().is_none());
        let aec = SelfEchoCanceller::new(&s);
        // On Windows `ApmStage` is a stub but still constructs, so the
        // slot is armed on every platform we build for.
        assert!(aec.is_some());
        assert!(s.lock().unwrap().is_some());
        disarm(&s);
        assert!(s.lock().unwrap().is_none());
    }

    #[test]
    fn disarm_on_a_never_armed_slot_is_a_no_op() {
        let s = slot();
        disarm(&s);
        assert!(s.lock().unwrap().is_none());
    }

    #[test]
    fn render_analysis_on_an_unarmed_slot_does_nothing() {
        let s = slot();
        analyze_render(&s, &vec![0.0; SHARED_AUDIO_FRAME_SAMPLES], 48_000);
    }

    /// A frame that isn't exactly 10 ms is dropped rather than handed to APM.
    #[test]
    fn wrong_sized_frames_are_left_alone() {
        let s = slot();
        let aec = SelfEchoCanceller::new(&s).expect("stage");
        let mut short = vec![1234i16; SHARED_AUDIO_FRAME_SAMPLES / 2];
        aec.process(&mut short);
        assert!(short.iter().all(|s| *s == 1234));
    }

    /// Core invariant: with the call in both streams, output must be quieter.
    ///
    /// Skipped on Windows, where `voice_apm` is a stub.
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn the_call_is_attenuated_out_of_the_shared_audio() {
        let s = slot();
        let aec = SelfEchoCanceller::new(&s).expect("stage");

        // 400 Hz tone as the call, identical in reference and loopback.
        let tone: Vec<f32> = (0..SHARED_AUDIO_FRAME_SAMPLES)
            .map(|i| {
                (i as f32 * 2.0 * std::f32::consts::PI * 400.0
                    / SHARED_AUDIO_RATE_HZ as f32)
                    .sin()
                    * 0.5
            })
            .collect();

        let mut input_energy = 0.0f64;
        let mut output_energy = 0.0f64;
        // Measure only the back half, after AEC3 converges.
        const FRAMES: usize = 300;
        for n in 0..FRAMES {
            analyze_render(&s, &tone, SHARED_AUDIO_RATE_HZ);
            let mut cap: Vec<i16> = tone
                .iter()
                .map(|v| (v * 32_767.0) as i16)
                .collect();
            let before: f64 = cap.iter().map(|v| f64::from(*v) * f64::from(*v)).sum();
            aec.process(&mut cap);
            let after: f64 = cap.iter().map(|v| f64::from(*v) * f64::from(*v)).sum();
            if n >= FRAMES / 2 {
                input_energy += before;
                output_energy += after;
            }
        }

        assert!(
            output_energy < input_energy * 0.5,
            "self-echo cancellation did not attenuate the call: \
             in={input_energy:.0} out={output_energy:.0}"
        );
    }

    /// Off-rate (44.1 kHz): passes only if the resampler's partial output persists.
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn an_off_rate_render_reference_still_cancels() {
        const MIX_RATE: u32 = 44_100;
        let s = slot();
        let aec = SelfEchoCanceller::new(&s).expect("stage");

        // The same tone, generated at each stream's own rate.
        let tone_at = |rate: u32, n: usize| -> Vec<f32> {
            (0..n)
                .map(|i| {
                    (i as f32 * 2.0 * std::f32::consts::PI * 400.0 / rate as f32).sin() * 0.5
                })
                .collect()
        };
        let mix = tone_at(MIX_RATE, MIX_RATE as usize / 100);
        let capture = tone_at(SHARED_AUDIO_RATE_HZ, SHARED_AUDIO_FRAME_SAMPLES);

        // Prime one reference tick first, else each reference lands after its capture (#1209).
        analyze_render(&s, &mix, MIX_RATE);

        let mut input_energy = 0.0f64;
        let mut output_energy = 0.0f64;
        const FRAMES: usize = 300;
        for n in 0..FRAMES {
            analyze_render(&s, &mix, MIX_RATE);
            let mut cap: Vec<i16> = capture.iter().map(|v| (v * 32_767.0) as i16).collect();
            let before: f64 = cap.iter().map(|v| f64::from(*v) * f64::from(*v)).sum();
            aec.process(&mut cap);
            let after: f64 = cap.iter().map(|v| f64::from(*v) * f64::from(*v)).sum();
            if n >= FRAMES / 2 {
                input_energy += before;
                output_energy += after;
            }
        }

        assert!(
            output_energy < input_energy * 0.5,
            "off-rate render reference did not reach the canceller: \
             in={input_energy:.0} out={output_energy:.0}"
        );
    }
}
