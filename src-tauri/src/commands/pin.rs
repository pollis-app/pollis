//! PIN-wrapped per-user key storage.
//!
//! The 4-digit PIN is a *local unlock* factor, not a server credential.
//! Entropy is ~13 bits, so on-disk material is protected by Argon2id
//! (memory-hard, tuned ~250ms/op) + XChaCha20-Poly1305.
//!
//! Concepts:
//! - `pin_meta_{user_id}` — carries the KDF parameters, salt, a verifier
//!   blob (constant plaintext), and the failed-attempt counter.
//!   Lets us reject a wrong PIN without unwrapping the big keys.
//! - `db_key_wrapped_{user_id}` — the SQLCipher key, AEAD-sealed under
//!   the KEK derived from the PIN.
//! - `account_id_key_wrapped_{user_id}` — the Ed25519 account identity
//!   key, AEAD-sealed under the same KEK.
//!
//! `pin_meta` holds the Argon2 parameters + salt. The two wrapped-key
//! blobs carry only a fresh nonce + ciphertext and reuse the already-
//! derived KEK — one Argon2 evaluation per unlock, not three.
//!
//! Nothing in this module runs until the frontend opts into the new
//! flow. Existing unwrapped `db_key_{user_id}` / `account_id_key_{user_id}`
//! slots are left alone by stage 3; stage 6 cuts them over.

use aes_gcm::aead::OsRng as AeadOsRng;
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tauri::State;
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::state::AppState;

// ── Keystore slot names ──────────────────────────────────────────────

const PIN_META_SLOT: &str = "pin_meta";
const DB_KEY_WRAPPED_SLOT: &str = "db_key_wrapped";
const ACCOUNT_ID_KEY_WRAPPED_SLOT: &str = "account_id_key_wrapped";
const DB_KEY_SLOT_LEGACY: &str = "db_key";
const ACCOUNT_ID_KEY_SLOT_LEGACY: &str = "account_id_key";
/// Legacy `session_{user_id}` blob — duplicate source of truth for
/// "who was signed in," written by pre-PIN `verify_otp`. A transient
/// read failure on this slot was one of the paths behind #184. Stage 3
/// added the PIN as the real unlock factor; `set_pin` deletes the blob
/// once the wrap succeeds so we don't have two sources of truth during
/// the migration window.
const SESSION_SLOT_LEGACY: &str = "session";

// ── KDF tuning ───────────────────────────────────────────────────────
//
// Target ~250ms on a mid-range M1 / Ryzen 5. Bumps are safe: `pin_meta`
// carries its own params, so a re-wrap (via `set_pin`) freely moves to
// newer parameters without migration.

const ARGON2_M_COST_KIB: u32 = 64 * 1024; // 64 MiB
const ARGON2_T_COST: u32 = 3;
const ARGON2_P_COST: u32 = 1;
const KEK_LEN: usize = 32;
const SALT_LEN: usize = 16;
const VERIFIER_PLAINTEXT: &[u8; 16] = b"pollis-pin-ok\0\0\0";

// ── Rate limit ───────────────────────────────────────────────────────

/// 10 wrong attempts wipe the wrapped blobs and force re-enrollment via
/// Secret Key. No time-based backoff — Argon2id's per-attempt cost is
/// already the real defense against an offline brute-forcer.
pub const MAX_FAILED_ATTEMPTS: u32 = 10;

// ── Blob format (pin_meta) ───────────────────────────────────────────
//
// Fixed layout, not serde-encoded. Version prefix guarantees we can
// evolve later without touching the parser:
//
//   version (1) = 1
//   m_cost_kib (4, big-endian)
//   t_cost (1)
//   p_cost (1)
//   salt (16)
//   nonce (24)
//   verifier_ct_len (2, big-endian) — always 32 (16 plaintext + 16 tag)
//   verifier_ct||tag (32)
//   failed_attempts (4, big-endian)
//   last_attempt_unix (8, big-endian)
//
// Total: 93 bytes. The attempt counters sit outside the ciphertext on
// purpose — the threat model is a local attacker who already has
// keystore read access, and they can count attempts regardless.

const PIN_META_VERSION: u8 = 1;

#[derive(Debug, Clone)]
struct PinMeta {
    m_cost_kib: u32,
    t_cost: u32,
    p_cost: u32,
    salt: [u8; SALT_LEN],
    verifier_nonce: [u8; 24],
    verifier_ct: Vec<u8>, // 32 bytes
    failed_attempts: u32,
    last_attempt_unix: u64,
}

impl PinMeta {
    fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(93);
        out.push(PIN_META_VERSION);
        out.extend_from_slice(&self.m_cost_kib.to_be_bytes());
        out.push(self.t_cost as u8);
        out.push(self.p_cost as u8);
        out.extend_from_slice(&self.salt);
        out.extend_from_slice(&self.verifier_nonce);
        out.extend_from_slice(&(self.verifier_ct.len() as u16).to_be_bytes());
        out.extend_from_slice(&self.verifier_ct);
        out.extend_from_slice(&self.failed_attempts.to_be_bytes());
        out.extend_from_slice(&self.last_attempt_unix.to_be_bytes());
        out
    }

    fn from_bytes(bytes: &[u8]) -> Result<Self> {
        // 1 + 4 + 1 + 1 + 16 + 24 + 2 + ct + 4 + 8; minimum with 0-byte ct = 61
        if bytes.len() < 61 {
            return Err(Error::Crypto("pin_meta: short blob".into()));
        }
        let mut cur = 0usize;
        let version = bytes[cur];
        cur += 1;
        if version != PIN_META_VERSION {
            return Err(Error::Crypto(format!(
                "pin_meta: unknown version {version}"
            )));
        }
        let m_cost_kib = u32::from_be_bytes(bytes[cur..cur + 4].try_into().unwrap());
        cur += 4;
        let t_cost = bytes[cur] as u32;
        cur += 1;
        let p_cost = bytes[cur] as u32;
        cur += 1;
        let mut salt = [0u8; SALT_LEN];
        salt.copy_from_slice(&bytes[cur..cur + SALT_LEN]);
        cur += SALT_LEN;
        let mut verifier_nonce = [0u8; 24];
        verifier_nonce.copy_from_slice(&bytes[cur..cur + 24]);
        cur += 24;
        let ct_len = u16::from_be_bytes(bytes[cur..cur + 2].try_into().unwrap()) as usize;
        cur += 2;
        if bytes.len() < cur + ct_len + 12 {
            return Err(Error::Crypto("pin_meta: truncated".into()));
        }
        let verifier_ct = bytes[cur..cur + ct_len].to_vec();
        cur += ct_len;
        let failed_attempts =
            u32::from_be_bytes(bytes[cur..cur + 4].try_into().unwrap());
        cur += 4;
        let last_attempt_unix =
            u64::from_be_bytes(bytes[cur..cur + 8].try_into().unwrap());
        Ok(Self {
            m_cost_kib,
            t_cost,
            p_cost,
            salt,
            verifier_nonce,
            verifier_ct,
            failed_attempts,
            last_attempt_unix,
        })
    }
}

// ── Wrapped-key blob format (db_key / account_id_key) ────────────────
//
//   version (1) = 1
//   nonce (24)
//   ct_len (2, BE)
//   ct||tag

const WRAPPED_VERSION: u8 = 1;

fn wrap_bytes(kek: &[u8; KEK_LEN], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(kek.into());
    let nonce = XChaCha20Poly1305::generate_nonce(&mut AeadOsRng);
    let ct = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| Error::Crypto(format!("wrap: {e}")))?;

    let mut out = Vec::with_capacity(1 + 24 + 2 + ct.len());
    out.push(WRAPPED_VERSION);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&(ct.len() as u16).to_be_bytes());
    out.extend_from_slice(&ct);
    Ok(out)
}

fn unwrap_bytes(kek: &[u8; KEK_LEN], blob: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
    if blob.len() < 1 + 24 + 2 {
        return Err(Error::Crypto("wrapped: short blob".into()));
    }
    if blob[0] != WRAPPED_VERSION {
        return Err(Error::Crypto(format!(
            "wrapped: unknown version {}",
            blob[0]
        )));
    }
    let nonce = XNonce::from_slice(&blob[1..25]);
    let ct_len = u16::from_be_bytes(blob[25..27].try_into().unwrap()) as usize;
    if blob.len() < 27 + ct_len {
        return Err(Error::Crypto("wrapped: truncated".into()));
    }
    let ct = &blob[27..27 + ct_len];
    let cipher = XChaCha20Poly1305::new(kek.into());
    let pt = cipher
        .decrypt(nonce, ct)
        .map_err(|_| Error::Crypto("wrapped: decrypt failed".into()))?;
    Ok(Zeroizing::new(pt))
}

// ── KDF ──────────────────────────────────────────────────────────────

fn derive_kek(
    pin: &str,
    salt: &[u8; SALT_LEN],
    m_cost_kib: u32,
    t_cost: u32,
    p_cost: u32,
) -> Result<Zeroizing<[u8; KEK_LEN]>> {
    let params = Params::new(m_cost_kib, t_cost, p_cost, Some(KEK_LEN))
        .map_err(|e| Error::Crypto(format!("argon2 params: {e}")))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; KEK_LEN]);
    argon
        .hash_password_into(pin.as_bytes(), salt, &mut *out)
        .map_err(|e| Error::Crypto(format!("argon2 hash: {e}")))?;
    Ok(out)
}

fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ── Helpers for the commands ─────────────────────────────────────────

fn validate_pin(pin: &str) -> Result<()> {
    if pin.len() != 4 || !pin.chars().all(|c| c.is_ascii_digit()) {
        return Err(Error::Crypto("PIN must be 4 digits".into()));
    }
    Ok(())
}

async fn load_pin_meta(
    keystore: &dyn crate::keystore::Keystore,
    user_id: &str,
) -> Result<Option<PinMeta>> {
    match keystore.load_for_user(PIN_META_SLOT, user_id).await? {
        None => Ok(None),
        Some(bytes) => Ok(Some(PinMeta::from_bytes(&bytes)?)),
    }
}

async fn store_pin_meta(
    keystore: &dyn crate::keystore::Keystore,
    user_id: &str,
    meta: &PinMeta,
) -> Result<()> {
    keystore
        .store_for_user(PIN_META_SLOT, user_id, &meta.to_bytes())
        .await
}

async fn nuke_wrapped(
    keystore: &dyn crate::keystore::Keystore,
    user_id: &str,
) -> Result<()> {
    let _ = keystore.delete_for_user(PIN_META_SLOT, user_id).await;
    let _ = keystore
        .delete_for_user(DB_KEY_WRAPPED_SLOT, user_id)
        .await;
    let _ = keystore
        .delete_for_user(ACCOUNT_ID_KEY_WRAPPED_SLOT, user_id)
        .await;
    Ok(())
}

// ── In-memory unlock state ───────────────────────────────────────────

/// Per-user in-memory unlock snapshot. Zeroized on drop.
pub struct UnlockState {
    pub user_id: String,
    pub db_key: Zeroizing<Vec<u8>>,
    pub account_id_key: Zeroizing<Vec<u8>>,
}

// ── Commands ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct UnlockStateSnapshot {
    pub last_active_user: Option<String>,
    pub is_unlocked: bool,
    /// True when a PIN has been set for `last_active_user`. Lets the
    /// frontend route between "enter PIN" and "set PIN to finish signup."
    pub pin_set: bool,
}

/// Set the PIN for the currently-active user.
///
/// * If `old_pin` is `None`, this is first-time setup: we look up the
///   user's existing unwrapped `db_key_{uid}` / `account_id_key_{uid}`
///   from the keystore (creating a fresh `db_key` if none exists, same
///   policy as `AppState::load_user_db`) and wrap them under the new
///   PIN.
/// * If `old_pin` is `Some`, we unwrap with the old PIN and re-wrap
///   under the new one. The write is atomic from the caller's point of
///   view — wrapped blobs only hit the keystore after both unwraps and
///   both new wraps succeed.
#[tauri::command]
pub async fn set_pin(
    state: State<'_, Arc<AppState>>,
    old_pin: Option<String>,
    new_pin: String,
) -> Result<()> {
    validate_pin(&new_pin)?;

    let user_id = crate::accounts::read_accounts_index()
        .unwrap_or_default()
        .last_active_user
        .ok_or_else(|| Error::Other(anyhow::anyhow!("no active user; call verify_otp first")))?;

    let keystore = state.keystore.as_ref();

    // Source the raw bytes we're about to wrap.
    let (db_key, account_id_key): (Zeroizing<Vec<u8>>, Zeroizing<Vec<u8>>) = match old_pin {
        Some(ref old) => {
            validate_pin(old)?;
            // Change path: unwrap with old PIN, rewrap under new PIN.
            let unlocked = unlock_inner(keystore, &user_id, old).await?;
            (unlocked.db_key, unlocked.account_id_key)
        }
        None => {
            // Initial-set path: the user's keys currently live at the
            // legacy unwrapped slots. Load or generate `db_key`, and
            // require an `account_id_key` to exist.
            let db_key = match keystore
                .load_for_user(DB_KEY_SLOT_LEGACY, &user_id)
                .await?
            {
                Some(k) => Zeroizing::new(k),
                None => {
                    let mut k = vec![0u8; 32];
                    rand::rngs::OsRng.fill_bytes(&mut k);
                    keystore
                        .store_for_user(DB_KEY_SLOT_LEGACY, &user_id, &k)
                        .await?;
                    Zeroizing::new(k)
                }
            };
            let account_id_key = keystore
                .load_for_user(ACCOUNT_ID_KEY_SLOT_LEGACY, &user_id)
                .await?
                .map(Zeroizing::new)
                .ok_or_else(|| {
                    Error::Other(anyhow::anyhow!(
                        "account_id_key missing; initialize_identity must run before set_pin"
                    ))
                })?;
            (db_key, account_id_key)
        }
    };

    // Fresh salt + nonce on every (re)wrap.
    let mut salt = [0u8; SALT_LEN];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let kek = derive_kek(
        &new_pin,
        &salt,
        ARGON2_M_COST_KIB,
        ARGON2_T_COST,
        ARGON2_P_COST,
    )?;

    // Verifier blob: encrypt a fixed plaintext so we can reject wrong
    // PINs without unwrapping the big keys.
    let verifier_nonce_raw = XChaCha20Poly1305::generate_nonce(&mut AeadOsRng);
    let verifier_nonce: [u8; 24] = verifier_nonce_raw.into();
    let verifier_cipher = XChaCha20Poly1305::new((&*kek).into());
    let verifier_ct = verifier_cipher
        .encrypt(&verifier_nonce_raw, VERIFIER_PLAINTEXT.as_slice())
        .map_err(|e| Error::Crypto(format!("verifier encrypt: {e}")))?;

    let db_key_blob = wrap_bytes(&kek, &db_key)?;
    let account_id_key_blob = wrap_bytes(&kek, &account_id_key)?;

    let meta = PinMeta {
        m_cost_kib: ARGON2_M_COST_KIB,
        t_cost: ARGON2_T_COST,
        p_cost: ARGON2_P_COST,
        salt,
        verifier_nonce,
        verifier_ct,
        failed_attempts: 0,
        last_attempt_unix: now_unix(),
    };

    // All three writes land together. If the process dies after two of
    // three, the next `unlock` sees an inconsistent state; the user
    // re-runs set_pin to heal. No half-wrapped account is produced.
    keystore
        .store_for_user(DB_KEY_WRAPPED_SLOT, &user_id, &db_key_blob)
        .await?;
    keystore
        .store_for_user(ACCOUNT_ID_KEY_WRAPPED_SLOT, &user_id, &account_id_key_blob)
        .await?;
    store_pin_meta(keystore, &user_id, &meta).await?;

    // Initial-set path only: drop the legacy session blob now that a
    // real unlock factor exists. We keep the unwrapped `db_key` /
    // `account_id_key` slots alive until stage 6 so the rest of the
    // app (which still reads them directly) keeps functioning.
    if old_pin.is_none() {
        let _ = keystore
            .delete_for_user(SESSION_SLOT_LEGACY, &user_id)
            .await;
    }

    // Seed AppState.unlock so the caller doesn't have to re-enter the
    // PIN they literally just set.
    *state.unlock.lock().await = Some(UnlockState {
        user_id: user_id.clone(),
        db_key,
        account_id_key,
    });

    Ok(())
}

#[derive(Debug, Serialize)]
pub struct UnlockOutcome {
    pub user_id: String,
    pub attempts_remaining: Option<u32>,
}

/// Verify a PIN and populate `AppState.unlock`.
///
/// Returns the user_id that was unlocked. The frontend then calls the
/// existing identity / session commands the same way it did before.
#[tauri::command]
pub async fn unlock(
    state: State<'_, Arc<AppState>>,
    user_id: String,
    pin: String,
) -> Result<UnlockOutcome> {
    validate_pin(&pin)?;
    let keystore = state.keystore.as_ref();
    let unlocked = unlock_inner(keystore, &user_id, &pin).await?;

    *state.unlock.lock().await = Some(UnlockState {
        user_id: unlocked.user_id.clone(),
        db_key: unlocked.db_key,
        account_id_key: unlocked.account_id_key,
    });
    Ok(UnlockOutcome {
        user_id: unlocked.user_id,
        attempts_remaining: None,
    })
}

/// Shared core for verify-and-unwrap, used by both `unlock` and the
/// `set_pin(Some(old), ...)` change flow.
async fn unlock_inner(
    keystore: &dyn crate::keystore::Keystore,
    user_id: &str,
    pin: &str,
) -> Result<UnlockState> {
    let mut meta = load_pin_meta(keystore, user_id)
        .await?
        .ok_or_else(|| Error::Other(anyhow::anyhow!("PIN not set for user {user_id}")))?;

    if meta.failed_attempts >= MAX_FAILED_ATTEMPTS {
        // Defense-in-depth: if the UI missed the countdown and didn't
        // nuke already, nuke on this attempt. The account is wiped
        // locally; remote data + Secret Key recovery are untouched.
        nuke_wrapped(keystore, user_id).await?;
        return Err(Error::Other(anyhow::anyhow!(
            "pin locked out; use Secret Key recovery"
        )));
    }

    let kek = derive_kek(
        pin,
        &meta.salt,
        meta.m_cost_kib,
        meta.t_cost,
        meta.p_cost,
    )?;

    // Verify against the fixed plaintext blob first. Wrong PIN =>
    // increment counter, store, bail. Cheap relative to two full unwraps.
    let verifier_cipher = XChaCha20Poly1305::new((&*kek).into());
    let verifier_nonce = XNonce::from_slice(&meta.verifier_nonce);
    let verifier_ok = verifier_cipher
        .decrypt(verifier_nonce, meta.verifier_ct.as_slice())
        .ok()
        .map(|pt| pt.as_slice() == VERIFIER_PLAINTEXT.as_slice())
        .unwrap_or(false);

    if !verifier_ok {
        meta.failed_attempts += 1;
        meta.last_attempt_unix = now_unix();
        store_pin_meta(keystore, user_id, &meta).await?;
        let attempts_remaining =
            MAX_FAILED_ATTEMPTS.saturating_sub(meta.failed_attempts);
        if meta.failed_attempts >= MAX_FAILED_ATTEMPTS {
            nuke_wrapped(keystore, user_id).await?;
        }
        return Err(Error::Other(anyhow::anyhow!(
            "pin incorrect; {attempts_remaining} attempts remaining"
        )));
    }

    // Unwrap the two real keys.
    let db_key_blob = keystore
        .load_for_user(DB_KEY_WRAPPED_SLOT, user_id)
        .await?
        .ok_or_else(|| Error::Other(anyhow::anyhow!("db_key_wrapped missing")))?;
    let account_id_key_blob = keystore
        .load_for_user(ACCOUNT_ID_KEY_WRAPPED_SLOT, user_id)
        .await?
        .ok_or_else(|| Error::Other(anyhow::anyhow!("account_id_key_wrapped missing")))?;
    let db_key = unwrap_bytes(&kek, &db_key_blob)?;
    let account_id_key = unwrap_bytes(&kek, &account_id_key_blob)?;

    // Reset counter on success.
    if meta.failed_attempts != 0 {
        meta.failed_attempts = 0;
        meta.last_attempt_unix = now_unix();
        store_pin_meta(keystore, user_id, &meta).await?;
    }

    Ok(UnlockState {
        user_id: user_id.to_string(),
        db_key,
        account_id_key,
    })
}

/// Drop the in-memory unlock state. Does NOT close the active local DB
/// or clear the accounts index — this is the "screen lock" primitive,
/// not the "log out" one.
#[tauri::command]
pub async fn lock(state: State<'_, Arc<AppState>>) -> Result<()> {
    *state.unlock.lock().await = None;
    Ok(())
}

/// Snapshot of auth/unlock state for the frontend. Never blocks on
/// keystore reads for secrets — just reads `accounts.json` and the
/// cheap `pin_meta` blob.
#[tauri::command]
pub async fn get_unlock_state(
    state: State<'_, Arc<AppState>>,
) -> Result<UnlockStateSnapshot> {
    let index = crate::accounts::read_accounts_index().unwrap_or_default();
    let last_active_user = index.last_active_user.clone();
    let is_unlocked = state.unlock.lock().await.is_some();
    let pin_set = match &last_active_user {
        Some(uid) => state
            .keystore
            .load_for_user(PIN_META_SLOT, uid)
            .await?
            .is_some(),
        None => false,
    };
    Ok(UnlockStateSnapshot {
        last_active_user,
        is_unlocked,
        pin_set,
    })
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keystore::InMemoryKeystore;
    use std::sync::Arc as StdArc;

    async fn seed_legacy(ks: &dyn crate::keystore::Keystore, uid: &str) {
        ks.store_for_user(DB_KEY_SLOT_LEGACY, uid, &vec![7u8; 32])
            .await
            .unwrap();
        ks.store_for_user(ACCOUNT_ID_KEY_SLOT_LEGACY, uid, &vec![42u8; 32])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn wrap_unwrap_roundtrip() {
        let mut salt = [0u8; SALT_LEN];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        // Use a cheap Argon2 setting for tests — the real one is 64 MiB
        // and would make the test suite glacial.
        let kek = derive_kek("1234", &salt, 8, 1, 1).unwrap();
        let pt = b"some secret bytes";
        let blob = wrap_bytes(&kek, pt).unwrap();
        let got = unwrap_bytes(&kek, &blob).unwrap();
        assert_eq!(&*got, pt);
    }

    #[tokio::test]
    async fn wrong_kek_fails() {
        let mut salt_a = [0u8; SALT_LEN];
        rand::rngs::OsRng.fill_bytes(&mut salt_a);
        let mut salt_b = [0u8; SALT_LEN];
        rand::rngs::OsRng.fill_bytes(&mut salt_b);
        let kek_a = derive_kek("1234", &salt_a, 8, 1, 1).unwrap();
        let kek_b = derive_kek("5678", &salt_b, 8, 1, 1).unwrap();
        let blob = wrap_bytes(&kek_a, b"x").unwrap();
        assert!(unwrap_bytes(&kek_b, &blob).is_err());
    }

    #[test]
    fn pin_meta_roundtrip() {
        let meta = PinMeta {
            m_cost_kib: 1024,
            t_cost: 2,
            p_cost: 1,
            salt: [3u8; SALT_LEN],
            verifier_nonce: [4u8; 24],
            verifier_ct: vec![5u8; 32],
            failed_attempts: 7,
            last_attempt_unix: 1_234_567,
        };
        let bytes = meta.to_bytes();
        let got = PinMeta::from_bytes(&bytes).unwrap();
        assert_eq!(got.m_cost_kib, 1024);
        assert_eq!(got.t_cost, 2);
        assert_eq!(got.p_cost, 1);
        assert_eq!(got.salt, [3u8; SALT_LEN]);
        assert_eq!(got.verifier_nonce, [4u8; 24]);
        assert_eq!(got.verifier_ct, vec![5u8; 32]);
        assert_eq!(got.failed_attempts, 7);
        assert_eq!(got.last_attempt_unix, 1_234_567);
    }

    #[test]
    fn pin_validation() {
        assert!(validate_pin("1234").is_ok());
        assert!(validate_pin("0000").is_ok());
        assert!(validate_pin("12").is_err());
        assert!(validate_pin("12345").is_err());
        assert!(validate_pin("abcd").is_err());
        assert!(validate_pin("12a4").is_err());
    }

    /// End-to-end: set PIN on a seeded user, unlock with correct PIN,
    /// verify unwrapped material matches what was seeded.
    #[tokio::test]
    async fn set_and_unlock_roundtrip_via_inner() {
        let ks: StdArc<dyn crate::keystore::Keystore> =
            StdArc::new(InMemoryKeystore::new());
        let uid = "user_01";
        seed_legacy(&*ks, uid).await;

        // Simulate set_pin's wrap step directly (the tauri command
        // wrapper requires a full AppState which is heavy to build in
        // a unit test — the flows harness covers the command path).
        let mut salt = [0u8; SALT_LEN];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let kek = derive_kek("4321", &salt, 8, 1, 1).unwrap();

        let db_key = ks
            .load_for_user(DB_KEY_SLOT_LEGACY, uid)
            .await
            .unwrap()
            .unwrap();
        let acct_key = ks
            .load_for_user(ACCOUNT_ID_KEY_SLOT_LEGACY, uid)
            .await
            .unwrap()
            .unwrap();

        ks.store_for_user(DB_KEY_WRAPPED_SLOT, uid, &wrap_bytes(&kek, &db_key).unwrap())
            .await
            .unwrap();
        ks.store_for_user(
            ACCOUNT_ID_KEY_WRAPPED_SLOT,
            uid,
            &wrap_bytes(&kek, &acct_key).unwrap(),
        )
        .await
        .unwrap();

        let verifier_nonce_raw = XChaCha20Poly1305::generate_nonce(&mut AeadOsRng);
        let verifier_nonce: [u8; 24] = verifier_nonce_raw.into();
        let verifier_ct = XChaCha20Poly1305::new((&*kek).into())
            .encrypt(&verifier_nonce_raw, VERIFIER_PLAINTEXT.as_slice())
            .unwrap();
        let meta = PinMeta {
            m_cost_kib: 8,
            t_cost: 1,
            p_cost: 1,
            salt,
            verifier_nonce,
            verifier_ct,
            failed_attempts: 0,
            last_attempt_unix: 0,
        };
        store_pin_meta(&*ks, uid, &meta).await.unwrap();

        let unlocked = unlock_inner(&*ks, uid, "4321").await.unwrap();
        assert_eq!(&*unlocked.db_key, &db_key);
        assert_eq!(&*unlocked.account_id_key, &acct_key);
    }

    #[tokio::test]
    async fn wrong_pin_increments_counter() {
        let ks: StdArc<dyn crate::keystore::Keystore> =
            StdArc::new(InMemoryKeystore::new());
        let uid = "user_02";
        seed_legacy(&*ks, uid).await;

        let mut salt = [0u8; SALT_LEN];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let kek = derive_kek("1111", &salt, 8, 1, 1).unwrap();

        let db_key = ks
            .load_for_user(DB_KEY_SLOT_LEGACY, uid)
            .await
            .unwrap()
            .unwrap();
        let acct_key = ks
            .load_for_user(ACCOUNT_ID_KEY_SLOT_LEGACY, uid)
            .await
            .unwrap()
            .unwrap();
        ks.store_for_user(DB_KEY_WRAPPED_SLOT, uid, &wrap_bytes(&kek, &db_key).unwrap())
            .await
            .unwrap();
        ks.store_for_user(
            ACCOUNT_ID_KEY_WRAPPED_SLOT,
            uid,
            &wrap_bytes(&kek, &acct_key).unwrap(),
        )
        .await
        .unwrap();

        let verifier_nonce_raw = XChaCha20Poly1305::generate_nonce(&mut AeadOsRng);
        let verifier_nonce: [u8; 24] = verifier_nonce_raw.into();
        let verifier_ct = XChaCha20Poly1305::new((&*kek).into())
            .encrypt(&verifier_nonce_raw, VERIFIER_PLAINTEXT.as_slice())
            .unwrap();
        let meta = PinMeta {
            m_cost_kib: 8,
            t_cost: 1,
            p_cost: 1,
            salt,
            verifier_nonce,
            verifier_ct,
            failed_attempts: 0,
            last_attempt_unix: 0,
        };
        store_pin_meta(&*ks, uid, &meta).await.unwrap();

        assert!(unlock_inner(&*ks, uid, "2222").await.is_err());
        let after = load_pin_meta(&*ks, uid).await.unwrap().unwrap();
        assert_eq!(after.failed_attempts, 1);

        // 9 more wrong attempts should trip the lockout nuke.
        for _ in 0..9 {
            let _ = unlock_inner(&*ks, uid, "2222").await;
        }
        assert!(ks
            .load_for_user(DB_KEY_WRAPPED_SLOT, uid)
            .await
            .unwrap()
            .is_none());
        assert!(ks
            .load_for_user(PIN_META_SLOT, uid)
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn correct_pin_resets_counter() {
        let ks: StdArc<dyn crate::keystore::Keystore> =
            StdArc::new(InMemoryKeystore::new());
        let uid = "user_03";
        seed_legacy(&*ks, uid).await;

        let mut salt = [0u8; SALT_LEN];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let kek = derive_kek("9999", &salt, 8, 1, 1).unwrap();

        let db_key = ks
            .load_for_user(DB_KEY_SLOT_LEGACY, uid)
            .await
            .unwrap()
            .unwrap();
        let acct_key = ks
            .load_for_user(ACCOUNT_ID_KEY_SLOT_LEGACY, uid)
            .await
            .unwrap()
            .unwrap();
        ks.store_for_user(DB_KEY_WRAPPED_SLOT, uid, &wrap_bytes(&kek, &db_key).unwrap())
            .await
            .unwrap();
        ks.store_for_user(
            ACCOUNT_ID_KEY_WRAPPED_SLOT,
            uid,
            &wrap_bytes(&kek, &acct_key).unwrap(),
        )
        .await
        .unwrap();

        let verifier_nonce_raw = XChaCha20Poly1305::generate_nonce(&mut AeadOsRng);
        let verifier_nonce: [u8; 24] = verifier_nonce_raw.into();
        let verifier_ct = XChaCha20Poly1305::new((&*kek).into())
            .encrypt(&verifier_nonce_raw, VERIFIER_PLAINTEXT.as_slice())
            .unwrap();
        let meta = PinMeta {
            m_cost_kib: 8,
            t_cost: 1,
            p_cost: 1,
            salt,
            verifier_nonce,
            verifier_ct,
            failed_attempts: 3, // pretend we had 3 wrong attempts
            last_attempt_unix: 0,
        };
        store_pin_meta(&*ks, uid, &meta).await.unwrap();

        unlock_inner(&*ks, uid, "9999").await.unwrap();
        let after = load_pin_meta(&*ks, uid).await.unwrap().unwrap();
        assert_eq!(after.failed_attempts, 0);
    }
}
