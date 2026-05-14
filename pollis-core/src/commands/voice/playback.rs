use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
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
use super::streams::start_speaker_stream;
use super::types::{
    user_id_from_voice_identity, TrackBuffers, VoiceEvent, VoiceState, TRACK_BUFFER_CAP_SAMPLES,
};

// ── Mixer + per-track drain ───────────────────────────────────────────────

/// Drain a remote track's `NativeAudioStream` into a per-track ring buffer
/// and emit speaking-state transitions. Runs as one tokio task per
/// subscribed remote audio track. The mixer reads from the buffer.
async fn run_drain_task(
    rtc_track: libwebrtc::audio_track::RtcAudioTrack,
    track_key: String,
    track_buffers: TrackBuffers,
    voice_arc: Arc<tokio::sync::Mutex<VoiceState>>,
    participant_identity: String,
    sample_rate: u32,
) {
    let mut audio_stream = NativeAudioStream::new(rtc_track, sample_rate as i32, 1);

    let mut onset_frames: u32 = 0;
    let mut speak_hold: u32 = 0;
    let mut is_speaking = false;

    eprintln!("[voice] remote drain task started for {track_key}");
    while let Some(frame) = audio_stream.next().await {
        let peak = frame.data.iter().map(|&s| s.abs()).max().unwrap_or(0);

        if peak > 1000 {
            onset_frames += 1;
            if onset_frames >= 2 {
                speak_hold = 12;
            }
        } else {
            onset_frames = 0;
            if speak_hold > 0 {
                speak_hold -= 1;
            }
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
        let buf = buffers.entry(track_key.clone()).or_insert_with(VecDeque::new);
        buf.extend(frame.data.iter().map(|&s| s as f32 / 32_768.0));
        while buf.len() > TRACK_BUFFER_CAP_SAMPLES {
            buf.pop_front();
        }
    }
    eprintln!("[voice] remote drain task ended for {track_key}");

    // Stream closed — clean up our buffer slot so the mixer doesn't keep
    // reading a dead entry.
    let mut buffers = track_buffers.lock().unwrap();
    buffers.remove(&track_key);
}

/// Mix every active per-track buffer into a single 10 ms frame, send a copy
/// to APM as the AEC render reference, and push the frame (channel-duplicated)
/// onto the cpal output ring. Runs at 100 Hz for the duration of the voice
/// session; aborted on `leave_voice_channel` or output-device switch.
async fn run_mixer_task(
    track_buffers: TrackBuffers,
    user_volumes: Arc<Mutex<HashMap<String, f32>>>,
    output_ring: Arc<Mutex<VecDeque<f32>>>,
    output_channels: u32,
    output_capacity_samples: usize,
    apm_processor: Option<Arc<ApmProcessor>>,
    apm_frame_samples: usize,
) {
    let mut interval = tokio::time::interval(Duration::from_millis(10));
    // Skip catch-up bursts: under sustained load we'd rather lose 10 ms than
    // process several frames back-to-back and inject a click.
    interval.set_missed_tick_behavior(MissedTickBehavior::Skip);

    let mut mix = vec![0.0f32; apm_frame_samples];

    loop {
        interval.tick().await;

        // Reset mix to silence
        for s in mix.iter_mut() {
            *s = 0.0;
        }

        // Snapshot per-user volumes once per tick so the mixer doesn't
        // hold the volumes lock across the buffer drain. Map is small
        // (one entry per remote participant the user has adjusted) so
        // cloning it is cheap compared to the per-sample work below.
        let volumes_snapshot: HashMap<String, f32> = {
            let guard = user_volumes.lock().unwrap();
            guard.clone()
        };

        // Sum available samples from each track, scaling by the
        // per-user gain. Tracks that don't have a full 10 ms ready
        // contribute partial silence — that's a tiny glitch but fixing
        // it would require waiting, which would back-pressure the mixer.
        {
            let mut buffers = track_buffers.lock().unwrap();
            for (track_key, buf) in buffers.iter_mut() {
                // Track key format is `"{identity}-{sid}"`. LiveKit SIDs
                // are dash-free alphanumerics (e.g. `TR_AVxN6oCmK4hDk7`),
                // so the last `-` always sits between identity and sid.
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

        // Soft-clip in case multiple participants stack onto a near-full
        // sample. Hard clipping past ±1.0 sounds harsh; we just hold here.
        for s in mix.iter_mut() {
            *s = s.clamp(-1.0, 1.0);
        }

        // AEC render reference: APM analyses the about-to-play signal so its
        // echo subtraction has something to subtract on the next capture.
        if let Some(apm) = &apm_processor {
            let _ = voice_apm::analyze_render(apm, &mix, apm_frame_samples);
        }

        // Push to the cpal output ring. The output stream's callback
        // de-interleaves, so we duplicate mono → output_channels here.
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

/// Open the speaker, spawn the mixer task. Idempotent: if a stream is
/// already running on `output_device_name`, return the existing
/// `(sample_rate, ring)` without rebuilding. Otherwise tear the old one
/// down first.
pub(crate) async fn ensure_playback(
    voice_arc: Arc<tokio::sync::Mutex<VoiceState>>,
    output_device_name: Option<String>,
    apm_handle: Option<Arc<ApmProcessor>>,
    apm_rate: u32,
    apm_frame_samples: usize,
) -> Result<()> {
    // ── Reuse existing stream if already on the requested device. ─────────
    {
        let voice = voice_arc.lock().await;
        let pb = voice.playback.lock().unwrap();
        if pb.output_stream.is_some()
            && pb.output_device_name.as_deref() == output_device_name.as_deref()
        {
            return Ok(());
        }
    }

    // Tear down any existing pipeline first so device-switch is clean.
    {
        let voice = voice_arc.lock().await;
        let mut pb = voice.playback.lock().unwrap();
        pb.stop_all();
    }

    // Open the new cpal output stream on a blocking thread (ALSA syscalls).
    // If the requested device fails (e.g. stale pref, or a duplex Bluetooth
    // device that cpal can't query in its current state), fall back to the
    // OS default output so the user at least hears remote audio.
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
                    eprintln!(
                        "[voice] speaker open failed for '{:?}': {e} — falling back to OS default output",
                        output_device_name_for_open
                    );
                    let (s, sr, ch, ring) = try_open(None)?;
                    Ok((s, sr, ch, ring, None))
                }
            }
        })
        .await
        .map_err(|e| anyhow::anyhow!("speaker init panicked: {e}"))??;
    let output_device_name = opened_device_name;

    // If the speaker can't run at the APM rate (very rare; e.g. forced
    // device override), the AEC render reference would be at the wrong
    // rate. Disabling the tap is preferable to feeding APM aliased data.
    let apm_for_mixer = if sample_rate == apm_rate {
        apm_handle
    } else {
        eprintln!(
            "[voice] speaker rate {sample_rate} Hz != APM rate {apm_rate} Hz; \
             AEC render reference disabled for this session"
        );
        None
    };

    let (track_buffers, user_volumes, output_capacity_samples) = {
        let voice = voice_arc.lock().await;
        let pb = voice.playback.lock().unwrap();
        let cap = (sample_rate as usize) * (channels as usize) / 5; // 200 ms
        (
            Arc::clone(&pb.track_buffers),
            Arc::clone(&pb.user_volumes),
            cap,
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

/// Subscribe a newly-arrived remote audio track to the playback pipeline:
/// spawn its drain task, register it for mixing. Output stream + mixer are
/// expected to be already running (set up at join time).
pub(crate) async fn register_remote_track(
    rtc_track: libwebrtc::audio_track::RtcAudioTrack,
    track_key: String,
    voice_arc: Arc<tokio::sync::Mutex<VoiceState>>,
    participant_identity: String,
    apm_rate: u32,
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
    ));

    let voice = voice_arc.lock().await;
    let mut pb = voice.playback.lock().unwrap();
    if let Some(prev) = pb.drain_tasks.insert(track_key.clone(), task) {
        prev.abort();
    }
    pb.rtc_tracks.insert(track_key.clone(), rtc_track);
    pb.identities.insert(track_key, participant_identity);
}
