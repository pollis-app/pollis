use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use futures_util::StreamExt;
use libwebrtc::{
    audio_stream::native::NativeAudioStream,
};
use tokio::time::MissedTickBehavior;

use crate::{
    commands::{
        voice_apm,
        voice_apm::Processor as ApmProcessor,
    },
    error::Result,
};

use super::devices::get_device;
use super::levels::BandAnalyzer;
use super::streams::start_speaker_stream;
use super::types::{
    user_id_from_voice_identity, TrackBuffers, VoiceEvent, VoiceState, TRACK_BUFFER_CAP_SAMPLES,
};

/// Emit an AudioBands event every N drained frames (~10 ms each) ⇒ ~20 Hz.
const BANDS_EMIT_EVERY: u32 = 5;

/// Drain a remote track into its ring buffer and emit speaking transitions;
/// one task per subscribed remote audio track.
///
/// `is_shared_audio` marks a screen share's soundtrack: mixed like any track,
/// but never driving the speaking indicator or level meter, or a shared video
/// would show the sharer talking throughout.
async fn run_drain_task(
    rtc_track: libwebrtc::audio_track::RtcAudioTrack,
    track_key: String,
    track_buffers: TrackBuffers,
    voice_arc: Arc<tokio::sync::Mutex<VoiceState>>,
    participant_identity: String,
    sample_rate: u32,
    is_shared_audio: bool,
) {
    let mut audio_stream = NativeAudioStream::new(rtc_track, sample_rate as i32, 1);

    let mut onset_frames: u32 = 0;
    let mut speak_hold: u32 = 0;
    let mut is_speaking = false;

    // Clone the sink once so the hot loop never re-locks VoiceState for levels.
    let event_sink = voice_arc.lock().await.channel.clone();
    let mut analyzer = BandAnalyzer::new(sample_rate);
    let mut bands_ctr: u32 = 0;

    crate::diag!("[voice] remote drain task started for {track_key}");
    while let Some(frame) = audio_stream.next().await {
        if is_shared_audio {
            let mut buffers = track_buffers.lock().unwrap();
            let buf = buffers.entry(track_key.clone()).or_default();
            buf.extend(frame.data.iter().map(|&s| s as f32 / 32_768.0));
            while buf.len() > TRACK_BUFFER_CAP_SAMPLES {
                buf.pop_front();
            }
            continue;
        }

        let peak = frame.data.iter().map(|&s| s.abs()).max().unwrap_or(0);

        analyzer.process(&frame.data);
        bands_ctr += 1;
        if bands_ctr >= BANDS_EMIT_EVERY {
            bands_ctr = 0;
            if let Some(ch) = &event_sink {
                let _ = ch.send(VoiceEvent::AudioBands {
                    identity: participant_identity.clone(),
                    bands: analyzer.levels(),
                });
            }
        }

        if peak > 1000 {
            onset_frames += 1;
            if onset_frames >= 2 {
                speak_hold = 12;
            }
        } else {
            onset_frames = 0;
            speak_hold = speak_hold.saturating_sub(1);
        }
        let now_speaking = speak_hold > 0;
        if now_speaking != is_speaking {
            is_speaking = now_speaking;
            let voice = voice_arc.lock().await;
            if let Some(ch) = &voice.channel {
                if is_speaking {
                    let _ = ch.send(VoiceEvent::SpeakingStarted {
                        identity: participant_identity.clone(),
                    });
                } else {
                    let _ = ch.send(VoiceEvent::SpeakingStopped {
                        identity: participant_identity.clone(),
                    });
                }
            }
        }

        let mut buffers = track_buffers.lock().unwrap();
        let buf = buffers.entry(track_key.clone()).or_default();
        buf.extend(frame.data.iter().map(|&s| s as f32 / 32_768.0));
        while buf.len() > TRACK_BUFFER_CAP_SAMPLES {
            buf.pop_front();
        }
    }
    crate::diag!("[voice] remote drain task ended for {track_key}");

    // Stream closed: drop the slot so the mixer stops reading a dead entry.
    let mut buffers = track_buffers.lock().unwrap();
    buffers.remove(&track_key);
}

/// Mix every track buffer into one 10 ms frame, hand it to APM as the AEC
/// render reference, and push it (channel-duplicated) to the output ring, at
/// 100 Hz until aborted.
// The mixer's whole working set, moved into the task once at spawn.
#[allow(clippy::too_many_arguments)]
async fn run_mixer_task(
    track_buffers: TrackBuffers,
    user_volumes: Arc<Mutex<HashMap<String, f32>>>,
    output_ring: Arc<Mutex<VecDeque<f32>>>,
    output_channels: u32,
    output_capacity_samples: usize,
    apm_processor: Option<Arc<ApmProcessor>>,
    apm_frame_samples: usize,
    deafened: Arc<AtomicBool>,
    apm_rate: u32,
    shared_audio_render: crate::commands::screenshare::self_echo::SelfEchoSlot,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(10));
    // Under sustained load, lose 10 ms rather than burst frames into a click.
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut mix = vec![0.0f32; apm_frame_samples];

    loop {
        interval.tick().await;

        for s in mix.iter_mut() {
            *s = 0.0;
        }

        // Snapshot volumes once per tick (a small map) rather than hold that
        // lock across the drain.
        let volumes_snapshot: HashMap<String, f32> = {
            let guard = user_volumes.lock().unwrap();
            guard.clone()
        };

        // A track without a full 10 ms contributes partial silence: a tiny
        // glitch, but waiting would back-pressure the mixer.
        {
            let mut buffers = track_buffers.lock().unwrap();
            for (track_key, buf) in buffers.iter_mut() {
                // `"{identity}-{sid}"`: LiveKit SIDs contain no dash, so the
                // last `-` splits identity from sid.
                let gain = track_key
                    .rsplit_once('-')
                    .map(|(id, _)| user_id_from_voice_identity(id))
                    .and_then(|uid| volumes_snapshot.get(uid).copied())
                    .unwrap_or(1.0);
                let take = buf.len().min(apm_frame_samples);
                if gain == 1.0 {
                    for slot in mix.iter_mut().take(take) {
                        if let Some(s) = buf.pop_front() {
                            *slot += s;
                        }
                    }
                } else {
                    for slot in mix.iter_mut().take(take) {
                        if let Some(s) = buf.pop_front() {
                            *slot += s * gain;
                        }
                    }
                }
            }
        }

        for s in mix.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }

        // Deafen (#849): after the drain, so buffers keep advancing and
        // undeafen resumes live; before the APM tap, so the echo canceller's
        // reference is silence too while nothing reaches the speaker.
        if deafened.load(Ordering::Relaxed) {
            for s in mix.iter_mut() {
                *s = 0.0;
            }
        }

        if let Some(apm) = &apm_processor {
            let _ = voice_apm::analyze_render(apm, &mix, apm_frame_samples);
        }

        // The same signal for screen-share self-echo cancellation, or a share
        // capturing system audio would re-publish the call.
        crate::commands::screenshare::self_echo::analyze_render(
            &shared_audio_render,
            &mix,
            apm_rate,
        );

        // The output callback de-interleaves: duplicate mono per channel.
        {
            let mut ring = output_ring.lock().unwrap();
            for s in mix.iter() {
                for _ in 0..output_channels {
                    ring.push_back(*s);
                }
            }
            while ring.len() > output_capacity_samples {
                ring.pop_front();
            }
        }
    }
}

/// Open the speaker and spawn the mixer. A no-op if already running on
/// `output_device_name`; otherwise the old pipeline is torn down first.
pub(crate) async fn ensure_playback(
    voice_arc: Arc<tokio::sync::Mutex<VoiceState>>,
    output_device_name: Option<String>,
    apm_handle: Option<Arc<ApmProcessor>>,
    apm_rate: u32,
    apm_frame_samples: usize,
) -> Result<()> {
    {
        let voice = voice_arc.lock().await;
        let pb = voice.playback.lock().unwrap();
        if pb.output_stream.is_some()
            && pb.output_device_name.as_deref() == output_device_name.as_deref()
        {
            return Ok(());
        }
    }

    {
        let voice = voice_arc.lock().await;
        let mut pb = voice.playback.lock().unwrap();
        pb.stop_all();
    }

    // On a blocking thread (ALSA syscalls). If the chosen device fails (stale
    // preference, a duplex Bluetooth device cpal can't query), fall back to
    // the OS default so remote audio is still heard.
    let output_device_name_for_open = output_device_name.clone();
    let (stream, sample_rate, channels, output_ring, opened_device_name) =
        tokio::task::spawn_blocking(move || -> Result<_> {
            let host = cpal::default_host();
            let try_open = |name: Option<&str>| -> Result<_> {
                let dev = get_device(&host, name, false)?;
                let (stream, sr, ch, ring) = start_speaker_stream(&dev, apm_rate)?;
                Ok((stream, sr, ch, ring))
            };
            match try_open(output_device_name_for_open.as_deref()) {
                Ok((s, sr, ch, ring)) => Ok((s, sr, ch, ring, output_device_name_for_open)),
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
                    let (s, sr, ch, ring) = try_open(None)?;
                    Ok((s, sr, ch, ring, None))
                }
            }
        })
        .await
        .map_err(|e| anyhow::anyhow!("speaker init panicked: {e}"))??;
    let output_device_name = opened_device_name;

    // At a speaker rate other than APM's, the AEC reference would be wrong;
    // disable the tap rather than feed APM aliased data.
    let apm_for_mixer = if sample_rate == apm_rate {
        apm_handle
    } else {
        crate::diag!(
            "[voice] speaker rate {sample_rate} Hz != APM rate {apm_rate} Hz; \
             AEC render reference disabled for this session"
        );
        None
    };

    // `deafened` and the self-echo slot come from VoiceState, not
    // PlaybackState, so they survive the mixer rebuild on a device switch.
    let (track_buffers, user_volumes, output_capacity_samples, deafened, shared_audio_render) = {
        let voice = voice_arc.lock().await;
        let deafened = Arc::clone(&voice.deafened);
        let shared_audio_render = Arc::clone(&voice.shared_audio_render);
        let pb = voice.playback.lock().unwrap();
        let cap = (sample_rate as usize) * (channels as usize) / 5; // 200 ms
        (
            Arc::clone(&pb.track_buffers),
            Arc::clone(&pb.user_volumes),
            cap,
            deafened,
            shared_audio_render,
        )
    };

    let mixer_task = tokio::spawn(run_mixer_task(
        track_buffers,
        user_volumes,
        Arc::clone(&output_ring),
        channels,
        output_capacity_samples,
        apm_for_mixer,
        apm_frame_samples,
        deafened,
        apm_rate,
        shared_audio_render,
    ));

    let voice = voice_arc.lock().await;
    let mut pb = voice.playback.lock().unwrap();
    pb.output_stream = Some(stream);
    pb.output_ring = Some(output_ring);
    pb.output_sample_rate = sample_rate;
    pb.output_channels = channels;
    pb.output_device_name = output_device_name;
    pb.mixer_task = Some(mixer_task);
    Ok(())
}

/// Subscribe a newly arrived remote audio track: spawn its drain task and
/// register it for mixing. The output stream and mixer already run.
pub(crate) async fn register_remote_track(
    rtc_track: libwebrtc::audio_track::RtcAudioTrack,
    track_key: String,
    voice_arc: Arc<tokio::sync::Mutex<VoiceState>>,
    participant_identity: String,
    apm_rate: u32,
    is_shared_audio: bool,
) {
    let track_buffers = {
        let voice = voice_arc.lock().await;
        let pb = voice.playback.lock().unwrap();
        Arc::clone(&pb.track_buffers)
    };

    let task_key = track_key.clone();
    let voice_for_task = Arc::clone(&voice_arc);
    let identity_for_task = participant_identity.clone();
    let task = tokio::spawn(run_drain_task(
        rtc_track.clone(),
        task_key,
        track_buffers,
        voice_for_task,
        identity_for_task,
        apm_rate,
        is_shared_audio,
    ));

    let voice = voice_arc.lock().await;
    let mut pb = voice.playback.lock().unwrap();
    if let Some(prev) = pb.drain_tasks.insert(track_key.clone(), task) {
        prev.abort();
    }
    pb.rtc_tracks.insert(track_key.clone(), rtc_track);
    pb.identities.insert(track_key, participant_identity);
}
