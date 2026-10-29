//! End-to-end encryption for voice channels.
//!
//! libwebrtc `FrameCryptor` (AES-GCM) with an MLS-exported key; the SFU sees ciphertext.
//! Key ring: epoch `e` lives in slot `e % 16`; local sender cryptors (born on
//! slot 0, never moved by the SDK) are re-pointed on rotation and publish.
//! The old slot survives `VOICE_KEY_SLOT_GRACE_SECS`, then gets random bytes
//! so an ex-member can't inject frames under it.

use std::sync::Arc;

use livekit::e2ee::{
    key_provider::{KeyProvider, KeyProviderOptions},
    E2eeOptions, EncryptionType,
};
use livekit::prelude::Room;
use openmls::prelude::*;
use openmls_traits::OpenMlsProvider;
use rand::rngs::OsRng;
use rand::RngCore;

use crate::commands::mls::MlsProvider;
use crate::commands::voice_key_ring::{
    slot_reused_since, voice_key_index, VOICE_KEY_RING_SIZE, VOICE_KEY_SLOT_GRACE_SECS,
};
use crate::error::{Error, Result};
use crate::state::AppState;

const VOICE_KEY_LABEL: &str = "pollis/voice/v1";
const VOICE_KEY_LEN: usize = 32;

/// Resolves a voice room's channel/conversation id to the MLS group id whose
/// exporter secret backs the voice key.
///
/// Group channel → parent group; DM → itself; `call-*` → the two users' 1:1 DM.
async fn resolve_mls_group_id(
    state: &Arc<AppState>,
    channel_id: &str,
    self_user_id: &str,
    counterparty_user_id: Option<&str>,
) -> Result<String> {
    if channel_id.starts_with("call-") {
        let other = counterparty_user_id.ok_or_else(|| {
            Error::Other(anyhow::anyhow!(
                "voice call room {channel_id} requires a counterparty user id"
            ))
        })?;
        // 1:1 DM only (DS checks `COUNT(*) = 2`): a group DM would leak the key.
        return match crate::commands::ds_reads::one_to_one_dm(state, self_user_id, other).await? {
            Some(id) => Ok(id),
            None => Err(Error::Other(anyhow::anyhow!(
                "no 1:1 DM channel exists between {self_user_id} and {other} \
                 — cannot derive voice key for call {channel_id}"
            ))),
        };
    }

    // A channel resolves to its group; a DM id is its own MLS group.
    crate::commands::mls::ds_reads::resolve_mls_group(state, channel_id).await
}

/// Join-time entry: returns `(key, key_index, epoch, mls_group_id)`.
///
/// `counterparty_user_id` is required for `call-*` rooms and ignored for
/// group channels / DMs.
pub async fn derive_voice_key(
    state: &Arc<AppState>,
    channel_id: &str,
    self_user_id: &str,
    counterparty_user_id: Option<&str>,
) -> Result<(Vec<u8>, i32, u64, String)> {
    let mls_group_id =
        resolve_mls_group_id(state, channel_id, self_user_id, counterparty_user_id).await?;
    // Catch up MLS first, else peers on different epochs derive different keys.
    // Runs concurrently with the remote-epoch read, which can only be stale.
    let (_, remote_epoch) = tokio::join!(
        catch_up_mls_group(state, &mls_group_id, self_user_id),
        published_group_epoch(state, &mls_group_id),
    );
    let (mut key, mut idx, mut epoch) = derive_voice_key_for_group(state, &mls_group_id).await?;
    crate::diag!("[voice-e2ee] catch-up: {mls_group_id} now at epoch {epoch}");

    // Still behind the published epoch: the commits are gone (#371), so delete
    // the stranded group and external-join (it refuses to clobber one, #1220).
    if let Some(remote_epoch) = remote_epoch {
        if epoch < remote_epoch {
            crate::diag!(
                "[voice-e2ee] catch-up: local epoch {epoch} < published {remote_epoch} for {mls_group_id} — external-join recovery"
            );
            if let Err(e) =
                crate::commands::mls::forget_local_mls_group(state, &mls_group_id).await
            {
                crate::diag!(
                    "[voice-e2ee] external-join recovery: could not drop stranded group {mls_group_id}: {e}"
                );
            }
            match crate::commands::mls::external_join_group(
                state,
                &mls_group_id,
                self_user_id,
            )
            .await
            {
                Ok(()) => {
                    let (k2, i2, e2) =
                        derive_voice_key_for_group(state, &mls_group_id).await?;
                    crate::diag!(
                        "[voice-e2ee] external-join recovery: {mls_group_id} epoch {epoch} → {e2}"
                    );
                    key = k2;
                    idx = i2;
                    epoch = e2;
                }
                Err(e) => {
                    crate::diag!(
                        "[voice-e2ee] external-join recovery failed for {mls_group_id}: {e}"
                    );
                }
            }
        }
    }

    Ok((key, idx, epoch, mls_group_id))
}

/// The highest epoch published in `mls_group_info` for this MLS group.
///
/// `None` on any failure: this gates a DESTRUCTIVE recovery, so "can't tell" = "don't".
///
/// Known limitation: compares `epoch` without `generation` (post-migration miss).
async fn published_group_epoch(state: &Arc<AppState>, mls_group_id: &str) -> Option<u64> {
    let snap = crate::commands::mls::ds_reads::conversation_snapshot(
        state,
        crate::commands::mls::ds_reads::state_query(mls_group_id, None),
    )
    .await
    .ok()?;
    snap.group_info.map(|gi| gi.epoch as u64)
}

/// Pull and process any unread envelopes for every conversation backed by this
/// MLS group so the local epoch matches whoever sent the most recent commit.
/// Best-effort; uses the shared interleaved catch-up (#987).
async fn catch_up_mls_group(state: &Arc<AppState>, mls_group_id: &str, user_id: &str) {
    if let Err(e) =
        crate::commands::messages::catch_up_mls_group_interleaved(state, mls_group_id, user_id)
            .await
    {
        crate::diag!("[voice-e2ee] catch-up for {mls_group_id}: {e}");
    }
}

/// The derived key, serialized for the renderer's screen-share view client.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct E2eeKeyInfo {
    pub key: Vec<u8>,
    pub key_index: i32,
    pub epoch: u64,
    pub mls_group_id: String,
}

/// Renderer entry: the same raw MLS export; both SDKs HKDF it identically.
pub async fn get_voice_e2ee_key(
    channel_id: String,
    user_id: String,
    counterparty_user_id: Option<String>,
    state: &Arc<AppState>,
) -> Result<E2eeKeyInfo> {
    let (key, key_index, epoch, mls_group_id) = derive_voice_key(
        state,
        &channel_id,
        &user_id,
        counterparty_user_id.as_deref(),
    )
    .await?;
    Ok(E2eeKeyInfo {
        key,
        key_index,
        epoch,
        mls_group_id,
    })
}

async fn derive_voice_key_for_group(
    state: &Arc<AppState>,
    mls_group_id: &str,
) -> Result<(Vec<u8>, i32, u64)> {
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    // The voice key is exported from the group this device currently reads and
    // writes, which after a suite migration is the successor lineage (#454 P4).
    let generation =
        crate::commands::mls::generation::local_generation(db.conn(), mls_group_id);
    let provider = crate::commands::mls::PollisProvider::new(db.conn());
    export_voice_key(&provider, mls_group_id, generation)
}

/// Export the LiveKit frame key from the group's exporter secret. Suite-generic
/// half of [`derive_voice_key_for_group`].
fn export_voice_key<C>(
    provider: &MlsProvider<'_, C>,
    mls_group_id: &str,
    generation: i64,
) -> Result<(Vec<u8>, i32, u64)>
where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let group_id = crate::commands::mls::generation::mls_group_id(mls_group_id, generation);

    let group = MlsGroup::load(provider.storage(), &group_id)
        .map_err(|e| Error::Other(anyhow::anyhow!("mls load: {e}")))?
        .ok_or_else(|| {
            Error::Other(anyhow::anyhow!(
                "MLS group not found for voice channel {mls_group_id}"
            ))
        })?;

    let epoch = group.epoch().as_u64();
    let context = epoch.to_be_bytes();
    let key = group
        .export_secret(provider.crypto(), VOICE_KEY_LABEL, &context, VOICE_KEY_LEN)
        .map_err(|e| Error::Other(anyhow::anyhow!("mls export_secret: {e}")))?;

    let key_index = voice_key_index(epoch);
    Ok((key, key_index, epoch))
}

/// LiveKit `E2eeOptions` with the join key in slot `key_index`; JS-compatible defaults.
///
/// `with_shared_key` seeds slot 0; scrub it unless that's the join slot.
pub fn build_e2ee_options(key: Vec<u8>, key_index: i32) -> E2eeOptions {
    let options = KeyProviderOptions {
        key_ring_size: VOICE_KEY_RING_SIZE,
        ..KeyProviderOptions::default()
    };
    let kp = KeyProvider::with_shared_key(options, key.clone());
    if key_index != 0 {
        // Scrub first so `latest_key_index` ends on the live slot.
        scrub_slot(&kp, 0);
        install_shared_key(&kp, key, key_index);
    }
    E2eeOptions {
        encryption_type: EncryptionType::Gcm,
        key_provider: kp,
    }
}

/// Install `key` in slot `key_index`; read-back is the only failure signal.
fn install_shared_key(kp: &KeyProvider, key: Vec<u8>, key_index: i32) -> bool {
    kp.set_shared_key(key, key_index);
    let landed = kp.get_shared_key(key_index).is_some();
    if !landed {
        crate::diag!(
            "[voice-e2ee] key install at ring slot {key_index} was rejected (ring size {VOICE_KEY_RING_SIZE}) — frames at this index will not decrypt"
        );
    }
    landed
}

/// Overwrite slot `key_index` with random bytes (libwebrtc can't remove keys).
///
/// Also moves `latest_key_index`; harmless, voice publishes no data packets.
fn scrub_slot(kp: &KeyProvider, key_index: i32) {
    let mut junk = vec![0u8; VOICE_KEY_LEN];
    OsRng.fill_bytes(&mut junk);
    kp.set_shared_key(junk, key_index);
}

/// Point every LOCAL sender `FrameCryptor` in `room` at ring slot
/// `key_index`. Returns how many were updated.
///
/// Receivers read the slot from the trailer; senders never leave slot 0 alone.
pub fn apply_sender_key_index(room: &Room, key_index: i32) -> usize {
    let local = room.local_participant().identity();
    let mut updated = 0;
    for ((identity, _sid), cryptor) in room.e2ee_manager().frame_cryptors() {
        if identity != local {
            continue;
        }
        cryptor.set_key_index(key_index);
        updated += 1;
    }
    updated
}

/// Re-point local sender cryptors on `LocalTrackPublished` (new ones start on slot 0).
pub async fn sync_local_sender_key_index(state: &Arc<AppState>) {
    let voice = state.voice.lock().await;
    let Some(room) = voice.room.as_ref() else {
        return;
    };
    if voice.e2ee_key_provider.is_none() {
        return;
    }
    let n = apply_sender_key_index(room, voice.e2ee_key_index);
    crate::diag!(
        "[voice-e2ee] local track published: {n} sender cryptor(s) on ring slot {}",
        voice.e2ee_key_index
    );
}

/// After a commit on the active voice group: install the new epoch's key and
/// re-point senders; the old slot is scrubbed after the grace period.
pub async fn on_mls_epoch_changed(state: &Arc<AppState>, mls_group_id: &str) {
    let (provider, prev_epoch) = {
        let voice = state.voice.lock().await;
        match (
            voice.e2ee_key_provider.clone(),
            voice.e2ee_mls_group_id.as_deref(),
            voice.e2ee_epoch,
        ) {
            (Some(kp), Some(active), prev) if active == mls_group_id => (kp, prev),
            _ => return,
        }
    };

    let (key, key_index, epoch) = match derive_voice_key_for_group(state, mls_group_id).await {
        Ok(t) => t,
        Err(e) => {
            crate::diag!("[voice-e2ee] re-derive failed for {mls_group_id}: {e}");
            return;
        }
    };

    if epoch == prev_epoch {
        return;
    }

    if !install_shared_key(&provider, key.clone(), key_index) {
        return;
    }

    let (channel, senders_moved) = {
        let mut voice = state.voice.lock().await;
        // A rotation for a group this state no longer backs (left and rejoined
        // elsewhere while the derive above ran) must not be recorded on it.
        if voice.e2ee_mls_group_id.as_deref() != Some(mls_group_id) {
            return;
        }
        voice.e2ee_epoch = epoch;
        voice.e2ee_key_index = key_index;
        let moved = voice
            .room
            .as_ref()
            .map(|room| apply_sender_key_index(room, key_index))
            .unwrap_or(0);
        (voice.channel.clone(), moved)
    };

    // Retire the previous epoch's slot once nothing legitimate can still be
    // using it. One-shot delay, not a poll: it fires once per rotation.
    let prev_index = voice_key_index(prev_epoch);
    if prev_index != key_index {
        let scrub_state = Arc::clone(state);
        let scrub_group = mls_group_id.to_string();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(VOICE_KEY_SLOT_GRACE_SECS)).await;
            let voice = scrub_state.voice.lock().await;
            // Skip if the call ended/moved, or a newer epoch re-occupied the slot.
            if voice.e2ee_mls_group_id.as_deref() != Some(scrub_group.as_str()) {
                return;
            }
            if voice.e2ee_epoch < epoch || slot_reused_since(prev_index, epoch, voice.e2ee_epoch) {
                return;
            }
            scrub_slot(&provider, prev_index);
            crate::diag!(
                "[voice-e2ee] retired ring slot {prev_index} (epoch {prev_epoch}) after grace period"
            );
        });
    }

    // Let the renderer's view-client key provider rotate too.
    if let Some(ch) = channel {
        let _ = ch.send(crate::commands::voice::VoiceEvent::VoiceE2eeKeyRotated {
            key,
            key_index,
            epoch,
            mls_group_id: mls_group_id.to_string(),
        });
    }

    crate::diag!(
        "[voice-e2ee] rotated key for {mls_group_id} to epoch {epoch} (ring slot {key_index}, {senders_moved} sender cryptor(s) moved)"
    );
}
