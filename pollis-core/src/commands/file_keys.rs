//! The per-conversation attachment key ring (#1382).
//!
//! Each MLS conversation has a ring of random file keys. An upload seals the file
//! under `HKDF(newest ring key, random file_id)` and records the key version in
//! the MLS-encrypted attachment reference. The ring reaches the DS only wrapped
//! under a KEK from the current epoch's exporter, like the pin key, so every
//! current member can open every file. Unlike the pin key, the ring grows: the
//! device committing a removal appends a key the removed member never sees. The
//! DS refuses every write that would fork the ring. See `.codesight/wiki/mls.md`
//! § Attachment file key ring.

use std::sync::Arc;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit};
use hkdf::Hkdf;
use openmls_traits::OpenMlsProvider;
use rand::rngs::OsRng;
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension};
use sha2::Sha256;
use zeroize::Zeroizing;

use pollis_api::file_keys::FileKeystateOp;

use crate::error::{Error, Result};
use crate::state::AppState;
use crate::util::b64;

use super::mls::generation::local_generation;
use super::mls::provider::load_stored_group;
use super::mls::PollisProvider;

/// Exporter label for the ring's KEK; the exporter changes every epoch, so the
/// wrap is epoch-bound without a context.
const FILE_KEK_EXPORT_LABEL: &str = "pollis-file-kek";

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

/// One serialized ring entry: key ‖ generation (BE i64) ‖ epoch (BE i64).
pub(crate) const RING_ENTRY_LEN: usize = KEY_LEN + 16;

/// HKDF `info` for a file's AES-256-GCM key and base nonce.
const FILE_KEY_INFO: &[u8] = b"pollis-file-key-v1";
const FILE_NONCE_INFO: &[u8] = b"pollis-file-nonce-v1";

/// A file's AES-256-GCM key and 12-byte base nonce from one ring key and the
/// file's random id (the HKDF salt), so every file gets its own key and nonce.
pub(crate) fn derive_file_key(
    ring_key: &[u8; KEY_LEN],
    file_id: &[u8; 32],
) -> (Zeroizing<[u8; KEY_LEN]>, [u8; NONCE_LEN]) {
    let hk = Hkdf::<Sha256>::new(Some(file_id), ring_key);
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    hk.expand(FILE_KEY_INFO, key.as_mut())
        .expect("HKDF expand for a 32-byte key cannot fail");
    let mut nonce = [0u8; NONCE_LEN];
    hk.expand(FILE_NONCE_INFO, &mut nonce)
        .expect("HKDF expand for a 12-byte nonce cannot fail");
    (key, nonce)
}

/// A decoded ring. Entry `i` is key version `i + 1`.
#[derive(Clone)]
struct Ring(Zeroizing<Vec<u8>>);

impl Ring {
    fn from_bytes(bytes: Vec<u8>) -> Result<Ring> {
        if bytes.is_empty() || bytes.len() % RING_ENTRY_LEN != 0 {
            return Err(Error::Crypto(format!(
                "file key ring has invalid length {}",
                bytes.len()
            )));
        }
        Ok(Ring(Zeroizing::new(bytes)))
    }

    fn version(&self) -> i64 {
        (self.0.len() / RING_ENTRY_LEN) as i64
    }

    fn key(&self, version: i64) -> Option<Zeroizing<[u8; KEY_LEN]>> {
        if version < 1 || version > self.version() {
            return None;
        }
        let start = (version as usize - 1) * RING_ENTRY_LEN;
        let mut key = Zeroizing::new([0u8; KEY_LEN]);
        key.copy_from_slice(&self.0[start..start + KEY_LEN]);
        Some(key)
    }

    /// The `(generation, epoch)` the newest key was added at.
    fn newest_added_at(&self) -> (i64, i64) {
        let start = (self.version() as usize - 1) * RING_ENTRY_LEN + KEY_LEN;
        let g = i64::from_be_bytes(self.0[start..start + 8].try_into().expect("8 bytes"));
        let e = i64::from_be_bytes(self.0[start + 8..start + 16].try_into().expect("8 bytes"));
        (g, e)
    }

    /// This ring with one fresh random key appended, added at `(g, e)`.
    fn with_new_key(&self, generation: i64, epoch: i64) -> Ring {
        let mut bytes = Zeroizing::new(Vec::with_capacity(self.0.len() + RING_ENTRY_LEN));
        bytes.extend_from_slice(&self.0);
        push_entry(&mut bytes, generation, epoch);
        Ring(bytes)
    }

    fn fresh(generation: i64, epoch: i64) -> Ring {
        let mut bytes = Zeroizing::new(Vec::with_capacity(RING_ENTRY_LEN));
        push_entry(&mut bytes, generation, epoch);
        Ring(bytes)
    }
}

fn push_entry(bytes: &mut Vec<u8>, generation: i64, epoch: i64) {
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    OsRng.fill_bytes(key.as_mut());
    bytes.extend_from_slice(key.as_ref());
    bytes.extend_from_slice(&generation.to_be_bytes());
    bytes.extend_from_slice(&epoch.to_be_bytes());
}

fn seal(key: &[u8], plaintext: &[u8]) -> Result<(Vec<u8>, Vec<u8>)> {
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key));
    let ct = cipher
        .encrypt(GenericArray::from_slice(&nonce), plaintext)
        .map_err(|e| Error::Crypto(format!("file key ring seal: {e}")))?;
    Ok((nonce.to_vec(), ct))
}

fn open(key: &[u8], nonce: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    if nonce.len() != NONCE_LEN {
        return Err(Error::Crypto("file key ring nonce has wrong length".into()));
    }
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key));
    cipher
        .decrypt(GenericArray::from_slice(nonce), ciphertext)
        .map_err(|e| Error::Crypto(format!("file key ring open: {e}")))
}

struct Kek {
    kek: Zeroizing<Vec<u8>>,
    generation: i64,
    epoch: i64,
}

/// The current KEK, or `None` with no local MLS group. Sync only: call inside a
/// `local_db` guard.
fn current_kek(conn: &Connection, mls_conversation_id: &str) -> Option<Kek> {
    let generation = local_generation(conn, mls_conversation_id);
    let group = load_stored_group(conn, mls_conversation_id)?;
    let provider = PollisProvider::new(conn);
    let epoch = group.epoch().as_u64() as i64;
    let kek = group
        .export_secret(provider.crypto(), FILE_KEK_EXPORT_LABEL, &[], KEY_LEN)
        .ok()?;
    Some(Kek {
        kek: Zeroizing::new(kek),
        generation,
        epoch,
    })
}

/// Past-epoch KEKs kept per MLS group (lineage). Wraps are re-wrapped as epochs
/// advance, so the needed one is almost always recent; this just bounds the table.
const KEPT_KEKS_PER_GROUP: i64 = 64;

/// Keep `group`'s current file KEK before a merge moves past this epoch, so this
/// device can open a ring wrapped at any epoch it was a member at. Reveals
/// nothing new: a wrap at epoch `e` holds only keys a member at `e` was entitled
/// to, and the key added at a removal is only wrapped past it. Every merge site
/// calls this (`every_merge_site_keeps_the_file_kek`). Best-effort; never blocks
/// the merge.
pub(crate) fn keep_epoch_kek<C>(
    provider: &super::mls::provider::MlsProvider<'_, C>,
    group: &openmls::prelude::MlsGroup,
) where
    C: openmls_traits::crypto::OpenMlsCrypto + openmls_traits::random::OpenMlsRand,
{
    let lineage = String::from_utf8_lossy(group.group_id().as_slice()).to_string();
    let epoch = group.epoch().as_u64() as i64;
    let kek = match group.export_secret(provider.crypto(), FILE_KEK_EXPORT_LABEL, &[], KEY_LEN) {
        Ok(k) => Zeroizing::new(k),
        Err(e) => {
            crate::diag!("[file-keys] could not export the KEK of {lineage} at epoch {epoch}: {e}");
            return;
        }
    };
    if let Err(e) = remember_kek(provider.raw_conn(), &lineage, epoch, &kek) {
        crate::diag!("[file-keys] could not keep the KEK of {lineage} at epoch {epoch}: {e}");
    }
}

fn remember_kek(conn: &Connection, lineage: &str, epoch: i64, kek: &[u8]) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO file_kek_history (mls_group_id, epoch, kek) VALUES (?1, ?2, ?3)",
        rusqlite::params![lineage, epoch, kek],
    )?;
    conn.execute(
        "DELETE FROM file_kek_history WHERE mls_group_id = ?1 AND epoch NOT IN \
             (SELECT epoch FROM file_kek_history WHERE mls_group_id = ?1 \
              ORDER BY epoch DESC LIMIT ?2)",
        rusqlite::params![lineage, KEPT_KEKS_PER_GROUP],
    )?;
    Ok(())
}

fn kept_kek(
    conn: &Connection,
    mls_conversation_id: &str,
    generation: i64,
    epoch: i64,
) -> Option<Zeroizing<Vec<u8>>> {
    let lineage = super::mls::generation::mls_group_id_str(mls_conversation_id, generation);
    conn.query_row(
        "SELECT kek FROM file_kek_history WHERE mls_group_id = ?1 AND epoch = ?2",
        rusqlite::params![lineage, epoch],
        |row| row.get::<_, Vec<u8>>(0),
    )
    .optional()
    .ok()
    .flatten()
    .map(Zeroizing::new)
}

struct Cached {
    ring: Ring,
    wrapped_at: (i64, i64),
    rotation_owed: Option<(i64, i64)>,
}

type CacheRow = (Vec<u8>, i64, i64, Option<i64>, Option<i64>);

fn read_cache(conn: &Connection, mls_conversation_id: &str) -> Result<Option<Cached>> {
    let row: Option<CacheRow> = conn
        .query_row(
            "SELECT keyring, wrapped_generation, wrapped_epoch, \
                    rotation_owed_generation, rotation_owed_epoch \
             FROM file_key_cache WHERE conversation_id = ?1",
            rusqlite::params![mls_conversation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()
        .map_err(|e| Error::Other(anyhow::anyhow!("file key cache read: {e}")))?;
    let Some((bytes, wg, we, og, oe)) = row else {
        return Ok(None);
    };
    Ok(Some(Cached {
        ring: Ring::from_bytes(bytes)?,
        wrapped_at: (wg, we),
        rotation_owed: og.zip(oe),
    }))
}

/// Store `ring` wrapped at `(g, e)`. Never overwrites a longer cached ring, so a
/// stale server row can't undo a local rotation.
fn write_cache(conn: &Connection, mls_conversation_id: &str, ring: &Ring, g: i64, e: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO file_key_cache \
             (conversation_id, keyring, wrapped_generation, wrapped_epoch, updated_at) \
         VALUES (?1, ?2, ?3, ?4, datetime('now')) \
         ON CONFLICT(conversation_id) DO UPDATE SET \
             keyring = excluded.keyring, \
             wrapped_generation = excluded.wrapped_generation, \
             wrapped_epoch = excluded.wrapped_epoch, \
             updated_at = excluded.updated_at \
         WHERE length(excluded.keyring) >= length(file_key_cache.keyring)",
        rusqlite::params![mls_conversation_id, ring.0.as_slice(), g, e],
    )
    .map_err(|e| Error::Other(anyhow::anyhow!("file key cache write: {e}")))?;
    Ok(())
}

fn set_rotation_owed(conn: &Connection, mls_conversation_id: &str, owed: Option<(i64, i64)>) -> Result<()> {
    conn.execute(
        "UPDATE file_key_cache SET rotation_owed_generation = ?2, rotation_owed_epoch = ?3 \
         WHERE conversation_id = ?1",
        rusqlite::params![mls_conversation_id, owed.map(|o| o.0), owed.map(|o| o.1)],
    )
    .map_err(|e| Error::Other(anyhow::anyhow!("file key cache owed-mark: {e}")))?;
    Ok(())
}

async fn post_ring(
    state: &Arc<AppState>,
    mls_conversation_id: &str,
    ring: &Ring,
    kek: &Kek,
    actor_id: &str,
    op: FileKeystateOp,
) -> Result<bool> {
    let device_id = {
        let guard = state.device_id.lock().await;
        guard
            .clone()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("no device id for file keystate")))?
    };
    let (nonce, wrapped) = seal(&kek.kek, &ring.0)?;
    let body = pollis_api::file_keys::UpsertFileKeystateBody {
        conversation_id: mls_conversation_id.to_string(),
        wrapped_keyring: b64(&wrapped),
        nonce: b64(&nonce),
        key_version: ring.version(),
        generation: kek.generation,
        epoch: kek.epoch,
        device_id,
        actor_id: Some(actor_id.to_string()),
        op,
    };
    let resp: pollis_api::file_keys::FileKeystateUpserted =
        super::mls::ds_post_json(state, &body).await?;
    let pollis_api::file_keys::FileKeystateUpserted::Ok { updated } = resp;
    Ok(updated)
}

/// Unwrap the DS's ring into the cache when this device is at its epoch or kept
/// that epoch's KEK. `Ok(None)` with no row, or a wrap from an epoch this device
/// has no KEK for (not reached yet, or from before it joined).
async fn adopt_from_server(
    state: &Arc<AppState>,
    mls_conversation_id: &str,
    user_id: &str,
) -> Result<Option<Ring>> {
    let resp = super::ds_reads::read_file_keystate(state, mls_conversation_id, user_id).await?;
    let Some(ks) = resp.keystate else {
        return Ok(None);
    };
    let wrapped = super::ds_reads::decode_b64("file keystate wrap", &ks.wrapped_keyring)?;
    let nonce = super::ds_reads::decode_b64("file keystate nonce", &ks.nonce)?;
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    let kek = match current_kek(db.conn(), mls_conversation_id) {
        Some(k) if (k.generation, k.epoch) == (ks.generation, ks.epoch) => k.kek,
        _ => match kept_kek(db.conn(), mls_conversation_id, ks.generation, ks.epoch) {
            Some(k) => k,
            None => return Ok(None),
        },
    };
    let ring = Ring::from_bytes(open(&kek, &nonce, &wrapped)?)?;
    if ring.version() != ks.key_version {
        return Err(Error::Crypto(
            "file key ring length disagrees with the stored key_version".into(),
        ));
    }
    // Marked where the DS's copy is wrapped; if opened with a kept KEK, the next
    // re-wrap moves the DS forward.
    write_cache(db.conn(), mls_conversation_id, &ring, ks.generation, ks.epoch)?;
    Ok(read_cache(db.conn(), mls_conversation_id)?.map(|c| c.ring))
}

async fn cached_ring(state: &Arc<AppState>, mls_conversation_id: &str) -> Result<Option<Cached>> {
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    read_cache(db.conn(), mls_conversation_id)
}

/// The conversation's ring: cached, adopted, or (with `mint_if_absent`) minted;
/// a lost mint race adopts the winner's.
async fn ensure_ring(
    state: &Arc<AppState>,
    mls_conversation_id: &str,
    user_id: &str,
    mint_if_absent: bool,
) -> Result<Option<Ring>> {
    if let Some(c) = cached_ring(state, mls_conversation_id).await? {
        return Ok(Some(c.ring));
    }
    if let Some(ring) = adopt_from_server(state, mls_conversation_id, user_id).await? {
        return Ok(Some(ring));
    }
    if !mint_if_absent {
        return Ok(None);
    }
    let resp = super::ds_reads::read_file_keystate(state, mls_conversation_id, user_id).await?;
    if resp.keystate.is_some() {
        // A ring exists but is wrapped at an epoch we can't open yet.
        return Ok(None);
    }
    let kek = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        match current_kek(db.conn(), mls_conversation_id) {
            Some(k) => k,
            None => return Ok(None),
        }
    };
    let ring = Ring::fresh(kek.generation, kek.epoch);
    if post_ring(state, mls_conversation_id, &ring, &kek, user_id, FileKeystateOp::Mint).await? {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        write_cache(db.conn(), mls_conversation_id, &ring, kek.generation, kek.epoch)?;
        return Ok(Some(ring));
    }
    adopt_from_server(state, mls_conversation_id, user_id).await
}

/// Perform any rotation this device owes for a removal; the mark clears once the
/// newest key was added at or after the removal point. Best-effort: the mark
/// survives for the next pass.
async fn settle_owed_rotation(state: &Arc<AppState>, mls_conversation_id: &str, user_id: &str) -> Result<()> {
    for _ in 0..3 {
        let Some(cached) = cached_ring(state, mls_conversation_id).await? else {
            return Ok(());
        };
        let Some(owed) = cached.rotation_owed else {
            return Ok(());
        };
        if cached.ring.newest_added_at() >= owed {
            let guard = state.local_db.lock().await;
            if let Some(db) = guard.as_ref() {
                set_rotation_owed(db.conn(), mls_conversation_id, None)?;
            }
            return Ok(());
        }
        let kek = {
            let guard = state.local_db.lock().await;
            let db = guard
                .as_ref()
                .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
            match current_kek(db.conn(), mls_conversation_id) {
                Some(k) => k,
                None => return Ok(()),
            }
        };
        let rotated = cached.ring.with_new_key(kek.generation, kek.epoch);
        if post_ring(state, mls_conversation_id, &rotated, &kek, user_id, FileKeystateOp::Rotate).await? {
            let guard = state.local_db.lock().await;
            let db = guard
                .as_ref()
                .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
            write_cache(db.conn(), mls_conversation_id, &rotated, kek.generation, kek.epoch)?;
            set_rotation_owed(db.conn(), mls_conversation_id, None)?;
            return Ok(());
        }
        // Another device rotated or re-wrapped first: adopt its ring if we can open it,
        // else the mark keeps us retrying.
        if adopt_from_server(state, mls_conversation_id, user_id).await?.is_none() {
            return Ok(());
        }
    }
    Ok(())
}

/// Record a committed member removal, then rotate. Called right after the removal
/// merged, so the current epoch is the first without the removed member.
pub async fn on_member_removed(state: &Arc<AppState>, mls_conversation_id: &str) {
    let Ok(user_id) = super::mls::current_user_id(state).await else {
        return;
    };
    if let Err(e) = ensure_ring(state, mls_conversation_id, &user_id, false).await {
        crate::diag!("[file-keys] ring fetch before rotation failed for {mls_conversation_id}: {e}");
    }
    {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else {
            return;
        };
        let Some(kek) = current_kek(db.conn(), mls_conversation_id) else {
            return;
        };
        if let Err(e) = set_rotation_owed(db.conn(), mls_conversation_id, Some((kek.generation, kek.epoch))) {
            crate::diag!("[file-keys] could not record owed rotation for {mls_conversation_id}: {e}");
            return;
        }
    }
    if let Err(e) = settle_owed_rotation(state, mls_conversation_id, &user_id).await {
        crate::diag!("[file-keys] rotation failed for {mls_conversation_id} (will retry): {e}");
    }
}

/// Re-wrap the cached ring if behind and settle any owed rotation; called at
/// every epoch-advance site.
pub async fn maybe_rewrap_file_keys(state: &Arc<AppState>, mls_conversation_id: &str) {
    let Ok(user_id) = super::mls::current_user_id(state).await else {
        return;
    };
    if let Err(e) = settle_owed_rotation(state, mls_conversation_id, &user_id).await {
        crate::diag!("[file-keys] owed rotation failed for {mls_conversation_id} (will retry): {e}");
    }
    rewrap_if_behind(state, mls_conversation_id, &user_id).await;
}

/// Post the cached ring wrapped at the current epoch if the known wrap is older.
async fn rewrap_if_behind(state: &Arc<AppState>, mls_conversation_id: &str, user_id: &str) {
    let staged = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else {
            return;
        };
        let cached = match read_cache(db.conn(), mls_conversation_id) {
            Ok(Some(c)) => c,
            Ok(None) => return,
            Err(e) => {
                crate::diag!("[file-keys] cache read failed for {mls_conversation_id}: {e}");
                return;
            }
        };
        let Some(kek) = current_kek(db.conn(), mls_conversation_id) else {
            return;
        };
        if (kek.generation, kek.epoch) <= cached.wrapped_at {
            return;
        }
        (cached.ring, kek)
    };
    let (ring, kek) = staged;
    match post_ring(state, mls_conversation_id, &ring, &kek, user_id, FileKeystateOp::Rewrap).await {
        Ok(true) => {
            let guard = state.local_db.lock().await;
            if let Some(db) = guard.as_ref() {
                if let Err(e) = write_cache(db.conn(), mls_conversation_id, &ring, kek.generation, kek.epoch) {
                    crate::diag!("[file-keys] cache marker update failed for {mls_conversation_id}: {e}");
                }
            }
        }
        Ok(false) => {
            // Someone already wrapped at this epoch, or rotated: pick up a longer ring.
            if let Err(e) = adopt_from_server(state, mls_conversation_id, user_id).await {
                crate::diag!("[file-keys] adopt after refused re-wrap failed for {mls_conversation_id}: {e}");
            }
        }
        Err(e) => {
            crate::diag!("[file-keys] re-wrap failed for {mls_conversation_id} (will retry): {e}");
        }
    }
}

/// Best-effort adopt right after a Welcome or external join.
pub async fn try_adopt_file_keys(state: &Arc<AppState>, mls_conversation_id: &str) {
    let Ok(user_id) = super::mls::current_user_id(state).await else {
        return;
    };
    if let Err(e) = ensure_ring(state, mls_conversation_id, &user_id, false).await {
        crate::diag!("[file-keys] adopt failed for {mls_conversation_id} (will retry lazily): {e}");
    }
}

/// Mint the ring at conversation creation. Best-effort: the first upload mints
/// otherwise.
pub async fn mint_file_keys_for_new_group(state: &Arc<AppState>, mls_conversation_id: &str) {
    let Ok(user_id) = super::mls::current_user_id(state).await else {
        return;
    };
    if let Err(e) = ensure_ring(state, mls_conversation_id, &user_id, true).await {
        crate::diag!("[file-keys] create-time mint failed for {mls_conversation_id} (first upload will mint): {e}");
    }
}

/// The conversation's file key hasn't reached this device: the only wrap is at an
/// epoch it has no KEK for. Not a message failure; the outbox keeps the send
/// queued.
#[derive(Debug)]
pub struct FileKeyUnavailable;

impl std::fmt::Display for FileKeyUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "this conversation's file key has not reached this device yet — \
             the attachment will send as soon as it does",
        )
    }
}

impl std::error::Error for FileKeyUnavailable {}

fn file_key_unavailable() -> Error {
    Error::Other(anyhow::Error::new(FileKeyUnavailable))
}

pub fn is_file_key_unavailable(e: &Error) -> bool {
    matches!(e, Error::Other(any) if any.downcast_ref::<FileKeyUnavailable>().is_some())
}

/// [`ensure_ring`], retried once after catching the conversation up: uploads run
/// before the send's own catch-up, so a device one commit behind would otherwise
/// fail every attempt. Callers must hold no MLS lock.
async fn ensure_ring_caught_up(
    state: &Arc<AppState>,
    mls_conversation_id: &str,
    user_id: &str,
    mint_if_absent: bool,
) -> Result<Option<Ring>> {
    if let Some(ring) = ensure_ring(state, mls_conversation_id, user_id, mint_if_absent).await? {
        return Ok(Some(ring));
    }
    if let Err(e) =
        crate::commands::messages::catch_up_mls_group_interleaved(state, mls_conversation_id, user_id)
            .await
    {
        crate::diag!("[file-keys] catch-up before re-reading the ring of {mls_conversation_id} failed: {e}");
    }
    ensure_ring(state, mls_conversation_id, user_id, mint_if_absent).await
}

/// The key and version a new upload must use: the newest, after settling any
/// owed rotation.
pub(crate) async fn upload_key(
    state: &Arc<AppState>,
    mls_conversation_id: &str,
    user_id: &str,
) -> Result<(i64, Zeroizing<[u8; KEY_LEN]>)> {
    let ring = ensure_ring_caught_up(state, mls_conversation_id, user_id, true)
        .await?
        .ok_or_else(file_key_unavailable)?;
    settle_owed_rotation(state, mls_conversation_id, user_id).await?;
    // Opened with a kept KEK means the DS's wrap is at an epoch we left; re-wrap so
    // other devices here can open it.
    rewrap_if_behind(state, mls_conversation_id, user_id).await;
    let ring = cached_ring(state, mls_conversation_id)
        .await?
        .map(|c| c.ring)
        .unwrap_or(ring);
    let version = ring.version();
    let key = ring.key(version).expect("a ring has at least one key");
    Ok((version, key))
}

/// The ring key at `version`, refetching when the cached ring is shorter.
pub(crate) async fn key_at_version(
    state: &Arc<AppState>,
    mls_conversation_id: &str,
    user_id: &str,
    version: i64,
) -> Result<Zeroizing<[u8; KEY_LEN]>> {
    if let Some(key) = ensure_ring_caught_up(state, mls_conversation_id, user_id, false)
        .await?
        .and_then(|r| r.key(version))
    {
        return Ok(key);
    }
    adopt_from_server(state, mls_conversation_id, user_id)
        .await?
        .and_then(|r| r.key(version))
        .ok_or_else(|| {
            Error::Other(anyhow::anyhow!(
                "file key version {version} is not available on this device yet"
            ))
        })
}

/// Test harness only: the newest ring version this device can open. `None` is
/// what a removed member must see.
#[cfg(feature = "test-harness")]
pub async fn test_file_key_version(
    conversation_id: String,
    state: &Arc<AppState>,
) -> Result<Option<i64>> {
    let user_id = super::mls::current_user_id(state).await?;
    let mls_id = super::mls::ds_reads::resolve_mls_group(state, &conversation_id).await?;
    ensure_ring(state, &mls_id, &user_id, false).await?;
    // A refused read (removed member) means "cannot reach the ring".
    let _ = adopt_from_server(state, &mls_id, &user_id).await;
    Ok(cached_ring(state, &mls_id).await?.map(|c| c.ring.version()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_file_gets_its_own_key_under_one_ring_key() {
        let ring_key = [9u8; 32];
        let (k1, n1) = derive_file_key(&ring_key, &[1u8; 32]);
        let (k2, n2) = derive_file_key(&ring_key, &[2u8; 32]);
        assert_ne!(k1.as_ref(), k2.as_ref());
        assert_ne!(n1, n2);
    }

    #[test]
    fn the_file_id_alone_does_not_determine_the_key() {
        // The server knows every file id; it must not reproduce the key without the
        // ring key.
        let file_id = [5u8; 32];
        let (a, _) = derive_file_key(&[1u8; 32], &file_id);
        let (b, _) = derive_file_key(&[2u8; 32], &file_id);
        assert_ne!(a.as_ref(), b.as_ref());
    }

    /// #1382's invariant: everything the server holds (file id and ciphertext) can't
    /// decrypt a file, under the retired convergent derivation or the new one.
    #[test]
    fn what_the_server_holds_cannot_decrypt_a_file() {
        let plaintext = b"budget-2026.pdf contents".to_vec();
        let mut ring_key = [0u8; 32];
        OsRng.fill_bytes(&mut ring_key);
        let mut file_id = [0u8; 32];
        OsRng.fill_bytes(&mut file_id);

        let (key, nonce) = derive_file_key(&ring_key, &file_id);
        let ciphertext = crate::commands::r2::encrypt_chunked(&plaintext, &key, &nonce);

        let hk = Hkdf::<Sha256>::new(None, &file_id);
        let mut convergent_key = [0u8; 32];
        hk.expand(b"pollis-att-key", &mut convergent_key).unwrap();
        let mut convergent_nonce = [0u8; 12];
        hk.expand(b"pollis-att-nonce", &mut convergent_nonce).unwrap();
        assert!(crate::commands::r2::decrypt_chunked(&ciphertext, &convergent_key, &convergent_nonce).is_err());

        let (k, n) = derive_file_key(&file_id, &file_id);
        assert!(crate::commands::r2::decrypt_chunked(&ciphertext, &k, &n).is_err());

        assert_eq!(crate::commands::r2::decrypt_chunked(&ciphertext, &key, &nonce).unwrap(), plaintext);
    }

    #[test]
    fn a_rotation_appends_a_new_key_and_keeps_the_old_ones() {
        let ring = Ring::fresh(0, 3);
        let rotated = ring.with_new_key(0, 7);
        assert_eq!(rotated.version(), 2);
        assert_eq!(rotated.key(1).unwrap().as_ref(), ring.key(1).unwrap().as_ref());
        assert_ne!(rotated.key(2).unwrap().as_ref(), ring.key(1).unwrap().as_ref());
        assert_eq!(rotated.newest_added_at(), (0, 7));
        assert!(rotated.key(3).is_none());
        assert!(rotated.key(0).is_none());
    }

    #[test]
    fn a_ring_of_the_wrong_length_is_rejected() {
        assert!(Ring::from_bytes(vec![0u8; RING_ENTRY_LEN + 1]).is_err());
        assert!(Ring::from_bytes(Vec::new()).is_err());
    }

    #[test]
    fn the_wrap_opens_only_under_its_own_kek() {
        let ring = Ring::fresh(0, 1);
        let (nonce, ct) = seal(&[3u8; 32], &ring.0).unwrap();
        assert!(open(&[4u8; 32], &nonce, &ct).is_err());
        assert_eq!(open(&[3u8; 32], &nonce, &ct).unwrap(), ring.0.to_vec());
    }
}
