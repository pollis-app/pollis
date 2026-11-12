use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, LazyLock,
};
use std::time::{Duration, Instant};

use libwebrtc::{
    audio_source::native::NativeAudioSource,
    prelude::{AudioSourceOptions, RtcAudioSource},
};
use livekit::{
    options::{AudioEncoding, TrackPublishOptions},
    prelude::*,
    track::{LocalAudioTrack, LocalTrack, RemoteTrack, TrackSource},
};
use tokio::sync::Notify;

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

use super::capture::{open_mic, spawn_capture_task, CaptureParams, CaptureTask, MicSource};
use super::devices::{get_device, warm_device_cache};
use super::gate::{TransmitGate, VoiceGateState, VoiceInputMode};
use super::join::{
    classify_connect_error, classify_ds_error, disconnect_is_retryable, reconnect_delay,
    JoinCancel, JoinClock, VoiceJoinError, VOICE_JOIN_DEADLINE, VOICE_RECONNECT_WINDOW,
};
use super::playback::{ensure_playback, register_remote_track, unregister_remote_track};
use super::resampler::MIX_RATE_HZ;
use super::types::{
    user_id_from_voice_identity, JoinTimings, SendableStream, VoiceEvent, VoiceState,
    VoiceWarmup, VOICE_WARMUP_TTL,
};

/// Opus cap for the mic; speech is transparent well below this and it keeps
/// a poor uplink from being asked for the 48 kbps MUSIC preset's worth.
const MIC_MAX_BITRATE: u64 = 64_000;

/// Publish options for the mic track. RED is requested but LiveKit forces it
/// off under E2EE (frame cryptor and redundancy don't compose), so loss
/// recovery is Opus in-band FEC + the DTX-friendly silent frames we send.
fn mic_publish_options() -> TrackPublishOptions {
    TrackPublishOptions {
        source: TrackSource::Microphone,
        audio_encoding: Some(AudioEncoding { max_bitrate: MIC_MAX_BITRATE }),
        dtx: true,
        ..Default::default()
    }
}

/// How long the join waits for the microphone before going on listen-only.
/// ALSA enumeration on a busy PipeWire box has been seen to take 4–8 s; the
/// join must not.
const MIC_OPEN_BUDGET: Duration = Duration::from_secs(6);

/// How long a join waits for a warmup already in flight for its channel before
/// minting its own token (the auto-join racing the route-entry warm).
const WARM_INFLIGHT_WAIT: Duration = Duration::from_millis(1500);

/// Mirror gate state onto the lock-free atomics the audio paths read.
fn publish_gate(voice: &VoiceState, gate: &TransmitGate) {
    voice
        .transmit_muted
        .store(!gate.transmitting(), Ordering::Relaxed);
    voice.deafened.store(gate.deafened(), Ordering::Relaxed);
    voice.self_muted.store(gate.self_muted(), Ordering::Relaxed);
}

/// Signal mic state to the room. Only the explicit mute (which deafen
/// implies) is a LiveKit mute; PTT-idle just sends silent frames, so a key
/// release costs no signalling round trip and no remote icon flicker.
fn apply_gate_to_room(voice: &VoiceState, gate: &TransmitGate) {
    // Hint APM when nothing we capture will be heard, so AGC/AEC don't adapt
    // to the gate.
    if let Some(apm) = &voice.apm {
        apm.handle().set_output_will_be_muted(!gate.transmitting());
    }

    let muted = gate.self_muted();
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

/// Warmups in flight, by channel, so a join that arrives mid-warm waits for that
/// token instead of minting a second one.
static WARM_INFLIGHT: LazyLock<std::sync::Mutex<HashMap<String, Arc<Notify>>>> =
    LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));

pub(super) fn warm_inflight(channel_id: &str) -> Option<Arc<Notify>> {
    WARM_INFLIGHT
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .get(channel_id)
        .cloned()
}

/// Marks `channel_id` as warming; the returned guard clears it and wakes waiters.
pub(super) fn begin_warm(channel_id: &str) -> Option<WarmGuard> {
    let mut map = WARM_INFLIGHT.lock().unwrap_or_else(|p| p.into_inner());
    if map.contains_key(channel_id) {
        return None;
    }
    let notify = Arc::new(Notify::new());
    map.insert(channel_id.to_string(), Arc::clone(&notify));
    Some(WarmGuard { channel_id: channel_id.to_string(), notify })
}

pub(super) struct WarmGuard {
    channel_id: String,
    notify: Arc<Notify>,
}

impl Drop for WarmGuard {
    fn drop(&mut self) {
        WARM_INFLIGHT
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.channel_id);
        self.notify.notify_waiters();
    }
}

/// Warmup before Join: pre-mint the token, prime DNS/TLS, and enumerate the
/// audio devices so the join opens the mic without an ALSA scan.
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
    let Some(_warming) = begin_warm(&channel_id) else {
        return Ok(());
    };
    warm_device_cache();

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
    room_task: Option<tokio::task::JoinHandle<()>>,
    mic_stream: Option<SendableStream>,
    frame_task: Option<CaptureTask>,
}

impl JoinPartial {
    async fn discard(self, state: &Arc<AppState>) {
        if let Some(t) = self.frame_task {
            t.abort();
        }
        if let Some(t) = self.room_task {
            t.abort();
        }
        if let Ok(mut slot) = state.voice.lock().await.denoiser.lock() {
            *slot = None;
        }
        if let Some(room) = self.room {
            // A room the task was already driving may be the one the state holds.
            let _ = release_voice_resources(state, false, Some(&room)).await;
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
    // The channel's group as the renderer knows it: saves the MLS-group resolve round trip.
    group_id: Option<String>,
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
        group_id,
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
    group_id: Option<String>,
}

/// What the room task needs to translate and attribute events for one session.
struct RoomContext {
    /// Logical room, to resolve room-scoped pseudonyms (#836).
    logical_room: String,
    /// The realtime room whose members watch this voice channel (the group, or the DM).
    presence_room: String,
    /// Self-hear filter: don't play this user's other devices (#140).
    local_user_id: String,
    local_identity: String,
    display_name: String,
    /// Internal identities of the remote participants the renderer currently shows.
    known_remote: Arc<std::sync::Mutex<HashSet<String>>>,
    /// Presence events run off the event loop but in arrival order.
    presence_lane: Arc<tokio::sync::Mutex<()>>,
    /// Raised once playback exists (or failed): `TrackSubscribed` must not register
    /// a track before the mixer, which would drop it on `ensure_playback`.
    playback_ready: Arc<Gate>,
}

/// A one-shot, many-waiter flag.
#[derive(Default)]
pub(super) struct Gate {
    open: AtomicBool,
    notify: Notify,
}

impl Gate {
    fn open(&self) {
        self.open.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    async fn wait(&self) {
        let notified = self.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.open.load(Ordering::Acquire) {
            return;
        }
        notified.await;
    }
}

/// The mic phase's outcome; listen-only when the device could not be opened in time.
struct MicOutcome {
    /// The cpal stream and the ring its callback fills; both `None` on listen-only.
    stream: Option<(SendableStream, MicSource)>,
    init_ms: u64,
}

impl MicOutcome {
    fn listen_only() -> Self {
        Self { stream: None, init_ms: 0 }
    }
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
        group_id,
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

    // A warm already running for this channel (route entry vs auto-join) is cheaper
    // to wait for than a second mint.
    if let Some(inflight) = warm_inflight(&channel_id) {
        let _ = clock
            .run("token", tokio::time::timeout(WARM_INFLIGHT_WAIT, inflight.notified()))
            .await?;
    }
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

    // Reset session gate bits; input mode (a pref) survives.
    {
        let voice = state.voice.lock().await;
        let mut gate = voice.gate.lock().unwrap();
        gate.reset_for_join();
        publish_gate(&voice, &gate);
    }

    // E2EE: the provider goes into `RoomOptions` now, its key lands before anything
    // is published; the MLS derivation runs alongside connect and the mic.
    let e2ee_options = voice_e2ee::build_unarmed_e2ee_options();
    let key_provider = e2ee_options.key_provider.clone();
    #[cfg(feature = "test-harness")]
    super::e2e_hooks::remember_join(&token, &url, &key_provider);
    let mut room_options = RoomOptions::default();
    room_options.encryption = Some(e2ee_options);
    // Subscriber-driven video: tiles report their size, the SFU sends only the layer that fits
    // and pauses what nobody shows; the publisher stops encoding layers nobody takes.
    (room_options.adaptive_stream, room_options.dynacast) = (true, true);

    let ctx = Arc::new(RoomContext {
        logical_room: channel_id.clone(),
        // Filled in once the derivation names the MLS group; see below.
        presence_room: String::new(),
        local_user_id: user_id.clone(),
        local_identity: local_identity.clone(),
        display_name: display_name.clone(),
        known_remote: Arc::new(std::sync::Mutex::new(HashSet::new())),
        presence_lane: Arc::new(tokio::sync::Mutex::new(())),
        playback_ready: Arc::new(Gate::default()),
    });

    crate::diag!("[voice] {channel_id}: connect ‖ mic+speaker ‖ MLS key, all at once");
    let connect_started = Instant::now();
    let connect_fut = {
        let state = Arc::clone(state);
        let ctx = Arc::clone(&ctx);
        async move {
            let r = Room::connect(&url, &token, room_options).await;
            let elapsed_ms = connect_started.elapsed().as_millis() as u64;
            // The event task starts the moment the room exists: already-present peers'
            // `TrackSubscribed` must not wait on the roster or the avatar lookup.
            let r = r.map(|(room, events)| {
                let room = Arc::new(room);
                let task = tokio::spawn(run_room_task(state, Arc::clone(&room), events, ctx));
                (room, task)
            });
            (r, elapsed_ms)
        }
    };

    let e2ee_start = Instant::now();
    let derive_fut = async {
        let r = voice_e2ee::derive_voice_key(
            state,
            &channel_id,
            &user_id,
            counterparty_user_id.as_deref(),
            group_id.as_deref(),
        )
        .await;
        (r, e2ee_start.elapsed().as_millis() as u64)
    };

    // Mic → APM → speaker, chained (A2 owns the bodies; this is only where they
    // are called from). The pipeline is 48 kHz whatever the devices do
    // (`capture.rs` and the mixer resample at the edges), so APM is always at a
    // supported rate and the speaker never waits on the mic's rate.
    let mic_started = Instant::now();
    let input_device_clone = input_device.clone();
    let audio_processing_for_mic = audio_processing.clone();
    let output_device_for_mic = output_device.clone();
    let voice_for_mic = Arc::clone(&state.voice);
    let ctx_for_mic = Arc::clone(&ctx);
    let mic_chain = async move {
        let opened = tokio::time::timeout(
            MIC_OPEN_BUDGET,
            tokio::task::spawn_blocking(move || {
                // Test seam: e2e listen-only path without unplugging a mic.
                if std::env::var("POLLIS_DISABLE_MIC").is_ok() {
                    return Err(anyhow::anyhow!("mic disabled via POLLIS_DISABLE_MIC").into());
                }
                let host = cpal::default_host();
                let dev = get_device(&host, input_device_clone.as_deref(), true)?;
                crate::diag!("[voice] mic device opened");
                open_mic(&dev)
            }),
        )
        .await;
        // Mic is best-effort: on failure (or a device that takes longer than the
        // budget) join listen-only. A late stream drops on its blocking thread.
        let mic = match opened {
            Ok(Ok(Ok((stream, source)))) => MicOutcome {
                stream: Some((stream, source)),
                init_ms: mic_started.elapsed().as_millis() as u64,
            },
            Ok(Ok(Err(e))) => {
                crate::diag!("[voice] mic unavailable — joining listen-only: {e}");
                MicOutcome::listen_only()
            }
            Ok(Err(e)) => {
                crate::diag!("[voice] mic init panicked — joining listen-only: {e}");
                MicOutcome::listen_only()
            }
            Err(_) => {
                crate::diag!(
                    "[voice] mic did not open within {}s — joining listen-only",
                    MIC_OPEN_BUDGET.as_secs()
                );
                MicOutcome::listen_only()
            }
        };
        let has_mic = mic.stream.is_some();

        // Skipped on listen-only; otherwise always at the fixed pipeline rate.
        let apm_stage = if has_mic {
            match voice_apm::ApmStage::new(MIX_RATE_HZ, audio_processing_for_mic.clone()) {
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

        let denoiser_arc = {
            let voice = voice_for_mic.lock().await;
            // The mixer's AEC render tap; cleared by `stop_all` on leave.
            voice.playback.lock().unwrap().shared.set_apm(apm_handle.clone());
            Arc::clone(&voice.denoiser)
        };
        {
            let mut slot = denoiser_arc.lock().unwrap();
            *slot = if has_mic && audio_processing_for_mic.click_suppression {
                crate::diag!("[voice/rnnoise] engaged @ {MIX_RATE_HZ} Hz");
                Some(voice_denoiser::DenoiserStage::new())
            } else {
                None
            };
        }

        // Open playback before any TrackSubscribed is processed (the gate below).
        let speaker_ok = match ensure_playback(Arc::clone(&voice_for_mic), output_device_for_mic).await {
            Ok(()) => true,
            Err(e) => {
                crate::diag!("[voice] playback init failed (speaker disabled this session): {e}");
                false
            }
        };
        ctx_for_mic.playback_ready.open();
        (mic, apm_stage, apm_handle, denoiser_arc, speaker_ok)
    };

    let ((connect_res, room_connect_ms), (mic, apm_stage, apm_handle, denoiser_arc, speaker_ok), (derive_res, e2ee_ms)) =
        match clock.run("connect", async { tokio::join!(connect_fut, mic_chain, derive_fut) }).await {
            Ok(all) => all,
            Err(e) => {
                // The gate opens on every path so a task already spawned never parks forever.
                ctx.playback_ready.open();
                partial.discard(state).await;
                return Err(e);
            }
        };
    let mic_init_ms = mic.init_ms;
    let (mic_stream, mic_source): (Option<SendableStream>, Option<MicSource>) = match mic.stream {
        Some((s, src)) => (Some(s), Some(src)),
        None => (None, None),
    };
    partial.mic_stream = mic_stream;
    let has_mic = partial.mic_stream.is_some();
    let mic_rate = mic_source.as_ref().map(|s| s.sample_rate).unwrap_or(MIX_RATE_HZ);

    let (room, room_task) = match connect_res {
        Ok(pair) => pair,
        Err(e) => {
            let err = classify_connect_error(&e);
            partial.discard(state).await;
            return Err(err);
        }
    };
    partial.room = Some(Arc::clone(&room));
    partial.room_task = Some(room_task);
    if has_mic {
        crate::diag!("[voice] connected to room {channel_id}, mic at {mic_rate} Hz");
    } else {
        crate::diag!("[voice] connected to room {channel_id}, listen-only (no mic)");
    }

    // Voice key from the MLS exporter; fail closed, never unencrypted.
    let (voice_key, voice_key_index, voice_epoch, voice_mls_group_id) = match derive_res {
        Ok(t) => t,
        Err(e) => {
            let err = classify_ds_error(&e);
            partial.discard(state).await;
            return Err(err);
        }
    };
    if !voice_e2ee::arm_key_provider(&key_provider, voice_key, voice_key_index) {
        partial.discard(state).await;
        return Err(VoiceJoinError::Internal { detail: "voice key install rejected".into() });
    }
    crate::diag!(
        "[voice] e2ee armed for {channel_id} (mls_group={voice_mls_group_id}, epoch={voice_epoch}, ring slot={voice_key_index})"
    );
    // The room task only needs this for a `ParticipantDisconnected` nudge, which it
    // can't see before the join is stored; set once, read after.
    let ctx = Arc::new(RoomContext {
        presence_room: voice_mls_group_id.clone(),
        ..RoomContext::clone_from(&ctx)
    });

    let mut audio_source_opt: Option<NativeAudioSource> = None;
    let mut local_track_opt: Option<LocalAudioTrack> = None;
    let mut first_publish_ms: u64 = 0;

    if let Some(mic_source) = mic_source {
        // libwebrtc's APM off: ours is the only mic processing stage. Queue
        // 0: we hand it exact 10 ms frames ourselves, so its 100 ms queue
        // would only add latency and pad silence into every dip.
        let audio_source = NativeAudioSource::new(
            AudioSourceOptions {
                echo_cancellation: false,
                noise_suppression: false,
                auto_gain_control: false,
            },
            MIX_RATE_HZ,
            1,
            0,
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
                room.local_participant()
                    .publish_track(LocalTrack::Audio(local_track.clone()), mic_publish_options()),
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

        let (transmit_muted, levels, sink) = {
            let voice = state.voice.lock().await;
            (
                Arc::clone(&voice.transmit_muted),
                Arc::clone(&voice.levels),
                voice.channel.clone(),
            )
        };
        let frame_task = match spawn_capture_task(CaptureParams {
            source: mic_source,
            audio_source: audio_source.clone(),
            apm: apm_handle.clone(),
            denoiser: Arc::clone(&denoiser_arc),
            transmit_muted,
            levels,
            sink,
            local_identity: local_identity.clone(),
        }) {
            Ok(t) => t,
            Err(e) => {
                let err = VoiceJoinError::Internal { detail: format!("mic DSP thread: {e}") };
                partial.discard(state).await;
                return Err(err);
            }
        };

        audio_source_opt = Some(audio_source);
        local_track_opt = Some(local_track);
        partial.frame_task = Some(frame_task);
    }

    // Seed roster: names now (one DS resolve), avatars after `Ok` (one more, off the join).
    let roster_start = Instant::now();
    let seeded = match clock.run("roster", roster_identities(state, &room, &channel_id)).await {
        Ok(s) => s,
        Err(e) => {
            partial.discard(state).await;
            return Err(e);
        }
    };
    let roster_ms = roster_start.elapsed().as_millis() as u64;
    {
        let mut known = ctx.known_remote.lock().unwrap();
        known.extend(seeded.iter().map(|p| p.identity.clone()));
    }

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

    // Store the session under ONE lock, checking the task is still driving a live
    // room: a `Disconnected` that raced the join must not be stored as a session.
    let mut voice = state.voice.lock().await;
    if clock_cancelled(&voice) {
        drop(voice);
        partial.discard(state).await;
        return Err(VoiceJoinError::Cancelled);
    }
    if partial.room_task.as_ref().is_some_and(|t| t.is_finished()) {
        drop(voice);
        partial.discard(state).await;
        return Err(VoiceJoinError::LivekitUnreachable { detail: "room closed during join".into() });
    }
    let JoinPartial { room: _, room_task, mic_stream, frame_task } = partial;
    if let Some(t) = voice.room_task.take() { t.abort(); }
    if let Some(t) = voice.frame_task.take() { t.abort(); }
    voice.room = Some(Arc::clone(&room));
    // All `None` on the listen-only path (no mic captured/published).
    voice.local_track = local_track_opt;
    voice.audio_source = audio_source_opt;
    voice.input_stream = mic_stream;
    voice.frame_task = frame_task;
    voice.room_task = room_task;
    voice.current_input_device = input_device;
    voice.apm = apm_stage;
    voice.e2ee_key_provider = Some(key_provider);
    voice.e2ee_mls_group_id = Some(voice_mls_group_id);
    voice.e2ee_epoch = voice_epoch;
    voice.e2ee_key_index = voice_key_index;
    *voice.last_join_timings.lock().unwrap() = Some(timings);

    // Publications start unmuted; sync so PTT joins aren't shown hot.
    {
        let gate = voice.gate.lock().unwrap();
        apply_gate_to_room(&voice, &gate);
    }

    let mut avatar_wanted: Vec<(String, String, bool)> = Vec::with_capacity(seeded.len() + 1);
    avatar_wanted.push((local_identity.clone(), display_name.clone(), false));
    if let Some(ch) = &voice.channel {
        for p in seeded {
            crate::diag!("[voice] existing participant: {} muted={}", p.identity, p.is_muted);
            avatar_wanted.push((p.identity.clone(), p.name.clone(), p.is_muted));
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
    drop(voice);

    // Avatars arrive as a second `participant_joined` per tile (an upsert in the
    // renderer), so the join never waits on them.
    tokio::spawn(deliver_avatars(Arc::clone(state), avatar_wanted));

    Ok(())
}

impl RoomContext {
    /// A copy sharing every live handle (gate, roster, lane); only plain fields are duplicated.
    fn clone_from(other: &Arc<RoomContext>) -> RoomContext {
        RoomContext {
            logical_room: other.logical_room.clone(),
            presence_room: other.presence_room.clone(),
            local_user_id: other.local_user_id.clone(),
            local_identity: other.local_identity.clone(),
            display_name: other.display_name.clone(),
            known_remote: Arc::clone(&other.known_remote),
            presence_lane: Arc::clone(&other.presence_lane),
            playback_ready: Arc::clone(&other.playback_ready),
        }
    }
}

/// One batched avatar lookup, then a `participant_joined` upsert per tile that has one.
async fn deliver_avatars(state: Arc<AppState>, wanted: Vec<(String, String, bool)>) {
    let identities: Vec<String> = wanted.iter().map(|(i, _, _)| i.clone()).collect();
    let avatars = lookup_avatar_urls_for_identities(&state, &identities).await;
    if avatars.is_empty() {
        return;
    }
    let voice = state.voice.lock().await;
    let Some(ch) = &voice.channel else { return };
    for (identity, name, is_muted) in wanted {
        if let Some(url) = avatars.get(&identity) {
            let _ = ch.send(VoiceEvent::ParticipantJoined {
                identity,
                name,
                is_muted,
                avatar_url: Some(url.clone()),
            });
        }
    }
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

/// Everyone in `room` right now, resolved (#836) in ONE batched DS call; no avatars.
async fn roster_identities(
    state: &Arc<AppState>,
    room: &Room,
    logical_room: &str,
) -> Vec<SeededParticipant> {
    let participants = room.remote_participants();
    let seen: Vec<String> = participants.keys().map(|k| k.to_string()).collect();
    let resolved = resolve_participants(state, logical_room, &seen).await;
    participants
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
            Some(SeededParticipant { identity, name, is_muted, avatar_url: None })
        })
        .collect()
}

/// Re-emit the roster after a link recovered: whoever vanished meanwhile leaves,
/// everyone present (re)joins. Both are idempotent for the renderer.
async fn reseed_roster(state: &Arc<AppState>, room: &Room, ctx: &RoomContext) {
    let snapshot = roster_identities(state, room, &ctx.logical_room).await;
    let present: HashSet<String> = snapshot.iter().map(|p| p.identity.clone()).collect();
    let gone: Vec<String> = {
        let mut known = ctx.known_remote.lock().unwrap();
        let gone = known.difference(&present).cloned().collect();
        *known = present;
        gone
    };
    for identity in &gone {
        crate::commands::screenshare::on_participant_left(identity, state).await;
    }
    let wanted: Vec<(String, String, bool)> = snapshot
        .iter()
        .map(|p| (p.identity.clone(), p.name.clone(), p.is_muted))
        .collect();
    {
        let voice = state.voice.lock().await;
        if let Some(ch) = &voice.channel {
            for identity in gone {
                let _ = ch.send(VoiceEvent::ParticipantLeft { identity });
            }
            for p in snapshot {
                let _ = ch.send(p.joined_event());
            }
        }
    }
    tokio::spawn(deliver_avatars(Arc::clone(state), wanted));
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
    // Nothing is registered before the mixer exists (it would be dropped by `ensure_playback`).
    ctx.playback_ready.wait().await;
    // The SDK's own resume/restart is attempt 1 of an episode; ours count on from there.
    let mut episode_attempt: u32 = 0;
    loop {
        let end = drive_room_events(&state, &room, &mut events, &ctx, &mut episode_attempt).await;
        if !is_current_room(&state, &room).await {
            // Left (or replaced) while we were draining; whoever did it released the state.
            // A join still in flight sees `is_finished()` and treats the room as dead.
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

/// The voice session's name in the SFU gate's prober election.
const SFU_GATE_VOICE: &str = "voice";

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
        // Same subscriber-driven video settings as the join (see `join_inner`).
        (options.adaptive_stream, options.dynacast) = (true, true);
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
                .publish_track(LocalTrack::Audio(track), mic_publish_options())
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

/// Drop every remote track's drain and ring; the speaker stream, the mixer and
/// its AEC tap stay up for the tracks the next room subscribes.
async fn clear_remote_audio_tracks(state: &Arc<AppState>) {
    let voice = state.voice.lock().await;
    voice.playback.lock().unwrap().stop_tracks();
    // Remote meters go with their tracks; the mic re-registers on its next frame.
    voice.levels.clear();
}

/// A participant came or went: resolve, attribute and tell the renderer, off the
/// event loop (two DS round trips) but in arrival order (the lane).
fn spawn_presence_event(state: &Arc<AppState>, ctx: &Arc<RoomContext>, p: RemoteParticipant, joined: bool) {
    let state = Arc::clone(state);
    let ctx = Arc::clone(ctx);
    tokio::spawn(async move {
        let _ordered = ctx.presence_lane.lock().await;
        let wire = p.identity().to_string();
        // Same filter as the seed roster — see `roster_identities`.
        if is_view_identity(&wire) {
            return;
        }
        // #836: translate before anything downstream sees it.
        let (identity, name) = resolve_participant(&state, &ctx.logical_room, &wire).await;
        if is_view_identity(&identity) {
            return;
        }
        if joined {
            crate::diag!("[voice] participant joined: {identity}");
            let is_muted = p.track_publications().values().any(|pub_| pub_.is_muted());
            let avatar_url = lookup_avatar_url_for_identity(&state, &identity).await;
            ctx.known_remote.lock().unwrap().insert(identity.clone());
            emit(&state, VoiceEvent::ParticipantJoined { identity, name, is_muted, avatar_url }).await;
        } else {
            crate::diag!("[voice] participant left: {identity}");
            ctx.known_remote.lock().unwrap().remove(&identity);
            // Clear their screenshare so a stale tile doesn't linger.
            crate::commands::screenshare::on_participant_left(&identity, &state).await;
            emit(&state, VoiceEvent::ParticipantLeft { identity }).await;
            // Observers outside the room only hear `voice_left` from a peer who left
            // cleanly; a crash is reported by whoever is still here. Idempotent: a refetch.
            crate::commands::livekit::publish_voice_roster_changed(&state, &ctx.presence_room, &ctx.logical_room).await;
        }
    });
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
    while let Some(event) = events.recv().await {
        match event {
            RoomEvent::ParticipantConnected(p) => {
                spawn_presence_event(state, ctx, p, true);
            }
            RoomEvent::ParticipantDisconnected(p) => {
                spawn_presence_event(state, ctx, p, false);
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
                        unregister_remote_track(&voice, &track_key);
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
                #[cfg(feature = "test-harness")]
                if matches!(participant, Participant::Remote(_)) {
                    super::e2e_hooks::record_frame_state(super::e2e_hooks::SCOPE_SESSION, &identity, &state);
                }
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
        let output_stream = voice.playback.lock().unwrap().stop_all();
        voice.levels.clear();

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

    // Close in the background, with a timeout (broken networks hang): the next
    // join starts now. The SFU evicts the stale participant on its own — the
    // per-room pseudonym is deterministic, so the rejoin is a DuplicateIdentity
    // for the old session, not for the new one.
    if let Some(room) = room {
        tokio::spawn(async move {
            let _ = tokio::time::timeout(Duration::from_secs(5), room.close()).await;
        });
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
    let mut pb = voice.playback.lock().unwrap();
    if (clamped - 1.0).abs() < f32::EPSILON {
        pb.user_volumes.remove(&user_id);
    } else {
        pb.user_volumes.insert(user_id.clone(), clamped);
    }
    pb.shared.set_user_gain(&user_id, clamped);
    Ok(())
}

/// Switch the microphone device mid-call.
///
/// The pipeline is 48 kHz regardless of the device, so APM and the AEC
/// render tap carry over untouched; only the cpal stream and the DSP thread
/// (which owns the per-device resampler) are replaced.
pub async fn set_voice_input_device(
    device_name: String,
    state: &Arc<AppState>,
) -> Result<()> {
    {
        let mut voice = state.voice.lock().await;
        if voice.audio_source.is_none() {
            voice.current_input_device = Some(device_name);
            return Ok(());
        }
    }

    // cpal init blocks (ALSA): off the worker and outside the voice lock.
    let device_name_clone = device_name.clone();
    let (new_mic, new_source) = tokio::task::spawn_blocking(move || {
        let host = cpal::default_host();
        let device = get_device(&host, Some(&device_name_clone), true)?;
        open_mic(&device)
    })
    .await
    .map_err(|e| anyhow::anyhow!("audio init panicked: {e}"))??;

    let mut voice = state.voice.lock().await;
    // Leave raced the open: nothing to attach to.
    let Some(audio_source) = voice.audio_source.clone() else {
        drop(voice);
        let _ = tokio::task::spawn_blocking(move || drop(new_mic)).await;
        return Err(anyhow::anyhow!("not in a voice channel").into());
    };

    // Reset RNNoise on device switch: its state is tied to the old mic.
    let want_denoiser = voice
        .apm
        .as_ref()
        .map(|a| a.config().click_suppression)
        .unwrap_or(false);
    {
        let mut slot = voice.denoiser.lock().unwrap();
        *slot = want_denoiser.then(voice_denoiser::DenoiserStage::new);
    }

    let Some(previous) = voice.frame_task.take() else {
        drop(voice);
        let _ = tokio::task::spawn_blocking(move || drop(new_mic)).await;
        return Err(anyhow::anyhow!("no mic session to switch").into());
    };
    let local_identity = previous.local_identity().to_string();
    previous.abort();
    let task = spawn_capture_task(CaptureParams {
        source: new_source,
        audio_source,
        apm: voice.apm.as_ref().map(|a| a.handle()),
        denoiser: Arc::clone(&voice.denoiser),
        transmit_muted: Arc::clone(&voice.transmit_muted),
        levels: Arc::clone(&voice.levels),
        sink: voice.channel.clone(),
        local_identity,
    })?;
    voice.frame_task = Some(task);
    let old_mic = voice.input_stream.replace(new_mic);
    voice.current_input_device = Some(device_name);
    drop(voice);

    // ALSA close blocks and macOS's mic indicator sticks if dropped on a
    // worker; never under the voice lock.
    if old_mic.is_some() {
        let _ = tokio::task::spawn_blocking(move || drop(old_mic)).await;
    }
    Ok(())
}

/// Switch the speaker mid-call; tracks, rings and the AEC tap survive.
pub async fn set_voice_output_device(
    device_name: String,
    state: &Arc<AppState>,
) -> Result<()> {
    ensure_playback(Arc::clone(&state.voice), Some(device_name)).await
}

/// Update the live APM config without rejoining; no-op outside a session.
pub async fn set_voice_audio_processing(
    config: voice_apm::ApmConfig,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut voice = state.voice.lock().await;

    let in_session = voice.apm.is_some();
    if let Some(stage) = voice.apm.as_mut() {
        stage.set_config(config.clone());
    }

    if in_session {
        let mut slot = voice.denoiser.lock().unwrap();
        let want = config.click_suppression;
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
