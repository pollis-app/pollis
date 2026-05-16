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

use crate::commands::mls::PollisProvider;
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
    let conn = state.remote_db.conn().await?;

    if channel_id.starts_with("call-") {
        let other = counterparty_user_id.ok_or_else(|| {
            Error::Other(anyhow::anyhow!(
                "voice call room {channel_id} requires a counterparty user id"
            ))
        })?;
        // Find the 1:1 DM channel between the two users. HAVING COUNT(*) = 2
        // guards against group DMs accidentally matching.
        let mut rows = conn
            .query(
                "SELECT dcm.dm_channel_id \
                 FROM dm_channel_member dcm \
                 WHERE dcm.dm_channel_id IN ( \
                     SELECT dm_channel_id FROM dm_channel_member WHERE user_id = ?1 \
                 ) \
                 AND dcm.user_id = ?2 \
                 AND ( \
                     SELECT COUNT(*) FROM dm_channel_member \
                     WHERE dm_channel_id = dcm.dm_channel_id \
                 ) = 2 \
                 ORDER BY dcm.dm_channel_id LIMIT 1",
                libsql::params![self_user_id.to_owned(), other.to_owned()],
            )
            .await?;
        return match rows.next().await? {
            Some(row) => Ok(row.get::<String>(0)?),
            None => Err(Error::Other(anyhow::anyhow!(
                "no 1:1 DM channel exists between {self_user_id} and {other} \
                 — cannot derive voice key for call {channel_id}"
            ))),
        };
    }

    let mut rows = conn
        .query(
            "SELECT group_id FROM channels WHERE id = ?1",
            libsql::params![channel_id.to_owned()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<String>(0)?),
        None => Ok(channel_id.to_owned()),
    }
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
    let (key, idx, epoch) = derive_voice_key_for_group(state, &mls_group_id).await?;
    Ok((key, idx, epoch, mls_group_id))
}

async fn derive_voice_key_for_group(
    state: &Arc<AppState>,
    mls_group_id: &str,
) -> Result<(Vec<u8>, i32, u64)> {
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    let provider = PollisProvider::new(db.conn());
    let group_id = GroupId::from_slice(mls_group_id.as_bytes());

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

    provider.set_shared_key(key, key_index);

    let mut voice = state.voice.lock().await;
    voice.e2ee_epoch = epoch;
    eprintln!(
        "[voice-e2ee] rotated key for {mls_group_id} to epoch {epoch} (idx {key_index})"
    );
}
