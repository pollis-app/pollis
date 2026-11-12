//! Test-only voice E2EE observation for the desktop e2e (#1850).
//!
//! Compiled only with the `test-harness` feature; the shell registers the
//! commands only under its `e2e-hooks` feature, which refuses to build without
//! `debug_assertions`, so none of this exists in a release app.
//!
//! What it gives `e2e/voice-removal-frame-keys.js`:
//! - a snapshot of this client's ring: the key its senders use now and, for the
//!   key provider of the most recent join (kept here after the session ends),
//!   a fingerprint per ring slot;
//! - every frame-cryptor verdict (`RoomEvent::E2eeStateChanged`) per scope;
//! - an adversary re-entry: the removed member dials the SFU again with the
//!   token and key provider it held at join time, so the test asserts on the
//!   crypto with the DS's eviction (#1102) out of the picture (#1097).
//!
//! Keys never leave as bytes: only a truncated SHA-256 fingerprint.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use futures_util::StreamExt;
use libwebrtc::audio_stream::native::NativeAudioStream;
use livekit::e2ee::key_provider::KeyProvider;
use livekit::prelude::*;
use livekit::track::RemoteTrack;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::commands::voice_key_ring::VOICE_KEY_RING_SIZE;
use crate::error::{Error, Result};
use crate::state::AppState;

/// Scope of a verdict recorded from the app's own voice session.
pub const SCOPE_SESSION: &str = "session";
/// Scope of a verdict recorded from the adversary re-entry.
pub const SCOPE_ADVERSARY: &str = "adversary";

/// One frame-cryptor verdict for one remote participant.
#[derive(Clone, Debug, Serialize)]
pub struct FrameStateRecord {
    pub scope: String,
    pub identity: String,
    pub state: String,
    pub at_ms: u64,
}

/// What the most recent join held: enough to dial the same room again.
struct JoinCapture {
    token: String,
    url: String,
    key_provider: KeyProvider,
}

static FRAME_STATES: Mutex<Vec<FrameStateRecord>> = Mutex::new(Vec::new());
static LAST_JOIN: Mutex<Option<JoinCapture>> = Mutex::new(None);
static ADVERSARY: Mutex<Option<Arc<Room>>> = Mutex::new(None);

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn fingerprint(key: &[u8]) -> String {
    hex::encode(&Sha256::digest(key)[..8])
}

/// Called by the join with the token it dials and the provider it hands the room.
pub fn remember_join(token: &str, url: &str, key_provider: &KeyProvider) {
    *LAST_JOIN.lock().unwrap() = Some(JoinCapture {
        token: token.to_string(),
        url: url.to_string(),
        key_provider: key_provider.clone(),
    });
}

/// Called for every `E2eeStateChanged` about a REMOTE participant.
pub fn record_frame_state(scope: &str, identity: &str, state: &impl std::fmt::Debug) {
    FRAME_STATES.lock().unwrap().push(FrameStateRecord {
        scope: scope.to_string(),
        identity: identity.to_string(),
        state: format!("{state:?}"),
        at_ms: now_ms(),
    });
}

/// This client's voice E2EE state, as fingerprints.
#[derive(Debug, Serialize)]
pub struct VoiceE2eeSnapshot {
    pub user_id: Option<String>,
    /// A live voice session holds a key provider right now.
    pub in_call: bool,
    pub mls_group_id: Option<String>,
    /// Epoch of the key the session's senders use (0 when not in a call).
    pub voice_epoch: u64,
    /// Ring slot the session's senders use.
    pub sender_key_index: i32,
    /// Fingerprint of the key in the sender slot of the live provider.
    pub sender_key_fp: Option<String>,
    /// The local MLS group's epoch now, and the frame key exported from it.
    pub mls_epoch: Option<u64>,
    pub mls_epoch_key_fp: Option<String>,
    /// Per ring slot, the fingerprint of what the most recent join's provider
    /// holds (it outlives the session; empty if this process never joined).
    pub last_join_ring: Vec<Option<String>>,
    pub frame_states: Vec<FrameStateRecord>,
}

pub async fn voice_e2ee_snapshot(state: &Arc<AppState>) -> Result<VoiceE2eeSnapshot> {
    let user_id = crate::commands::mls::ds_client::current_user_id(state).await.ok();
    let (provider, mls_group_id, voice_epoch, sender_key_index) = {
        let voice = state.voice.lock().await;
        (
            voice.e2ee_key_provider.clone(),
            voice.e2ee_mls_group_id.clone(),
            voice.e2ee_epoch,
            voice.e2ee_key_index,
        )
    };
    let sender_key_fp = provider
        .as_ref()
        .and_then(|kp| kp.get_shared_key(sender_key_index))
        .map(|k| fingerprint(&k));

    let (mls_epoch, mls_epoch_key_fp) = match mls_group_id.as_deref() {
        Some(group) => {
            match crate::commands::voice_e2ee::derive_voice_key_for_group(state, group).await {
                Ok((key, _, epoch)) => (Some(epoch), Some(fingerprint(&key))),
                Err(_) => (None, None),
            }
        }
        None => (None, None),
    };

    let last_join_ring = match LAST_JOIN.lock().unwrap().as_ref() {
        Some(capture) => (0..VOICE_KEY_RING_SIZE)
            .map(|slot| capture.key_provider.get_shared_key(slot).map(|k| fingerprint(&k)))
            .collect(),
        None => Vec::new(),
    };

    Ok(VoiceE2eeSnapshot {
        user_id,
        in_call: provider.is_some(),
        mls_group_id,
        voice_epoch,
        sender_key_index,
        sender_key_fp,
        mls_epoch,
        mls_epoch_key_fp,
        last_join_ring,
        frame_states: FRAME_STATES.lock().unwrap().clone(),
    })
}

/// Dial the SFU again with the most recent join's token and key provider,
/// subscribe to everything, and record each remote track's verdict under
/// [`SCOPE_ADVERSARY`]. Refuses while a voice session is live (same identity).
pub async fn voice_e2ee_adversary_reenter(state: &Arc<AppState>) -> Result<()> {
    if state.voice.lock().await.e2ee_key_provider.is_some() {
        return Err(Error::Other(anyhow::anyhow!(
            "adversary re-entry needs the voice session to have ended"
        )));
    }
    let (token, url, key_provider) = {
        let guard = LAST_JOIN.lock().unwrap();
        let capture = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("no voice join to re-enter with")))?;
        (capture.token.clone(), capture.url.clone(), capture.key_provider.clone())
    };
    let mut options = RoomOptions::default();
    options.auto_subscribe = true;
    options.encryption = Some(livekit::e2ee::E2eeOptions {
        encryption_type: livekit::e2ee::EncryptionType::Gcm,
        key_provider,
    });
    let (room, mut events) = Room::connect(&url, &token, options)
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("adversary connect: {e}")))?;
    crate::diag!("[voice-e2e-hooks] adversary re-entered the room with its join-time token and keys");
    let room = Arc::new(room);
    *ADVERSARY.lock().unwrap() = Some(Arc::clone(&room));

    tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                // Pull the audio so the receive path runs exactly as a listener's would.
                RoomEvent::TrackSubscribed { track: RemoteTrack::Audio(audio), .. } => {
                    let mut stream = NativeAudioStream::new(audio.rtc_track(), 48_000, 1);
                    tokio::spawn(async move { while stream.next().await.is_some() {} });
                }
                RoomEvent::E2eeStateChanged { participant: Participant::Remote(p), state } => {
                    let identity = p.identity().to_string();
                    crate::diag!("[voice-e2e-hooks] adversary frame state for {identity}: {state:?}");
                    record_frame_state(SCOPE_ADVERSARY, &identity, &state);
                }
                RoomEvent::Disconnected { reason } => {
                    crate::diag!("[voice-e2e-hooks] adversary disconnected: {reason:?}");
                    return;
                }
                _ => {}
            }
        }
    });
    Ok(())
}

/// Close the adversary room, if any.
pub async fn voice_e2ee_adversary_leave() -> Result<()> {
    let room = ADVERSARY.lock().unwrap().take();
    if let Some(room) = room {
        let _ = room.close().await;
    }
    Ok(())
}
