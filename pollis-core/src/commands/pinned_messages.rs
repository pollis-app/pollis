//! Pinned messages (#99) — pin, unpin, list, and the pin-key lifecycle.
//!
//! (Named `pinned_messages`, not `pin` — `commands::pin` is the device-unlock
//! PIN and the two must never be conflated.)
//!
//! # The key model
//!
//! Every MLS conversation (a group — whose channels share one MLS group — or a
//! DM) has one random 32-byte AES-256-GCM pin master key, `Kpin`. Pin content
//! is sealed directly under `Kpin` exactly once, at pin time, and never
//! re-encrypted. What rotates with the MLS epoch is only the ~60-byte WRAP of
//! `Kpin` stored server-side in `pin_keystate`: AES-256-GCM under a KEK
//! exported from the current epoch's MLS exporter secret
//! (`export_secret("pollis-pin-kek")`). O(1) work per epoch regardless of pin
//! count, no application data on Welcomes, and a device joining at epoch N
//! unwraps with the exporter it already has.
//!
//! # Why the plaintext `Kpin` is cached locally
//!
//! `max_past_epochs = 0`: the moment a commit merges, the previous epoch's
//! exporter is gone. So "unwrap with the old KEK, re-wrap with the new" is
//! impossible AFTER a merge, and the merge sites are many (committer,
//! receiver replay, external join, welcome, migration). Instead, a device
//! that has ever held `Kpin` keeps it in `pin_key_cache` — the same
//! SQLCipher-encrypted local DB that holds the MLS state itself, so this adds
//! no new exposure — and after any epoch advance simply wraps the CACHED key
//! under the CURRENT exporter ([`maybe_rewrap_pin_key`], a pure local check
//! in the steady state). A device that never held `Kpin` (a fresh external
//! joiner) unwraps the stored wrap as soon as its `(generation, epoch)`
//! matches the device's own — which any cache-holding member's next re-wrap
//! makes true.
//!
//! # What the honest guarantee is
//!
//! Any current member, including a device enrolled long after a pin was
//! created, can read every pin. A member removed from the conversation cannot
//! unwrap any wrap produced after its removal, and the membership-gated DS
//! reads refuse it the rows — but because `Kpin` itself never rotates, a
//! removed member who EXTRACTED it while a member and who can additionally
//! read Turso directly (a breach) could open pins created after its removal.
//! Rotating `Kpin` on removal would close that at O(pins) re-encryption per
//! membership change; the issue explicitly traded it away, since a removed
//! member could equally have cached every plaintext it ever saw. If every
//! device that ever held `Kpin` is lost AND the stored wrap has gone stale
//! (nobody re-wrapped at the current epoch before vanishing), existing pins
//! become unreadable — the same class of loss as losing an entire identity,
//! and the price of the server never holding key material.
//!
//! # Forks are unrepresentable
//!
//! Two devices holding DIFFERENT `Kpin`s for one conversation would silently
//! split pins into two undecryptable halves. The mint path closes it: a mint
//! is an INSERT-ONLY write on the DS (`mint: true`, refused whenever any row
//! exists, even at a newer epoch), the local cache is only ever written from
//! a mint the server accepted or an unwrap of the server's row, and re-wraps
//! carry that same canonical key. There is no path by which a non-canonical
//! key enters the cache.

use std::sync::Arc;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit};
use base64::Engine as _;
use rand::rngs::OsRng;
use rand::RngCore;
use openmls_traits::OpenMlsProvider;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::mls::generation::local_generation;
use super::mls::provider::load_stored_group;
use super::mls::PollisProvider;

/// Exporter label for the pin KEK. The exporter secret itself changes every
/// epoch, so the wrap is epoch-bound without an explicit context.
const PIN_KEK_EXPORT_LABEL: &str = "pollis-pin-kek";

const NONCE_LEN: usize = 12;
const KPIN_LEN: usize = 32;

/// One decrypted pin, ready for the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PinnedMessage {
    pub id: String,
    pub conversation_id: String,
    pub message_id: String,
    pub pinned_by: String,
    pub pinned_at: String,
    /// The pinned message's content, snapshotted at pin time — the same
    /// content-string shape the `message` table caches, so attachments render
    /// through the exact same parser.
    pub content: String,
    pub sender_id: String,
    /// Resolved from the local `user_cache` when this device knows the name.
    pub sender_username: Option<String>,
    pub sent_at: String,
}

/// What gets sealed under `Kpin`: a self-describing snapshot of the message at
/// pin time. `message_id`/`conversation_id` are INSIDE the ciphertext so a
/// server that shuffles rows between messages or channels produces a visible
/// mismatch instead of a silently relocated pin.
#[derive(Debug, Serialize, Deserialize)]
struct PinSnapshot {
    v: u8,
    message_id: String,
    conversation_id: String,
    sender_id: String,
    sent_at: String,
    content: String,
}

// ── AES-256-GCM helpers ──────────────────────────────────────────────────────

fn seal(key: &[u8], plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key));
    let ct = cipher
        .encrypt(GenericArray::from_slice(&nonce), plaintext)
        .map_err(|e| Error::Crypto(format!("pin seal: {e}")))?;
    Ok((nonce.to_vec(), ct))
}

fn open(key: &[u8], nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    if nonce.len() != NONCE_LEN {
        return Err(Error::Crypto(format!(
            "pin nonce has wrong length: {} (expected {NONCE_LEN})",
            nonce.len()
        )));
    }
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key));
    cipher
        .decrypt(GenericArray::from_slice(nonce), ciphertext)
        .map_err(|e| Error::Crypto(format!("pin open: {e}")))
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ── The exporter-derived KEK ─────────────────────────────────────────────────

/// The current `(KEK, generation, epoch)` for a conversation, or `None` when
/// no local MLS group exists (pre-join, evicted, or a conversation this
/// device has never synced).
///
/// Pure sync — call inside a `local_db` guard scope; nothing here may cross
/// an await (`PollisProvider` wraps a `!Send` connection).
fn current_pin_kek(
    conn: &Connection,
    mls_conversation_id: &str,
) -> Option<(Zeroizing<Vec<u8>>, i64, i64)> {
    let generation = local_generation(conn, mls_conversation_id);
    let group = load_stored_group(conn, mls_conversation_id)?;
    let provider = PollisProvider::new(conn);
    let epoch = group.epoch().as_u64() as i64;
    let kek = group
        .export_secret(provider.crypto(), PIN_KEK_EXPORT_LABEL, &[], KPIN_LEN)
        .ok()?;
    Some((Zeroizing::new(kek), generation, epoch))
}

// ── Local Kpin cache ─────────────────────────────────────────────────────────

fn read_cache(conn: &Connection, mls_conversation_id: &str) -> Result<Option<(Vec<u8>, i64, i64)>> {
    conn.query_row(
        "SELECT kpin, wrapped_generation, wrapped_epoch FROM pin_key_cache \
         WHERE conversation_id = ?1",
        rusqlite::params![mls_conversation_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()
    .map_err(|e| Error::Other(anyhow::anyhow!("pin key cache read: {e}")))
}

fn write_cache(
    conn: &Connection,
    mls_conversation_id: &str,
    kpin: &[u8],
    generation: i64,
    epoch: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO pin_key_cache \
             (conversation_id, kpin, wrapped_generation, wrapped_epoch, updated_at) \
         VALUES (?1, ?2, ?3, ?4, datetime('now')) \
         ON CONFLICT(conversation_id) DO UPDATE SET \
             kpin = excluded.kpin, \
             wrapped_generation = excluded.wrapped_generation, \
             wrapped_epoch = excluded.wrapped_epoch, \
             updated_at = excluded.updated_at",
        rusqlite::params![mls_conversation_id, kpin, generation, epoch],
    )
    .map_err(|e| Error::Other(anyhow::anyhow!("pin key cache write: {e}")))?;
    Ok(())
}

// ── Keystate upload ──────────────────────────────────────────────────────────

/// Wrap `kpin` under the KEK and post it. Returns the DS's `updated` answer.
async fn post_keystate(
    state: &Arc<AppState>,
    mls_conversation_id: &str,
    kpin: &[u8],
    kek: &[u8],
    generation: i64,
    epoch: i64,
    actor_id: &str,
    mint: bool,
) -> Result<bool> {
    let device_id = {
        let guard = state.device_id.lock().await;
        guard
            .clone()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("no device id for pin keystate")))?
    };
    let (nonce, wrapped) = seal(kek, kpin)?;
    let body = pollis_api::pins::UpsertPinKeystateBody {
        conversation_id: mls_conversation_id.to_string(),
        wrapped_kpin: b64(&wrapped),
        nonce: b64(&nonce),
        generation,
        epoch,
        device_id,
        actor_id: Some(actor_id.to_string()),
        mint,
    };
    let resp: pollis_api::pins::PinKeystateUpserted =
        super::mls::ds_post_json(state, &body).await?;
    let pollis_api::pins::PinKeystateUpserted::Ok { updated } = resp;
    Ok(updated)
}

// ── Kpin acquisition ─────────────────────────────────────────────────────────

/// Get this conversation's `Kpin`: from the local cache, by unwrapping the
/// server's wrap, or — when `mint_if_absent` and no keystate exists — by
/// minting a fresh key (insert-only on the DS, so a lost race adopts the
/// winner's key instead of forking).
///
/// `Ok(None)` means the key exists but is UNREACHABLE from here right now:
/// the stored wrap was produced at a `(generation, epoch)` other than this
/// device's current one, and this device holds no cache. That heals when any
/// cache-holding member re-wraps ([`maybe_rewrap_pin_key`] fires on every
/// epoch advance and catch-up pass); until then pins are unavailable rather
/// than wrong.
async fn ensure_kpin(
    state: &Arc<AppState>,
    mls_conversation_id: &str,
    user_id: &str,
    mint_if_absent: bool,
) -> Result<Option<Zeroizing<Vec<u8>>>> {
    // Cache hit — the common case for every device that has pinned, listed,
    // or re-wrapped before.
    {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        if let Some((kpin, _, _)) = read_cache(db.conn(), mls_conversation_id)? {
            return Ok(Some(Zeroizing::new(kpin)));
        }
    }

    let resp = super::ds_reads::read_pin_keystate(state, mls_conversation_id, user_id).await?;
    match resp.keystate {
        Some(ks) => {
            let wrapped = super::ds_reads::decode_b64("pin keystate wrap", &ks.wrapped_kpin)?;
            let nonce = super::ds_reads::decode_b64("pin keystate nonce", &ks.nonce)?;
            let guard = state.local_db.lock().await;
            let db = guard
                .as_ref()
                .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
            let Some((kek, generation, epoch)) = current_pin_kek(db.conn(), mls_conversation_id)
            else {
                return Ok(None);
            };
            if ks.generation != generation || ks.epoch != epoch {
                // Wrap from an epoch we are not at: with `max_past_epochs = 0`
                // there is no exporter to open it with. A member's re-wrap
                // heals this; report unavailable rather than guessing.
                return Ok(None);
            }
            let kpin = open(&kek, &nonce, &wrapped)?;
            if kpin.len() != KPIN_LEN {
                return Err(Error::Crypto("unwrapped Kpin has wrong length".into()));
            }
            write_cache(db.conn(), mls_conversation_id, &kpin, generation, epoch)?;
            Ok(Some(Zeroizing::new(kpin)))
        }
        None if mint_if_absent => {
            let mut kpin = Zeroizing::new(vec![0u8; KPIN_LEN]);
            OsRng.fill_bytes(kpin.as_mut());
            let (kek, generation, epoch) = {
                let guard = state.local_db.lock().await;
                let db = guard
                    .as_ref()
                    .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
                match current_pin_kek(db.conn(), mls_conversation_id) {
                    Some(t) => t,
                    None => return Ok(None),
                }
            };
            let won = post_keystate(
                state,
                mls_conversation_id,
                &kpin,
                &kek,
                generation,
                epoch,
                user_id,
                true,
            )
            .await?;
            if won {
                let guard = state.local_db.lock().await;
                let db = guard
                    .as_ref()
                    .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
                write_cache(db.conn(), mls_conversation_id, &kpin, generation, epoch)?;
                return Ok(Some(kpin));
            }
            // Lost the mint race: a row landed between our read and our
            // insert. Adopt the winner's key — one refetch, no second mint.
            let resp =
                super::ds_reads::read_pin_keystate(state, mls_conversation_id, user_id).await?;
            let Some(ks) = resp.keystate else {
                return Ok(None);
            };
            let wrapped = super::ds_reads::decode_b64("pin keystate wrap", &ks.wrapped_kpin)?;
            let nonce = super::ds_reads::decode_b64("pin keystate nonce", &ks.nonce)?;
            let guard = state.local_db.lock().await;
            let db = guard
                .as_ref()
                .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
            let Some((kek, generation, epoch)) = current_pin_kek(db.conn(), mls_conversation_id)
            else {
                return Ok(None);
            };
            if ks.generation != generation || ks.epoch != epoch {
                return Ok(None);
            }
            let kpin = open(&kek, &nonce, &wrapped)?;
            if kpin.len() != KPIN_LEN {
                return Err(Error::Crypto("unwrapped Kpin has wrong length".into()));
            }
            write_cache(db.conn(), mls_conversation_id, &kpin, generation, epoch)?;
            Ok(Some(Zeroizing::new(kpin)))
        }
        None => Ok(None),
    }
}

// ── Epoch hooks ──────────────────────────────────────────────────────────────

/// Re-wrap the cached `Kpin` under the current exporter if this device is
/// behind — the pin sibling of `voice_e2ee::on_mls_epoch_changed`, called at
/// every epoch-advance site and once per catch-up pass as a durability
/// backstop.
///
/// Steady-state cost is ONE local SELECT (no cache row → nothing to do; row
/// current → nothing to do). Only a device that actually holds the key and is
/// actually behind pays for a group load and a DS post. Best-effort by
/// design: a failed re-wrap costs a new device some waiting, never
/// correctness, and the next pass retries.
pub async fn maybe_rewrap_pin_key(state: &Arc<AppState>, mls_conversation_id: &str) {
    let staged: Option<(Zeroizing<Vec<u8>>, Zeroizing<Vec<u8>>, i64, i64)> = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else {
            return;
        };
        let cached = match read_cache(db.conn(), mls_conversation_id) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("[pins] key cache read failed for {mls_conversation_id}: {e}");
                return;
            }
        };
        let Some((kpin, wrapped_generation, wrapped_epoch)) = cached else {
            return;
        };
        let Some((kek, generation, epoch)) = current_pin_kek(db.conn(), mls_conversation_id)
        else {
            return;
        };
        if (generation, epoch) <= (wrapped_generation, wrapped_epoch) {
            return;
        }
        Some((Zeroizing::new(kpin), kek, generation, epoch))
    };
    let Some((kpin, kek, generation, epoch)) = staged else {
        return;
    };

    let user_id = match super::mls::current_user_id(state).await {
        Ok(u) => u,
        Err(_) => return,
    };
    match post_keystate(
        state,
        mls_conversation_id,
        &kpin,
        &kek,
        generation,
        epoch,
        &user_id,
        false,
    )
    .await
    {
        // `updated == false` means somebody newer-or-equal already covered
        // this epoch — either way the server is current, so stop retrying.
        Ok(_) => {
            let guard = state.local_db.lock().await;
            let Some(db) = guard.as_ref() else {
                return;
            };
            if let Err(e) = conn_update_markers(db.conn(), mls_conversation_id, generation, epoch)
            {
                eprintln!("[pins] cache marker update failed for {mls_conversation_id}: {e}");
            }
        }
        Err(e) => {
            eprintln!("[pins] re-wrap failed for {mls_conversation_id} (will retry on the next pass): {e}");
        }
    }
}

fn conn_update_markers(
    conn: &Connection,
    mls_conversation_id: &str,
    generation: i64,
    epoch: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE pin_key_cache SET wrapped_generation = ?2, wrapped_epoch = ?3, \
             updated_at = datetime('now') \
         WHERE conversation_id = ?1",
        rusqlite::params![mls_conversation_id, generation, epoch],
    )
    .map_err(|e| Error::Other(anyhow::anyhow!("pin key cache marker update: {e}")))?;
    Ok(())
}

/// Best-effort adopt of a conversation's pin key at a moment the wrap is
/// likely to match this device's epoch exactly — right after a Welcome lands
/// (before the post-join self-update moves the epoch) and after an external
/// join completes. Failure is silent by design: a device that cannot adopt
/// yet simply reads pins later, once a key-holding member re-wraps.
pub async fn try_adopt_pin_key(state: &Arc<AppState>, mls_conversation_id: &str) {
    let user_id = match super::mls::current_user_id(state).await {
        Ok(u) => u,
        Err(_) => return,
    };
    match ensure_kpin(state, mls_conversation_id, &user_id, false).await {
        Ok(Some(_)) => {}
        Ok(None) => {}
        Err(e) => {
            eprintln!("[pins] pin-key adopt failed for {mls_conversation_id} (will retry lazily): {e}");
        }
    }
}

/// Mint the pin key at group creation, while the creator is the sole member
/// at epoch 0. Best-effort: on any failure the first pin mints instead, so a
/// dropped create-time mint costs nothing but that later round trip.
pub async fn mint_pin_key_for_new_group(state: &Arc<AppState>, conversation_id: &str) {
    let user_id = match super::mls::current_user_id(state).await {
        Ok(u) => u,
        Err(_) => return,
    };
    if let Err(e) = ensure_kpin(state, conversation_id, &user_id, true).await {
        eprintln!("[pins] create-time key mint failed for {conversation_id} (first pin will mint): {e}");
    }
}

// ── Commands ─────────────────────────────────────────────────────────────────

/// Pin a message: snapshot its content, seal the snapshot under the
/// conversation's `Kpin`, and store it via the DS. Idempotent — pinning an
/// already-pinned message leaves the existing pin in place.
pub async fn pin_message(
    conversation_id: String,
    message_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let cu = super::ds_reads::catch_up(state, &conversation_id, false, false).await?;
    if !cu.authorized {
        return Err(Error::Other(anyhow::anyhow!(
            "not a member of this conversation"
        )));
    }
    let mls_id = cu.mls_group_id;

    // The snapshot comes from this device's own decrypted copy — a message
    // this device cannot read is a message it cannot pin.
    let (content, sender_id, sent_at): (String, String, String) = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        db.conn()
            .query_row(
                "SELECT content, sender_id, sent_at FROM message \
                 WHERE id = ?1 AND conversation_id = ?2 AND deleted_at IS NULL \
                   AND content IS NOT NULL",
                rusqlite::params![message_id, conversation_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|e| Error::Other(anyhow::anyhow!("pin snapshot lookup: {e}")))?
            .ok_or_else(|| Error::Other(anyhow::anyhow!("message not available to pin")))?
    };

    let kpin = ensure_kpin(state, &mls_id, &user_id, true)
        .await?
        .ok_or_else(|| {
            Error::Other(anyhow::anyhow!(
                "pin key not yet available on this device — try again after the \
                 conversation syncs"
            ))
        })?;

    let snapshot = PinSnapshot {
        v: 1,
        message_id: message_id.clone(),
        conversation_id: conversation_id.clone(),
        sender_id,
        sent_at,
        content,
    };
    let json = serde_json::to_vec(&snapshot)
        .map_err(|e| Error::Other(anyhow::anyhow!("serialize pin snapshot: {e}")))?;
    // Same size-bucket padding messages use, so a pin's length does not count
    // its words for the server.
    let padded = super::messages::framing::pad(&json);
    let (nonce, ct) = seal(&kpin, &padded)?;

    let body = pollis_api::pins::PinMessageBody {
        id: ulid::Ulid::new().to_string(),
        conversation_id,
        message_id,
        pinned_by: Some(user_id),
        encrypted_content: b64(&ct),
        nonce: b64(&nonce),
    };
    super::mls::ds_post_ok(state, &body).await
}

/// Remove a pin. Any current member may unpin; unpinning something already
/// unpinned is a no-op.
pub async fn unpin_message(
    conversation_id: String,
    message_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let body = pollis_api::pins::UnpinMessageBody {
        conversation_id,
        message_id,
        actor_id: Some(user_id),
    };
    super::mls::ds_post_ok(state, &body).await
}

/// One conversation's pins, newest first, decrypted.
///
/// A snapshot that fails to open (tampered, or sealed under a key this
/// conversation no longer uses) is skipped with a log line rather than
/// failing the whole list — one bad row must not hide the good ones. A
/// snapshot that opens but names a DIFFERENT message or conversation is also
/// skipped: that is the server (or another member) splicing rows, and
/// rendering it would relocate a pin.
pub async fn list_pinned_messages(
    conversation_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<PinnedMessage>> {
    let resp = super::ds_reads::read_pins(state, &conversation_id, &user_id).await?;
    if resp.pins.is_empty() {
        return Ok(Vec::new());
    }

    let cu = super::ds_reads::catch_up(state, &conversation_id, false, false).await?;
    if !cu.authorized {
        return Err(Error::Other(anyhow::anyhow!(
            "not a member of this conversation"
        )));
    }
    let kpin = ensure_kpin(state, &cu.mls_group_id, &user_id, false)
        .await?
        .ok_or_else(|| {
            Error::Other(anyhow::anyhow!(
                "pin key not yet available on this device — pins will appear \
                 after another member syncs"
            ))
        })?;

    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;

    let mut out = Vec::with_capacity(resp.pins.len());
    for pin in resp.pins {
        let ct = super::ds_reads::decode_b64("pin content", &pin.encrypted_content)?;
        let nonce = super::ds_reads::decode_b64("pin nonce", &pin.nonce)?;
        let padded = match open(&kpin, &nonce, &ct) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("[pins] undecryptable pin {} skipped: {e}", pin.id);
                continue;
            }
        };
        let json = super::messages::framing::strip(&padded);
        let snapshot: PinSnapshot = match serde_json::from_slice(&json) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[pins] unparseable pin {} skipped: {e}", pin.id);
                continue;
            }
        };
        if snapshot.message_id != pin.message_id || snapshot.conversation_id != conversation_id {
            eprintln!(
                "[pins] pin {} names a different message/conversation inside its \
                 ciphertext — skipped (possible row splice)",
                pin.id
            );
            continue;
        }
        let sender_username: Option<String> = db
            .conn()
            .query_row(
                "SELECT username FROM user_cache WHERE id = ?1",
                rusqlite::params![snapshot.sender_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| Error::Other(anyhow::anyhow!("pin username lookup: {e}")))?;
        out.push(PinnedMessage {
            id: pin.id,
            conversation_id: pin.conversation_id,
            message_id: pin.message_id,
            pinned_by: pin.pinned_by,
            pinned_at: pin.pinned_at,
            content: snapshot.content,
            sender_id: snapshot.sender_id,
            sender_username,
            sent_at: snapshot.sent_at,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &[u8; 32] = b"0123456789abcdef0123456789abcdef";

    fn snapshot() -> PinSnapshot {
        PinSnapshot {
            v: 1,
            message_id: "m1".into(),
            conversation_id: "c1".into(),
            sender_id: "u1".into(),
            sent_at: "2026-01-01T00:00:00Z".into(),
            content: "the pinned words".into(),
        }
    }

    #[test]
    fn a_sealed_snapshot_opens_back_to_itself() {
        let json = serde_json::to_vec(&snapshot()).unwrap();
        let padded = crate::commands::messages::framing::pad(&json);
        let (nonce, ct) = seal(KEY, &padded).unwrap();
        let opened = open(KEY, &nonce, &ct).unwrap();
        let stripped = crate::commands::messages::framing::strip(&opened);
        let back: PinSnapshot = serde_json::from_slice(&stripped).unwrap();
        assert_eq!(back.content, "the pinned words");
        assert_eq!(back.message_id, "m1");
    }

    /// What the DS stores must not carry the pin's words or ids.
    #[test]
    fn the_stored_bytes_do_not_contain_the_plaintext() {
        let json = serde_json::to_vec(&snapshot()).unwrap();
        let padded = crate::commands::messages::framing::pad(&json);
        let (_, ct) = seal(KEY, &padded).unwrap();
        for needle in [
            b"the pinned words".as_slice(),
            b"message_id".as_slice(),
            b"m1".as_slice(),
        ] {
            assert!(
                !ct.windows(needle.len()).any(|w| w == needle),
                "plaintext leaked into the stored pin bytes"
            );
        }
    }

    #[test]
    fn a_wrong_key_cannot_open_a_pin() {
        let (nonce, ct) = seal(KEY, b"snapshot").unwrap();
        let other = b"ffffffffffffffffffffffffffffffff";
        assert!(open(other, &nonce, &ct).is_err());
    }

    #[test]
    fn a_tampered_pin_is_rejected() {
        let (nonce, mut ct) = seal(KEY, b"snapshot").unwrap();
        ct[0] ^= 1;
        assert!(open(KEY, &nonce, &ct).is_err());
    }

    /// Two pins of identical content produce different bytes — fresh nonce
    /// every seal, and no equality oracle for the server.
    #[test]
    fn two_seals_of_the_same_content_differ() {
        let (n1, c1) = seal(KEY, b"same").unwrap();
        let (n2, c2) = seal(KEY, b"same").unwrap();
        assert_ne!(n1, n2);
        assert_ne!(c1, c2);
    }

    /// Padding puts short pins in one bucket, so pin length does not count
    /// words for the server.
    #[test]
    fn short_pins_share_a_size_bucket() {
        let a = crate::commands::messages::framing::pad(b"hi");
        let b = crate::commands::messages::framing::pad(b"a somewhat longer pin body");
        assert_eq!(a.len(), b.len());
    }

    /// The wrap-format round trip the keystate row depends on: KEK wraps the
    /// 32-byte Kpin, and the wrapped blob stays small enough for the DS bound.
    #[test]
    fn kpin_wrap_round_trips_and_stays_small() {
        let mut kpin = [0u8; KPIN_LEN];
        OsRng.fill_bytes(&mut kpin);
        let (nonce, wrapped) = seal(KEY, &kpin).unwrap();
        assert!(wrapped.len() <= 64, "wrap unexpectedly large: {}", wrapped.len());
        assert_eq!(open(KEY, &nonce, &wrapped).unwrap(), kpin);
    }
}
