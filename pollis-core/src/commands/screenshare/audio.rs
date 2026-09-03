//! Shared-audio normalisation: the single place every platform's captured
//! system audio is turned into the exact frames LiveKit wants.
//!
//! The three capture backends hand us three different shapes — macOS
//! ScreenCaptureKit delivers planar f32 scoped to the content filter,
//! Linux PipeWire delivers whatever the sink monitor runs at, Windows
//! WASAPI delivers the render endpoint's mix format — so each one is
//! normalised here rather than three times over:
//!
//! ```text
//!   interleaved s16 @ (rate, channels)
//!     ─→ downmix to mono          (average, not sum: summing clips)
//!     ─→ resample to 48 kHz       (passthrough when already 48 kHz)
//!     ─→ rebuffer to exact 10 ms  (480 samples — LiveKit's frame size)
//! ```
//!
//! **Mono is not a compromise here.** The receiving end mixes every
//! remote track down to mono in `voice::playback`'s mixer before it
//! reaches the speaker, so a stereo shared-audio track would be folded
//! back to mono on arrival anyway — publishing stereo would cost double
//! the bandwidth to reach an identical result.

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

/// The rate every shared-audio track publishes at. Matches
/// `voice_apm::DEFAULT_APM_RATE_HZ` and LiveKit's Opus encoding rate, so
/// the self-echo canceller can run without a second resampling stage.
pub(crate) const SHARED_AUDIO_RATE_HZ: u32 = 48_000;

/// Samples in one 10 ms mono frame at [`SHARED_AUDIO_RATE_HZ`].
pub(crate) const SHARED_AUDIO_FRAME_SAMPLES: usize = (SHARED_AUDIO_RATE_HZ / 100) as usize;

/// Rebuffers arbitrary-length captured blocks into exact 10 ms mono
/// frames at 48 kHz. One instance per share; `push` is called with each
/// block the capture backend produces and returns however many whole
/// frames that block completed (usually one, sometimes zero or two).
///
/// Holds the resampler's fractional read position across calls, so a
/// capture device running at a rate that doesn't divide evenly into
/// 48 kHz (44.1 kHz being the common one) doesn't accumulate drift or
/// click at every block boundary.
pub(crate) struct SharedAudioResampler {
    /// Source rate as last announced. A mid-stream rate change (device
    /// switch under a live share) resets the interpolator rather than
    /// reading the tail of the old rate's samples at the new one.
    src_rate: u32,
    /// Mono samples awaiting resampling, at `src_rate`.
    pending: Vec<f32>,
    /// Fractional read cursor into `pending`, in source samples. Carried
    /// across `push` calls — this is what keeps 44.1 kHz from drifting.
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

    /// Point the resampler at a new source rate, discarding any partially
    /// consumed input. Output already resampled to 48 kHz is kept — it is
    /// rate-correct regardless of where the next block comes from.
    pub(crate) fn set_src_rate(&mut self, src_rate: u32) {
        if src_rate == self.src_rate {
            return;
        }
        self.src_rate = src_rate;
        self.pending.clear();
        self.cursor = 0.0;
    }

    /// Feed one captured block and take back every whole 10 ms mono frame
    /// it completed. `pcm` is interleaved at `channels`; a `channels` of 0
    /// is treated as mono so a helper that forgets to fill the field
    /// cannot divide by zero.
    pub(crate) fn push(&mut self, pcm: &[i16], channels: u32) -> Vec<Vec<i16>> {
        let channels = channels.max(1) as usize;
        self.pending.reserve(pcm.len() / channels + 1);
        if channels == 1 {
            self.pending
                .extend(pcm.iter().map(|s| f32::from(*s) / 32_768.0));
        } else {
            // Average rather than sum. Summing two correlated channels —
            // which most music is — produces a signal up to 2x full scale
            // that then hard-clips on the way back to i16.
            for frame in pcm.chunks_exact(channels) {
                let sum: f32 = frame.iter().map(|s| f32::from(*s)).sum();
                self.pending.push(sum / (channels as f32) / 32_768.0);
            }
        }
        self.resample();
        self.take_frames()
    }

    /// Linear interpolation from `src_rate` to 48 kHz. Linear is the right
    /// choice here and not a shortcut: the overwhelmingly common case is
    /// 48 kHz in, where `step == 1.0` makes this an exact passthrough with
    /// no filtering at all, and the only realistic non-48 case (44.1 kHz)
    /// is upsampled, where linear interpolation adds no aliasing — only a
    /// gentle high-shelf roll-off well above the Opus encoder's own.
    fn resample(&mut self) {
        if self.src_rate == 0 {
            return;
        }
        // Exact fast path. Almost every capture backend already runs at
        // 48 kHz, and it matters that this is a byte-for-byte copy rather
        // than "interpolation that happens to land on integers": the
        // general branch below cannot emit a sample until its successor
        // arrives, so it runs one sample behind forever. That lag is
        // inaudible in the published audio but it would make the echo
        // canceller's render reference slip a sample per block against the
        // capture stream it is supposed to cancel.
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
        // Drop the fully-consumed prefix and rebase the cursor onto what
        // is left, so `pending` doesn't grow without bound across a share.
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

/// Create and publish the second LiveKit track a share carries when the
/// user asked for its audio, and park it in screenshare state so `stop`
/// can unpublish it.
///
/// Called only once the capture backend has announced a real audio format
/// — publishing on intent rather than on arrival would show every viewer a
/// speaker icon for a track that may never carry a sample.
///
/// The track needs no E2EE wiring of its own: encryption is configured at
/// the room (`RoomOptions::encryption`, set in `voice::lifecycle` from the
/// MLS-exported key), so libwebrtc's `FrameCryptor` attaches to *every*
/// track the participant publishes. Shared audio is ciphertext to the SFU
/// on exactly the same terms as the mic, and rotates with the same
/// `KeyProvider` on every MLS epoch advance.
///
/// Returns `None` after emitting `LocalAudioUnavailable` if the publish
/// fails; the caller keeps the video share running.
pub(super) async fn publish_shared_audio_track(
    state: &Arc<AppState>,
    room: &Arc<Room>,
) -> Option<NativeAudioSource> {
    // Every processing stage off. This is a verbatim copy of what the
    // machine is playing, not a microphone: noise suppression and AGC
    // would chew up music, and there is no acoustic echo to cancel.
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
                // The tag that lets a receiver tell shared audio from a
                // microphone; `voice`'s room loop reads it to keep a
                // shared video's soundtrack from lighting up the sharer's
                // speaking indicator.
                source: TrackSource::ScreenshareAudio,
                ..Default::default()
            },
        )
        .await
    {
        eprintln!("[screenshare/audio] publish error: {e}");
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
    eprintln!("[screenshare/audio] shared-audio track published @ 48 kHz mono");
    {
        let mut ss = state.screenshare.lock().await;
        ss.local_audio_source = Some(source.clone());
        ss.local_audio_track = Some(track);
    }
    emit(state, ScreenShareEvent::LocalAudioStarted).await;
    Some(source)
}

/// Take the shared-audio track + source out of state and disarm the
/// self-echo tap — the state transition behind an audio-only failure
/// mid-share. Returns the pair for the caller to unpublish: that needs
/// the room, and keeping the room out of here is what lets a unit test
/// prove the transition leaves nothing behind.
///
/// Both halves come out together on purpose. Leaving the track parked
/// would keep every viewer's speaker indicator lit on a track that no
/// longer carries a sample; leaving the tap armed would keep the voice
/// mixer feeding a canceller nothing is reading any more.
pub(super) fn retire_shared_audio(
    ss: &mut ScreenShareState,
    slot: &SelfEchoSlot,
) -> Option<(LocalAudioTrack, NativeAudioSource)> {
    self_echo::disarm(slot);
    let track = ss.local_audio_track.take();
    let source = ss.local_audio_source.take();
    match (track, source) {
        (Some(track), Some(source)) => Some((track, source)),
        // A track without its source (or the reverse) is not a state
        // `publish_shared_audio_track` can produce; if one half is
        // somehow missing there is nothing coherent to unpublish.
        _ => None,
    }
}

/// Tear down the shared-audio track mid-share, leaving the video share
/// untouched. Unpublishes before dropping the source, in the same order
/// `stop` uses, so an in-flight `capture_frame` cannot outlive its
/// backing. Idempotent: a second call finds nothing parked and returns.
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
        eprintln!("[screenshare/audio] unpublish error: {e}");
    }
    drop(source);
    eprintln!("[screenshare/audio] shared-audio track unpublished");
}

/// Send one screen-share event without holding the state lock across the
/// send. Shared by the audio paths, which emit from several places.
pub(super) async fn emit(state: &Arc<AppState>, event: ScreenShareEvent) {
    let sink = {
        let ss = state.screenshare.lock().await;
        ss.events.clone()
    };
    if let Some(sink) = sink {
        let _ = sink.send(event);
    }
}

/// Classify a helper `Error` message. A capture backend reports a failure
/// to open *audio* through the same 0xFF channel it uses for fatal video
/// errors, distinguished only by this prefix — see the wire-protocol table
/// in `pollis-capture-proto`. Getting this wrong in the fatal direction
/// would tear down a perfectly good video share because the machine had no
/// output device.
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
        // Only whole frames come out, so up to one frame of resampled
        // audio is still buffered when the input runs out.
        assert!(
            out.abs_diff(expected) <= SHARED_AUDIO_FRAME_SAMPLES,
            "44.1k->48k produced {out} samples, expected ~{expected}"
        );
    }

    /// The drift case: at 44.1 kHz the resample ratio is irrational in
    /// whole samples, so a resampler that restarted its cursor on every
    /// block would lose a fraction of a sample each time. Over 500 blocks
    /// that is an audible, accumulating pitch/timing error. Pushing the
    /// same total input as many small blocks must land within one frame of
    /// pushing it as one big one.
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
            // The f32 round trip is the same precision loss the mic path
            // already takes; allow the resulting +/-1 LSB, nothing more.
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

    /// The prefix is load-bearing: a mis-classified audio failure would
    /// end a working video share, and a mis-classified fatal error would
    /// leave a dead share on screen. Pinned in both directions.
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

    /// A parked track + source, the shape `publish_shared_audio_track`
    /// leaves in state once the track is live.
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

    /// The #1040 regression: an `audio:` error mid-share must leave neither
    /// the track parked (viewers' speaker indicator stays lit) nor the
    /// self-echo tap armed (the mixer keeps feeding a dead canceller).
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
