//! End-to-end encryption for voice channels.
//!
//! LiveKit is an SFU: every audio frame goes through the server. To keep the
//! server from being able to listen, frames are encrypted post-Opus/pre-SRTP
//! by libwebrtc's native `FrameCryptor` (AES-GCM), driven by a shared
//! symmetric key derived from the channel's MLS exporter secret. The server
//! routes ciphertext; only current MLS members of the group can decrypt it.
//!
//! Key handoff:
//!   - Each voice channel resolves to its parent MLS group (same group the
//!     channel's messages use).
//!   - Both peers derive the same 32-byte key via
//!     `MlsGroup::export_secret("pollis/voice/v1", epoch_be_bytes, 32)`.
//!   - On every MLS epoch advance, `on_mls_epoch_changed` re-derives and
//!     rotates the key in the live `KeyProvider` without reconnecting.

use std::sync::Arc;

use livekit::e2ee::{
    key_provider::{KeyProvider, KeyProviderOptions},
    E2eeOptions, EncryptionType,
};
use openmls::prelude::*;
use openmls_traits::OpenMlsProvider;

use crate::commands::mls::MlsProvider;
use crate::error::{Error, Result};
use crate::state::AppState;

const VOICE_KEY_LABEL: &str = "pollis/voice/v1";
const VOICE_KEY_LEN: usize = 32;

/// Resolves a voice room's channel/conversation id to the MLS group id whose
/// exporter secret backs the voice key.
///
/// Three room shapes are recognised:
///   1. **Group channels** (`channels` table row) — use the parent group's
///      MLS group, same as `messages::send_message`.
///   2. **DM channels** (no `channels` row, no `call-` prefix) — the channel
///      id IS the MLS group id.
///   3. **1:1 calls** (`call-<ulid>` ephemeral rooms) — minted on the fly by
///      `start_call`, no DB row of their own. Resolve to the MLS group of
///      the DM channel between the two participants. Requires
///      `counterparty_user_id`; both sides know the other party (caller from
///      `start_call`, callee from the `call_invite` payload).
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
        // The 1:1 DM channel between the two users. The DS guards it with
        // `HAVING COUNT(*) = 2` so a group DM containing both cannot match —
        // exporting a call key from a three-person group's tree would hand it to
        // the third party.
        return match crate::commands::ds_reads::one_to_one_dm(state, self_user_id, other).await? {
            Some(id) => Ok(id),
            None => Err(Error::Other(anyhow::anyhow!(
                "no 1:1 DM channel exists between {self_user_id} and {other} \
                 — cannot derive voice key for call {channel_id}"
            ))),
        };
    }

    // A channel resolves to its owning group; anything else (a DM id) is its own
    // MLS group.
    crate::commands::mls::ds_reads::resolve_mls_group(state, channel_id).await
}

/// Public entry point used at voice-join time. Returns
/// `(key, key_index, epoch, mls_group_id)`. Caller stashes `mls_group_id` and
/// `epoch` on `VoiceState` so the epoch-rotation hook can match on group id
/// and skip duplicate work.
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
    // Catch up local MLS state before exporting the key. Without this, the
    // two peers race: whichever one joined voice first holds a stale epoch
    // and derives a key the other peer can't decrypt — black tile on the
    // receiver, "InvalidKey: Decryption failed" in livekit-client.
    //
    // `process_pending_commits` alone isn't enough — commits live in
    // `message_envelope` rows on Turso that this client may not have
    // pulled yet. We have to do a real ingest of the conversations
    // backing this MLS group so any unprocessed commits get applied.
    let epoch_before = derive_voice_key_for_group(state, &mls_group_id)
        .await
        .map(|(_, _, e)| e)
        .unwrap_or(u64::MAX);
    catch_up_mls_group(state, &mls_group_id, self_user_id).await;
    let (mut key, mut idx, mut epoch) = derive_voice_key_for_group(state, &mls_group_id).await?;
    eprintln!(
        "[voice-e2ee] catch-up: {mls_group_id} epoch {epoch_before} → {epoch}"
    );

    // If the catch-up couldn't bring us level with the published GroupInfo,
    // we're stranded behind commits that are no longer in `mls_commit_log`
    // (e.g. the rejection-and-delete path in `process_pending_commits_locked`
    // wiped them — see issue #371). External-join rebuilds local state at
    // the current epoch from the published GroupInfo, which is how `process_
    // pending_commits` already recovers when there's no local group at all.
    if let Some(remote_epoch) =
        published_group_epoch(state, &mls_group_id).await
    {
        if epoch < remote_epoch {
            eprintln!(
                "[voice-e2ee] catch-up: local epoch {epoch} < published {remote_epoch} for {mls_group_id} — external-join recovery"
            );
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
                    eprintln!(
                        "[voice-e2ee] external-join recovery: {mls_group_id} epoch {epoch} → {e2}"
                    );
                    key = k2;
                    idx = i2;
                    epoch = e2;
                }
                Err(e) => {
                    eprintln!(
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
/// `None` on any failure — "can't tell, don't trigger recovery" — so a transient
/// blip never forces an unnecessary external-join. That bias is the opposite of
/// `group_state::published_group_info_head`'s, deliberately: this one gates a
/// DESTRUCTIVE recovery, that one gates a redundant republish.
///
/// Known limitation, out of scope for #987 and tracked separately: this compares
/// `epoch` WITHOUT `generation`. After a suite migration the successor lineage's
/// numerically-lower epoch reads as "not stale" and recovery silently never
/// fires — the same bug class `group_info_is_stale` already fixes for the
/// republish path.
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
/// Best-effort: failures are logged and ignored so a transient blip doesn't block
/// the voice join.
///
/// #987 folded this into the ONE catch-up entry point. It used to run its own
/// copy of the resolve → is-DM → channel-enumerate chain and then ingest each
/// conversation, which is exactly what `catch_up_mls_group_interleaved` does —
/// except that this copy did NOT interleave, so a voice join could advance the
/// shared group past an epoch at which a text channel held an un-ingested
/// message and (with `max_past_epochs = 0`) discard its keys.
///
/// It also carried a diagnostic that dumped up to 20 `mls_commit_log` rows on
/// every voice join, with a silent fall-back to the MAIN database if the log DB
/// connection failed — a fall-back that would have quietly queried a database
/// with no such table. Both are gone.
async fn catch_up_mls_group(state: &Arc<AppState>, mls_group_id: &str, user_id: &str) {
    if let Err(e) =
        crate::commands::messages::catch_up_mls_group_interleaved(state, mls_group_id, user_id)
            .await
    {
        eprintln!("[voice-e2ee] catch-up for {mls_group_id}: {e}");
    }
}

/// JSON-serializable wrapper around the derived key for the napi command.
/// Used by the renderer-side livekit-client view connection to enable
/// E2EE on screen-share publishes/subscribes with the same MLS-derived
/// key the Rust voice path uses.
#[derive(serde::Serialize, serde::Deserialize)]
pub struct E2eeKeyInfo {
    pub key: Vec<u8>,
    pub key_index: i32,
    pub epoch: u64,
    pub mls_group_id: String,
}

/// Renderer-facing entry point. Returns the same MLS-derived shared key
/// the Rust voice path uses for `KeyProvider::with_shared_key`, so the
/// JS-side `livekit-client`'s ExternalE2EEKeyProvider can encrypt the
/// screen-share video track with an interop-compatible key. Both SDKs
/// HKDF the shared key with the canonical `LKFrameEncryptionKey` salt
/// internally, so passing the raw 32-byte MLS export to both produces
/// the same per-frame encryption key on every peer.
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

    let key_index = (epoch & 0x7FFF_FFFF) as i32;
    Ok((key, key_index, epoch))
}

/// Build LiveKit `E2eeOptions` backed by a shared symmetric key. Defaults
/// match `livekit-client` JS (`LKFrameEncryptionKey` salt, 16-key ring,
/// PBKDF2 derivation) so peers across SDKs interop.
pub fn build_e2ee_options(key: Vec<u8>) -> E2eeOptions {
    let kp = KeyProvider::with_shared_key(KeyProviderOptions::default(), key);
    E2eeOptions {
        encryption_type: EncryptionType::Gcm,
        key_provider: kp,
    }
}

/// Called from `mls::process_pending_commits_inner` after any commit is
/// merged. If the changed group is the one currently backing the active
/// voice room, re-derive the voice key for the new epoch and rotate it on
/// the live `KeyProvider`. Live frames published after this call use the
/// new key; libwebrtc's key ring keeps the previous key available for
/// in-flight frames during the changeover.
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
            eprintln!("[voice-e2ee] re-derive failed for {mls_group_id}: {e}");
            return;
        }
    };

    if epoch == prev_epoch {
        return;
    }

    provider.set_shared_key(key.clone(), key_index);

    let channel = {
        let mut voice = state.voice.lock().await;
        voice.e2ee_epoch = epoch;
        voice.channel.clone()
    };

    // Notify the renderer so the screen-share view client's
    // ExternalE2EEKeyProvider can rotate too. Without this, the JS-side
    // key stays at its connect-time epoch and decrypt fails on every
    // remote video frame after a commit.
    if let Some(ch) = channel {
        let _ = ch.send(crate::commands::voice::VoiceEvent::VoiceE2eeKeyRotated {
            key,
            key_index,
            epoch,
            mls_group_id: mls_group_id.to_string(),
        });
    }

    eprintln!(
        "[voice-e2ee] rotated key for {mls_group_id} to epoch {epoch} (idx {key_index})"
    );
}
