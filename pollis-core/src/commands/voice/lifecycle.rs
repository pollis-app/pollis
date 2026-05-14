use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use cpal::traits::DeviceTrait;
use libwebrtc::{
    audio_source::native::NativeAudioSource,
    prelude::{AudioFrame, AudioSourceOptions, RtcAudioSource},
};
use livekit::{
    options::TrackPublishOptions,
    prelude::*,
    track::{LocalAudioTrack, LocalTrack, RemoteTrack},
};

use crate::{
    commands::{
        livekit::{lookup_avatar_url, lookup_avatar_url_for_identity, make_token},
        voice_apm,
        voice_denoiser,
        voice_e2ee,
    },
    error::Result,
    state::AppState,
};

use super::devices::get_device;
use super::playback::{ensure_playback, register_remote_track};
use super::streams::start_mic_stream;
use super::types::{JoinTimings, VoiceEvent, VoiceWarmup, VOICE_WARMUP_TTL};

// ── Tauri commands ────────────────────────────────────────────────────────

/// Register the Tauri Channel used to push VoiceEvents to the frontend.
/// Call once on app startup, just like subscribe_realtime.
pub async fn subscribe_voice_events(
    on_event: std::sync::Arc<dyn crate::sink::EventSink<VoiceEvent>>,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut voice = state.voice.lock().await;
    voice.channel = Some(on_event);
    Ok(())
}

/// "Hot mic" warmup: signals user intent to (maybe) join `channel_id` so the
/// client can pre-pay the latency before they actually click Join. Mirrors
/// `room.prepareConnection(url, token)` from the JS livekit-client SDK,
/// which has no equivalent in the `livekit` Rust crate (v0.7) — the Rust
/// `Room::connect` is the only entry point and it commits to the room.
///
/// What we do instead:
///  1. Mint and cache the LiveKit token so the synchronous JWT work is
///     already done by the time the user clicks Join.
///  2. Fire a one-shot HTTPS request to the LiveKit server's RoomService
///     endpoint (`twirp_base/.../ListParticipants`). This warms:
///       - DNS for the LiveKit host in the OS resolver cache,
///       - the TLS session ticket cache used by `rustls` (rustls keeps a
///         per-process cache that the LiveKit WS handshake can reuse),
///       - reqwest's HTTPS connection pool, used for the same host's API.
///     The body of the response is irrelevant; we want the network plumbing
///     to be primed.
///
/// Idempotent + cancel-safe: a second call with the same channel_id while
/// the first warmup is still running is a no-op. Calling with a different
/// channel_id supersedes the pending one (its DNS/TLS work is wasted, but
/// it's a background task — no user impact).
///
/// Frontend should call this on intent points: route entry to a voice
/// channel page, hover/keyboard-select on a voice item in TerminalMenu /
/// SearchPanel, etc. Cheap enough to call eagerly.
pub async fn prepare_voice_connection(
    channel_id: String,
    user_id: String,
    display_name: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let url = state.config.livekit_url.clone();
    if url.is_empty() {
        // No LiveKit configured — nothing to warm. Silent success so the
        // frontend can call this unconditionally.
        return Ok(());
    }

    // ── De-dupe: skip if we already have a fresh warmup for this exact
    // channel + identity. Cheap to mint a token, but firing the HTTPS
    // request again would be wasted work.
    {
        let voice = state.voice.lock().await;
        if let Some(w) = &voice.warmup {
            if w.channel_id == channel_id
                && w.user_id == user_id
                && w.display_name == display_name
                && w.created_at.elapsed() < VOICE_WARMUP_TTL
            {
                return Ok(());
            }
        }
    }

    let token = make_token(
        &state.config,
        &channel_id,
        &format!("voice-{user_id}"),
        &display_name,
    )?;

    // Fire the DNS/TLS warmup in the background. If the user immediately
    // clicks Join, they'll race this — that's fine, the worst case is a
    // single redundant TLS handshake. Errors are non-fatal: this whole
    // command is best-effort.
    let warm_url = url.clone();
    let task = tokio::spawn(async move {
        // Reuse the same twirp transform `livekit::room_service_list_participants`
        // uses; we don't import it to keep the dependency direction clean.
        let twirp = if let Some(rest) = warm_url.strip_prefix("wss://") {
            format!("https://{rest}")
        } else if let Some(rest) = warm_url.strip_prefix("ws://") {
            format!("http://{rest}")
        } else {
            warm_url.clone()
        };
        let probe = format!("{twirp}/rtc/validate");
        // Short timeout — if the server is slow there's nothing to gain by
        // hanging on. The handshake is what we care about, not the response.
        let client = match reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        let started = Instant::now();
        match client.get(&probe).send().await {
            Ok(_resp) => {
                eprintln!(
                    "[voice] warmup probe to {twirp} completed in {:.0}ms",
                    started.elapsed().as_secs_f64() * 1000.0
                );
            }
            Err(e) => {
                eprintln!("[voice] warmup probe failed (non-fatal): {e}");
            }
        }
    });

    // Stash the warm credentials. Replacing any prior entry triggers Drop
    // on the previous warmup, which aborts its still-running background task
    // so a fast hover-flip doesn't pile up redundant probes.
    let mut voice = state.voice.lock().await;
    voice.warmup = Some(VoiceWarmup {
        channel_id,
        token,
        created_at: Instant::now(),
        user_id,
        display_name,
        task: Some(task),
    });

    Ok(())
}

/// Connect to a LiveKit voice room, publish the local microphone, and start
/// the remote-playback pipeline (single shared mixer + cpal output stream).
///
/// `audio_processing` carries the user's APM preferences (AGC, NS, AEC).
/// libwebrtc's internal AudioProcessingModule is disabled at the
/// `AudioSourceOptions` level so we don't double-process: APM is the only
/// stage that touches the mic signal between cpal and `capture_frame`.
///
/// Performance: the LiveKit network handshake (DNS, TLS, WS upgrade) and
/// the cpal mic init (ALSA enumeration + device open) are independent and
/// both block for hundreds of milliseconds on cold starts. We run them
/// concurrently with `tokio::join!` so total join latency is ~max(net, mic)
/// rather than net+mic. If `prepare_voice_connection` was called for this
/// channel, DNS/TLS is already warm and we reuse the precomputed token.
pub async fn join_voice_channel(
    channel_id: String,
    user_id: String,
    display_name: String,
    input_device: Option<String>,
    output_device: Option<String>,
    audio_processing: voice_apm::ApmConfig,
    // The other participant in a 1:1 call (`call-<ulid>` room). Required for
    // those rooms because they have no DB row; ignored for group channels
    // and DMs, which carry their MLS group id implicitly.
    counterparty_user_id: Option<String>,
    state: &Arc<AppState>,
) -> Result<()> {
    // Wall-clock anchor for `total_join_ms` and per-phase deltas.
    let join_start = Instant::now();
    let join_started_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    // Refuse re-entry if a join is already in flight or a room is already
    // connected. Two concurrent joins with the same identity (`voice-{user_id}`)
    // race LiveKit's session bookkeeping and trigger DuplicateIdentity, which
    // disconnects the surviving session shortly after Connected. Holds the
    // voice lock just long enough to swap the flag, then releases.
    let _join_guard = {
        let voice = state.voice.lock().await;
        if voice.room.is_some() {
            return Err(anyhow::anyhow!("already connected to a voice channel").into());
        }
        if voice.joining.swap(true, Ordering::AcqRel) {
            return Err(anyhow::anyhow!("voice channel join already in progress").into());
        }
        struct JoinGuard(Arc<AtomicBool>);
        impl Drop for JoinGuard {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        JoinGuard(Arc::clone(&voice.joining))
    };

    let url = state.config.livekit_url.clone();
    if url.is_empty() {
        return Err(anyhow::anyhow!("LiveKit is not configured on this server").into());
    }

    // Try to consume a fresh warmup for this exact channel + identity. If
    // the warmup is stale or for a different room/user we mint a new token.
    // VoiceWarmup implements Drop (to abort its background task) so we can't
    // simply destructure it; clone the token out before dropping the entry.
    let cached_token: Option<String> = {
        let mut voice = state.voice.lock().await;
        let usable = voice
            .warmup
            .as_ref()
            .map(|w| {
                w.channel_id == channel_id
                    && w.user_id == user_id
                    && w.display_name == display_name
                    && w.created_at.elapsed() < VOICE_WARMUP_TTL
            })
            .unwrap_or(false);
        if usable {
            let cloned = voice.warmup.as_ref().map(|w| w.token.clone());
            // Drop the entry now — its background task is no longer needed.
            voice.warmup = None;
            cloned
        } else {
            // Anything else cached (different channel / stale) is dead weight.
            voice.warmup = None;
            None
        }
    };
    // ── Phase: jwt_mint ────────────────────────────────────────────────────
    // 0 if a warmup-cached token was usable.
    let jwt_mint_ms;
    let token: String = match cached_token {
        Some(t) => {
            jwt_mint_ms = 0;
            t
        }
        None => {
            let jwt_start = Instant::now();
            let t = make_token(
                &state.config,
                &channel_id,
                &format!("voice-{user_id}"),
                &display_name,
            )?;
            jwt_mint_ms = jwt_start.elapsed().as_millis() as u64;
            t
        }
    };

    // ── Run room connect and mic init concurrently ─────────────────────────
    // Both are independent and individually expensive on cold starts. Running
    // them with `tokio::join!` cuts the user-visible delay to ~max(net, mic).
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<i16>, u32)>();
    let is_muted = {
        let voice = state.voice.lock().await;
        voice.is_muted.store(false, Ordering::Relaxed);
        Arc::clone(&voice.is_muted)
    };

    let input_device_clone = input_device.clone();

    // Derive the per-room voice key from the channel's MLS exporter secret.
    // Both peers compute the same key from the same (group, epoch) so the
    // SFU never sees plaintext audio. Fails closed: if the local MLS group
    // isn't ready, refuse to join rather than fall back to unencrypted.
    let (voice_key, voice_key_index, voice_epoch, voice_mls_group_id) = voice_e2ee::derive_voice_key(
        state,
        &channel_id,
        &user_id,
        counterparty_user_id.as_deref(),
    )
    .await?;
    let e2ee_options = voice_e2ee::build_e2ee_options(voice_key);
    let key_provider_for_state = e2ee_options.key_provider.clone();
    eprintln!(
        "[voice] e2ee armed for {channel_id} (mls_group={voice_mls_group_id}, epoch={voice_epoch}, idx={voice_key_index})"
    );

    let connect_started = Instant::now();
    let mic_started = Instant::now();
    eprintln!("[voice] connecting to room {channel_id} and opening mic in parallel…");

    // `RoomOptions` is #[non_exhaustive] so build it via Default + field
    // mutation rather than a struct literal.
    let mut room_options = RoomOptions::default();
    room_options.encryption = Some(e2ee_options);
    let connect_fut = async {
        let r = Room::connect(&url, &token, room_options).await;
        let elapsed_ms = connect_started.elapsed().as_millis() as u64;
        (r, elapsed_ms)
    };
    let mic_fut = async {
        tokio::task::spawn_blocking(move || {
            let host = cpal::default_host();
            let dev = get_device(&host, input_device_clone.as_deref(), true)?;
            eprintln!("[voice] mic device: {:?}", dev.name());
            let r = start_mic_stream(&dev, frame_tx, is_muted);
            let elapsed_ms = mic_started.elapsed().as_millis() as u64;
            r.map(|(stream, rate)| (stream, rate, elapsed_ms))
        })
        .await
    };

    let (connect_pair, mic_res) = tokio::join!(connect_fut, mic_fut);
    let (connect_res, room_connect_ms) = connect_pair;
    let (room, mut events) =
        connect_res.map_err(|e| anyhow::anyhow!("LiveKit connect: {e}"))?;
    let (mic_stream, mic_rate, mic_init_ms) = mic_res
        .map_err(|e| anyhow::anyhow!("mic init panicked: {e}"))??;
    eprintln!("[voice] connected to room {channel_id}, mic at {mic_rate} Hz");

    let room = Arc::new(room);

    // ── Build APM at the mic's actual rate ────────────────────────────────
    // WebRTC supports 8/16/32/48 kHz. Anything else (e.g. legacy 44.1) means
    // we can't run APM for this session; we log and proceed without it.
    let apm_stage = match voice_apm::ApmStage::new(mic_rate, audio_processing.clone()) {
        Ok(stage) => Some(stage),
        Err(e) => {
            eprintln!("[voice] APM disabled: {e}");
            None
        }
    };
    let apm_handle = apm_stage.as_ref().map(|s| s.handle());
    let apm_frame_samples = voice_apm::frame_samples(mic_rate);

    // Build the RNNoise denoiser if the user wants click suppression and
    // the mic is at the rate the model was trained on. Other rates pass
    // through to APM unchanged — RNNoise is rate-locked at 48 kHz.
    let denoiser_arc = Arc::clone(&{
        let voice = state.voice.lock().await;
        Arc::clone(&voice.denoiser)
    });
    {
        let mut slot = denoiser_arc.lock().unwrap();
        *slot = if audio_processing.click_suppression && mic_rate == voice_denoiser::REQUIRED_RATE_HZ {
            eprintln!("[voice/rnnoise] engaged @ {mic_rate} Hz");
            Some(voice_denoiser::DenoiserStage::new())
        } else {
            if audio_processing.click_suppression {
                eprintln!(
                    "[voice/rnnoise] requested but mic rate is {mic_rate} Hz; \
                     RNNoise needs 48000 Hz — disabling for this session"
                );
            }
            None
        };
    }

    // Disable libwebrtc's internal AudioProcessingModule — APM is the only
    // stage that touches the mic signal. Leaving libwebrtc's APM on would
    // double-process and produce pumping/swirling artefacts.
    let audio_source = NativeAudioSource::new(
        AudioSourceOptions {
            echo_cancellation: false,
            noise_suppression: false,
            auto_gain_control: false,
        },
        mic_rate,
        1,
        100,
    );

    let local_track = LocalAudioTrack::create_audio_track(
        "microphone",
        RtcAudioSource::Native(audio_source.clone()),
    );

    eprintln!("[voice] publishing track…");
    // ── Phase: first_publish ───────────────────────────────────────────────
    let publish_start = Instant::now();
    room.local_participant()
        .publish_track(
            LocalTrack::Audio(local_track.clone()),
            TrackPublishOptions::default(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("publish track: {e}"))?;
    let first_publish_ms = publish_start.elapsed().as_millis() as u64;
    eprintln!("[voice] track published");

    // ── Mic frame task: rebuffer to exact 10ms, run APM, capture_frame ────
    // Speaking detection runs on the post-APM peak so the indicator follows
    // the user's effective level (after AGC + NS) rather than raw input.
    let audio_source_task = audio_source.clone();
    let voice_arc_frame = Arc::clone(&state.voice);
    let local_identity = format!("voice-{user_id}");
    let apm_for_capture = apm_handle.clone();
    let denoiser_for_capture = Arc::clone(&denoiser_arc);
    let frame_task = tokio::spawn(async move {
        let chunk_size = (mic_rate / 100) as usize;
        let mut buf: Vec<i16> = Vec::new();
        let mut speak_hold: u32 = 0; // counts down after speech stops (12 × 10ms = 120ms hold)
        let mut onset_frames: u32 = 0; // consecutive above-threshold frames; 2 required to (re)trigger
        let mut is_speaking = false;

        while let Some((samples, rate)) = frame_rx.recv().await {
            buf.extend_from_slice(&samples);
            while buf.len() >= chunk_size {
                let mut chunk: Vec<i16> = buf.drain(..chunk_size).collect();

                // RNNoise (if enabled) runs first so APM gets a cleaner signal:
                // its NS / AGC adapt to actual voice energy instead of the
                // typing/click noise floor. Std Mutex; the lock is held for
                // ~0.1 ms per frame (single-writer) and never crosses an await.
                {
                    let mut guard = denoiser_for_capture.lock().unwrap();
                    if let Some(d) = guard.as_mut() {
                        d.process(&mut chunk);
                    }
                }

                // APM mutates the chunk in place (AGC + NS + HPF + AEC capture).
                if let Some(apm) = &apm_for_capture {
                    if let Err(e) = voice_apm::run_capture(apm, &mut chunk, chunk_size) {
                        eprintln!("[voice] APM capture error (frame dropped): {e}");
                        continue;
                    }
                }

                let peak = chunk.iter().map(|&s| s.abs()).max().unwrap_or(0);

                // Speaking detection: require 2 consecutive above-threshold frames to trigger,
                // preventing single trailing spikes from resetting the hold counter.
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
                    let voice = voice_arc_frame.lock().await;
                    if let Some(ch) = &voice.channel {
                        if is_speaking {
                            let _ = ch.send(VoiceEvent::SpeakingStarted { identity: local_identity.clone() });
                        } else {
                            let _ = ch.send(VoiceEvent::SpeakingStopped { identity: local_identity.clone() });
                        }
                    }
                }

                let frame = AudioFrame {
                    data: chunk.into(),
                    sample_rate: rate,
                    num_channels: 1,
                    samples_per_channel: chunk_size as u32,
                };
                if let Err(e) = audio_source_task.capture_frame(&frame).await {
                    eprintln!("[voice] capture_frame error: {e:?}");
                }
            }
        }
    });

    // ── Open the speaker pipeline: single output stream + mixer task ──────
    // Doing this BEFORE the room event loop starts means the very first
    // TrackSubscribed has somewhere to push its decoded frames.
    if let Err(e) = ensure_playback(
        Arc::clone(&state.voice),
        output_device.clone(),
        apm_handle.clone(),
        mic_rate,
        apm_frame_samples,
    )
    .await
    {
        eprintln!("[voice] playback init failed (speaker disabled this session): {e}");
    }

    // ── Seed participant list ─────────────────────────────────────────────────
    // Emit ParticipantJoined for participants already in the room.
    // Do NOT attach tracks here — TrackSubscribed fires for pre-existing
    // subscribed tracks once the event loop drains buffered events, and
    // attaching twice creates competing draining tasks.
    let local_avatar_url = lookup_avatar_url(&state, &user_id).await;
    let existing_remote: Vec<(String, String, bool)> = room
        .remote_participants()
        .into_iter()
        .map(|(_id, p)| {
            // Seed mute state from current publications — TrackMuted only
            // fires on transitions, so a participant who muted before we
            // joined would otherwise render as unmuted indefinitely.
            let is_muted = p.track_publications().values().any(|pub_| pub_.is_muted());
            (p.identity().to_string(), p.name(), is_muted)
        })
        .collect();
    let mut existing_with_avatars: Vec<(String, String, bool, Option<String>)> =
        Vec::with_capacity(existing_remote.len());
    for (identity, name, is_muted) in existing_remote {
        let avatar = lookup_avatar_url_for_identity(&state, &identity).await;
        existing_with_avatars.push((identity, name, is_muted, avatar));
    }
    {
        let voice = state.voice.lock().await;
        if let Some(ch) = &voice.channel {
            let _ = ch.send(VoiceEvent::ParticipantJoined {
                identity: format!("voice-{user_id}"),
                name: display_name.clone(),
                is_muted: false,
                avatar_url: local_avatar_url,
            });
            for (identity, name, is_muted, avatar_url) in existing_with_avatars {
                eprintln!("[voice] existing participant: {} muted={}", identity, is_muted);
                let _ = ch.send(VoiceEvent::ParticipantJoined {
                    identity,
                    name,
                    is_muted,
                    avatar_url,
                });
            }
        }
    }

    let voice_arc = Arc::clone(&state.voice);
    let state_for_room = Arc::clone(state);
    let apm_rate_for_room = mic_rate;
    let room_task = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                RoomEvent::ParticipantConnected(p) => {
                    eprintln!("[voice] participant joined: {}", p.identity());
                    let identity = p.identity().to_string();
                    let is_muted = p.track_publications().values().any(|pub_| pub_.is_muted());
                    let avatar_url =
                        lookup_avatar_url_for_identity(&state_for_room, &identity).await;
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::ParticipantJoined {
                            identity,
                            name: p.name(),
                            is_muted,
                            avatar_url,
                        });
                    }
                }
                RoomEvent::ParticipantDisconnected(p) => {
                    eprintln!("[voice] participant left: {}", p.identity());
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::ParticipantLeft {
                            identity: p.identity().to_string(),
                        });
                    }
                }
                RoomEvent::TrackSubscribed { track, publication: _, participant } => {
                    if let RemoteTrack::Audio(audio_track) = track {
                        let track_key = format!("{}-{}", participant.identity(), audio_track.sid());
                        eprintln!("[voice] track subscribed: {track_key}");
                        register_remote_track(
                            audio_track.rtc_track(),
                            track_key,
                            Arc::clone(&voice_arc),
                            participant.identity().to_string(),
                            apm_rate_for_room,
                        )
                        .await;
                    }
                }
                RoomEvent::TrackUnsubscribed { track, publication: _, participant } => {
                    if let RemoteTrack::Audio(audio_track) = track {
                        let track_key = format!("{}-{}", participant.identity(), audio_track.sid());
                        eprintln!("[voice] track unsubscribed: {track_key}");
                        let voice = voice_arc.lock().await;
                        let mut pb = voice.playback.lock().unwrap();
                        if let Some(t) = pb.drain_tasks.remove(&track_key) { t.abort(); }
                        pb.rtc_tracks.remove(&track_key);
                        pb.identities.remove(&track_key);
                        let buffers_arc = Arc::clone(&pb.track_buffers);
                        drop(pb);
                        drop(voice);
                        buffers_arc.lock().unwrap().remove(&track_key);
                    }
                }

                RoomEvent::TrackMuted { participant, publication: _ } => {
                    let identity = participant.identity().to_string();
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::Muted { identity });
                    }
                }
                RoomEvent::TrackUnmuted { participant, publication: _ } => {
                    let identity = participant.identity().to_string();
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::Unmuted { identity });
                    }
                }

                RoomEvent::Disconnected { reason } => {
                    eprintln!("[voice] disconnected: {reason:?}");
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::Disconnected);
                    }
                    break;
                }
                RoomEvent::ConnectionStateChanged(conn_state) => {
                    eprintln!("[voice] connection state: {conn_state:?}");
                }
                RoomEvent::ConnectionQualityChanged { quality, participant } => {
                    let quality_str = match quality {
                        ConnectionQuality::Excellent => "excellent",
                        ConnectionQuality::Good => "good",
                        ConnectionQuality::Poor => "poor",
                        ConnectionQuality::Lost => "lost",
                    };
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::ConnectionQualityChanged {
                            identity: participant.identity().to_string(),
                            quality: quality_str.to_string(),
                        });
                    }
                }
                _ => {}
            }
        }
    });

    // ── Phase: total_join + record timings ─────────────────────────────────
    let total_join_ms = join_start.elapsed().as_millis() as u64;

    let timings = JoinTimings {
        channel_id: channel_id.clone(),
        jwt_mint_ms,
        room_connect_ms,
        mic_init_ms,
        first_publish_ms,
        total_join_ms,
        join_started_at_ms,
    };
    eprintln!(
        "[voice/timings] channel={} jwt={}ms connect={}ms mic={}ms publish={}ms total={}ms",
        channel_id,
        jwt_mint_ms,
        room_connect_ms,
        mic_init_ms,
        first_publish_ms,
        total_join_ms,
    );

    // ── Store state ───────────────────────────────────────────────────────────
    let mut voice = state.voice.lock().await;
    if let Some(t) = voice.room_task.take() { t.abort(); }
    if let Some(t) = voice.frame_task.take() { t.abort(); }
    voice.room = Some(room);
    voice.local_track = Some(local_track);
    voice.audio_source = Some(audio_source);
    voice.input_stream = Some(mic_stream);
    voice.frame_task = Some(frame_task);
    voice.room_task = Some(room_task);
    voice.current_input_device = input_device;
    voice.apm = apm_stage;
    voice.e2ee_key_provider = Some(key_provider_for_state);
    voice.e2ee_mls_group_id = Some(voice_mls_group_id);
    voice.e2ee_epoch = voice_epoch;
    *voice.last_join_timings.lock().unwrap() = Some(timings);

    Ok(())
}

/// Return the most recent `join_voice_channel` timing record. The frontend
/// calls this immediately after a successful join and dumps the values into
/// the dev console for analysis. Returns `None` if no join has completed
/// since process start.
pub async fn get_last_join_timings(
    state: &Arc<AppState>,
) -> Result<Option<JoinTimings>> {
    let voice = state.voice.lock().await;
    let snapshot = voice.last_join_timings.lock().unwrap().clone();
    Ok(snapshot)
}

/// Disconnect from the current voice room and release all audio resources.
pub async fn leave_voice_channel(state: &Arc<AppState>) -> Result<()> {
    // Extract everything that needs cleanup while holding the lock, then release
    // the lock before awaiting room.close(). If the network is broken (e.g. VPN
    // dropped), room.close() hangs sending a disconnect signal — holding the lock
    // during that await deadlocks every subsequent command that needs voice state.
    let (room, input_stream, output_stream) = {
        let mut voice = state.voice.lock().await;

        // Kill the frame feed first so no more frames are pushed into the
        // audio source / room while we tear them down.
        if let Some(t) = voice.frame_task.take() { t.abort(); }
        if let Some(t) = voice.room_task.take() { t.abort(); }

        // Take the cpal output stream out of playback state so we can drop
        // it on a blocking thread (CoreAudio dispose isn't safe to run on a
        // tokio worker — on macOS the mic-in-use indicator can otherwise
        // stay on until the process exits).
        let output_stream = {
            let mut pb = voice.playback.lock().unwrap();
            let s = pb.output_stream.take();
            pb.stop_all();
            pb.rtc_tracks.clear();
            pb.identities.clear();
            pb.output_device_name = None;
            s
        };

        voice.local_track = None;
        voice.audio_source = None;
        let input_stream = voice.input_stream.take();
        voice.apm = None;
        if let Ok(mut slot) = voice.denoiser.lock() {
            *slot = None;
        }
        voice.is_muted.store(false, Ordering::Relaxed);
        voice.current_input_device = None;
        voice.e2ee_key_provider = None;
        voice.e2ee_mls_group_id = None;
        voice.e2ee_epoch = 0;

        (voice.room.take(), input_stream, output_stream)
    }; // voice lock released here

    // Drop cpal streams on a blocking thread. cpal's macOS Drop calls
    // AudioOutputUnitStop + AudioUnitUninitialize + AudioComponentInstanceDispose;
    // when run from a tokio worker those calls can leave the OS "microphone
    // in use" indicator on until process exit. Running drop on the blocking
    // pool lets CoreAudio fully tear down the AudioUnit synchronously.
    if input_stream.is_some() || output_stream.is_some() {
        let _ = tokio::task::spawn_blocking(move || {
            drop(input_stream);
            drop(output_stream);
        })
        .await;
    }

    // Close outside the lock with a timeout so a broken connection (dropped VPN,
    // network change) can't stall a reconnect attempt indefinitely.
    if let Some(room) = room {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            room.close(),
        ).await;
    }

    Ok(())
}

/// Toggle the local microphone mute. Returns the new muted state (true = muted).
/// Also signals the muted state to remote participants via the LiveKit publication.
pub async fn toggle_voice_mute(state: &Arc<AppState>) -> Result<bool> {
    let voice = state.voice.lock().await;
    let new_muted = !voice.is_muted.load(Ordering::Relaxed);
    voice.is_muted.store(new_muted, Ordering::Relaxed);

    // Hint APM that the output is muted so its AGC/AEC stop adapting to
    // silence frames during the mute window.
    if let Some(apm) = &voice.apm {
        apm.handle().set_output_will_be_muted(new_muted);
    }

    // Signal to remote participants via the LiveKit publication
    if let Some(room) = &voice.room {
        let pubs = room.local_participant().track_publications();
        for (_, pub_) in pubs {
            if pub_.kind() == TrackKind::Audio {
                if new_muted {
                    pub_.mute();
                } else {
                    pub_.unmute();
                }
            }
        }
    }

    Ok(new_muted)
}

/// Set the per-user output gain multiplier for a remote participant.
///
/// `user_id` is the bare user id (no `voice-` prefix, no `:device_id`
/// suffix — see `user_id_from_voice_identity`). `volume` is clamped to
/// 0.0..=2.0; 1.0 is unity, values <1 attenuate, values >1 boost.
///
/// Persistence is handled by the frontend via the existing
/// `save_preferences` command — this only updates the live mixer state.
/// Setting volume == 1.0 removes the entry so unity-gain tracks take the
/// fast path in the mixer.
pub async fn set_remote_user_volume(
    user_id: String,
    volume: f32,
    state: &Arc<AppState>,
) -> Result<()> {
    let clamped = if volume.is_finite() {
        volume.clamp(0.0, 2.0)
    } else {
        1.0
    };

    let voice = state.voice.lock().await;
    let pb = voice.playback.lock().unwrap();
    let mut volumes = pb.user_volumes.lock().unwrap();
    if (clamped - 1.0).abs() < f32::EPSILON {
        volumes.remove(&user_id);
    } else {
        volumes.insert(user_id, clamped);
    }
    Ok(())
}

/// Switch the microphone device mid-call. Stops the current input stream and
/// restarts it on the new device. Rebuilds APM if the new device's sample
/// rate differs from the current one — APM is rate-locked at construction.
pub async fn set_voice_input_device(
    device_name: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut voice = state.voice.lock().await;

    // Only valid while in a call
    if voice.audio_source.is_none() {
        voice.current_input_device = Some(device_name);
        return Ok(());
    }

    // Extract shared atomics + current APM rate / config before dropping the
    // lock — cpal init makes blocking ALSA syscalls and must not hold the
    // async mutex.
    let is_muted_clone = Arc::clone(&voice.is_muted);
    let prev_apm_rate = voice.apm.as_ref().map(|a| a.sample_rate_hz());
    let prev_apm_config = voice.apm.as_ref().map(|a| a.config().clone());
    drop(voice);

    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<i16>, u32)>();
    let device_name_clone = device_name.clone();
    let (new_mic, new_rate) = tokio::task::spawn_blocking(move || {
        let host = cpal::default_host();
        let device = get_device(&host, Some(&device_name_clone), true)?;
        start_mic_stream(&device, frame_tx, is_muted_clone)
    })
    .await
    .map_err(|e| anyhow::anyhow!("audio init panicked: {e}"))??;

    // Rebuild APM if the new mic rate differs from the previous one.
    let rate_changed = prev_apm_rate.map(|r| r != new_rate).unwrap_or(true);
    let new_apm_stage = if rate_changed {
        match (
            prev_apm_config.clone(),
            voice_apm::ApmStage::new(
                new_rate,
                prev_apm_config.unwrap_or_default(),
            ),
        ) {
            (_, Ok(s)) => Some(s),
            (_, Err(e)) => {
                eprintln!("[voice] APM rebuild on mic switch failed: {e}");
                None
            }
        }
    } else {
        None
    };

    let mut voice = state.voice.lock().await;

    // Swap the input stream — dropping the old one stops it
    voice.input_stream = Some(new_mic);
    voice.current_input_device = Some(device_name);
    if rate_changed {
        voice.apm = new_apm_stage;
    }

    // Reset the RNNoise state on every device switch — RNN hidden state is
    // tied to the previous mic's spectrum and isn't useful afterwards.
    // Re-engage it iff the new mic is at the right rate and the user still
    // has click suppression enabled.
    let want_denoiser = voice
        .apm
        .as_ref()
        .map(|a| a.config().click_suppression)
        .unwrap_or(false);
    {
        let mut slot = voice.denoiser.lock().unwrap();
        *slot = if want_denoiser && new_rate == voice_denoiser::REQUIRED_RATE_HZ {
            Some(voice_denoiser::DenoiserStage::new())
        } else {
            None
        };
    }

    // Abort the old frame-feed task and start a new one on the new channel.
    if let Some(t) = voice.frame_task.take() { t.abort(); }
    let source = voice.audio_source.clone().unwrap();
    let apm_for_capture = voice.apm.as_ref().map(|a| a.handle());
    let denoiser_for_capture = Arc::clone(&voice.denoiser);
    let task = tokio::spawn(async move {
        let mut buf: Vec<i16> = Vec::new();
        while let Some((samples, rate)) = frame_rx.recv().await {
            buf.extend_from_slice(&samples);
            let chunk_size = (rate / 100) as usize;
            while buf.len() >= chunk_size {
                let mut chunk: Vec<i16> = buf.drain(..chunk_size).collect();
                {
                    let mut guard = denoiser_for_capture.lock().unwrap();
                    if let Some(d) = guard.as_mut() {
                        d.process(&mut chunk);
                    }
                }
                if let Some(apm) = &apm_for_capture {
                    if let Err(e) = voice_apm::run_capture(apm, &mut chunk, chunk_size) {
                        eprintln!("[voice] APM capture error (frame dropped): {e}");
                        continue;
                    }
                }
                let frame = AudioFrame {
                    data: chunk.into(),
                    sample_rate: rate,
                    num_channels: 1,
                    samples_per_channel: chunk_size as u32,
                };
                let _ = source.capture_frame(&frame).await;
            }
        }
    });
    voice.frame_task = Some(task);

    Ok(())
}

/// Switch the speaker device mid-call. Tears down the current cpal output
/// stream + mixer task and rebuilds them on the new device. Per-track drain
/// tasks keep running — `track_buffers` is preserved across the switch, so
/// the new mixer picks up where the old one left off without re-subscribing.
pub async fn set_voice_output_device(
    device_name: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let voice_arc = Arc::clone(&state.voice);

    let (apm_handle, apm_rate, apm_frame_samples) = {
        let voice = voice_arc.lock().await;
        let (handle, rate, samples) = match &voice.apm {
            Some(stage) => (
                Some(stage.handle()),
                stage.sample_rate_hz(),
                stage.frame_samples(),
            ),
            None => (None, voice_apm::DEFAULT_APM_RATE_HZ, voice_apm::frame_samples(voice_apm::DEFAULT_APM_RATE_HZ)),
        };
        (handle, rate, samples)
    };

    ensure_playback(
        voice_arc,
        Some(device_name),
        apm_handle,
        apm_rate,
        apm_frame_samples,
    )
    .await
}

/// Update the live APM configuration without rejoining. Internal AEC / AGC /
/// NS state is preserved across config changes — only the changed submodule
/// re-initialises. The RNNoise denoiser is created or dropped to match the
/// new `click_suppression` flag (its RNN state isn't worth preserving across
/// toggles). No-op when no voice session is active.
pub async fn set_voice_audio_processing(
    config: voice_apm::ApmConfig,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut voice = state.voice.lock().await;

    // Mic rate is fixed at session start; reuse it for the denoiser-rate check.
    let mic_rate = voice.apm.as_ref().map(|a| a.sample_rate_hz());

    if let Some(stage) = voice.apm.as_mut() {
        stage.set_config(config.clone());
    }

    // Reconcile the RNNoise slot with the new flag.
    if let Some(rate) = mic_rate {
        let mut slot = voice.denoiser.lock().unwrap();
        let want = config.click_suppression && rate == voice_denoiser::REQUIRED_RATE_HZ;
        match (want, slot.is_some()) {
            (true, false) => {
                eprintln!("[voice/rnnoise] enabled mid-call");
                *slot = Some(voice_denoiser::DenoiserStage::new());
            }
            (false, true) => {
                eprintln!("[voice/rnnoise] disabled mid-call");
                *slot = None;
            }
            _ => { /* already in the desired state */ }
        }
    }

    Ok(())
}
