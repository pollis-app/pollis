//! Shared-audio normalisation: the single place every platform's captured
//! system audio is turned into the exact frames LiveKit wants.
//!
//! Every backend's shape is normalised here, once:
//!
//! ```text
//!   interleaved s16 @ (rate, channels)
//!     ─→ downmix to mono          (average, not sum: summing clips)
//!     ─→ resample to 48 kHz       (passthrough when already 48 kHz)
//!     ─→ rebuffer to exact 10 ms  (480 samples — LiveKit's frame size)
//! ```
//!
//! Mono: the receiving mixer folds every track to mono anyway.

use std::sync::Arc;

use libwebrtc::{
    audio_source::native::NativeAudioSource,
    prelude::{AudioSourceOptions, RtcAudioSource},
};
use livekit::{
    options::TrackPublishOptions,
    prelude::*,
    track::{LocalAudioTrack, LocalTrack},
};

use crate::state::AppState;

use super::{
    self_echo::{self, SelfEchoSlot},
    state::ScreenShareState,
    ScreenShareEvent,
};

/// Shared-audio publish rate; matches APM and Opus, so no second resample.
pub(crate) const SHARED_AUDIO_RATE_HZ: u32 = 48_000;

/// Samples in one 10 ms mono frame at [`SHARED_AUDIO_RATE_HZ`].
pub(crate) const SHARED_AUDIO_FRAME_SAMPLES: usize = (SHARED_AUDIO_RATE_HZ / 100) as usize;

/// Rebuffers captured blocks into exact 10 ms 48 kHz mono frames.
///
/// Keeps the fractional read cursor across calls so 44.1 kHz doesn't drift.
pub(crate) struct SharedAudioResampler {
    /// Last announced source rate; a change resets the interpolator.
    src_rate: u32,
    /// Mono samples awaiting resampling, at `src_rate`.
    pending: Vec<f32>,
    /// Fractional read cursor into `pending`, carried across `push` calls.
    cursor: f64,
    /// Resampled 48 kHz mono samples not yet emitted as a whole frame.
    out: Vec<i16>,
}

impl SharedAudioResampler {
    pub(crate) fn new(src_rate: u32) -> Self {
        Self {
            src_rate,
            pending: Vec::new(),
            cursor: 0.0,
            out: Vec::new(),
        }
    }

    /// The source rate this resampler is currently configured for.
    pub(crate) fn src_rate(&self) -> u32 {
        self.src_rate
    }

    /// Switch source rate, dropping unconsumed input but keeping output.
    pub(crate) fn set_src_rate(&mut self, src_rate: u32) {
        if src_rate == self.src_rate {
            return;
        }
        self.src_rate = src_rate;
        self.pending.clear();
        self.cursor = 0.0;
    }

    /// Feed one interleaved block; returns each whole 10 ms frame.
    /// `channels == 0` is treated as mono (no divide by zero).
    pub(crate) fn push(&mut self, pcm: &[i16], channels: u32) -> Vec<Vec<i16>> {
        let channels = channels.max(1) as usize;
        self.pending.reserve(pcm.len() / channels + 1);
        if channels == 1 {
            self.pending
                .extend(pcm.iter().map(|s| f32::from(*s) / 32_768.0));
        } else {
            // Average, not sum: correlated channels would clip.
            for frame in pcm.chunks_exact(channels) {
                let sum: f32 = frame.iter().map(|s| f32::from(*s)).sum();
                self.pending.push(sum / (channels as f32) / 32_768.0);
            }
        }
        self.resample();
        self.take_frames()
    }

    /// Linear interpolation to 48 kHz: exact at 48 in, alias-free upsampling 44.1.
    fn resample(&mut self) {
        if self.src_rate == 0 {
            return;
        }
        // Exact copy at 48 kHz: interpolation would lag a sample, skewing the AEC reference.
        if self.src_rate == SHARED_AUDIO_RATE_HZ {
            self.out.extend(
                self.pending
                    .drain(..)
                    .map(|s| (s * 32_767.0).clamp(-32_768.0, 32_767.0) as i16),
            );
            return;
        }
        let step = f64::from(self.src_rate) / f64::from(SHARED_AUDIO_RATE_HZ);
        // Interpolating at index i reads i and i+1, so the last sample can
        // only be consumed once its successor has arrived in a later block.
        while self.cursor + 1.0 < self.pending.len() as f64 {
            let i = self.cursor as usize;
            let frac = (self.cursor - i as f64) as f32;
            let a = self.pending[i];
            let b = self.pending[i + 1];
            let s = a + (b - a) * frac;
            self.out
                .push((s * 32_767.0).clamp(-32_768.0, 32_767.0) as i16);
            self.cursor += step;
        }
        // Drop the consumed prefix so `pending` stays bounded.
        let consumed = self.cursor as usize;
        if consumed > 0 {
            self.pending.drain(..consumed);
            self.cursor -= consumed as f64;
        }
    }

    fn take_frames(&mut self) -> Vec<Vec<i16>> {
        let mut frames = Vec::new();
        while self.out.len() >= SHARED_AUDIO_FRAME_SAMPLES {
            frames.push(self.out.drain(..SHARED_AUDIO_FRAME_SAMPLES).collect());
        }
        frames
    }
}

/// Publish the share's audio track and park it for `stop`.
///
/// Only called once a real audio format arrives (no phantom speaker icons).
///
/// E2EE comes from the room's `RoomOptions::encryption`, like the mic.
///
/// Returns `None` after emitting `LocalAudioUnavailable` if the publish
/// fails; the caller keeps the video share running.
pub(super) async fn publish_shared_audio_track(
    state: &Arc<AppState>,
    room: &Arc<Room>,
) -> Option<NativeAudioSource> {
    // All processing off: this is the machine's playback, not a mic.
    let source = NativeAudioSource::new(
        AudioSourceOptions {
            echo_cancellation: false,
            noise_suppression: false,
            auto_gain_control: false,
        },
        SHARED_AUDIO_RATE_HZ,
        1,
        100,
    );
    let track = LocalAudioTrack::create_audio_track(
        "screenshare-audio",
        RtcAudioSource::Native(source.clone()),
    );
    if let Err(e) = room
        .local_participant()
        .publish_track(
            LocalTrack::Audio(track.clone()),
            TrackPublishOptions {
                // Lets receivers keep shared audio off the speaking indicator.
                source: TrackSource::ScreenshareAudio,
                ..Default::default()
            },
        )
        .await
    {
        crate::diag!("[screenshare/audio] publish error: {e}");
        emit(
            state,
            ScreenShareEvent::LocalAudioUnavailable {
                message: "Could not publish the shared audio to the call. \
                          Sharing without sound."
                    .into(),
            },
        )
        .await;
        return None;
    }
    crate::diag!("[screenshare/audio] shared-audio track published @ 48 kHz mono");
    {
        let mut ss = state.screenshare.lock().await;
        ss.local_audio_source = Some(source.clone());
        ss.local_audio_track = Some(track);
    }
    emit(state, ScreenShareEvent::LocalAudioStarted).await;
    Some(source)
}

/// Take the shared-audio track + source out of state and disarm the tap.
pub(super) fn retire_shared_audio(
    ss: &mut ScreenShareState,
    slot: &SelfEchoSlot,
) -> Option<(LocalAudioTrack, NativeAudioSource)> {
    self_echo::disarm(slot);
    let track = ss.local_audio_track.take();
    let source = ss.local_audio_source.take();
    match (track, source) {
        (Some(track), Some(source)) => Some((track, source)),
        // Half a pair isn't a reachable state; nothing coherent to unpublish.
        _ => None,
    }
}

/// Unpublish shared audio mid-share (video untouched); unpublish before drop.
pub(super) async fn unpublish_shared_audio(state: &Arc<AppState>, room: &Arc<Room>) {
    // Same lock order as `stop_screen_share`: screenshare, then voice.
    let retired = {
        let mut ss = state.screenshare.lock().await;
        let voice = state.voice.lock().await;
        retire_shared_audio(&mut ss, &voice.shared_audio_render)
    };
    let Some((track, source)) = retired else {
        return;
    };
    let sid = track.sid();
    if let Err(e) = room.local_participant().unpublish_track(&sid).await {
        crate::diag!("[screenshare/audio] unpublish error: {e}");
    }
    drop(source);
    crate::diag!("[screenshare/audio] shared-audio track unpublished");
}

/// Send a screen-share event without holding the state lock.
pub(super) async fn emit(state: &Arc<AppState>, event: ScreenShareEvent) {
    let sink = {
        let ss = state.screenshare.lock().await;
        ss.events.clone()
    };
    if let Some(sink) = sink {
        let _ = sink.send(event);
    }
}

/// Classify a helper `Error`: an `audio:` prefix means video-only failure.
pub(crate) fn audio_error_message(message: &str) -> Option<&str> {
    message
        .strip_prefix("audio:")
        .map(|rest| rest.trim())
        .filter(|rest| !rest.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sum of the whole-frame samples a sequence of pushes produced.
    fn total_samples(frames: &[Vec<i16>]) -> usize {
        frames.iter().map(Vec::len).sum()
    }

    #[test]
    fn frames_are_exactly_ten_milliseconds() {
        let mut r = SharedAudioResampler::new(48_000);
        // 2.5 frames' worth in one block: two come out, half a frame stays.
        let block = vec![0i16; SHARED_AUDIO_FRAME_SAMPLES * 5 / 2];
        let frames = r.push(&block, 1);
        assert_eq!(frames.len(), 2);
        for f in &frames {
            assert_eq!(f.len(), SHARED_AUDIO_FRAME_SAMPLES);
        }
    }

    #[test]
    fn short_blocks_accumulate_into_whole_frames() {
        let mut r = SharedAudioResampler::new(48_000);
        let mut got = 0;
        // 100 blocks of 48 samples = 4800 samples = exactly 10 frames.
        for _ in 0..100 {
            got += r.push(&[0i16; 48], 1).len();
        }
        assert_eq!(got, 10);
    }

    #[test]
    fn stereo_is_averaged_not_summed() {
        let mut r = SharedAudioResampler::new(48_000);
        // Identical channels at half scale: an average keeps half scale, a
        // sum would reach full scale and clip on louder material.
        let half = i16::MAX / 2;
        let block: Vec<i16> = std::iter::repeat_n(half, SHARED_AUDIO_FRAME_SAMPLES * 2 * 2)
            .collect();
        let frames = r.push(&block, 2);
        assert!(!frames.is_empty());
        let peak = frames[0].iter().map(|s| s.abs()).max().unwrap();
        assert!(
            (half - 2..=half + 2).contains(&peak),
            "stereo downmix peaked at {peak}, expected ~{half}"
        );
    }

    #[test]
    fn forty_four_one_upsamples_to_forty_eight() {
        let mut r = SharedAudioResampler::new(44_100);
        // One second of 44.1 kHz input must yield ~one second at 48 kHz.
        let frames = r.push(&vec![0i16; 44_100], 1);
        let out = total_samples(&frames);
        let expected = 48_000;
        // Up to one frame may still be buffered.
        assert!(
            out.abs_diff(expected) <= SHARED_AUDIO_FRAME_SAMPLES,
            "44.1k->48k produced {out} samples, expected ~{expected}"
        );
    }

    /// Drift: many small 44.1 kHz blocks must match one big block within a frame.
    #[test]
    fn fractional_cursor_survives_block_boundaries() {
        let mut chunked = SharedAudioResampler::new(44_100);
        let mut total = 0;
        for _ in 0..500 {
            total += total_samples(&chunked.push(&[0i16; 441], 1));
        }
        let mut single = SharedAudioResampler::new(44_100);
        let one_shot = total_samples(&single.push(&vec![0i16; 441 * 500], 1));
        assert!(
            total.abs_diff(one_shot) <= SHARED_AUDIO_FRAME_SAMPLES,
            "chunked resampling drifted: {total} vs {one_shot}"
        );
    }

    #[test]
    fn forty_eight_khz_mono_is_a_bit_exact_passthrough() {
        let mut r = SharedAudioResampler::new(48_000);
        let input: Vec<i16> = (0..SHARED_AUDIO_FRAME_SAMPLES as i32 + 1)
            .map(|i| ((i * 37) % 20_000 - 10_000) as i16)
            .collect();
        let frames = r.push(&input, 1);
        assert_eq!(frames.len(), 1);
        for (i, got) in frames[0].iter().enumerate() {
            // Allow the f32 round trip's +/-1 LSB.
            assert!(
                (*got as i32 - input[i] as i32).abs() <= 1,
                "sample {i}: got {got}, want {}",
                input[i]
            );
        }
    }

    #[test]
    fn zero_channels_is_treated_as_mono_rather_than_dividing_by_zero() {
        let mut r = SharedAudioResampler::new(48_000);
        let frames = r.push(&vec![0i16; SHARED_AUDIO_FRAME_SAMPLES + 1], 0);
        assert_eq!(frames.len(), 1);
    }

    /// The prefix is load-bearing in both directions.
    #[test]
    fn only_the_audio_prefix_is_non_fatal() {
        assert_eq!(
            audio_error_message("audio: no output device"),
            Some("no output device")
        );
        assert_eq!(audio_error_message("audio:pipewire missing"), Some("pipewire missing"));
        assert_eq!(audio_error_message("portal: denied"), None);
        assert_eq!(audio_error_message("cancel: user dismissed picker"), None);
        assert_eq!(audio_error_message("unsupported: no ScreenCast"), None);
        // A bare prefix carries no reason, so it is not a usable message.
        assert_eq!(audio_error_message("audio:"), None);
        assert_eq!(audio_error_message("audio:   "), None);
    }

    /// State as `publish_shared_audio_track` leaves it.
    fn parked_state() -> ScreenShareState {
        let source = NativeAudioSource::new(
            AudioSourceOptions {
                echo_cancellation: false,
                noise_suppression: false,
                auto_gain_control: false,
            },
            SHARED_AUDIO_RATE_HZ,
            1,
            100,
        );
        let track = LocalAudioTrack::create_audio_track(
            "screenshare-audio",
            RtcAudioSource::Native(source.clone()),
        );
        let mut ss = ScreenShareState::new();
        ss.local_audio_source = Some(source);
        ss.local_audio_track = Some(track);
        ss
    }

    /// #1040: an `audio:` error mid-share leaves no track parked and no tap armed.
    #[test]
    fn retiring_shared_audio_clears_the_track_and_disarms_the_tap() {
        let mut ss = parked_state();
        let slot: SelfEchoSlot = Arc::new(std::sync::Mutex::new(None));
        let armed = super::super::self_echo::SelfEchoCanceller::new(&slot);
        assert!(armed.is_some());
        assert!(slot.lock().unwrap().is_some());

        let retired = retire_shared_audio(&mut ss, &slot);

        assert!(retired.is_some(), "the live pair comes back for unpublishing");
        assert!(ss.local_audio_track.is_none());
        assert!(ss.local_audio_source.is_none());
        assert!(slot.lock().unwrap().is_none(), "tap must be disarmed");
        // The video half is untouched by an audio-only failure.
        assert!(ss.local_track.is_none() && ss.local_source.is_none());
    }

    #[test]
    fn retiring_shared_audio_twice_is_a_no_op() {
        let mut ss = parked_state();
        let slot: SelfEchoSlot = Arc::new(std::sync::Mutex::new(None));
        assert!(retire_shared_audio(&mut ss, &slot).is_some());
        assert!(retire_shared_audio(&mut ss, &slot).is_none());
        assert!(slot.lock().unwrap().is_none());
    }

    #[test]
    fn retiring_with_no_audio_live_still_disarms_the_tap() {
        let mut ss = ScreenShareState::new();
        let slot: SelfEchoSlot = Arc::new(std::sync::Mutex::new(None));
        let _armed = super::super::self_echo::SelfEchoCanceller::new(&slot);
        assert!(retire_shared_audio(&mut ss, &slot).is_none());
        assert!(slot.lock().unwrap().is_none());
    }

    #[test]
    fn rate_change_resets_the_interpolator_without_dropping_finished_output() {
        let mut r = SharedAudioResampler::new(44_100);
        r.push(&[0i16; 100], 1);
        r.set_src_rate(48_000);
        assert_eq!(r.src_rate(), 48_000);
        let frames = r.push(&vec![0i16; SHARED_AUDIO_FRAME_SAMPLES * 2], 1);
        for f in &frames {
            assert_eq!(f.len(), SHARED_AUDIO_FRAME_SAMPLES);
        }
    }
}
