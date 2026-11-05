//! Remote playback: one drain task per track feeding the pull-model mixer
//! (`mixer.rs`), and the single cpal output stream that pulls from it.

use std::{sync::Arc, time::Instant};

use futures_util::StreamExt;
use libwebrtc::audio_stream::native::NativeAudioStream;

use crate::error::Result;

use super::devices::get_device;
use super::levels::{frame_peak, BandAnalyzer, LevelsHub};
use super::mixer::{OutputMixer, TrackFeed};
use super::resampler::{MIX_FRAME_SAMPLES, MIX_RATE_HZ};
use super::streams::build_output_stream;
use super::types::{user_id_from_voice_identity, VoiceEvent, VoiceState};

/// Speaking: two consecutive loud frames to trigger, 120 ms hold.
const SPEAK_PEAK: u16 = 1_000;
const SPEAK_HOLD_FRAMES: u32 = 12;

/// Drain a remote track into its ring and emit speaking transitions.
///
/// Shared audio (`is_shared_audio`) mixes but never drives speaking/meters.
async fn run_drain_task(
    rtc_track: libwebrtc::audio_track::RtcAudioTrack,
    track_key: String,
    mut feed: TrackFeed,
    levels: Arc<LevelsHub>,
    event_sink: Option<Arc<dyn crate::sink::EventSink<VoiceEvent>>>,
    participant_identity: String,
    is_shared_audio: bool,
) {
    // libwebrtc resamples to the mix rate for us; 10 ms mono frames.
    let mut audio_stream = NativeAudioStream::new(rtc_track, MIX_RATE_HZ as i32, 1);

    let mut onset_frames: u32 = 0;
    let mut speak_hold: u32 = 0;
    let mut is_speaking = false;
    let mut analyzer = BandAnalyzer::new(MIX_RATE_HZ);
    let mut f32_frame = vec![0.0f32; MIX_FRAME_SAMPLES];

    crate::diag!("[voice] remote drain task started for {track_key}");
    while let Some(frame) = audio_stream.next().await {
        if !is_shared_audio {
            let peak = frame_peak(&frame.data);
            analyzer.process(&frame.data);
            if let Some(ev) =
                levels.update(&participant_identity, analyzer.levels(), Instant::now())
            {
                if let Some(ch) = &event_sink {
                    let _ = ch.send(ev);
                }
            }
            if peak > SPEAK_PEAK {
                onset_frames += 1;
                if onset_frames >= 2 {
                    speak_hold = SPEAK_HOLD_FRAMES;
                }
            } else {
                onset_frames = 0;
                speak_hold = speak_hold.saturating_sub(1);
            }
            let now_speaking = speak_hold > 0;
            if now_speaking != is_speaking {
                is_speaking = now_speaking;
                if let Some(ch) = &event_sink {
                    let ev = if is_speaking {
                        VoiceEvent::SpeakingStarted {
                            identity: participant_identity.clone(),
                        }
                    } else {
                        VoiceEvent::SpeakingStopped {
                            identity: participant_identity.clone(),
                        }
                    };
                    let _ = ch.send(ev);
                }
            }
        }

        f32_frame.clear();
        f32_frame.extend(frame.data.iter().map(|&s| f32::from(s) / 32_768.0));
        feed.push(&f32_frame);
    }
    if !is_shared_audio {
        levels.remove(&participant_identity);
    }
    crate::diag!(
        "[voice] remote drain task ended for {track_key} ({} samples dropped at the ring)",
        feed.dropped_samples()
    );
}

/// Open the speaker on `output_device_name` (reusing an open stream on the
/// same device). Tracks, rings and the AEC tap live in `MixerShared` and
/// are untouched: switching devices mid-call keeps every participant.
pub(crate) async fn ensure_playback(
    voice_arc: Arc<tokio::sync::Mutex<VoiceState>>,
    output_device_name: Option<String>,
) -> Result<()> {
    let (shared, previous) = {
        let voice = voice_arc.lock().await;
        let mut pb = voice.playback.lock().unwrap();
        if pb.output_stream.is_some()
            && pb.output_device_name.as_deref() == output_device_name.as_deref()
        {
            return Ok(());
        }
        (Arc::clone(&pb.shared), pb.stop_output())
    };

    // Open on a blocking thread; on failure fall back to the OS default output.
    // The old stream drops there too (ALSA close blocks; macOS indicator).
    let output_device_name_for_open = output_device_name.clone();
    let (stream, sample_rate, channels, opened_device_name) =
        tokio::task::spawn_blocking(move || -> Result<_> {
            drop(previous);
            let host = cpal::default_host();
            let try_open = |name: Option<&str>| -> Result<_> {
                let dev = get_device(&host, name, false)?;
                let shared = Arc::clone(&shared);
                build_output_stream(&dev, move |rate, channels| {
                    let mut mixer = OutputMixer::new(shared, rate, channels);
                    move |out: &mut [f32]| mixer.fill(out)
                })
            };
            match try_open(output_device_name_for_open.as_deref()) {
                Ok((s, sr, ch)) => Ok((s, sr, ch, output_device_name_for_open)),
                Err(e) => {
                    let was_explicit = output_device_name_for_open
                        .as_deref()
                        .map(|n| !n.is_empty() && n != "default")
                        .unwrap_or(false);
                    if !was_explicit {
                        return Err(e);
                    }
                    crate::diag!(
                        "[voice] speaker open failed for the selected device: {e} — falling back to OS default output"
                    );
                    let (s, sr, ch) = try_open(None)?;
                    Ok((s, sr, ch, None))
                }
            }
        })
        .await
        .map_err(|e| anyhow::anyhow!("speaker init panicked: {e}"))??;

    let voice = voice_arc.lock().await;
    let mut pb = voice.playback.lock().unwrap();
    pb.output_stream = Some(stream);
    pb.output_sample_rate = sample_rate;
    pb.output_channels = channels;
    pb.output_device_name = opened_device_name;
    Ok(())
}

/// Spawn a drain task for a new remote audio track.
pub(crate) async fn register_remote_track(
    rtc_track: libwebrtc::audio_track::RtcAudioTrack,
    track_key: String,
    voice_arc: Arc<tokio::sync::Mutex<VoiceState>>,
    participant_identity: String,
    is_shared_audio: bool,
) {
    let user_id = user_id_from_voice_identity(&participant_identity).to_string();
    let (feed, levels, event_sink) = {
        let voice = voice_arc.lock().await;
        let pb = voice.playback.lock().unwrap();
        let gain = pb.user_volumes.get(&user_id).copied().unwrap_or(1.0);
        (
            pb.shared.add_track(track_key.clone(), user_id, gain),
            Arc::clone(&voice.levels),
            voice.channel.clone(),
        )
    };

    let task = tokio::spawn(run_drain_task(
        rtc_track,
        track_key.clone(),
        feed,
        levels,
        event_sink,
        participant_identity,
        is_shared_audio,
    ));

    let voice = voice_arc.lock().await;
    let mut pb = voice.playback.lock().unwrap();
    if let Some(prev) = pb.drain_tasks.insert(track_key, task) {
        prev.abort();
    }
}

/// Stop a track's drain task and free its ring (TrackUnsubscribed).
pub(crate) fn unregister_remote_track(voice: &VoiceState, track_key: &str) {
    let mut pb = voice.playback.lock().unwrap();
    if let Some(t) = pb.drain_tasks.remove(track_key) {
        t.abort();
    }
    // An aborted drain never reaches its own cleanup; the key is
    // `"{identity}-{sid}"` and SIDs have no `-`. A participant's other track
    // (shared audio) re-registers the meter on its next frame if it has one.
    if let Some((identity, _)) = track_key.rsplit_once('-') {
        voice.levels.remove(identity);
    }
    if let Some(stats) = pb.shared.remove_track(track_key) {
        if stats.underruns > 0 || stats.drift_drops > 0 {
            crate::diag!(
                "[voice] track {track_key}: {} underruns concealed, {} frames dropped for drift",
                stats.underruns,
                stats.drift_drops
            );
        }
    }
}
