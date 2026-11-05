use std::collections::HashSet;
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
use super::join::{
    classify_connect_error, classify_ds_error, disconnect_is_retryable, reconnect_delay,
    JoinCancel, JoinClock, VoiceJoinError, VOICE_JOIN_DEADLINE, VOICE_RECONNECT_WINDOW,
};
use super::levels::BandAnalyzer;
use super::playback::{ensure_playback, register_remote_track};
use super::streams::start_mic_stream;
use super::types::{
    user_id_from_voice_identity, JoinTimings, SendableStream, VoiceEvent, VoiceState,
    VoiceWarmup, VOICE_WARMUP_TTL,
};

/// Mirror gate state onto the lock-free atomics the audio paths read.
fn publish_gate(voice: &VoiceState, gate: &TransmitGate) {
    voice
        .transmit_muted
        .store(!gate.transmitting(), Ordering::Relaxed);
    voice.deafened.store(gate.deafened(), Ordering::Relaxed);
    voice.self_muted.store(gate.self_muted(), Ordering::Relaxed);
}

/// Signal mic state to the room; PTT-idle and deafen show as plain mute.
fn apply_gate_to_room(voice: &VoiceState, gate: &TransmitGate) {
    let muted = !gate.transmitting();

    // Hint APM that the output is muted so its AGC/AEC stop adapting to
    // silence frames during the mute window.
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

// The LOCAL identity; remote peers are per-room pseudonyms (#836).
use crate::commands::livekit_identity::voice_identity;

/// Register the Tauri Channel used to push VoiceEvents to the frontend.
pub async fn subscribe_voice_events(
    on_event: std::sync::Arc<dyn crate::sink::EventSink<VoiceEvent>>,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut voice = state.voice.lock().await;
    voice.channel = Some(on_event);
    Ok(())
}

/// Warmup before Join: pre-mint the token and prime DNS/TLS/conn pool.
/// Same channel_id is a no-op; a different one supersedes it.
pub async fn prepare_voice_connection(
    channel_id: String,
    user_id: String,
    display_name: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let url = state.config.livekit_url.clone();
    if url.is_empty() {
        return Ok(());
    }

    // De-dupe: skip if already warm for this channel + identity.
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

    // Pre-fetch the DS-minted token; best-effort.
    let (token, ds_url) = match crate::commands::mls::ds_livekit_token(state, &channel_id, "voice").await {
        Ok(pair) => pair,
        Err(e) => {
            crate::diag!("[voice] warmup token error (non-fatal): {e}");
            return Ok(());
        }
    };

    // Best-effort; a racing Join costs at most one extra TLS handshake.
    let task = tokio::spawn(warmup_probe(
        Arc::clone(state),
        channel_id.clone(),
        token.clone(),
        ds_url.clone(),
    ));

    // Replacing an entry drops it, aborting its probe task.
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

/// The SFU's token check (`/rtc/validate`, the same probe the SDK runs before a
/// websocket), which warms DNS/TLS and proves the warm token. A rejected token
/// is dropped from the warmup so the join mints a fresh one instead of dialling
/// with one the SFU has already refused.
async fn warmup_probe(state: Arc<AppState>, channel_id: String, token: String, ws_url: String) {
    let twirp = livekit_http_base(&ws_url);
    let probe = format!("{twirp}/rtc/validate?access_token={token}");
    // The SHARED client, so the warmed pool and TLS cache survive (§14.4).
    let client = crate::net::overlay::http_client(None);
    let started = Instant::now();
    let status = match client.get(&probe).timeout(Duration::from_secs(5)).send().await {
        Ok(resp) => resp.status(),
        Err(e) => {
            crate::diag!("[voice] warmup probe to {twirp} failed (non-fatal): {e}");
            return;
        }
    };
    let elapsed = started.elapsed().as_secs_f64() * 1000.0;
    if status.is_success() {
        crate::diag!("[voice] warmup probe to {twirp} validated the token in {elapsed:.0}ms");
        return;
    }
    crate::diag!("[voice] warmup probe to {twirp} rejected the token ({status}); discarding the warm entry");
    let mut voice = state.voice.lock().await;
    if voice
        .warmup
        .as_ref()
        .is_some_and(|w| w.channel_id == channel_id && w.token == token)
    {
        voice.warmup = None;
    }
}

/// The SFU's HTTP origin for its websocket address (`wss://` → `https://`).
fn livekit_http_base(ws_url: &str) -> String {
    if let Some(rest) = ws_url.strip_prefix("wss://") {
        format!("https://{rest}")
    } else if let Some(rest) = ws_url.strip_prefix("ws://") {
        format!("http://{rest}")
    } else {
        ws_url.to_string()
    }
}

/// Clears the `joining` flag when the join returns, whichever way.
struct JoinGuard(Arc<AtomicBool>);

impl Drop for JoinGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Everything a join has acquired so far, so a failed or cancelled later phase
/// can give it all back instead of leaving a room open or a mic held.
#[derive(Default)]
struct JoinPartial {
    room: Option<Arc<Room>>,
    mic_stream: Option<SendableStream>,
    frame_task: Option<tokio::task::JoinHandle<()>>,
}

impl JoinPartial {
    async fn discard(self, state: &Arc<AppState>) {
        if let Some(t) = self.frame_task {
            t.abort();
        }
        if let Ok(mut slot) = state.voice.lock().await.denoiser.lock() {
            *slot = None;
        }
        if let Some(room) = self.room {
            // The caller is on its way out; closing can hang on a dead link.
            tokio::spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(5), room.close()).await;
            });
        }
        if let Some(stream) = self.mic_stream {
            // Drop off-worker, else macOS's mic indicator can stick on.
            let _ = tokio::task::spawn_blocking(move || drop(stream)).await;
        }
    }
}

/// Connect to a LiveKit voice room, publish the local microphone, and start
/// the remote-playback pipeline (single shared mixer + cpal output stream).
///
/// Every phase runs under [`VOICE_JOIN_DEADLINE`] and the cancel flag a
/// concurrent `leave_voice_channel` raises; failures are typed [`VoiceJoinError`]s.
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
    // 1:1 call peer; required for `call-` rooms (no DB row), else ignored.
    counterparty_user_id: Option<String>,
    state: &Arc<AppState>,
) -> Result<()> {
    // Refuse re-entry: two same-identity joins trip DuplicateIdentity.
    let (cancel, _join_guard) = {
        let mut voice = state.voice.lock().await;
        if voice.room.is_some() {
            return Err(VoiceJoinError::AlreadyJoined.into());
        }
        if voice.joining.swap(true, Ordering::AcqRel) {
            return Err(VoiceJoinError::JoinInProgress.into());
        }
        let cancel = Arc::new(JoinCancel::default());
        voice.join_cancel = Some(Arc::clone(&cancel));
        (cancel, JoinGuard(Arc::clone(&voice.joining)))
    };
    let clock = JoinClock::new(cancel, VOICE_JOIN_DEADLINE);

    let args = JoinArgs {
        channel_id,
        user_id,
        display_name,
        input_device,
        output_device,
        audio_processing,
        counterparty_user_id,
    };
    match join_inner(&clock, args, state).await {
        Ok(()) => Ok(()),
        Err(e) => {
            crate::diag!("[voice] join failed: {e}");
            Err(e.into())
        }
    }
}

struct JoinArgs {
    channel_id: String,
    user_id: String,
    display_name: String,
    input_device: Option<String>,
    output_device: Option<String>,
    audio_processing: voice_apm::ApmConfig,
    counterparty_user_id: Option<String>,
}

/// What the room task needs to translate and attribute events for one session.
struct RoomContext {
    /// Logical room, to resolve room-scoped pseudonyms (#836).
    logical_room: String,
    /// Self-hear filter: don't play this user's other devices (#140).
    local_user_id: String,
    local_identity: String,
    display_name: String,
    apm_rate: u32,
    /// Internal identities of the remote participants the renderer currently shows.
    known_remote: Arc<std::sync::Mutex<HashSet<String>>>,
}

async fn join_inner(clock: &JoinClock, args: JoinArgs, state: &Arc<AppState>) -> std::result::Result<(), VoiceJoinError> {
    let JoinArgs {
        channel_id,
        user_id,
        display_name,
        input_device,
        output_device,
        audio_processing,
        counterparty_user_id,
    } = args;
    let join_start = Instant::now();
    let join_started_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut partial = JoinPartial::default();

    // Per-device identity (#140) so a user's second device isn't kicked.
    let device_id = state.device_id.lock().await.clone();
    let local_identity = voice_identity(&user_id, device_id.as_deref());

    // VoiceWarmup is Drop: clone the token out, don't destructure.
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
            // Token and URL travel together — see `VoiceWarmup::url`.
            let cloned = voice.warmup.as_ref().map(|w| (w.token.clone(), w.url.clone()));
            voice.warmup = None;
            cloned
        } else {
            voice.warmup = None;
            None
        }
    };
    // jwt_mint: 0 if a warmup token was usable.
    let jwt_mint_ms;
    let (token, url): (String, String) = match cached_token {
        Some(pair) => {
            jwt_mint_ms = 0;
            pair
        }
        None => {
            let jwt_start = Instant::now();
            let minted = clock
                .run("token", crate::commands::mls::ds_livekit_token(state, &channel_id, "voice"))
                .await?;
            let pair = minted.map_err(|e| classify_ds_error(&e))?;
            jwt_mint_ms = jwt_start.elapsed().as_millis() as u64;
            pair
        }
    };
    if url.is_empty() {
        return Err(VoiceJoinError::NotConfigured);
    }

    // #836: pin our pseudonym first so our own events land on our tile.
    pin_local_identity(state, &channel_id, &token, &local_identity, &display_name).await;

    // Room connect and mic init run concurrently (tokio::join!).
    let (frame_tx, mut frame_rx) = tokio::sync::mpsc::unbounded_channel::<(Vec<i16>, u32)>();
    // Reset session gate bits; input mode (a pref) survives.
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

    // Voice key from the MLS exporter; fail closed, never unencrypted.
    // Must precede connect: `RoomOptions::encryption` carries the key.
    let e2ee_start = Instant::now();
    let derived = clock
        .run(
            "e2ee",
            voice_e2ee::derive_voice_key(
                state,
                &channel_id,
                &user_id,
                counterparty_user_id.as_deref(),
            ),
        )
        .await?;
    let (voice_key, voice_key_index, voice_epoch, voice_mls_group_id) =
        derived.map_err(|e| classify_ds_error(&e))?;
    let e2ee_ms = e2ee_start.elapsed().as_millis() as u64;
    let e2ee_options = voice_e2ee::build_e2ee_options(voice_key, voice_key_index);
    let key_provider_for_state = e2ee_options.key_provider.clone();
    crate::diag!(
        "[voice] e2ee armed for {channel_id} (mls_group={voice_mls_group_id}, epoch={voice_epoch}, ring slot={voice_key_index})"
    );

    let connect_started = Instant::now();
    let mic_started = Instant::now();
    crate::diag!("[voice] connecting to room {channel_id} and opening mic in parallel…");

    let mut room_options = RoomOptions::default();
    room_options.encryption = Some(e2ee_options);
    let connect_fut = async {
        let r = Room::connect(&url, &token, room_options).await;
        let elapsed_ms = connect_started.elapsed().as_millis() as u64;
        (r, elapsed_ms)
    };
    let mic_fut = async {
        tokio::task::spawn_blocking(move || {
            // Test seam: e2e listen-only path without unplugging a mic.
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

    // A cancelled connect drops the half-open session; a cancelled mic open
    // finishes on its blocking thread and drops the stream there.
    let (connect_pair, mic_res) = clock.run("connect", async { tokio::join!(connect_fut, mic_fut) }).await?;
    let (connect_res, room_connect_ms) = connect_pair;
    let (room, events) = match connect_res {
        Ok(pair) => pair,
        Err(e) => {
            if let Ok(Ok((stream, _, _))) = mic_res {
                partial.mic_stream = Some(stream);
            }
            let err = classify_connect_error(&e);
            partial.discard(state).await;
            return Err(err);
        }
    };

    // Mic is best-effort: on failure join listen-only.
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
    // Listen-only still needs a nominal rate; 48 kHz.
    let mic_rate = mic.as_ref().map(|(_, r, _)| *r).unwrap_or(48_000);
    let mic_init_ms = mic.as_ref().map(|(_, _, ms)| *ms).unwrap_or(0);
    let mic_stream = mic.map(|(s, _, _)| s);
    if has_mic {
        crate::diag!("[voice] connected to room {channel_id}, mic at {mic_rate} Hz");
    } else {
        crate::diag!("[voice] connected to room {channel_id}, listen-only (no mic)");
    }

    let room = Arc::new(room);
    partial.room = Some(Arc::clone(&room));
    partial.mic_stream = mic_stream;

    // APM needs 8/16/32/48 kHz; skipped otherwise and on listen-only.
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

    // RNNoise is 48 kHz-only; other rates pass straight to APM.
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

    let mut audio_source_opt: Option<NativeAudioSource> = None;
    let mut local_track_opt: Option<LocalAudioTrack> = None;
    let mut first_publish_ms: u64 = 0;

    if has_mic {
        // libwebrtc's APM off: ours is the only mic processing stage.
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
        let published = match clock
            .run(
                "publish",
                room.local_participant().publish_track(
                    LocalTrack::Audio(local_track.clone()),
                    TrackPublishOptions::default(),
                ),
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                partial.discard(state).await;
                return Err(e);
            }
        };
        if let Err(e) = published {
            let err = VoiceJoinError::PublishFailed { detail: format!("publish track: {e}") };
            partial.discard(state).await;
            return Err(err);
        }
        first_publish_ms = publish_start.elapsed().as_millis() as u64;
        // Move the new mic cryptor (slot 0) to the join epoch's slot.
        let moved = voice_e2ee::apply_sender_key_index(&room, voice_key_index);
        crate::diag!("[voice] track published ({moved} sender cryptor(s) on ring slot {voice_key_index})");

        // Mic task: 10 ms rebuffer → APM → capture_frame.
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

            // Own-tile meter; sink cloned once so emits never re-lock.
            let bands_sink = voice_arc_frame.lock().await.channel.clone();
            let mut analyzer = BandAnalyzer::new(mic_rate);
            let mut bands_ctr: u32 = 0;

            while let Some((samples, rate)) = frame_rx.recv().await {
                buf.extend_from_slice(&samples);
                while buf.len() >= chunk_size {
                    let mut chunk: Vec<i16> = buf.drain(..chunk_size).collect();

                    // RNNoise first so APM's NS/AGC adapt to voice, not clicks.
                    {
                        let mut guard = denoiser_for_capture.lock().unwrap();
                        if let Some(d) = guard.as_mut() {
                            d.process(&mut chunk);
                        }
                    }

                    // APM mutates the chunk in place (AGC + NS + HPF + AEC capture).
                    if let Some(apm) = &apm_for_capture {
                        if let Err(e) = voice_apm::run_capture(apm, &mut chunk, chunk_size) {
                            crate::diag!("[voice] APM capture error (frame dropped): {e}");
                            continue;
                        }
                    }

                    let peak = chunk.iter().map(|&s| s.abs()).max().unwrap_or(0);

                    // Speaking needs 2 consecutive above-threshold frames.
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
        partial.frame_task = Some(frame_task);
    }

    // Open playback before the event loop: first TrackSubscribed needs it.
    let speaker_ok = match clock
        .run(
            "speaker",
            ensure_playback(
                Arc::clone(&state.voice),
                output_device.clone(),
                apm_handle.clone(),
                mic_rate,
                apm_frame_samples,
            ),
        )
        .await
    {
        Ok(Ok(())) => true,
        Ok(Err(e)) => {
            crate::diag!("[voice] playback init failed (speaker disabled this session): {e}");
            false
        }
        Err(e) => {
            partial.discard(state).await;
            return Err(e);
        }
    };

    // Seed roster only; TrackSubscribed attaches tracks (twice would race).
    let roster_start = Instant::now();
    let seeded = match clock
        .run("roster", roster_snapshot(state, &room, &channel_id, Some(&local_identity)))
        .await
    {
        Ok(s) => s,
        Err(e) => {
            partial.discard(state).await;
            return Err(e);
        }
    };
    let roster_ms = roster_start.elapsed().as_millis() as u64;

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

    let known_remote: HashSet<String> = seeded.remote.iter().map(|p| p.identity.clone()).collect();
    let ctx = Arc::new(RoomContext {
        logical_room: channel_id.clone(),
        local_user_id: user_id.clone(),
        local_identity: local_identity.clone(),
        display_name: display_name.clone(),
        apm_rate: mic_rate,
        known_remote: Arc::new(std::sync::Mutex::new(known_remote)),
    });

    // Store the session and start its event task under ONE lock: a `Disconnected`
    // that races the store must find the room it belongs to, not an empty slot.
    let mut voice = state.voice.lock().await;
    if clock_cancelled(&voice) {
        drop(voice);
        partial.discard(state).await;
        return Err(VoiceJoinError::Cancelled);
    }
    let JoinPartial { room: _, mic_stream, frame_task } = partial;
    if let Some(t) = voice.room_task.take() { t.abort(); }
    if let Some(t) = voice.frame_task.take() { t.abort(); }
    voice.room = Some(Arc::clone(&room));
    // All `None` on the listen-only path (no mic captured/published).
    voice.local_track = local_track_opt;
    voice.audio_source = audio_source_opt;
    voice.input_stream = mic_stream;
    voice.frame_task = frame_task;
    voice.current_input_device = input_device;
    voice.apm = apm_stage;
    voice.e2ee_key_provider = Some(key_provider_for_state);
    voice.e2ee_mls_group_id = Some(voice_mls_group_id);
    voice.e2ee_epoch = voice_epoch;
    voice.e2ee_key_index = voice_key_index;
    *voice.last_join_timings.lock().unwrap() = Some(timings);
    voice.room_task = Some(tokio::spawn(run_room_task(
        Arc::clone(state),
        Arc::clone(&room),
        events,
        ctx,
    )));

    // Publications start unmuted; sync so PTT joins aren't shown hot.
    {
        let gate = voice.gate.lock().unwrap();
        apply_gate_to_room(&voice, &gate);
    }

    if let Some(ch) = &voice.channel {
        let _ = ch.send(VoiceEvent::ParticipantJoined {
            identity: local_identity.clone(),
            name: display_name.clone(),
            is_muted: false,
            avatar_url: seeded.local_avatar_url,
        });
        for p in seeded.remote {
            crate::diag!("[voice] existing participant: {} muted={}", p.identity, p.is_muted);
            let _ = ch.send(p.joined_event());
        }
        // Tell the renderer if this session can transmit (listen-only UI) and hear.
        let _ = ch.send(VoiceEvent::MicAvailability {
            identity: local_identity.clone(),
            available: has_mic,
        });
        if !speaker_ok {
            let _ = ch.send(VoiceEvent::SpeakerAvailability {
                identity: local_identity.clone(),
                available: false,
            });
        }
    }

    Ok(())
}

/// Whether the in-flight join's cancel flag is up; read under the voice lock.
fn clock_cancelled(voice: &VoiceState) -> bool {
    voice.join_cancel.as_ref().is_some_and(|c| c.is_cancelled())
}

/// One remote participant as the renderer first learns of it.
struct SeededParticipant {
    identity: String,
    name: String,
    is_muted: bool,
    avatar_url: Option<String>,
}

impl SeededParticipant {
    fn joined_event(self) -> VoiceEvent {
        VoiceEvent::ParticipantJoined {
            identity: self.identity,
            name: self.name,
            is_muted: self.is_muted,
            avatar_url: self.avatar_url,
        }
    }
}

struct RosterSnapshot {
    remote: Vec<SeededParticipant>,
    local_avatar_url: Option<String>,
}

/// Everyone in `room` right now, resolved (#836) and with avatars, in TWO DS
/// round trips whatever the room size: one batched identity resolve, one batched
/// avatar lookup (the local identity rides along when given).
async fn roster_snapshot(
    state: &Arc<AppState>,
    room: &Room,
    logical_room: &str,
    local_identity: Option<&str>,
) -> RosterSnapshot {
    let participants = room.remote_participants();
    // #836: resolve all pseudonyms in ONE batched DS call.
    let seen: Vec<String> = participants.keys().map(|k| k.to_string()).collect();
    let resolved = resolve_participants(state, logical_room, &seen).await;
    let existing: Vec<(String, String, bool)> = participants
        .into_values()
        .filter_map(|p| {
            let wire = p.identity().to_string();
            let (identity, name) = resolved
                .get(&wire)
                .cloned()
                .unwrap_or_else(|| (wire.clone(), String::new()));
            // Skip `view` clients (both resolved and wire forms).
            if is_view_identity(&identity) || is_view_identity(&wire) {
                return None;
            }
            // Seed mute state: TrackMuted only fires on transitions.
            let is_muted = p.track_publications().values().any(|pub_| pub_.is_muted());
            Some((identity, name, is_muted))
        })
        .collect();
    // All avatars (self + roster) in ONE DS round trip, not N+1.
    let mut avatar_identities: Vec<String> = Vec::with_capacity(existing.len() + 1);
    if let Some(local) = local_identity {
        avatar_identities.push(local.to_string());
    }
    avatar_identities.extend(existing.iter().map(|(identity, _, _)| identity.clone()));
    let avatars = lookup_avatar_urls_for_identities(state, &avatar_identities).await;
    let local_avatar_url = local_identity.and_then(|l| avatars.get(l).cloned());
    let remote = existing
        .into_iter()
        .map(|(identity, name, is_muted)| SeededParticipant {
            avatar_url: avatars.get(&identity).cloned(),
            identity,
            name,
            is_muted,
        })
        .collect();
    RosterSnapshot { remote, local_avatar_url }
}

/// Re-emit the roster after a link recovered: whoever vanished meanwhile leaves,
/// everyone present (re)joins. Both are idempotent for the renderer.
async fn reseed_roster(state: &Arc<AppState>, room: &Room, ctx: &RoomContext) {
    let snapshot = roster_snapshot(state, room, &ctx.logical_room, None).await;
    let present: HashSet<String> = snapshot.remote.iter().map(|p| p.identity.clone()).collect();
    let gone: Vec<String> = {
        let mut known = ctx.known_remote.lock().unwrap();
        let gone = known.difference(&present).cloned().collect();
        *known = present;
        gone
    };
    for identity in &gone {
        crate::commands::screenshare::on_participant_left(identity, state).await;
    }
    let voice = state.voice.lock().await;
    if let Some(ch) = &voice.channel {
        for identity in gone {
            let _ = ch.send(VoiceEvent::ParticipantLeft { identity });
        }
        for p in snapshot.remote {
            let _ = ch.send(p.joined_event());
        }
    }
}

/// Whether `room` is still the session the state holds (a leave swaps it out).
async fn is_current_room(state: &Arc<AppState>, room: &Arc<Room>) -> bool {
    state
        .voice
        .lock()
        .await
        .room
        .as_ref()
        .is_some_and(|r| Arc::ptr_eq(r, room))
}

async fn emit(state: &Arc<AppState>, event: VoiceEvent) {
    let voice = state.voice.lock().await;
    if let Some(ch) = &voice.channel {
        let _ = ch.send(event);
    }
}

/// How one room's event stream ended.
enum StreamEnd {
    Disconnected(livekit::DisconnectReason),
    Closed,
}

/// Owns the session from join to teardown: drains one room's events, and when
/// the SFU drops a link it can't recover, re-mints a token and connects again
/// inside the same session (mic, APM, playback and key provider all kept).
async fn run_room_task(
    state: Arc<AppState>,
    mut room: Arc<Room>,
    mut events: tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
    ctx: Arc<RoomContext>,
) {
    // The SDK's own resume/restart is attempt 1 of an episode; ours count on from there.
    let mut episode_attempt: u32 = 0;
    loop {
        let end = drive_room_events(&state, &room, &mut events, &ctx, &mut episode_attempt).await;
        if !is_current_room(&state, &room).await {
            // Left (or replaced) while we were draining; whoever did it released the state.
            return;
        }
        let reason = match end {
            StreamEnd::Disconnected(r) => r,
            StreamEnd::Closed => livekit::DisconnectReason::UnknownReason,
        };
        if !disconnect_is_retryable(reason) {
            let error = (reason != livekit::DisconnectReason::ClientInitiated).then(|| VoiceJoinError::Internal {
                detail: format!("disconnected: {reason:?}"),
            });
            finish_session(&state, &room, error).await;
            return;
        }
        crate::diag!("[voice] link lost ({reason:?}); reconnecting in-session");
        match reconnect_session(&state, &room, &ctx, &mut episode_attempt).await {
            Ok((new_room, new_events)) => {
                room = new_room;
                events = new_events;
                episode_attempt = 0;
            }
            Err(ReconnectEnd::Left) => return,
            Err(ReconnectEnd::GaveUp(error)) => {
                finish_session(&state, &room, Some(error)).await;
                return;
            }
        }
    }
}

/// Final teardown from inside the room task: tell the renderer, then release
/// everything (the task itself is left to return).
async fn finish_session(state: &Arc<AppState>, room: &Arc<Room>, error: Option<VoiceJoinError>) {
    crate::diag!("[voice] session over: {error:?}");
    emit(state, VoiceEvent::Disconnected { error }).await;
    crate::commands::screenshare::on_room_disconnected(state).await;
    // We ARE room_task (returning next), so don't abort it.
    let _ = release_voice_resources(state, false, Some(room)).await;
}

enum ReconnectEnd {
    /// The user left during the attempt; nothing to report.
    Left,
    GaveUp(VoiceJoinError),
}

/// Re-mint and reconnect until [`VOICE_RECONNECT_WINDOW`] runs out. Each attempt
/// is a reaction to the previous failure, never a periodic poll.
async fn reconnect_session(
    state: &Arc<AppState>,
    old_room: &Arc<Room>,
    ctx: &Arc<RoomContext>,
    episode_attempt: &mut u32,
) -> std::result::Result<(Arc<Room>, tokio::sync::mpsc::UnboundedReceiver<RoomEvent>), ReconnectEnd> {
    // Local shares published into the dead room; their state resets like a leave's.
    crate::commands::screenshare::on_room_disconnected(state).await;
    let _ = crate::commands::camera::stop_camera(state).await;
    clear_remote_audio_tracks(state).await;

    let window_end = Instant::now() + VOICE_RECONNECT_WINDOW;
    let mut last_error = VoiceJoinError::LivekitUnreachable { detail: "reconnect window elapsed".into() };
    let mut attempt: u32 = 0;
    let gate = crate::commands::livekit::sfu_gate();
    while Instant::now() < window_end {
        attempt += 1;
        *episode_attempt += 1;
        emit(state, VoiceEvent::Reconnecting { attempt: *episode_attempt }).await;
        // A known SFU outage: let the realtime prober dial; we follow it back.
        if gate.is_down() && gate.report_down(SFU_GATE_VOICE) == crate::commands::livekit::SfuRole::Waiter {
            let remaining = window_end.saturating_duration_since(Instant::now());
            gate.wait_up(remaining).await;
        } else {
            tokio::time::sleep(reconnect_delay(attempt)).await;
        }
        if !is_current_room(state, old_room).await {
            gate.release_prober(SFU_GATE_VOICE);
            return Err(ReconnectEnd::Left);
        }

        // Fresh token every time: the SDK's own retries reuse the join token, which
        // the DS mints for 15 minutes, so a long outage needs a new one.
        let (token, url) = match crate::commands::mls::ds_livekit_token(state, &ctx.logical_room, "voice").await {
            Ok(pair) => pair,
            Err(e) => {
                last_error = classify_ds_error(&e);
                crate::diag!("[voice] reconnect attempt {attempt}: token mint failed: {e}");
                continue;
            }
        };
        pin_local_identity(state, &ctx.logical_room, &token, &ctx.local_identity, &ctx.display_name).await;

        let key_provider = {
            let voice = state.voice.lock().await;
            match voice.e2ee_key_provider.clone() {
                Some(kp) => kp,
                None => return Err(ReconnectEnd::Left),
            }
        };
        let mut options = RoomOptions::default();
        options.encryption = Some(livekit::e2ee::E2eeOptions {
            encryption_type: livekit::e2ee::EncryptionType::Gcm,
            key_provider,
        });
        let (new_room, new_events) = match Room::connect(&url, &token, options).await {
            Ok(pair) => pair,
            Err(e) => {
                last_error = classify_connect_error(&e);
                if matches!(last_error, VoiceJoinError::LivekitUnreachable { .. }) {
                    gate.report_down(SFU_GATE_VOICE);
                }
                crate::diag!("[voice] reconnect attempt {attempt}: connect failed: {e}");
                continue;
            }
        };
        gate.report_up();
        let new_room = Arc::new(new_room);

        // Swap only if nobody left meanwhile; otherwise the new room is ours to close.
        let (local_track, key_index) = {
            let mut voice = state.voice.lock().await;
            let still_ours = voice.room.as_ref().is_some_and(|r| Arc::ptr_eq(r, old_room));
            if !still_ours {
                drop(voice);
                let _ = tokio::time::timeout(Duration::from_secs(5), new_room.close()).await;
                return Err(ReconnectEnd::Left);
            }
            voice.room = Some(Arc::clone(&new_room));
            (voice.local_track.clone(), voice.e2ee_key_index)
        };

        if let Some(track) = local_track {
            match new_room
                .local_participant()
                .publish_track(LocalTrack::Audio(track), TrackPublishOptions::default())
                .await
            {
                Ok(_) => {
                    let moved = voice_e2ee::apply_sender_key_index(&new_room, key_index);
                    crate::diag!("[voice] reconnected: mic republished ({moved} sender cryptor(s) on ring slot {key_index})");
                }
                Err(e) => {
                    crate::diag!("[voice] reconnected listen-only: republish failed: {e}");
                    emit(state, VoiceEvent::MicAvailability { identity: ctx.local_identity.clone(), available: false }).await;
                }
            }
        }
        {
            let voice = state.voice.lock().await;
            let gate = voice.gate.lock().unwrap();
            apply_gate_to_room(&voice, &gate);
        }
        reseed_roster(state, &new_room, ctx).await;
        emit(state, VoiceEvent::Reconnected).await;
        crate::diag!("[voice] reconnected in-session after {attempt} attempt(s)");
        return Ok((new_room, new_events));
    }
    gate.release_prober(SFU_GATE_VOICE);
    Err(ReconnectEnd::GaveUp(last_error))
}

/// The voice session's name in the SFU gate's prober election.
const SFU_GATE_VOICE: &str = "voice";

/// Drop every remote track's drain and buffer; the speaker stream and mixer stay
/// up for the tracks the next room subscribes.
async fn clear_remote_audio_tracks(state: &Arc<AppState>) {
    let buffers = {
        let voice = state.voice.lock().await;
        let mut pb = voice.playback.lock().unwrap();
        for (_, t) in pb.drain_tasks.drain() {
            t.abort();
        }
        pb.rtc_tracks.clear();
        pb.identities.clear();
        Arc::clone(&pb.track_buffers)
    };
    let _ = buffers.lock().map(|mut guard| guard.clear());
}

/// Drain one room's events until its stream ends.
async fn drive_room_events(
    state: &Arc<AppState>,
    room: &Arc<Room>,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<RoomEvent>,
    ctx: &Arc<RoomContext>,
    episode_attempt: &mut u32,
) -> StreamEnd {
    let state_for_room = state;
    let voice_arc = &state.voice;
    let logical_room = &ctx.logical_room;
    let local_user_id = &ctx.local_user_id;
    let apm_rate_for_room = ctx.apm_rate;
    while let Some(event) = events.recv().await {
        match event {
            RoomEvent::ParticipantConnected(p) => {
                let wire = p.identity().to_string();
                // Same filter as the seed loop above — see comment there.
                if is_view_identity(&wire) {
                    continue;
                }
                // #836: translate before anything downstream sees it.
                let (identity, name) =
                    resolve_participant(state_for_room, logical_room, &wire).await;
                if is_view_identity(&identity) {
                    continue;
                }
                crate::diag!("[voice] participant joined: {identity}");
                let is_muted = p.track_publications().values().any(|pub_| pub_.is_muted());
                let avatar_url =
                    lookup_avatar_url_for_identity(state_for_room, &identity).await;
                ctx.known_remote.lock().unwrap().insert(identity.clone());
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
                    resolve_participant(state_for_room, logical_room, &wire).await;
                if is_view_identity(&identity) {
                    continue;
                }
                crate::diag!("[voice] participant left: {identity}");
                ctx.known_remote.lock().unwrap().remove(&identity);
                // Clear their screenshare so a stale tile doesn't linger.
                crate::commands::screenshare::on_participant_left(
                    &identity,
                    state_for_room,
                )
                .await;
                let voice = voice_arc.lock().await;
                if let Some(ch) = &voice.channel {
                    let _ = ch.send(VoiceEvent::ParticipantLeft { identity });
                }
            }
            RoomEvent::TrackSubscribed { track, publication, participant } => {
                let (participant_identity, _) = resolve_participant(
                    state_for_room,
                    logical_room,
                    &participant.identity().to_string(),
                )
                .await;
                match track {
                    RemoteTrack::Audio(audio_track) => {
                        // #140: never play our other devices' mic.
                        if user_id_from_voice_identity(&participant_identity) == local_user_id {
                            crate::diag!(
                                "[voice] skipping own-device audio track for self-hear mute: {participant_identity}"
                            );
                            continue;
                        }
                        let track_key = format!("{}-{}", participant_identity, audio_track.sid());
                        // TrackSource marks shared audio.
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
                            Arc::clone(voice_arc),
                            participant_identity,
                            apm_rate_for_room,
                            is_shared_audio,
                        )
                        .await;
                    }
                    RemoteTrack::Video(video_track) => {
                        let track_key = format!("{}-{}", participant_identity, video_track.sid());
                        // TrackSource tells screen share from webcam.
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
                            state_for_room,
                        )
                        .await;
                    }
                }
            }
            RoomEvent::TrackUnsubscribed { track, publication: _, participant } => {
                let (participant_identity, _) = resolve_participant(
                    state_for_room,
                    logical_room,
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
                            state_for_room,
                        )
                        .await;
                    }
                }
            }

            RoomEvent::TrackMuted { participant, publication: _ } => {
                let (identity, _) = resolve_participant(
                    state_for_room,
                    logical_room,
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
                    state_for_room,
                    logical_room,
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
                return StreamEnd::Disconnected(reason);
            }
            // The SDK is resuming or restarting the link itself (same token).
            RoomEvent::Reconnecting => {
                *episode_attempt = (*episode_attempt).max(1);
                crate::diag!("[voice] SDK reconnecting (attempt {episode_attempt})");
                emit(state, VoiceEvent::Reconnecting { attempt: *episode_attempt }).await;
            }
            RoomEvent::Reconnected => {
                crate::diag!("[voice] SDK reconnected");
                *episode_attempt = 0;
                {
                    let voice = voice_arc.lock().await;
                    let gate = voice.gate.lock().unwrap();
                    apply_gate_to_room(&voice, &gate);
                }
                voice_e2ee::sync_local_sender_key_index(state_for_room).await;
                reseed_roster(state_for_room, room, ctx).await;
                emit(state, VoiceEvent::Reconnected).await;
            }
            RoomEvent::ConnectionStateChanged(conn_state) => {
                crate::diag!("[voice] connection state: {conn_state:?}");
            }
            // #1097: logged so e2e can assert voice forward secrecy.
            RoomEvent::E2eeStateChanged { participant, state } => {
                let (identity, _) = resolve_participant(
                    state_for_room,
                    logical_room,
                    &participant.identity().to_string(),
                )
                .await;
                crate::diag!(
                    "[voice-e2ee] frame state for {identity}: {state:?}"
                );
            }
            RoomEvent::LocalTrackPublished { .. } => {
                // New cryptors start on slot 0; move to current epoch.
                voice_e2ee::sync_local_sender_key_index(state_for_room).await;
            }
            RoomEvent::ConnectionQualityChanged { quality, participant } => {
                let quality_str = match quality {
                    ConnectionQuality::Excellent => "excellent",
                    ConnectionQuality::Good => "good",
                    ConnectionQuality::Poor => "poor",
                    ConnectionQuality::Lost => "lost",
                };
                let (identity, _) = resolve_participant(
                    state_for_room,
                    logical_room,
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
    StreamEnd::Closed
}

/// Return the most recent `join_voice_channel` timing record, if any.
pub async fn get_last_join_timings(
    state: &Arc<AppState>,
) -> Result<Option<JoinTimings>> {
    let voice = state.voice.lock().await;
    let snapshot = voice.last_join_timings.lock().unwrap().clone();
    Ok(snapshot)
}

/// Tear down voice resources; returns the room for the caller to close.
/// Pass `abort_room_task = false` from inside the room task itself, and
/// `only_if = Some(room)` to release nothing unless that room is still the
/// live session (a stale task must not tear down its successor).
/// Idempotent.
async fn release_voice_resources(
    state: &Arc<AppState>,
    abort_room_task: bool,
    only_if: Option<&Arc<Room>>,
) -> Option<Arc<Room>> {
    // Don't hold the lock across room.close(): it hangs on a broken network.
    let (room, input_stream, output_stream) = {
        let mut voice = state.voice.lock().await;
        if let Some(expected) = only_if {
            let is_live = voice.room.as_ref().is_some_and(|r| Arc::ptr_eq(r, expected));
            if !is_live {
                return None;
            }
        }

        // Kill the frame feed first.
        if let Some(t) = voice.frame_task.take() { t.abort(); }
        if abort_room_task {
            if let Some(t) = voice.room_task.take() { t.abort(); }
        }

        // Taken out so it can drop on a blocking thread (see below).
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
        // Clear gate bits: stale `deafened` mutes next call.
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

    // Drop off-worker, else macOS's mic indicator can stick on.
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
    // A join still in flight stops at its next phase and gives back what it holds.
    {
        let voice = state.voice.lock().await;
        if voice.joining.load(Ordering::Acquire) {
            if let Some(cancel) = &voice.join_cancel {
                cancel.cancel();
            }
        }
    }

    // room_task abort skips on_room_disconnected; stop here.
    crate::commands::screenshare::stop_screen_share(state).await.ok();
    // Same for the webcam, which publishes into this room.
    crate::commands::camera::stop_camera(state).await.ok();

    let room = release_voice_resources(state, true, None).await;

    // Close outside the lock, with a timeout (broken networks hang).
    if let Some(room) = room {
        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            room.close(),
        ).await;
    }

    Ok(())
}

/// Toggle the local microphone mute. Returns the new muted state (true = muted).
pub async fn toggle_voice_mute(state: &Arc<AppState>) -> Result<VoiceGateState> {
    mutate_gate(state, |g| g.toggle_mute()).await
}

/// Toggle self-deafen (#849).
///
/// Deafen implies mute; undeafen restores the prior mute state.
pub async fn toggle_voice_deafen(state: &Arc<AppState>) -> Result<VoiceGateState> {
    mutate_gate(state, |g| g.toggle_deafen()).await
}

/// Mic gating mode: voice-activity (default) or push-to-talk.
///
/// Safe outside a call (persisted pref); always drops any held PTT latch.
pub async fn set_voice_input_mode(
    mode: VoiceInputMode,
    state: &Arc<AppState>,
) -> Result<VoiceGateState> {
    mutate_gate(state, |g| g.set_mode(mode)).await
}

/// Push-to-talk key down (`held: true`) / up (`held: false`).
pub async fn set_voice_ptt_held(held: bool, state: &Arc<AppState>) -> Result<VoiceGateState> {
    mutate_gate(state, |g| g.set_ptt_held(held)).await
}

/// Release the push-to-talk latch because the window lost keyboard focus.
///
/// The OS drops the keyup after focus moves; else the mic stays hot.
pub async fn release_voice_ptt(state: &Arc<AppState>) -> Result<VoiceGateState> {
    mutate_gate(state, |g| g.on_focus_lost()).await
}

/// Read the current gate without changing it.
pub async fn get_voice_gate_state(state: &Arc<AppState>) -> Result<VoiceGateState> {
    let voice = state.voice.lock().await;
    let gate = voice.gate.lock().unwrap();
    Ok(gate.snapshot())
}

/// Apply one transition to the gate, then republish it everywhere that
/// depends on it: the audio hot-path atomics and the LiveKit publication.
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
/// `user_id` is bare; `volume` is clamped to 0.0..=2.0. Live only.
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

/// Switch the microphone device mid-call; APM is rebuilt on a rate change.
pub async fn set_voice_input_device(
    device_name: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut voice = state.voice.lock().await;

    if voice.audio_source.is_none() {
        voice.current_input_device = Some(device_name);
        return Ok(());
    }

    // Copy out before unlocking: cpal init blocks (ALSA).
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

    // Reset RNNoise on device switch: its state is tied to the old mic.
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

/// Switch the speaker mid-call; track buffers survive the switch.
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

/// Update the live APM config without rejoining; no-op outside a session.
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
