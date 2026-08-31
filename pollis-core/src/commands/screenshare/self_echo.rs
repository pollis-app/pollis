//! Keeping the call out of the shared audio.
//!
//! Sharing "computer sound" means capturing what the machine is playing.
//! On Linux that is the default sink's monitor — and the call itself is
//! playing into that sink, so a naive capture re-publishes every remote
//! participant's voice back to them a few hundred milliseconds late. That
//! is the single worst failure mode this feature has, and it is not a
//! quality nit: it makes a call unusable for everyone but the sharer.
//!
//! macOS and Windows solve it at the OS: ScreenCaptureKit takes
//! `excludesCurrentProcessAudio`, and WASAPI takes a process-loopback
//! activation that excludes our own process tree. **Neither has an
//! equivalent on the PipeWire monitor path**, which is a mix — by the
//! time we see it, our contribution is already summed in and cannot be
//! filtered out by source.
//!
//! But it can be *subtracted*, because we are the ones who put it there.
//! `voice::playback`'s mixer produces the exact mono signal that goes to
//! the speaker, at a known 10 ms cadence. Feeding that to an echo
//! canceller as the render reference and running the captured loopback
//! through it as the capture stream removes the call from the share:
//!
//! ```text
//!   voice mixer 10 ms mix ──→ analyze_render ─┐
//!                                             ├─→ AEC ──→ published
//!   sink-monitor loopback ──→ run_capture ────┘           shared audio
//! ```
//!
//! Two things make this converge far better than the mic-side AEC it
//! borrows: the echo path is a pure digital loopback, so it is linear and
//! time-invariant with no room, no speaker distortion and no clock drift
//! between reference and capture; and the reference is bit-exact rather
//! than an estimate of what a speaker radiated.
//!
//! **AEC only — every other APM stage is off.** Noise suppression and AGC
//! are tuned for speech and would audibly wreck music, which is most of
//! what anyone shares audio for. The high-pass filter would strip the bass
//! for the same reason.
//!
//! Not needed, and deliberately not engaged, where the OS already excludes
//! us: [`SelfEchoCanceller::new`] is only called on the Linux capture path.
//! On Windows it would be inert anyway — `webrtc-audio-processing` has no
//! MSVC build, so `voice_apm`'s processor is a stub there.

use std::sync::{Arc, Mutex};

use crate::commands::voice_apm::{self, ApmConfig, ApmStage, NsLevel, Processor};

use super::audio::{SharedAudioResampler, SHARED_AUDIO_FRAME_SAMPLES, SHARED_AUDIO_RATE_HZ};

/// The slot the voice mixer publishes its render reference into.
///
/// Lives on `VoiceState` rather than in screenshare state because the
/// mixer task outlives any individual share and is rebuilt on an
/// output-device switch: both sides hold the same `Arc`, so a share that
/// starts after the mixer, or survives a device switch, still gets its
/// reference. `None` means no share is capturing system audio and the
/// mixer's per-tick cost is one uncontended lock and a null check.
pub type SelfEchoSlot = Arc<Mutex<Option<RenderTap>>>;

/// What an armed slot holds: the canceller's processor handle plus the
/// resampler that carries an off-rate reference's partial frames across
/// mixer ticks. The resampler must live here and not per call — one 10 ms
/// block can never complete a whole 48 kHz frame on its own (the
/// interpolator holds back the last input sample until its successor
/// arrives), so a per-call resampler emits nothing, ever, and the
/// canceller silently runs without a reference.
pub struct RenderTap {
    processor: Arc<Processor>,
    resampler: SharedAudioResampler,
}

/// Feed the mixer's 10 ms mono frame to the shared-audio echo canceller,
/// if one is armed. Called from the voice mixer's hot loop; resampling to
/// the canceller's 48 kHz happens here so the mixer stays rate-agnostic.
///
/// `mix_rate` is the voice pipeline's APM rate, which tracks the mic
/// device and is 48 kHz in every case but a Bluetooth headset stuck in
/// SCO. Nothing here fails loudly: a render frame the canceller cannot use
/// is dropped, and the worst outcome is that the call leaks back into the
/// shared audio — the same place we would be without this module.
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
        // The common case, and the one that matters: the reference is
        // already the canceller's rate and frame size, so it goes in
        // untouched and stays sample-aligned with the capture stream.
        let _ = voice_apm::analyze_render(&tap.processor, mix, mix.len());
        return;
    }
    // Off-rate voice session (Bluetooth SCO mic). Resample the reference
    // rather than disable cancellation — a slightly imperfect reference
    // still removes most of the call, and no reference removes none of it.
    // The slot's resampler carries the partial frame each tick leaves
    // behind, so roughly every tick from the second onward completes one.
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
    /// Build the canceller and arm `slot` so the voice mixer starts
    /// publishing its render reference. Returns `None` if APM is
    /// unavailable, in which case the caller publishes the raw loopback —
    /// audio with an echo beats no audio, and the UI has already told the
    /// user this platform captures the whole system.
    pub fn new(slot: &SelfEchoSlot) -> Option<Self> {
        let config = ApmConfig {
            mic_boost_db: 0,
            agc_enabled: false,
            // Ignored while `agc_enabled` is false; the field has no
            // "unset" and the struct is not `Default`-friendly here
            // because every other field is a deliberate off.
            agc_target_dbfs: 6,
            ns_level: NsLevel::Off,
            aec_enabled: true,
            click_suppression: false,
        };
        let stage = match ApmStage::new(SHARED_AUDIO_RATE_HZ, config) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[screenshare/audio] self-echo canceller unavailable: {e}");
                return None;
            }
        };
        if let Ok(mut guard) = slot.lock() {
            *guard = Some(RenderTap {
                processor: stage.handle(),
                resampler: SharedAudioResampler::new(SHARED_AUDIO_RATE_HZ),
            });
        } else {
            eprintln!("[screenshare/audio] render slot poisoned; self-echo cancellation off");
            return None;
        }
        eprintln!("[screenshare/audio] self-echo cancellation armed @ 48 kHz (AEC only)");
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
            eprintln!("[screenshare/audio] self-echo capture error: {e}");
        }
    }
}

/// Disarm the mixer's render tap. Idempotent, and safe to call when no
/// share ever armed it — every stop path runs through here.
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

    /// A frame that isn't exactly 10 ms is dropped rather than handed to
    /// APM, which would trip its debug assertion and, in release, read a
    /// mismatched buffer.
    #[test]
    fn wrong_sized_frames_are_left_alone() {
        let s = slot();
        let aec = SelfEchoCanceller::new(&s).expect("stage");
        let mut short = vec![1234i16; SHARED_AUDIO_FRAME_SAMPLES / 2];
        aec.process(&mut short);
        assert!(short.iter().all(|s| *s == 1234));
    }

    /// The invariant this module exists for: with the call as the only
    /// content in both the render reference and the capture stream, the
    /// published frame must come out quieter than it went in. Anything
    /// else means the reference is not reaching the canceller and every
    /// participant would hear themselves.
    ///
    /// Skipped on Windows, where `voice_apm` is a documented no-op stub —
    /// there the OS excludes our process from the loopback instead.
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn the_call_is_attenuated_out_of_the_shared_audio() {
        let s = slot();
        let aec = SelfEchoCanceller::new(&s).expect("stage");

        // 400 Hz tone standing in for the call, present identically in the
        // reference and in the "loopback" — the exact digital-loopback
        // case, no room and no delay.
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
        // AEC3's adaptive filter needs time to converge; measure only the
        // back half so the pre-convergence frames don't mask the result.
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

    /// The off-rate regression: with the voice pipeline at 44.1 kHz (a
    /// Bluetooth mic pulls the whole APM to the device rate), the render
    /// reference is resampled before it reaches the canceller. One 10 ms
    /// block can never complete a whole 48 kHz frame on its own, so this
    /// only passes if the resampler's partial output survives across
    /// ticks — a per-call resampler delivers no reference at all and the
    /// tone comes out untouched.
    #[test]
    #[cfg(not(target_os = "windows"))]
    fn an_off_rate_render_reference_still_cancels() {
        const MIX_RATE: u32 = 44_100;
        let s = slot();
        let aec = SelfEchoCanceller::new(&s).expect("stage");

        // The same 400 Hz call stand-in, generated at each stream's own
        // rate so reference and capture describe the same signal.
        let tone_at = |rate: u32, n: usize| -> Vec<f32> {
            (0..n)
                .map(|i| {
                    (i as f32 * 2.0 * std::f32::consts::PI * 400.0 / rate as f32).sin() * 0.5
                })
                .collect()
        };
        let mix = tone_at(MIX_RATE, MIX_RATE as usize / 100);
        let capture = tone_at(SHARED_AUDIO_RATE_HZ, SHARED_AUDIO_FRAME_SAMPLES);

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
