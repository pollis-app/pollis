//! Pinned messages (#99) — pin, unpin, list, and the pin-key lifecycle.
//!
//! (Not `pin`: `commands::pin` is the device-unlock PIN.)
//! Pin content is sealed once under a per-conversation `Kpin`; only its wrap rotates with the epoch.
//! Mints are insert-only on the DS, so devices can never fork `Kpin`.

use std::sync::Arc;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit};
use crate::util::b64;
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
    /// The pinned message's content at pin time, in the `message` table's content shape.
    pub content: String,
    pub sender_id: String,
    /// Resolved from the local `user_cache` when this device knows the name.
    pub sender_username: Option<String>,
    pub sent_at: String,
}

/// The sealed snapshot; ids sit inside the ciphertext so a server reshuffle shows as a mismatch.
#[derive(Debug, Serialize, Deserialize)]
struct PinSnapshot {
    v: u8,
    message_id: String,
    conversation_id: String,
    sender_id: String,
    sent_at: String,
    content: String,
}

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

/// The KEK from a group's current exporter, with the `(generation, epoch)` that produced it.
struct PinKek {
    kek: Zeroizing<Vec<u8>>,
    generation: i64,
    epoch: i64,
}

/// The current [`PinKek`], or `None` when no local MLS group exists.
///
/// Pure sync — call inside a `local_db` guard scope; nothing here may cross
/// an await (`PollisProvider` wraps a `!Send` connection).
fn current_pin_kek(conn: &Connection, mls_conversation_id: &str) -> Option<PinKek> {
    let generation = local_generation(conn, mls_conversation_id);
    let group = load_stored_group(conn, mls_conversation_id)?;
    let provider = PollisProvider::new(conn);
    let epoch = group.epoch().as_u64() as i64;
    let kek = group
        .export_secret(provider.crypto(), PIN_KEK_EXPORT_LABEL, &[], KPIN_LEN)
        .ok()?;
    Some(PinKek {
        kek: Zeroizing::new(kek),
        generation,
        epoch,
    })
}

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

/// Wrap `kpin` under `kek.kek` and post it at `kek`'s `(generation, epoch)`.
/// Returns the DS's `updated` answer.
async fn post_keystate(
    state: &Arc<AppState>,
    mls_conversation_id: &str,
    kpin: &[u8],
    kek: &PinKek,
    actor_id: &str,
    mint: bool,
) -> Result<bool> {
    let device_id = {
        let guard = state.device_id.lock().await;
        guard
            .clone()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("no device id for pin keystate")))?
    };
    let (nonce, wrapped) = seal(&kek.kek, kpin)?;
    let body = pollis_api::pins::UpsertPinKeystateBody {
        conversation_id: mls_conversation_id.to_string(),
        wrapped_kpin: b64(&wrapped),
        nonce: b64(&nonce),
        generation: kek.generation,
        epoch: kek.epoch,
        device_id,
        actor_id: Some(actor_id.to_string()),
        mint,
    };
    let resp: pollis_api::pins::PinKeystateUpserted =
        super::mls::ds_post_json(state, &body).await?;
    let pollis_api::pins::PinKeystateUpserted::Ok { updated } = resp;
    Ok(updated)
}

/// Get this conversation's `Kpin`: from the local cache, by unwrapping the
/// server's wrap, or — when `mint_if_absent` and no keystate exists — by
/// minting a fresh key (insert-only on the DS, so a lost race adopts the
/// winner's key instead of forking).
///
/// `Ok(None)`: the wrap is at another epoch and nothing is cached; heals on a member's re-wrap.
async fn ensure_kpin(
    state: &Arc<AppState>,
    mls_conversation_id: &str,
    user_id: &str,
    mint_if_absent: bool,
) -> Result<Option<Zeroizing<Vec<u8>>>> {
    // Cache hit — the common case.
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
            let Some(kek) = current_pin_kek(db.conn(), mls_conversation_id) else {
                return Ok(None);
            };
            if ks.generation != kek.generation || ks.epoch != kek.epoch {
                // Wrap from another epoch: no exporter can open it; a member's re-wrap heals this.
                return Ok(None);
            }
            let kpin = open(&kek.kek, &nonce, &wrapped)?;
            if kpin.len() != KPIN_LEN {
                return Err(Error::Crypto("unwrapped Kpin has wrong length".into()));
            }
            write_cache(db.conn(), mls_conversation_id, &kpin, kek.generation, kek.epoch)?;
            Ok(Some(Zeroizing::new(kpin)))
        }
        None if mint_if_absent => {
            let mut kpin = Zeroizing::new(vec![0u8; KPIN_LEN]);
            OsRng.fill_bytes(kpin.as_mut());
            let kek = {
                let guard = state.local_db.lock().await;
                let db = guard
                    .as_ref()
                    .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
                match current_pin_kek(db.conn(), mls_conversation_id) {
                    Some(t) => t,
                    None => return Ok(None),
                }
            };
            let won = post_keystate(state, mls_conversation_id, &kpin, &kek, user_id, true).await?;
            if won {
                let guard = state.local_db.lock().await;
                let db = guard
                    .as_ref()
                    .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
                write_cache(db.conn(), mls_conversation_id, &kpin, kek.generation, kek.epoch)?;
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
            let Some(kek) = current_pin_kek(db.conn(), mls_conversation_id) else {
                return Ok(None);
            };
            if ks.generation != kek.generation || ks.epoch != kek.epoch {
                return Ok(None);
            }
            let kpin = open(&kek.kek, &nonce, &wrapped)?;
            if kpin.len() != KPIN_LEN {
                return Err(Error::Crypto("unwrapped Kpin has wrong length".into()));
            }
            write_cache(db.conn(), mls_conversation_id, &kpin, kek.generation, kek.epoch)?;
            Ok(Some(Zeroizing::new(kpin)))
        }
        None => Ok(None),
    }
}

/// Re-wrap the cached `Kpin` under the current exporter if this device is behind.
///
/// Steady state is one local SELECT. Best-effort; the next pass retries.
pub async fn maybe_rewrap_pin_key(state: &Arc<AppState>, mls_conversation_id: &str) {
    let staged: Option<(Zeroizing<Vec<u8>>, PinKek)> = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else {
            return;
        };
        let cached = match read_cache(db.conn(), mls_conversation_id) {
            Ok(c) => c,
            Err(e) => {
                crate::diag!("[pins] key cache read failed for {mls_conversation_id}: {e}");
                return;
            }
        };
        let Some((kpin, wrapped_generation, wrapped_epoch)) = cached else {
            return;
        };
        let Some(kek) = current_pin_kek(db.conn(), mls_conversation_id) else {
            return;
        };
        if (kek.generation, kek.epoch) <= (wrapped_generation, wrapped_epoch) {
            return;
        }
        Some((Zeroizing::new(kpin), kek))
    };
    let Some((kpin, kek)) = staged else {
        return;
    };

    let user_id = match super::mls::current_user_id(state).await {
        Ok(u) => u,
        Err(_) => return,
    };
    match post_keystate(state, mls_conversation_id, &kpin, &kek, &user_id, false).await {
        // `updated == false` means somebody newer-or-equal already covered
        // this epoch — either way the server is current, so stop retrying.
        Ok(_) => {
            let guard = state.local_db.lock().await;
            let Some(db) = guard.as_ref() else {
                return;
            };
            if let Err(e) =
                conn_update_markers(db.conn(), mls_conversation_id, kek.generation, kek.epoch)
            {
                crate::diag!("[pins] cache marker update failed for {mls_conversation_id}: {e}");
            }
        }
        Err(e) => {
            crate::diag!("[pins] re-wrap failed for {mls_conversation_id} (will retry on the next pass): {e}");
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

/// Best-effort adopt of the pin key right after a Welcome or external join, when the wrap likely matches.
pub async fn try_adopt_pin_key(state: &Arc<AppState>, mls_conversation_id: &str) {
    let user_id = match super::mls::current_user_id(state).await {
        Ok(u) => u,
        Err(_) => return,
    };
    match ensure_kpin(state, mls_conversation_id, &user_id, false).await {
        Ok(Some(_)) => {}
        Ok(None) => {}
        Err(e) => {
            crate::diag!("[pins] pin-key adopt failed for {mls_conversation_id} (will retry lazily): {e}");
        }
    }
}

/// Mint the pin key at group creation. Best-effort: the first pin mints otherwise.
pub async fn mint_pin_key_for_new_group(state: &Arc<AppState>, conversation_id: &str) {
    let user_id = match super::mls::current_user_id(state).await {
        Ok(u) => u,
        Err(_) => return,
    };
    if let Err(e) = ensure_kpin(state, conversation_id, &user_id, true).await {
        crate::diag!("[pins] create-time key mint failed for {conversation_id} (first pin will mint): {e}");
    }
}

/// Pin a message: seal a snapshot under `Kpin` and store it via the DS. Idempotent.
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
/// Snapshots that fail to open, or name a different message or conversation, are skipped.
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
                crate::diag!("[pins] undecryptable pin {} skipped: {e}", pin.id);
                continue;
            }
        };
        let json = super::messages::framing::strip(&padded);
        let snapshot: PinSnapshot = match serde_json::from_slice(&json) {
            Ok(s) => s,
            Err(e) => {
                crate::diag!("[pins] unparseable pin {} skipped: {e}", pin.id);
                continue;
            }
        };
        if snapshot.message_id != pin.message_id || snapshot.conversation_id != conversation_id {
            crate::diag!(
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

    /// Ids are long so the plaintext-search test is a real leak detector.
    fn snapshot() -> PinSnapshot {
        PinSnapshot {
            v: 1,
            message_id: "msg-01M2PINNEDSNAPSHOT".into(),
            conversation_id: "conv-01M2PINNEDSNAPSHOT".into(),
            sender_id: "user-01M2PINNEDSNAPSHOT".into(),
            sent_at: "2026-01-01T00:00:00Z".into(),
            content: "the pinned words".into(),
        }
    }

    #[test]
    fn a_sealed_snapshot_opens_back_to_itself() {
        let snap = snapshot();
        let json = serde_json::to_vec(&snap).unwrap();
        let padded = crate::commands::messages::framing::pad(&json);
        let (nonce, ct) = seal(KEY, &padded).unwrap();
        let opened = open(KEY, &nonce, &ct).unwrap();
        let stripped = crate::commands::messages::framing::strip(&opened);
        let back: PinSnapshot = serde_json::from_slice(&stripped).unwrap();
        assert_eq!(back.content, snap.content);
        assert_eq!(back.message_id, snap.message_id);
        assert_eq!(back.conversation_id, snap.conversation_id);
        assert_eq!(back.sender_id, snap.sender_id);
    }

    /// What the DS stores must not carry the pin's words or ids.
    ///
    /// Every needle must be long enough that finding one is evidence, not coincidence.
    #[test]
    fn the_stored_bytes_do_not_contain_the_plaintext() {
        let snap = snapshot();
        let json = serde_json::to_vec(&snap).unwrap();
        let padded = crate::commands::messages::framing::pad(&json);
        let (_, ct) = seal(KEY, &padded).unwrap();
        for needle in [
            snap.content.as_bytes(),
            snap.message_id.as_bytes(),
            snap.conversation_id.as_bytes(),
            snap.sender_id.as_bytes(),
            b"message_id".as_slice(),
        ] {
            assert!(
                needle.len() >= 8,
                "a needle shorter than 8 bytes cannot distinguish a leak from a \
                 coincidence: {:?}",
                String::from_utf8_lossy(needle)
            );
            assert!(
                !ct.windows(needle.len()).any(|w| w == needle),
                "plaintext leaked into the stored pin bytes: {:?}",
                String::from_utf8_lossy(needle)
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
