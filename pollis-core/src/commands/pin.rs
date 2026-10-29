//! PIN-wrapped per-user key storage. The 4-digit PIN is a local unlock factor
//! (~13 bits), so the keys are protected by Argon2id (~250 ms) and
//! XChaCha20-Poly1305.
//!
//! Slots per user: `pin_meta` (KDF params, salt, verifier, attempt counter),
//! `db_key_wrapped` (the SQLCipher key) and `account_id_key_wrapped`, both sealed
//! under the one PIN-derived KEK. Upgraders from pre-PIN builds have unwrapped
//! legacy slots that [`install_wrapped_slots`] migrates (#951). See
//! `.codesight/wiki/pin-design.md`.

use aes_gcm::aead::OsRng as AeadOsRng;
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::state::AppState;

const PIN_META_SLOT: &str = "pin_meta";
const DB_KEY_WRAPPED_SLOT: &str = "db_key_wrapped";
const ACCOUNT_ID_KEY_WRAPPED_SLOT: &str = "account_id_key_wrapped";
const DB_KEY_SLOT_LEGACY: &str = "db_key";
const ACCOUNT_ID_KEY_SLOT_LEGACY: &str = "account_id_key";
/// Legacy `session_{user_id}` blob from pre-PIN builds (#184), deleted once the
/// wrap succeeds.
const SESSION_SLOT_LEGACY: &str = "session";

// Target ~250 ms on a mid-range machine. `pin_meta` carries its own params, so
// raising these needs no migration.

const ARGON2_M_COST_KIB: u32 = 64 * 1024; // 64 MiB
const ARGON2_T_COST: u32 = 3;
const ARGON2_P_COST: u32 = 1;
const KEK_LEN: usize = 32;
const SALT_LEN: usize = 16;
const VERIFIER_PLAINTEXT: &[u8; 16] = b"pollis-pin-ok\0\0\0";

/// Wrong attempts before the wrapped blobs are wiped (recovery via Secret Key).
/// No time backoff: Argon2id's cost is the defense against offline brute force.
pub const MAX_FAILED_ATTEMPTS: u32 = 10;

// `pin_meta` layout (93 bytes, fixed, not serde): version (1) = 1, m_cost_kib
// (4 BE), t_cost (1), p_cost (1), salt (16), nonce (24), verifier_ct_len (2 BE),
// verifier_ct||tag (32), failed_attempts (4 BE), last_attempt_unix (8 BE). The
// counters are outside the ciphertext on purpose: an attacker with keystore
// access can count attempts anyway.

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

// Wrapped-key layout: version (1) = 1, nonce (24), ct_len (2 BE), ct||tag.

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

/// Per-user in-memory unlock state; zeroized on drop.
pub struct UnlockState {
    pub user_id: String,
    pub db_key: Zeroizing<Vec<u8>>,
    pub account_id_key: Zeroizing<Vec<u8>>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct UnlockStateSnapshot {
    pub last_active_user: Option<String>,
    pub is_unlocked: bool,
    /// Whether `last_active_user` has a PIN: routes between "enter PIN" and
    /// "set PIN".
    pub pin_set: bool,
}

/// Set the PIN for the active user.
///
/// With `old_pin = None` (first-time setup) the keys come from `AppState.unlock`,
/// or from legacy unwrapped slots on a pre-PIN upgrader. With `Some` it is a
/// change: unwrap with the old PIN, rewrap under the new. On success the wrapped
/// slots are written and verified, the local DB opens, and the device cert is
/// published.
pub async fn set_pin(
    state: &Arc<AppState>,
    old_pin: Option<String>,
    new_pin: String,
) -> Result<()> {
    validate_pin(&new_pin)?;

    let user_id = crate::accounts::read_accounts_index()
        .unwrap_or_default()
        .last_active_user
        .ok_or_else(|| Error::Other(anyhow::anyhow!("no active user; call verify_otp first")))?;

    let keystore = state.keystore.as_ref();

    let (db_key, account_id_key): (Zeroizing<Vec<u8>>, Zeroizing<Vec<u8>>) = match old_pin {
        Some(ref old) => {
            validate_pin(old)?;
            let unlocked = unlock_inner(keystore, &user_id, old).await?;
            (unlocked.db_key, unlocked.account_id_key)
        }
        None => source_initial_keys(state, &user_id).await?,
    };

    let mut salt = [0u8; SALT_LEN];
    rand::rngs::OsRng.fill_bytes(&mut salt);
    let kek = derive_kek(
        &new_pin,
        &salt,
        ARGON2_M_COST_KIB,
        ARGON2_T_COST,
        ARGON2_P_COST,
    )?;

    // Encrypt a fixed plaintext so a wrong PIN is rejected without unwrapping the
    // keys.
    let verifier_nonce_raw = XChaCha20Poly1305::generate_nonce(&mut AeadOsRng);
    let verifier_nonce: [u8; 24] = verifier_nonce_raw.into();
    let verifier_cipher = XChaCha20Poly1305::new((&*kek).into());
    let verifier_ct = verifier_cipher
        .encrypt(&verifier_nonce_raw, VERIFIER_PLAINTEXT.as_slice())
        .map_err(|e| Error::Crypto(format!("verifier encrypt: {e}")))?;

    let meta = PinMeta {
        m_cost_kib: ARGON2_M_COST_KIB,
        t_cost: ARGON2_T_COST,
        p_cost: ARGON2_P_COST,
        salt,
        verifier_nonce,
        verifier_ct,
        failed_attempts: 0,
        last_attempt_unix: crate::util::now_unix(),
    };

    install_wrapped_slots(
        keystore,
        &user_id,
        &kek,
        &meta,
        &db_key,
        &account_id_key,
        old_pin.is_none(),
    )
    .await?;

    *state.unlock.lock().await = Some(UnlockState {
        user_id: user_id.clone(),
        db_key: db_key.clone(),
        account_id_key,
    });

    // Another user's cached media is unreadable under our `db_key`; purge it. Scoped
    // to `user_id` explicitly: the cache's ambient user isn't set until
    // `load_user_db_with_key` below.
    crate::commands::r2::clear_media_cache(crate::commands::r2::CacheScope::User(&user_id));

    // Rotate the loopback media token so the previous session's URLs die.
    *state.media_server_token.lock().await =
        Some(crate::media_server::fresh_token());

    let mls_was_empty = state.load_user_db_with_key(&user_id, &db_key).await?;

    // Copy the device id out first: a guard taken in an `if let` scrutinee lives
    // for the whole body (edition 2021), which would hold this hot, non-reentrant
    // lock across `ensure_device_cert`, and the signed `ds_post` takes it too.
    let device_id = state.device_id.lock().await.clone();
    if let Some(device_id) = device_id {
        if let Err(e) =
            crate::commands::mls::ensure_device_cert(state, &user_id, &device_id).await
        {
            crate::diag!("[pin] set_pin: ensure_device_cert failed (non-fatal): {e}");
        }
    }

    // A fresh local DB must re-arm Welcome delivery to restore MLS memberships.
    // After `ensure_device_cert` so the signed reset authenticates. Best-effort.
    if mls_was_empty {
        if let Err(e) = crate::commands::mls::reset_welcome_delivery(state, &user_id).await {
            crate::diag!("[pin] set_pin: reset_welcome_delivery failed (non-fatal): {e}");
        }
    }

    Ok(())
}

/// Write the three PIN-protected slots, read them back, and only then delete the
/// unwrapped legacy originals.
///
/// The order is the point (#951): until this finishes, a pre-PIN upgrader's
/// legacy slot is the only local copy of the account identity key, which is
/// unrecoverable without the Secret Key. A `store` returning `Ok(())` is a claim,
/// not evidence (a keychain can accept a write it won't return, #184), so the
/// delete is gated on a real round-trip. Any failure leaves the legacy slots
/// untouched; writes are idempotent, so a retry heals.
#[allow(clippy::too_many_arguments)]
async fn install_wrapped_slots(
    keystore: &dyn crate::keystore::Keystore,
    user_id: &str,
    kek: &Zeroizing<[u8; KEK_LEN]>,
    meta: &PinMeta,
    db_key: &[u8],
    account_id_key: &[u8],
    drop_legacy_session: bool,
) -> Result<()> {
    keystore
        .store_for_user(DB_KEY_WRAPPED_SLOT, user_id, &wrap_bytes(kek, db_key)?)
        .await?;
    keystore
        .store_for_user(
            ACCOUNT_ID_KEY_WRAPPED_SLOT,
            user_id,
            &wrap_bytes(kek, account_id_key)?,
        )
        .await?;
    store_pin_meta(keystore, user_id, meta).await?;

    verify_wrapped_slots(keystore, user_id, kek, db_key, account_id_key).await?;

    // Durable and readable: delete every unwrapped slot. A no-op for post-PIN
    // users.
    let _ = keystore.delete_for_user(DB_KEY_SLOT_LEGACY, user_id).await;
    let _ = keystore
        .delete_for_user(ACCOUNT_ID_KEY_SLOT_LEGACY, user_id)
        .await;
    if drop_legacy_session {
        let _ = keystore.delete_for_user(SESSION_SLOT_LEGACY, user_id).await;
    }
    Ok(())
}

/// Read back what `install_wrapped_slots` wrote and unwrap it under the same KEK.
/// Same shape as [`unlock_inner`]'s read half, so it proves a future `unlock`
/// recovers these keys.
async fn verify_wrapped_slots(
    keystore: &dyn crate::keystore::Keystore,
    user_id: &str,
    kek: &Zeroizing<[u8; KEK_LEN]>,
    db_key: &[u8],
    account_id_key: &[u8],
) -> Result<()> {
    let fail = |what: &str| {
        Error::Crypto(format!(
            "PIN setup could not verify {what} after writing it; the device \
             keystore accepted the write but did not return it. Your existing \
             keys have been left in place — try again."
        ))
    };

    let meta = load_pin_meta(keystore, user_id)
        .await?
        .ok_or_else(|| fail("pin_meta"))?;
    let verifier_ok = XChaCha20Poly1305::new((&**kek).into())
        .decrypt(
            XNonce::from_slice(&meta.verifier_nonce),
            meta.verifier_ct.as_slice(),
        )
        .ok()
        .map(|pt| pt.as_slice() == VERIFIER_PLAINTEXT.as_slice())
        .unwrap_or(false);
    if !verifier_ok {
        return Err(fail("the PIN verifier"));
    }

    for (slot, expected) in [
        (DB_KEY_WRAPPED_SLOT, db_key),
        (ACCOUNT_ID_KEY_WRAPPED_SLOT, account_id_key),
    ] {
        let blob = keystore
            .load_for_user(slot, user_id)
            .await?
            .ok_or_else(|| fail(slot))?;
        let got = unwrap_bytes(kek, &blob).map_err(|_| fail(slot))?;
        if *got != *expected {
            return Err(fail(slot));
        }
    }
    Ok(())
}

/// Source the raw `(db_key, account_id_key)` for first-time setup: from
/// `AppState.unlock` (signup, identity reset, enrollment), else from legacy
/// unwrapped slots, which must both exist. Erroring leaves whichever exists
/// untouched.
async fn source_initial_keys(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<(Zeroizing<Vec<u8>>, Zeroizing<Vec<u8>>)> {
    {
        let guard = state.unlock.lock().await;
        if let Some(u) = guard.as_ref() {
            if u.user_id == user_id {
                return Ok((u.db_key.clone(), u.account_id_key.clone()));
            }
        }
    }

    let keystore = state.keystore.as_ref();
    let legacy_db_key = keystore.load_for_user(DB_KEY_SLOT_LEGACY, user_id).await?;
    let legacy_account_id_key = keystore
        .load_for_user(ACCOUNT_ID_KEY_SLOT_LEGACY, user_id)
        .await?;

    match (legacy_db_key, legacy_account_id_key) {
        (Some(dk), Some(aik)) => Ok((Zeroizing::new(dk), Zeroizing::new(aik))),
        _ => Err(Error::Other(anyhow::anyhow!(
            "no key material to wrap; sign in again"
        ))),
    }
}

#[derive(Debug, Serialize)]
pub struct UnlockOutcome {
    pub user_id: String,
    pub attempts_remaining: Option<u32>,
}

/// Verify a PIN, populate `AppState.unlock` and open the local DB. Also drops
/// leftover legacy slots and re-publishes the device cert if needed.
pub async fn unlock(
    state: &Arc<AppState>,
    user_id: String,
    pin: String,
) -> Result<UnlockOutcome> {
    validate_pin(&pin)?;
    let keystore = state.keystore.as_ref();
    let unlocked = unlock_inner(keystore, &user_id, &pin).await?;

    let db_key = unlocked.db_key.clone();
    *state.unlock.lock().await = Some(UnlockState {
        user_id: unlocked.user_id.clone(),
        db_key: unlocked.db_key,
        account_id_key: unlocked.account_id_key,
    });

    // Wipe the media cache; see `set_pin`.
    crate::commands::r2::clear_media_cache(crate::commands::r2::CacheScope::User(
        &unlocked.user_id,
    ));

    // Rotate the loopback media token; see `set_pin`.
    *state.media_server_token.lock().await =
        Some(crate::media_server::fresh_token());

    // Leftover legacy slots beside the wrapped blobs (#195-era builds) are stale.
    let _ = keystore
        .delete_for_user(DB_KEY_SLOT_LEGACY, &user_id)
        .await;
    let _ = keystore
        .delete_for_user(ACCOUNT_ID_KEY_SLOT_LEGACY, &user_id)
        .await;

    let mls_was_empty = state.load_user_db_with_key(&user_id, &db_key).await?;

    // Copy the device id out first; see `set_pin`.
    let device_id = state.device_id.lock().await.clone();
    if let Some(device_id) = device_id {
        if let Err(e) =
            crate::commands::mls::ensure_device_cert(state, &user_id, &device_id).await
        {
            crate::diag!("[pin] unlock: ensure_device_cert failed (non-fatal): {e}");
        }
    }

    // A fresh local DB must re-arm Welcome delivery; after `ensure_device_cert`.
    if mls_was_empty {
        if let Err(e) = crate::commands::mls::reset_welcome_delivery(state, &user_id).await {
            crate::diag!("[pin] unlock: reset_welcome_delivery failed (non-fatal): {e}");
        }
    }

    // Self-heal: re-sign device rows whose `cert_identity_version` trails the
    // account's after a sibling rotated the identity offline, or they fail
    // cross-signing on every commit. Best-effort.
    if let Err(e) =
        crate::commands::mls::resign_stale_device_certs(state, &user_id).await
    {
        crate::diag!("[pin] unlock: resign_stale_device_certs failed (non-fatal): {e}");
    }

    // Declare attachment references from pre-#1330 sends, in the background.
    crate::commands::messages::spawn_attachment_ref_backfill(state, &user_id);

    // Deliver what a killed process left in the outbox (#1335), in the background.
    crate::commands::messages::spawn_outbox_flush(state);

    Ok(UnlockOutcome {
        user_id: unlocked.user_id,
        attempts_remaining: None,
    })
}

/// Re-verify the PIN of an unlocked device for an action the person at the
/// keyboard must confirm (e.g. a QR device link, #1207). Same attempt counter as
/// [`unlock`]; leaves `AppState` alone.
pub(crate) async fn verify_pin(state: &Arc<AppState>, user_id: &str, pin: &str) -> Result<()> {
    validate_pin(pin)?;
    unlock_inner(state.keystore.as_ref(), user_id, pin).await.map(|_| ())
}

/// Shared verify-and-unwrap for `unlock` and the change-PIN flow.
async fn unlock_inner(
    keystore: &dyn crate::keystore::Keystore,
    user_id: &str,
    pin: &str,
) -> Result<UnlockState> {
    let mut meta = load_pin_meta(keystore, user_id)
        .await?
        .ok_or_else(|| Error::Other(anyhow::anyhow!("PIN not set for user {user_id}")))?;

    if meta.failed_attempts >= MAX_FAILED_ATTEMPTS {
        // Defense in depth if the UI didn't wipe already. Remote data and Secret Key
        // recovery are untouched.
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

    // Check the verifier first; a wrong PIN increments the counter.
    let verifier_cipher = XChaCha20Poly1305::new((&*kek).into());
    let verifier_nonce = XNonce::from_slice(&meta.verifier_nonce);
    let verifier_ok = verifier_cipher
        .decrypt(verifier_nonce, meta.verifier_ct.as_slice())
        .ok()
        .map(|pt| pt.as_slice() == VERIFIER_PLAINTEXT.as_slice())
        .unwrap_or(false);

    if !verifier_ok {
        meta.failed_attempts += 1;
        meta.last_attempt_unix = crate::util::now_unix();
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

    if meta.failed_attempts != 0 {
        meta.failed_attempts = 0;
        meta.last_attempt_unix = crate::util::now_unix();
        store_pin_meta(keystore, user_id, &meta).await?;
    }

    Ok(UnlockState {
        user_id: user_id.to_string(),
        db_key,
        account_id_key,
    })
}

/// Drop the in-memory unlock state and close the local DB: the screen-lock
/// primitive, not logout. The accounts index is untouched.
pub async fn lock(state: &Arc<AppState>) -> Result<()> {
    *state.unlock.lock().await = None;
    // Media URLs from the locked session stop resolving; the encrypted cache files
    // stay.
    *state.media_server_token.lock().await = None;
    // Staged attachments are plaintext in memory; release them with the session.
    crate::commands::staging::discard_all_staged();
    state.unload_user_db().await;
    Ok(())
}

/// Auth/unlock state for the frontend. Reads only `accounts.json` and the cheap
/// `pin_meta` blob.
pub async fn get_unlock_state(
    state: &Arc<AppState>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keystore::{InMemoryKeystore, Keystore as _};
    use std::sync::Arc as StdArc;

    async fn seed_legacy(ks: &dyn crate::keystore::Keystore, uid: &str) {
        ks.store_for_user(DB_KEY_SLOT_LEGACY, uid, &[7u8; 32])
            .await
            .unwrap();
        ks.store_for_user(ACCOUNT_ID_KEY_SLOT_LEGACY, uid, &[42u8; 32])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn wrap_unwrap_roundtrip() {
        let mut salt = [0u8; SALT_LEN];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        // Cheap Argon2 for tests.
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

    #[tokio::test]
    async fn set_and_unlock_roundtrip_via_inner() {
        let ks: StdArc<dyn crate::keystore::Keystore> =
            StdArc::new(InMemoryKeystore::new());
        let uid = "user_01";
        seed_legacy(&*ks, uid).await;

        // The command path needs a full `AppState`; the flows harness covers it.
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

    // #951: these drive the real `install_wrapped_slots`, so they pin the order
    // production runs.

    /// A keystore that accepts a write for one slot and then doesn't have it (#184's
    /// failure class), which no return code can distinguish from success.
    struct DropsWritesTo {
        inner: InMemoryKeystore,
        slot_prefix: &'static str,
        /// False refuses the write loudly instead; both must be non-destructive.
        silently: bool,
    }

    #[async_trait::async_trait]
    impl crate::keystore::Keystore for DropsWritesTo {
        async fn store(&self, key: &str, value: &[u8]) -> Result<()> {
            if key.starts_with(self.slot_prefix) {
                return if self.silently {
                    Ok(())
                } else {
                    Err(Error::Crypto("keystore write refused (test)".into()))
                };
            }
            self.inner.store(key, value).await
        }
        async fn load(&self, key: &str) -> Result<Option<Vec<u8>>> {
            self.inner.load(key).await
        }
        async fn delete(&self, key: &str) -> Result<()> {
            self.inner.delete(key).await
        }
    }

    fn cheap_kek_and_meta(pin: &str) -> (Zeroizing<[u8; KEK_LEN]>, PinMeta) {
        let mut salt = [0u8; SALT_LEN];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let kek = derive_kek(pin, &salt, 8, 1, 1).unwrap();
        let verifier_nonce_raw = XChaCha20Poly1305::generate_nonce(&mut AeadOsRng);
        let verifier_ct = XChaCha20Poly1305::new((&*kek).into())
            .encrypt(&verifier_nonce_raw, VERIFIER_PLAINTEXT.as_slice())
            .unwrap();
        let meta = PinMeta {
            m_cost_kib: 8,
            t_cost: 1,
            p_cost: 1,
            salt,
            verifier_nonce: verifier_nonce_raw.into(),
            verifier_ct,
            failed_attempts: 0,
            last_attempt_unix: 0,
        };
        (kek, meta)
    }

    const LEGACY_DB_KEY: [u8; 32] = [7u8; 32];
    const LEGACY_ACCOUNT_KEY: [u8; 32] = [42u8; 32];

    #[tokio::test]
    async fn legacy_slots_are_wrapped_then_removed() {
        let ks: StdArc<dyn crate::keystore::Keystore> = StdArc::new(InMemoryKeystore::new());
        let uid = "upgrader_01";
        seed_legacy(&*ks, uid).await;
        ks.store_for_user(SESSION_SLOT_LEGACY, uid, b"stale-session")
            .await
            .unwrap();

        let (kek, meta) = cheap_kek_and_meta("1357");
        install_wrapped_slots(
            &*ks,
            uid,
            &kek,
            &meta,
            &LEGACY_DB_KEY,
            &LEGACY_ACCOUNT_KEY,
            true,
        )
        .await
        .expect("migration must succeed on a healthy keystore");

        assert!(
            ks.load_for_user(DB_KEY_SLOT_LEGACY, uid).await.unwrap().is_none(),
            "the unwrapped db_key slot must not survive the migration"
        );
        assert!(
            ks.load_for_user(ACCOUNT_ID_KEY_SLOT_LEGACY, uid).await.unwrap().is_none(),
            "the unwrapped account_id_key slot must not survive the migration"
        );
        assert!(
            ks.load_for_user(SESSION_SLOT_LEGACY, uid).await.unwrap().is_none(),
            "the legacy session blob goes with them on the initial-set path"
        );

        let unlocked = unlock_inner(&*ks, uid, "1357").await.unwrap();
        assert_eq!(&*unlocked.db_key, &LEGACY_DB_KEY);
        assert_eq!(&*unlocked.account_id_key, &LEGACY_ACCOUNT_KEY);
        assert!(
            unlock_inner(&*ks, uid, "2468").await.is_err(),
            "the migrated key must now be behind the PIN, not beside it"
        );
    }

    /// An interruption before the wrapped copies are proven readable must leave the
    /// legacy slots exactly as they were. The silent failure (`Ok(())`, value gone)
    /// is caught only by the read-back.
    #[tokio::test]
    async fn an_interrupted_migration_loses_nothing() {
        for silently in [true, false] {
            for slot_prefix in [ACCOUNT_ID_KEY_WRAPPED_SLOT, DB_KEY_WRAPPED_SLOT, PIN_META_SLOT] {
                let ks = DropsWritesTo {
                    inner: InMemoryKeystore::new(),
                    slot_prefix,
                    silently,
                };
                let uid = "upgrader_02";
                seed_legacy(&ks, uid).await;

                let (kek, meta) = cheap_kek_and_meta("1357");
                let err = install_wrapped_slots(
                    &ks,
                    uid,
                    &kek,
                    &meta,
                    &LEGACY_DB_KEY,
                    &LEGACY_ACCOUNT_KEY,
                    true,
                )
                .await
                .expect_err(
                    "a migration whose new state is not readable must fail, \
                     not proceed to the delete",
                );

                assert_eq!(
                    ks.load_for_user(DB_KEY_SLOT_LEGACY, uid).await.unwrap(),
                    Some(LEGACY_DB_KEY.to_vec()),
                    "{slot_prefix} (silently={silently}, err={err}): the legacy \
                     db_key must survive an interrupted migration"
                );
                assert_eq!(
                    ks.load_for_user(ACCOUNT_ID_KEY_SLOT_LEGACY, uid).await.unwrap(),
                    Some(LEGACY_ACCOUNT_KEY.to_vec()),
                    "{slot_prefix} (silently={silently}, err={err}): losing the \
                     account identity key here is unrecoverable"
                );

                // And a retry against a healthy keystore completes.
                let healthy: StdArc<dyn crate::keystore::Keystore> =
                    StdArc::new(InMemoryKeystore::new());
                seed_legacy(&*healthy, uid).await;
                install_wrapped_slots(
                    &*healthy,
                    uid,
                    &kek,
                    &meta,
                    &LEGACY_DB_KEY,
                    &LEGACY_ACCOUNT_KEY,
                    true,
                )
                .await
                .expect("the retry completes");
                assert_eq!(
                    &*unlock_inner(&*healthy, uid, "1357").await.unwrap().account_id_key,
                    &LEGACY_ACCOUNT_KEY,
                );
            }
        }
    }

    /// A post-PIN user with no legacy slot is unaffected, and other users' slots
    /// aren't collateral.
    #[tokio::test]
    async fn a_user_with_no_legacy_slot_is_unaffected() {
        let ks: StdArc<dyn crate::keystore::Keystore> = StdArc::new(InMemoryKeystore::new());
        let modern = "modern_01";
        let bystander = "upgrader_03";

        seed_legacy(&*ks, bystander).await;

        let db_key = [1u8; 32];
        let account_id_key = [2u8; 32];
        let (kek, meta) = cheap_kek_and_meta("8642");
        install_wrapped_slots(&*ks, modern, &kek, &meta, &db_key, &account_id_key, true)
            .await
            .expect("no legacy slot is not an error");

        let unlocked = unlock_inner(&*ks, modern, "8642").await.unwrap();
        assert_eq!(&*unlocked.db_key, &db_key);
        assert_eq!(&*unlocked.account_id_key, &account_id_key);

        // Slots are per-user; the delete is `delete_for_user`, not a sweep.
        assert_eq!(
            ks.load_for_user(DB_KEY_SLOT_LEGACY, bystander).await.unwrap(),
            Some(LEGACY_DB_KEY.to_vec()),
        );
        assert_eq!(
            ks.load_for_user(ACCOUNT_ID_KEY_SLOT_LEGACY, bystander).await.unwrap(),
            Some(LEGACY_ACCOUNT_KEY.to_vec()),
        );
    }
}
