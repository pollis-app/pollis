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

/// Look up the highest epoch we've seen published in `mls_group_info` for
/// this MLS group. None on any query failure (treated as "can't tell —
/// don't trigger recovery"), so a transient Turso blip never forces an
/// unnecessary external-join.
async fn published_group_epoch(state: &Arc<AppState>, mls_group_id: &str) -> Option<u64> {
    // Read-only GroupInfo epoch lookup → log_db (falls back to remote_db pre-cutover).
    let conn = state.log_db.conn().await.ok()?;
    let mut rows = conn
        .query(
            "SELECT epoch FROM mls_group_info WHERE conversation_id = ?1",
            libsql::params![mls_group_id.to_string()],
        )
        .await
        .ok()?;
    let row = rows.next().await.ok()??;
    row.get::<i64>(0).ok().map(|v| v as u64)
}

/// Pull and process any unread envelopes for every conversation backed by
/// this MLS group so the local epoch matches whoever sent the most recent
/// commit. Best-effort: ingest failures are logged and ignored so a
/// transient network blip doesn't block the voice join.
async fn catch_up_mls_group(state: &Arc<AppState>, mls_group_id: &str, user_id: &str) {
    // Look up the conversations that share this MLS group. Two shapes:
    //   1. `mls_group_id` IS a DM channel id — the DM table is keyed by it.
    //   2. `mls_group_id` is a group id — fan out across every `channels.id`
    //      where `group_id = mls_group_id`.
    let conn = match state.remote_db.conn().await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[voice-e2ee] catch-up: remote_db conn failed: {e}");
            return;
        }
    };

    let is_dm = match conn
        .query(
            "SELECT 1 FROM dm_channel WHERE id = ?1 LIMIT 1",
            libsql::params![mls_group_id.to_string()],
        )
        .await
    {
        Ok(mut rows) => matches!(rows.next().await, Ok(Some(_))),
        Err(e) => {
            eprintln!("[voice-e2ee] catch-up: dm_channel probe failed: {e}");
            false
        }
    };

    if is_dm {
        eprintln!("[voice-e2ee] catch-up: ingesting DM {mls_group_id}");
        match crate::commands::messages::ingest_dm_envelopes_inner(
            state,
            user_id,
            mls_group_id,
        )
        .await
        {
            Ok(()) => eprintln!("[voice-e2ee] catch-up: DM ingest OK {mls_group_id}"),
            Err(e) => {
                eprintln!("[voice-e2ee] catch-up ingest_dm for {mls_group_id}: {e}")
            }
        }
        return;
    }

    let channel_ids: Vec<String> = match conn
        .query(
            "SELECT id FROM channels WHERE group_id = ?1",
            libsql::params![mls_group_id.to_string()],
        )
        .await
    {
        Ok(mut rows) => {
            let mut out = Vec::new();
            loop {
                match rows.next().await {
                    Ok(Some(row)) => match row.get::<String>(0) {
                        Ok(id) => out.push(id),
                        Err(e) => {
                            eprintln!("[voice-e2ee] catch-up: channel row decode: {e}");
                            break;
                        }
                    },
                    Ok(None) => break,
                    Err(e) => {
                        eprintln!("[voice-e2ee] catch-up: channel row read: {e}");
                        break;
                    }
                }
            }
            out
        }
        Err(e) => {
            eprintln!("[voice-e2ee] catch-up: channels query failed for {mls_group_id}: {e}");
            return;
        }
    };

    eprintln!(
        "[voice-e2ee] catch-up: group {mls_group_id} → {} channel(s)",
        channel_ids.len()
    );
    // Diagnostic: dump what's in `mls_commit_log` for this MLS group on
    // Turso. process_pending_commits filters by `epoch >= local_epoch`, so
    // if the rows we'd need (e.g. epochs 5..N for a local at epoch 4) are
    // missing, advancement is impossible regardless of how many times we
    // ingest. Lists at most ~20 rows so the log stays manageable.
    // The commit-log read targets the read-only log DB; fall back to the main
    // connection (this `conn` already served the dm_channel/channels reads above,
    // which live in the main DB, so shadowing it here is safe).
    let conn = state.log_db.conn().await.unwrap_or(conn);
    match conn
        .query(
            "SELECT seq, epoch, sender_id, added_user_id \
             FROM mls_commit_log \
             WHERE conversation_id = ?1 \
             ORDER BY epoch ASC, seq ASC \
             LIMIT 20",
            libsql::params![mls_group_id.to_string()],
        )
        .await
    {
        Ok(mut rows) => {
            let mut lines: Vec<String> = Vec::new();
            while let Ok(Some(row)) = rows.next().await {
                let seq: i64 = row.get(0).unwrap_or(-1);
                let epoch: i64 = row.get(1).unwrap_or(-1);
                let sender_id: Option<String> = row.get(2).ok().flatten();
                let added: Option<String> = row.get(3).ok().flatten();
                lines.push(format!(
                    "seq={seq} epoch={epoch} sender={} added={}",
                    sender_id.as_deref().unwrap_or("-"),
                    added.as_deref().unwrap_or("-")
                ));
            }
            eprintln!(
                "[voice-e2ee] catch-up: mls_commit_log for {mls_group_id} ({} rows): {}",
                lines.len(),
                if lines.is_empty() {
                    "<empty>".to_string()
                } else {
                    lines.join(" | ")
                }
            );
        }
        Err(e) => {
            eprintln!(
                "[voice-e2ee] catch-up: mls_commit_log query for {mls_group_id} failed: {e}"
            );
        }
    }
    for ch in channel_ids {
        match crate::commands::messages::ingest_channel_envelopes_inner(state, user_id, &ch).await
        {
            Ok(()) => eprintln!("[voice-e2ee] catch-up: channel ingest OK {ch}"),
            Err(e) => eprintln!("[voice-e2ee] catch-up ingest_channel for {ch}: {e}"),
        }
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
