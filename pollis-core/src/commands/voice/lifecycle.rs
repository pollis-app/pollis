use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

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
        livekit::{
            is_view_identity, lookup_avatar_url_for_identity, lookup_avatar_urls_for_identities,
            pin_local_identity, resolve_participant, resolve_participants,
        },
        voice_apm,
        voice_denoiser,
        voice_e2ee,
    },
    error::Result,
    state::AppState,
};

use super::devices::get_device;
use super::gate::{TransmitGate, VoiceGateState, VoiceInputMode};
use super::levels::BandAnalyzer;
use super::playback::{ensure_playback, register_remote_track};
use super::streams::start_mic_stream;
use super::types::{
    user_id_from_voice_identity, JoinTimings, VoiceEvent, VoiceState, VoiceWarmup,
    VOICE_WARMUP_TTL,
};

// `TransmitGate` (#849) is the authority; these two helpers are the only way
// its decisions reach the process. Every gate mutation calls both, in this
// order, under the voice lock.

/// Republish the gate's derived booleans onto the atomics the capture callback
/// and the mixer read; neither can afford a lock per frame.
fn publish_gate(voice: &VoiceState, gate: &TransmitGate) {
    voice
        .transmit_muted
        .store(!gate.transmitting(), Ordering::Relaxed);
    voice.deafened.store(gate.deafened(), Ordering::Relaxed);
    voice.self_muted.store(gate.self_muted(), Ordering::Relaxed);
}

/// Signal the local mic state to the room. A released push-to-talk key is a
/// real mute to everyone else; "PTT idle" and "deafened" are local
/// distinctions and would tell the room more than it needs. Never appear
/// unmuted while the gate is closed.
fn apply_gate_to_room(voice: &VoiceState, gate: &TransmitGate) {
    let muted = !gate.transmitting();

    // Tell APM the output is muted, so AGC/AEC stop adapting to silence.
    if let Some(apm) = &voice.apm {
        apm.handle().set_output_will_be_muted(muted);
    }

    if let Some(room) = &voice.room {
        let pubs = room.local_participant().track_publications();
        for (_, pub_) in pubs {
            if pub_.kind() == TrackKind::Audio {
                if muted {
                    pub_.mute();
                } else {
                    pub_.unmute();
                }
            }
        }
    }
}

// The local participant's internal identity. Since #836 LiveKit sees an opaque
// per-room pseudonym; remote peers are translated by
// `livekit::identity::resolve_participants`.
use crate::commands::livekit_identity::voice_identity;

/// Register the channel used to push VoiceEvents to the frontend, once on
/// startup.
pub async fn subscribe_voice_events(
    on_event: std::sync::Arc<dyn crate::sink::EventSink<VoiceEvent>>,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut voice = state.voice.lock().await;
    voice.channel = Some(on_event);
    Ok(())
}

/// Pre-pay join latency on user intent (route entry, hover or keyboard focus
/// on a voice item). The Rust `livekit` crate has no `prepareConnection`, so
/// this mints and caches the token and fires a one-shot HTTPS probe to warm
/// DNS, the rustls session cache and the connection pool.
///
/// Idempotent for the same channel; a different channel supersedes the
/// pending warmup. Cheap enough to call eagerly.
pub async fn prepare_voice_connection(
    channel_id: String,
    user_id: String,
    display_name: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let url = state.config.livekit_url.clone();
    if url.is_empty() {
        // No LiveKit configured: silent success, so callers needn't check.
        return Ok(());
    }

    // Skip a fresh warmup for the same channel and identity.
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

    // DS-minted (#393); identity derived server-side. Best effort.
    let (token, ds_url) = match crate::commands::mls::ds_livekit_token(state, &channel_id, "voice").await {
        Ok(pair) => pair,
        Err(e) => {
            crate::diag!("[voice] warmup token error (non-fatal): {e}");
            return Ok(());
        }
    };

    // Background probe; racing an immediate Join costs at most one redundant
    // TLS handshake.
    let warm_url = ds_url.clone();
    let task = tokio::spawn(async move {
        let twirp = if let Some(rest) = warm_url.strip_prefix("wss://") {
            format!("https://{rest}")
        } else if let Some(rest) = warm_url.strip_prefix("ws://") {
            format!("http://{rest}")
        } else {
            warm_url.clone()
        };
        let probe = format!("{twirp}/rtc/validate");
        // The shared direct client (LiveKit is not overlaid, §14.4): a fresh
        // client's pool and TLS cache would die with it, wasting the warmup.
        let client = crate::net::overlay::http_client(None);
        let started = Instant::now();
        match client.get(&probe).timeout(Duration::from_secs(5)).send().await {
            Ok(_resp) => {
                crate::diag!(
                    "[voice] warmup probe to {twirp} completed in {:.0}ms",
                    started.elapsed().as_secs_f64() * 1000.0
                );
            }
            Err(e) => {
                crate::diag!("[voice] warmup probe failed (non-fatal): {e}");
            }
        }
    });

    // Replacing a prior entry drops it, aborting its background probe.
    let mut voice = state.voice.lock().await;
    voice.warmup = Some(VoiceWarmup {
        channel_id,
        token,
        url: ds_url,
        created_at: Instant::now(),
        user_id,
        display_name,
        task: Some(task),
    });

    Ok(())
}

/// Connect to a LiveKit voice room, publish the local microphone, and start
/// the remote-playback pipeline.
///
/// libwebrtc's own audio processing is disabled so APM (`audio_processing`) is
/// the only stage on the mic signal. The room connect and the mic init run
/// concurrently, so join latency is ~max(net, mic); a warmup from
/// `prepare_voice_connection` supplies the token.
// Mirrors the `#[tauri::command]` shim's signature, which is the IPC contract the
// renderer calls by argument name — it cannot be regrouped on this side alone.
#[allow(clippy::too_many_arguments)]
pub async fn join_voice_channel(
    channel_id: String,
    user_id: String,
    display_name: String,
    input_device: Option<String>,
    output_device: Option<String>,
    audio_processing: voice_apm::ApmConfig,
    // The other party of a 1:1 call (`call-<ulid>` room), which has no DB row;
    // ignored for group channels and DMs.
    counterparty_user_id: Option<String>,
    state: &Arc<AppState>,
) -> Result<()> {
    let join_start = Instant::now();
    let join_started_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);

    // Refuse re-entry: two concurrent joins with one identity trip LiveKit's
    // DuplicateIdentity, disconnecting the survivor.
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

    // Per-device identity (#140): `voice-{user_id}:{device_id}`, so a user's
    // second device joins as a distinct participant.
    let device_id = state.device_id.lock().await.clone();
    let local_identity = voice_identity(&user_id, device_id.as_deref());

    // Consume a fresh warmup for this channel and identity. VoiceWarmup's Drop
    // aborts its task, so clone the pair out first.
    let cached_token: Option<(String, String)> = {
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
            let cloned = voice.warmup.as_ref().map(|w| (w.token.clone(), w.url.clone()));
            voice.warmup = None;
            cloned
        } else {
            voice.warmup = None;
            None
        }
    };
    // 0 when a warmup token was usable.
    let jwt_mint_ms;
    let (token, url): (String, String) = match cached_token {
        Some(pair) => {
            jwt_mint_ms = 0;
            pair
        }
        None => {
            // DS-minted for `voice-{user}:{device}` from this device's verified
            // signature: a network round trip, so phase-timed.
            let jwt_start = Instant::now();
            let pair = crate::commands::mls::ds_livekit_token(state, &channel_id, "voice").await?;
            jwt_mint_ms = jwt_start.elapsed().as_millis() as u64;
            pair
        }
    };
    if url.is_empty() {
        return Err(anyhow::anyhow!("LiveKit is not configured on this server").into());
    }

    // #836: pin our pseudonym to `local_identity` before connecting, so events
    // about our own participant (the `TrackMuted` on each push-to-talk press)
    // land on the local tile. The pseudonym travels only in the token.
    pin_local_identity(state, &channel_id, &token, &local_identity, &display_name).await;

    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<i16>, u32)>();
    // Reset the per-session gate bits; the input mode is a preference and
    // survives.
    let is_muted = {
        let voice = state.voice.lock().await;
        {
            let mut gate = voice.gate.lock().unwrap();
            gate.reset_for_join();
            publish_gate(&voice, &gate);
        }
        Arc::clone(&voice.transmit_muted)
    };

    let input_device_clone = input_device.clone();

    // The voice key comes from the channel's MLS exporter, so the SFU never
    // hears plaintext. Fails closed: no local MLS group means no join. It must
    // be in `RoomOptions` before connecting, so this phase is pure added
    // latency.
    let e2ee_start = Instant::now();
    let (voice_key, voice_key_index, voice_epoch, voice_mls_group_id) = voice_e2ee::derive_voice_key(
        state,
        &channel_id,
        &user_id,
        counterparty_user_id.as_deref(),
    )
    .await?;
    let e2ee_ms = e2ee_start.elapsed().as_millis() as u64;
    let e2ee_options = voice_e2ee::build_e2ee_options(voice_key, voice_key_index);
    let key_provider_for_state = e2ee_options.key_provider.clone();
    crate::diag!(
        "[voice] e2ee armed for {channel_id} (mls_group={voice_mls_group_id}, epoch={voice_epoch}, ring slot={voice_key_index})"
    );

    let connect_started = Instant::now();
    let mic_started = Instant::now();
    crate::diag!("[voice] connecting to room {channel_id} and opening mic in parallel…");

    // `RoomOptions` is #[non_exhaustive].
    let mut room_options = RoomOptions::default();
    room_options.encryption = Some(e2ee_options);
    let connect_fut = async {
        let r = Room::connect(&url, &token, room_options).await;
        let elapsed_ms = connect_started.elapsed().as_millis() as u64;
        (r, elapsed_ms)
    };
    let mic_fut = async {
        tokio::task::spawn_blocking(move || {
            // Test seam: the e2e suite proves a join succeeds without capture.
            if std::env::var("POLLIS_DISABLE_MIC").is_ok() {
                return Err(anyhow::anyhow!("mic disabled via POLLIS_DISABLE_MIC").into());
            }
            let host = cpal::default_host();
            let dev = get_device(&host, input_device_clone.as_deref(), true)?;
            crate::diag!("[voice] mic device opened");
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

    // The mic is best-effort: without one we join listen-only (receiving, not
    // publishing).
    let mic = match mic_res {
        Ok(Ok(v)) => Some(v),
        Ok(Err(e)) => {
            crate::diag!("[voice] mic unavailable — joining listen-only: {e}");
            None
        }
        Err(e) => {
            crate::diag!("[voice] mic init panicked — joining listen-only: {e}");
            None
        }
    };
    let has_mic = mic.is_some();
    // Listen-only still needs a nominal rate downstream.
    let mic_rate = mic.as_ref().map(|(_, r, _)| *r).unwrap_or(48_000);
    let mic_init_ms = mic.as_ref().map(|(_, _, ms)| *ms).unwrap_or(0);
    let mic_stream = mic.map(|(s, _, _)| s);
    if has_mic {
        crate::diag!("[voice] connected to room {channel_id}, mic at {mic_rate} Hz");
    } else {
        crate::diag!("[voice] connected to room {channel_id}, listen-only (no mic)");
    }

    let room = Arc::new(room);

    // APM runs at the mic's rate (8/16/32/48 kHz only; otherwise without APM),
    // and not at all listen-only.
    let apm_stage = if has_mic {
        match voice_apm::ApmStage::new(mic_rate, audio_processing.clone()) {
            Ok(stage) => Some(stage),
            Err(e) => {
                crate::diag!("[voice] APM disabled: {e}");
                None
            }
        }
    } else {
        None
    };
    let apm_handle = apm_stage.as_ref().map(|s| s.handle());
    let apm_frame_samples = voice_apm::frame_samples(mic_rate);

    // RNNoise for click suppression; rate-locked at 48 kHz.
    let denoiser_arc = Arc::clone(&{
        let voice = state.voice.lock().await;
        Arc::clone(&voice.denoiser)
    });
    {
        let mut slot = denoiser_arc.lock().unwrap();
        *slot = if has_mic && audio_processing.click_suppression && mic_rate == voice_denoiser::REQUIRED_RATE_HZ {
            crate::diag!("[voice/rnnoise] engaged @ {mic_rate} Hz");
            Some(voice_denoiser::DenoiserStage::new())
        } else {
            if has_mic && audio_processing.click_suppression {
                crate::diag!(
                    "[voice/rnnoise] requested but mic rate is {mic_rate} Hz; \
                     RNNoise needs 48000 Hz — disabling for this session"
                );
            }
            None
        };
    }

    // Listen-only skips the mic track and capture loop.
    let mut audio_source_opt: Option<NativeAudioSource> = None;
    let mut local_track_opt: Option<LocalAudioTrack> = None;
    let mut frame_task_opt: Option<tokio::task::JoinHandle<()>> = None;
    let mut first_publish_ms: u64 = 0;

    if has_mic {
        // libwebrtc's processing off: doubling it with APM causes pumping.
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

        crate::diag!("[voice] publishing track…");
        let publish_start = Instant::now();
        room.local_participant()
            .publish_track(
                LocalTrack::Audio(local_track.clone()),
                TrackPublishOptions::default(),
            )
            .await
            .map_err(|e| anyhow::anyhow!("publish track: {e}"))?;
        first_publish_ms = publish_start.elapsed().as_millis() as u64;
        // The mic's FrameCryptor starts on ring slot 0; move it to the join
        // epoch's slot before the first frame. The room loop does this for
        // later publishes, but `voice.room` is not stored yet.
        let moved = voice_e2ee::apply_sender_key_index(&room, voice_key_index);
        crate::diag!("[voice] track published ({moved} sender cryptor(s) on ring slot {voice_key_index})");

        // Rebuffer to exact 10 ms, run RNNoise and APM, then capture_frame.
        // Speaking detection uses the post-APM peak.
        let audio_source_task = audio_source.clone();
        let voice_arc_frame = Arc::clone(&state.voice);
        let local_identity_for_speaking = local_identity.clone();
        let apm_for_capture = apm_handle.clone();
        let denoiser_for_capture = Arc::clone(&denoiser_arc);
        let frame_task = tokio::spawn(async move {
            let chunk_size = (mic_rate / 100) as usize;
            let mut buf: Vec<i16> = Vec::new();
            let mut speak_hold: u32 = 0; // counts down after speech stops (12 × 10ms = 120ms hold)
            let mut onset_frames: u32 = 0; // consecutive above-threshold frames; 2 required to (re)trigger
            let mut is_speaking = false;

            // Sink cloned once; bands emitted every 5 chunks (~20 Hz).
            let bands_sink = voice_arc_frame.lock().await.channel.clone();
            let mut analyzer = BandAnalyzer::new(mic_rate);
            let mut bands_ctr: u32 = 0;

            while let Some((samples, rate)) = frame_rx.recv().await {
                buf.extend_from_slice(&samples);
                while buf.len() >= chunk_size {
                    let mut chunk: Vec<i16> = buf.drain(..chunk_size).collect();

                    // RNNoise first, so APM's NS/AGC adapt to voice rather than
                    // clicks. The std Mutex never crosses an await.
                    {
                        let mut guard = denoiser_for_capture.lock().unwrap();
                        if let Some(d) = guard.as_mut() {
                            d.process(&mut chunk);
                        }
                    }

                    if let Some(apm) = &apm_for_capture {
                        if let Err(e) = voice_apm::run_capture(apm, &mut chunk, chunk_size) {
                            crate::diag!("[voice] APM capture error (frame dropped): {e}");
                            continue;
                        }
                    }

                    let peak = chunk.iter().map(|&s| s.abs()).max().unwrap_or(0);

                    // Two consecutive loud frames to trigger, so a lone spike
                    // cannot reset the hold.
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
                        let voice = voice_arc_frame.lock().await;
                        if let Some(ch) = &voice.channel {
                            if is_speaking {
                                let _ = ch.send(VoiceEvent::SpeakingStarted { identity: local_identity_for_speaking.clone() });
                            } else {
                                let _ = ch.send(VoiceEvent::SpeakingStopped { identity: local_identity_for_speaking.clone() });
                            }
                        }
                    }

                    // Post-APM, so the meter matches what others hear.
                    analyzer.process(&chunk);
                    bands_ctr += 1;
                    if bands_ctr >= 5 {
                        bands_ctr = 0;
                        if let Some(ch) = &bands_sink {
                            let _ = ch.send(VoiceEvent::AudioBands {
                                identity: local_identity_for_speaking.clone(),
                                bands: analyzer.levels(),
                            });
                        }
                    }

                    let frame = AudioFrame {
                        data: chunk.into(),
                        sample_rate: rate,
                        num_channels: 1,
                        samples_per_channel: chunk_size as u32,
                    };
                    if let Err(e) = audio_source_task.capture_frame(&frame).await {
                        crate::diag!("[voice] capture_frame error: {e:?}");
                    }
                }
            }
        });

        audio_source_opt = Some(audio_source);
        local_track_opt = Some(local_track);
        frame_task_opt = Some(frame_task);
    }

    // Open playback before the room loop, so the first TrackSubscribed has
    // somewhere to push frames.
    if let Err(e) = ensure_playback(
        Arc::clone(&state.voice),
        output_device.clone(),
        apm_handle.clone(),
        mic_rate,
        apm_frame_samples,
    )
    .await
    {
        crate::diag!("[voice] playback init failed (speaker disabled this session): {e}");
    }

    // Seed the participant list. Tracks are not attached here: TrackSubscribed
    // fires for pre-existing tracks once the loop runs, and attaching twice
    // would start competing drain tasks.
    let roster_start = Instant::now();
    // #836: resolve the whole seed roster's pseudonyms in one batched call.
    let seen: Vec<String> = room
        .remote_participants()
        .into_values()
        .map(|p| p.identity().to_string())
        .collect();
    let resolved = resolve_participants(state, &channel_id, &seen).await;
    let existing_remote: Vec<(String, String, bool)> = room
        .remote_participants()
        .into_values()
        .filter_map(|p| {
            let wire = p.identity().to_string();
            let (identity, name) = resolved
                .get(&wire)
                .cloned()
                .unwrap_or_else(|| (wire.clone(), String::new()));
            // Skip renderer-side `view` clients (same user as their voice
            // participant); their tracks still route. Checks both the resolved
            // and wire identity, so a resolve failure adds no phantom.
            if is_view_identity(&identity) || is_view_identity(&wire) {
                return None;
            }
            // TrackMuted fires only on transitions, so seed mute state.
            let is_muted = p.track_publications().values().any(|pub_| pub_.is_muted());
            Some((identity, name, is_muted))
        })
        .collect();
    // Avatars for ourselves and everyone present, in one round trip.
    let mut avatar_identities: Vec<String> = Vec::with_capacity(existing_remote.len() + 1);
    avatar_identities.push(local_identity.clone());
    avatar_identities.extend(existing_remote.iter().map(|(identity, _, _)| identity.clone()));
    let avatars = lookup_avatar_urls_for_identities(state, &avatar_identities).await;
    let local_avatar_url = avatars.get(&local_identity).cloned();
    let existing_with_avatars: Vec<(String, String, bool, Option<String>)> = existing_remote
        .into_iter()
        .map(|(identity, name, is_muted)| {
            let avatar = avatars.get(&identity).cloned();
            (identity, name, is_muted, avatar)
        })
        .collect();
    let roster_ms = roster_start.elapsed().as_millis() as u64;
    {
        let voice = state.voice.lock().await;
        if let Some(ch) = &voice.channel {
            let _ = ch.send(VoiceEvent::ParticipantJoined {
                identity: local_identity.clone(),
                name: display_name.clone(),
                is_muted: false,
                avatar_url: local_avatar_url,
            });
            for (identity, name, is_muted, avatar_url) in existing_with_avatars {
                crate::diag!("[voice] existing participant: {} muted={}", identity, is_muted);
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
    // Pseudonyms are room-scoped (#836), so resolve against this room.
    let logical_room = channel_id.clone();
    let apm_rate_for_room = mic_rate;
    // Self-hear filter: never play our own other devices' audio (#140).
    let local_user_id = user_id.clone();
    let room_task = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                RoomEvent::ParticipantConnected(p) => {
                    let wire = p.identity().to_string();
                    if is_view_identity(&wire) {
                        continue;
                    }
                    // #836: translate first; the display name comes back with it
                    // (the LiveKit `name` claim is empty).
                    let (identity, name) =
                        resolve_participant(&state_for_room, &logical_room, &wire).await;
                    if is_view_identity(&identity) {
                        continue;
                    }
                    crate::diag!("[voice] participant joined: {identity}");
                    let is_muted = p.track_publications().values().any(|pub_| pub_.is_muted());
                    let avatar_url =
                        lookup_avatar_url_for_identity(&state_for_room, &identity).await;
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::ParticipantJoined {
                            identity,
                            name,
                            is_muted,
                            avatar_url,
                        });
                    }
                }
                RoomEvent::ParticipantDisconnected(p) => {
                    let wire = p.identity().to_string();
                    if is_view_identity(&wire) {
                        continue;
                    }
                    let (identity, _) =
                        resolve_participant(&state_for_room, &logical_room, &wire).await;
                    if is_view_identity(&identity) {
                        continue;
                    }
                    crate::diag!("[voice] participant left: {identity}");
                    // Clear their screenshare so no black tile lingers.
                    crate::commands::screenshare::on_participant_left(
                        &identity,
                        &state_for_room,
                    )
                    .await;
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::ParticipantLeft { identity });
                    }
                }
                RoomEvent::TrackSubscribed { track, publication, participant } => {
                    let (participant_identity, _) = resolve_participant(
                        &state_for_room,
                        &logical_room,
                        &participant.identity().to_string(),
                    )
                    .await;
                    match track {
                        RemoteTrack::Audio(audio_track) => {
                            // Self-hear mute (#140): the participant shows, but
                            // our other devices' mic is never played back here.
                            if user_id_from_voice_identity(&participant_identity) == local_user_id {
                                crate::diag!(
                                    "[voice] skipping own-device audio track for self-hear mute: {participant_identity}"
                                );
                                continue;
                            }
                            let track_key = format!("{}-{}", participant_identity, audio_track.sid());
                            // Only the publication's TrackSource tells a shared
                            // soundtrack from a voice.
                            let is_shared_audio = matches!(
                                publication.source(),
                                livekit::track::TrackSource::ScreenshareAudio
                            );
                            crate::diag!(
                                "[voice] track subscribed: {track_key} (shared_audio={is_shared_audio})"
                            );
                            register_remote_track(
                                audio_track.rtc_track(),
                                track_key,
                                Arc::clone(&voice_arc),
                                participant_identity,
                                apm_rate_for_room,
                                is_shared_audio,
                            )
                            .await;
                        }
                        RemoteTrack::Video(video_track) => {
                            let track_key = format!("{}-{}", participant_identity, video_track.sid());
                            // TrackSource tells screen share from webcam, so
                            // the renderer picks the right tile.
                            let source = match publication.source() {
                                livekit::track::TrackSource::Camera => {
                                    crate::commands::screenshare::RemoteVideoSource::Camera
                                }
                                _ => crate::commands::screenshare::RemoteVideoSource::Screen,
                            };
                            crate::diag!("[voice] video track subscribed: {track_key} (source={source:?})");
                            crate::commands::screenshare::on_remote_video_subscribed(
                                video_track,
                                participant_identity,
                                source,
                                &state_for_room,
                            )
                            .await;
                        }
                    }
                }
                RoomEvent::TrackUnsubscribed { track, publication: _, participant } => {
                    let (participant_identity, _) = resolve_participant(
                        &state_for_room,
                        &logical_room,
                        &participant.identity().to_string(),
                    )
                    .await;
                    match track {
                        RemoteTrack::Audio(audio_track) => {
                            let track_key = format!("{}-{}", participant_identity, audio_track.sid());
                            crate::diag!("[voice] track unsubscribed: {track_key}");
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
                        RemoteTrack::Video(video_track) => {
                            crate::commands::screenshare::on_remote_video_unsubscribed(
                                video_track,
                                participant_identity,
                                &state_for_room,
                            )
                            .await;
                        }
                    }
                }

                RoomEvent::TrackMuted { participant, publication: _ } => {
                    let (identity, _) = resolve_participant(
                        &state_for_room,
                        &logical_room,
                        &participant.identity().to_string(),
                    )
                    .await;
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::Muted { identity });
                    }
                }
                RoomEvent::TrackUnmuted { participant, publication: _ } => {
                    let (identity, _) = resolve_participant(
                        &state_for_room,
                        &logical_room,
                        &participant.identity().to_string(),
                    )
                    .await;
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::Unmuted { identity });
                    }
                }

                RoomEvent::Disconnected { reason } => {
                    crate::diag!("[voice] disconnected: {reason:?}");
                    {
                        let voice = voice_arc.lock().await;
                        if let Some(ch) = &voice.channel {
                            let _ = ch.send(VoiceEvent::Disconnected);
                        }
                    }
                    crate::commands::screenshare::on_room_disconnected(&state_for_room).await;
                    // Release our resources on a server-initiated disconnect.
                    // `false`: we are room_task and break next.
                    // release_voice_resources is idempotent if the frontend
                    // also leaves.
                    let _ = release_voice_resources(&state_for_room, false).await;
                    break;
                }
                RoomEvent::ConnectionStateChanged(conn_state) => {
                    crate::diag!("[voice] connection state: {conn_state:?}");
                }
                // #1097: log the frame-cryptor verdict. `MissingKey` /
                // `DecryptionFailed` on a remaining member's track is the
                // observable half of voice forward secrecy after a removal, so
                // e2e tests and log readers can tell "key rotated" from "mic
                // broke".
                RoomEvent::E2eeStateChanged { participant, state } => {
                    let (identity, _) = resolve_participant(
                        &state_for_room,
                        &logical_room,
                        &participant.identity().to_string(),
                    )
                    .await;
                    crate::diag!(
                        "[voice-e2ee] frame state for {identity}: {state:?}"
                    );
                }
                RoomEvent::LocalTrackPublished { .. } => {
                    // New local publications start on ring slot 0.
                    voice_e2ee::sync_local_sender_key_index(&state_for_room).await;
                }
                RoomEvent::ConnectionQualityChanged { quality, participant } => {
                    let quality_str = match quality {
                        ConnectionQuality::Excellent => "excellent",
                        ConnectionQuality::Good => "good",
                        ConnectionQuality::Poor => "poor",
                        ConnectionQuality::Lost => "lost",
                    };
                    let (identity, _) = resolve_participant(
                        &state_for_room,
                        &logical_room,
                        &participant.identity().to_string(),
                    )
                    .await;
                    let voice = voice_arc.lock().await;
                    if let Some(ch) = &voice.channel {
                        let _ = ch.send(VoiceEvent::ConnectionQualityChanged {
                            identity,
                            quality: quality_str.to_string(),
                        });
                    }
                }
                _ => {}
            }
        }
    });

    let total_join_ms = join_start.elapsed().as_millis() as u64;

    let timings = JoinTimings {
        channel_id: channel_id.clone(),
        jwt_mint_ms,
        e2ee_ms,
        room_connect_ms,
        mic_init_ms,
        first_publish_ms,
        roster_ms,
        total_join_ms,
        join_started_at_ms,
    };
    crate::diag!(
        "[voice/timings] channel={} jwt={}ms e2ee={}ms connect={}ms mic={}ms publish={}ms roster={}ms total={}ms",
        channel_id,
        jwt_mint_ms,
        e2ee_ms,
        room_connect_ms,
        mic_init_ms,
        first_publish_ms,
        roster_ms,
        total_join_ms,
    );

    let mut voice = state.voice.lock().await;
    if let Some(t) = voice.room_task.take() { t.abort(); }
    if let Some(t) = voice.frame_task.take() { t.abort(); }
    voice.room = Some(room);
    // All `None` on the listen-only path.
    voice.local_track = local_track_opt;
    voice.audio_source = audio_source_opt;
    voice.input_stream = mic_stream;
    voice.frame_task = frame_task_opt;
    voice.room_task = Some(room_task);
    voice.current_input_device = input_device;
    voice.apm = apm_stage;
    voice.e2ee_key_provider = Some(key_provider_for_state);
    voice.e2ee_mls_group_id = Some(voice_mls_group_id);
    voice.e2ee_epoch = voice_epoch;
    voice.e2ee_key_index = voice_key_index;
    *voice.last_join_timings.lock().unwrap() = Some(timings);

    // A publication is born unmuted, so sync it to the gate now; otherwise a
    // push-to-talk join would advertise a hot mic while capture is gated.
    {
        let gate = voice.gate.lock().unwrap();
        apply_gate_to_room(&voice, &gate);
    }

    // Lets the UI show "listening only" when there is no capture device.
    if let Some(ch) = &voice.channel {
        let _ = ch.send(VoiceEvent::MicAvailability {
            identity: local_identity.clone(),
            available: has_mic,
        });
    }

    Ok(())
}

/// The most recent `join_voice_channel` timings, dumped to the dev console.
/// `None` before the first join.
pub async fn get_last_join_timings(
    state: &Arc<AppState>,
) -> Result<Option<JoinTimings>> {
    let voice = state.voice.lock().await;
    let snapshot = voice.last_join_timings.lock().unwrap().clone();
    Ok(snapshot)
}

/// Tear down all live voice resources and return the room for the caller to
/// close. cpal streams are dropped on a blocking thread, and the lock is held
/// only briefly.
///
/// `abort_room_task` is `false` from the `Disconnected` handler, which runs
/// inside that task and breaks on its own. Idempotent.
async fn release_voice_resources(
    state: &Arc<AppState>,
    abort_room_task: bool,
) -> Option<Arc<Room>> {
    // Release the lock before any await: room.close() can hang on a broken
    // network, and holding it would deadlock every voice command.
    let (room, input_stream, output_stream) = {
        let mut voice = state.voice.lock().await;

        // Stop the frame feed first.
        if let Some(t) = voice.frame_task.take() { t.abort(); }
        if abort_room_task {
            if let Some(t) = voice.room_task.take() { t.abort(); }
        }

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
        // Reset the gate, or a leftover `deafened` would silence the next
        // call's mixer.
        {
            let mut gate = voice.gate.lock().unwrap();
            gate.reset_for_join();
            publish_gate(&voice, &gate);
        }
        voice.current_input_device = None;
        voice.e2ee_key_provider = None;
        voice.e2ee_mls_group_id = None;
        voice.e2ee_epoch = 0;
        voice.e2ee_key_index = 0;

        (voice.room.take(), input_stream, output_stream)
    }; // voice lock released here

    // cpal's macOS Drop (AudioUnit stop + dispose) run on a tokio worker can
    // leave the OS mic-in-use indicator on until exit; drop on the blocking
    // pool.
    if input_stream.is_some() || output_stream.is_some() {
        let _ = tokio::task::spawn_blocking(move || {
            drop(input_stream);
            drop(output_stream);
        })
        .await;
    }

    room
}

pub async fn leave_voice_channel(state: &Arc<AppState>) -> Result<()> {
    // Stop any screen share first: release aborts room_task, so the
    // Disconnected path never runs on an explicit leave. Idempotent.
    crate::commands::screenshare::stop_screen_share(state).await.ok();
    // The webcam publishes into this room too.
    crate::commands::camera::stop_camera(state).await.ok();

    let room = release_voice_resources(state, true).await;

    // Close with a timeout, so a broken connection cannot stall a reconnect.
    if let Some(room) = room {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            room.close(),
        ).await;
    }

    Ok(())
}

/// Toggle the local microphone mute, signalled to the room.
pub async fn toggle_voice_mute(state: &Arc<AppState>) -> Result<VoiceGateState> {
    mutate_gate(state, |g| g.toggle_mute()).await
}

/// Toggle self-deafen (#849).
/// Deafening silences incoming audio and implies self-mute; undeafening
/// restores the displaced mute state (`gate::TransmitGate`).
pub async fn toggle_voice_deafen(state: &Arc<AppState>) -> Result<VoiceGateState> {
    mutate_gate(state, |g| g.toggle_deafen()).await
}

/// Choose voice-activity or push-to-talk gating. Safe outside a call: the mode
/// is a preference the renderer pushes on load. Drops any held latch.
pub async fn set_voice_input_mode(
    mode: VoiceInputMode,
    state: &Arc<AppState>,
) -> Result<VoiceGateState> {
    mutate_gate(state, |g| g.set_mode(mode)).await
}

/// Push-to-talk key down (`held: true`) / up (`held: false`).
/// Also called with `false` on blur, via `release_voice_ptt`.
pub async fn set_voice_ptt_held(held: bool, state: &Arc<AppState>) -> Result<VoiceGateState> {
    mutate_gate(state, |g| g.set_ptt_held(held)).await
}

/// Release the push-to-talk latch because the window lost keyboard focus.
///
/// A separate name so the call site reads as the safety measure it is: after
/// focus moves, the OS never delivers the keyup. Idempotent; never clears an
/// explicit mute.
pub async fn release_voice_ptt(state: &Arc<AppState>) -> Result<VoiceGateState> {
    mutate_gate(state, |g| g.on_focus_lost()).await
}

/// Read the gate without changing it (resync after a reload).
pub async fn get_voice_gate_state(state: &Arc<AppState>) -> Result<VoiceGateState> {
    let voice = state.voice.lock().await;
    let gate = voice.gate.lock().unwrap();
    Ok(gate.snapshot())
}

/// Apply one transition, then republish to the hot-path atomics and the
/// LiveKit publication. The single chokepoint, so no caller can forget half
/// the propagation.
async fn mutate_gate<F>(state: &Arc<AppState>, f: F) -> Result<VoiceGateState>
where
    F: FnOnce(&mut TransmitGate),
{
    let voice = state.voice.lock().await;
    let snapshot = {
        let mut gate = voice.gate.lock().unwrap();
        f(&mut gate);
        publish_gate(&voice, &gate);
        apply_gate_to_room(&voice, &gate);
        gate.snapshot()
    };
    Ok(snapshot)
}

/// Set the per-user output gain multiplier for a remote participant.
///
/// `user_id` is the bare user id (see `user_id_from_voice_identity`); `volume`
/// is clamped to 0.0..=2.0. Live mixer state only (the frontend persists it);
/// 1.0 removes the entry so unity tracks take the mixer's fast path.
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

/// Switch the microphone mid-call. APM is rate-locked, so it is rebuilt when
/// the new device's rate differs.
pub async fn set_voice_input_device(
    device_name: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut voice = state.voice.lock().await;

    if voice.audio_source.is_none() {
        voice.current_input_device = Some(device_name);
        return Ok(());
    }

    // cpal init makes blocking ALSA syscalls: release the lock first.
    let is_muted_clone = Arc::clone(&voice.transmit_muted);
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
                crate::diag!("[voice] APM rebuild on mic switch failed: {e}");
                None
            }
        }
    } else {
        None
    };

    let mut voice = state.voice.lock().await;

    voice.input_stream = Some(new_mic);
    voice.current_input_device = Some(device_name);
    if rate_changed {
        voice.apm = new_apm_stage;
    }

    // RNNoise state is tied to the old mic; reset it, re-engaging only at the
    // right rate with click suppression on.
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
                        crate::diag!("[voice] APM capture error (frame dropped): {e}");
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

/// Switch the speaker mid-call. Drain tasks and `track_buffers` survive, so
/// the new mixer resumes without re-subscribing.
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

/// Update the live APM configuration (only the changed submodule
/// re-initialises) and match the RNNoise slot to `click_suppression`. A no-op
/// outside a session.
pub async fn set_voice_audio_processing(
    config: voice_apm::ApmConfig,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut voice = state.voice.lock().await;

    let mic_rate = voice.apm.as_ref().map(|a| a.sample_rate_hz());

    if let Some(stage) = voice.apm.as_mut() {
        stage.set_config(config.clone());
    }

    if let Some(rate) = mic_rate {
        let mut slot = voice.denoiser.lock().unwrap();
        let want = config.click_suppression && rate == voice_denoiser::REQUIRED_RATE_HZ;
        match (want, slot.is_some()) {
            (true, false) => {
                crate::diag!("[voice/rnnoise] enabled mid-call");
                *slot = Some(voice_denoiser::DenoiserStage::new());
            }
            (false, true) => {
                crate::diag!("[voice/rnnoise] disabled mid-call");
                *slot = None;
            }
            _ => { /* already in the desired state */ }
        }
    }

    Ok(())
}
