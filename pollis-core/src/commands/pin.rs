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
//! ## The legacy unwrapped slots (#194 stage 6, finished by #951)
//!
//! Before #194 the two secrets lived in `db_key_{user_id}` and
//! `account_id_key_{user_id}` as raw bytes with no PIN layer over them.
//! #882 later put the file backend's whole contents inside AES-256-GCM,
//! which helped those slots incidentally, but they still skipped the
//! Argon2id KEK everyone enrolled after #194 gets — so an attacker who
//! defeated the file layer read them directly instead of meeting a slow
//! KDF.
//!
//! `set_pin` is the cutover: an upgrader boots with `pin_set = false`,
//! the frontend routes them to the create-PIN screen (`App.tsx`), and
//! [`source_initial_keys`] reads the legacy slots so [`install_wrapped_slots`]
//! can wrap them. The **ordering** in that function is the load-bearing
//! part — see its documentation.

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
/// * If `old_pin` is `None`, this is first-time setup. The raw key
///   material to wrap is sourced in priority order from:
///     1. `AppState.unlock` — populated by `verify_otp` /
///        `/v1/auth/establish-identity` at signup, by `reset_identity`, or by an
///        enrollment unwrap (the canonical path).
///     2. Legacy `db_key_{uid}` / `account_id_key_{uid}` keystore
///        slots — pre-PIN builds. Read once, then deleted after the
///        wrap succeeds.
/// * If `old_pin` is `Some`, this is the change-PIN flow: unwrap with
///   the old PIN, rewrap under the new one.
///
/// On success: writes the three PIN-protected blobs (`pin_meta`,
/// `db_key_wrapped`, `account_id_key_wrapped`), verifies they read back
/// (see [`install_wrapped_slots`] — this is what makes the next step
/// safe), deletes the legacy unwrapped slots, populates
/// `AppState.unlock`, and opens the local SQLCipher DB under the same
/// `db_key`. Best-effort `ensure_device_cert` follows so a fresh signup
/// publishes its cert as part of the same roundtrip.
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

    // Verifier blob: encrypt a fixed plaintext so wrong-PIN rejection
    // doesn't have to unwrap the big keys.
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

    // Wipe the media cache on unlock. If a different user previously cached
    // media on this device, their files were encrypted under their db_key
    // and are unreadable to us — purge them so they don't consume cap space.
    crate::commands::r2::clear_media_cache();

    // Rotate the loopback media server token. Any URL the previous
    // session handed out becomes invalid the moment the new token is
    // installed, which is the behaviour we want when the active user
    // changes.
    *state.media_server_token.lock().await =
        Some(crate::media_server::fresh_token());

    let mls_was_empty = state.load_user_db_with_key(&user_id, &db_key).await?;

    // Move remote_db onto a DS-minted short-TTL read-only token (#393). Idempotent
    // + best-effort: keeps the baked read-only token if the DS can't mint one.
    crate::commands::turso_token::spawn_turso_token_refresh(state);

    // Read the device id into a `let` FIRST, then release. In edition 2021 the
    // guard produced by `state.device_id.lock().await` inside an `if let`
    // scrutinee lives until the END of the `if let` body, so the `.clone()` only
    // LOOKS like it releases the lock — the old shape held `device_id`, one of
    // the hottest mutexes in the crate, across `ensure_device_cert`'s Turso
    // read, OS-keystore signature and DS POST. That stalled every concurrent
    // task needing the device id, and it was one refactor away from a hard
    // self-deadlock: `tokio::sync::Mutex` is not reentrant, and the signed
    // `ds_post` helper takes this very lock (`mls::ds_client`). Today
    // `ensure_device_cert` only uses the sign-free bootstrap posts, so nothing
    // deadlocks — which is exactly why the shape had to go before someone
    // routed it through `ds_post`.
    let device_id = state.device_id.lock().await.clone();
    if let Some(device_id) = device_id {
        if let Err(e) =
            crate::commands::mls::ensure_device_cert(state, &user_id, &device_id).await
        {
            eprintln!("[pin] set_pin: ensure_device_cert failed (non-fatal): {e}");
        }
    }

    // A freshly-created/wiped local DB: re-arm welcome delivery so
    // `initialize_identity`'s poll re-processes Welcomes and restores MLS
    // group memberships. Runs AFTER `ensure_device_cert` so the signed DS
    // reset authenticates against the just-published device key. Best-effort.
    if mls_was_empty {
        if let Err(e) = crate::commands::mls::reset_welcome_delivery(state, &user_id).await {
            eprintln!("[pin] set_pin: reset_welcome_delivery failed (non-fatal): {e}");
        }
    }

    Ok(())
}

/// Write the three PIN-protected slots, prove they are readable, and only then
/// remove the unwrapped legacy originals.
///
/// # Why the order is the whole point (#951)
///
/// For a pre-#194 upgrader the legacy `db_key_{uid}` / `account_id_key_{uid}`
/// slots are not stale duplicates — until this function finishes they are the
/// **only** copy of the account identity key on this device, and losing an
/// account identity key is unrecoverable (`account_identity.rs`: the sole other
/// copy is the server-side `account_recovery` blob, which needs the one-time
/// Secret Key the user was told to write down and typically has not).
///
/// So the sequence mirrors the atomic-rename migration #882 gave the keystore
/// file, where every interruption leaves either the complete old state or the
/// complete new one:
///
///   1. wrap under the PIN-derived KEK and write `db_key_wrapped`,
///      `account_id_key_wrapped`, `pin_meta`;
///   2. **read all three back and unwrap them**, asserting they reproduce the
///      exact bytes that went in;
///   3. only then delete the unwrapped slots.
///
/// Step 2 is not ceremony. A `store` returning `Ok(())` is a claim by the
/// backend, not evidence: the OS keychain can report a successful write for an
/// entry a later read does not return (a locked or half-available Secret
/// Service is the #184 failure class), and the file backend can fail to seal on
/// a host whose machine identity has gone missing. Every one of those is
/// survivable while the legacy slot still exists and unrecoverable the moment it
/// does not, so the delete is gated on a real round-trip rather than on a
/// return code.
///
/// A failure anywhere in 1–2 propagates with the legacy slots untouched. The
/// user is told to retry, and the retry re-runs the whole sequence — writes are
/// idempotent overwrites, so a partially-written triple heals rather than
/// accumulating.
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
    // 1. Write the new state.
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

    // 2. Prove the new state is real before discarding the old one.
    verify_wrapped_slots(keystore, user_id, kek, db_key, account_id_key).await?;

    // 3. The wrapped blobs are now durable AND readable. Delete every unwrapped
    //    slot we know about. Idempotent for the post-#194 path, where these
    //    slots never existed in the first place.
    let _ = keystore.delete_for_user(DB_KEY_SLOT_LEGACY, user_id).await;
    let _ = keystore
        .delete_for_user(ACCOUNT_ID_KEY_SLOT_LEGACY, user_id)
        .await;
    if drop_legacy_session {
        let _ = keystore.delete_for_user(SESSION_SLOT_LEGACY, user_id).await;
    }
    Ok(())
}

/// Read back the triple `install_wrapped_slots` just wrote and unwrap it under
/// the same KEK, checking it reproduces the supplied key material.
///
/// This is deliberately the same shape as the read half of [`unlock_inner`] —
/// `pin_meta` present and its verifier decrypting, both wrapped blobs present
/// and unwrapping — so what it proves is exactly "a future `unlock` with this
/// PIN will recover these keys", not merely "some bytes were stored".
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

/// Source the raw `(db_key, account_id_key)` for the initial-set path.
///
/// Tier 1: `AppState.unlock` is the canonical post-#194 source. Fresh
/// signup, identity reset, and device enrollment all populate it.
///
/// Tier 2: legacy plaintext keystore slots from a pre-PIN build. Read
/// the bytes here so the wrap step works; [`install_wrapped_slots`]
/// deletes the slots once the wrapped copies are proven readable.
///
/// Both slots are required together. A device holding only one of them
/// has nothing this function can turn into a working unlock, and
/// erroring out leaves whichever slot does exist untouched — the
/// non-destructive answer, since the caller's next move is to retry or
/// re-enroll rather than to overwrite.
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

/// Verify a PIN, populate `AppState.unlock`, open the local DB.
///
/// Side effects on success:
/// - Drops any legacy plaintext keystore slots left behind by a #195-
///   vintage build (where `set_pin` wrote the wrapped form but didn't
///   delete the unwrapped originals).
/// - Opens the per-user SQLCipher DB under the unwrapped `db_key`.
/// - Best-effort `ensure_device_cert` so a freshly-resumed device
///   re-publishes its cert if the previous attempt was deferred.
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

    // Wipe the media cache on unlock — see equivalent comment in `set_pin`.
    crate::commands::r2::clear_media_cache();

    // Rotate the loopback media server token so URLs minted under the
    // pre-unlock (or previous-session) token stop working. See the
    // matching site in `set_pin`.
    *state.media_server_token.lock().await =
        Some(crate::media_server::fresh_token());

    // Migrate away from #195-vintage residue — the wrapped blobs are
    // already in place; the legacy plaintext slots sitting next to
    // them are stale duplicates.
    let _ = keystore
        .delete_for_user(DB_KEY_SLOT_LEGACY, &user_id)
        .await;
    let _ = keystore
        .delete_for_user(ACCOUNT_ID_KEY_SLOT_LEGACY, &user_id)
        .await;

    let mls_was_empty = state.load_user_db_with_key(&user_id, &db_key).await?;

    // Move remote_db onto a DS-minted short-TTL read-only token (#393). Idempotent
    // + best-effort: keeps the baked read-only token if the DS can't mint one.
    crate::commands::turso_token::spawn_turso_token_refresh(state);

    // Read the device id into a `let` FIRST, then release. In edition 2021 the
    // guard produced by `state.device_id.lock().await` inside an `if let`
    // scrutinee lives until the END of the `if let` body, so the `.clone()` only
    // LOOKS like it releases the lock — the old shape held `device_id`, one of
    // the hottest mutexes in the crate, across `ensure_device_cert`'s Turso
    // read, OS-keystore signature and DS POST. That stalled every concurrent
    // task needing the device id, and it was one refactor away from a hard
    // self-deadlock: `tokio::sync::Mutex` is not reentrant, and the signed
    // `ds_post` helper takes this very lock (`mls::ds_client`). Today
    // `ensure_device_cert` only uses the sign-free bootstrap posts, so nothing
    // deadlocks — which is exactly why the shape had to go before someone
    // routed it through `ds_post`.
    let device_id = state.device_id.lock().await.clone();
    if let Some(device_id) = device_id {
        if let Err(e) =
            crate::commands::mls::ensure_device_cert(state, &user_id, &device_id).await
        {
            eprintln!("[pin] unlock: ensure_device_cert failed (non-fatal): {e}");
        }
    }

    // A freshly-created/wiped local DB: re-arm welcome delivery so the startup
    // poll re-processes Welcomes and restores MLS group memberships. Runs AFTER
    // `ensure_device_cert` so the signed DS reset authenticates. Best-effort.
    if mls_was_empty {
        if let Err(e) = crate::commands::mls::reset_welcome_delivery(state, &user_id).await {
            eprintln!("[pin] unlock: reset_welcome_delivery failed (non-fatal): {e}");
        }
    }

    // Opportunistic self-heal: if a sibling device rotated the account
    // identity while this device was offline, every other device's row
    // (including this one's, before `ensure_device_cert` above) is
    // stale on the server and would fail cross-signing verification on
    // every commit until that device next logs in. Re-sign any rows
    // whose `cert_identity_version` is behind `users.identity_version`
    // so the fleet self-heals as users come online. Best-effort.
    if let Err(e) =
        crate::commands::mls::resign_stale_device_certs(state, &user_id).await
    {
        eprintln!("[pin] unlock: resign_stale_device_certs failed (non-fatal): {e}");
    }

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
        meta.last_attempt_unix = crate::util::now_unix();
        store_pin_meta(keystore, user_id, &meta).await?;
    }

    Ok(UnlockState {
        user_id: user_id.to_string(),
        db_key,
        account_id_key,
    })
}

/// Drop the in-memory unlock state and close the open local DB. The
/// next DB-touching command will fail with "Not signed in" until
/// `unlock` runs. Accounts index is left intact — this is the "screen
/// lock" primitive, not the "log out" one.
pub async fn lock(state: &Arc<AppState>) -> Result<()> {
    *state.unlock.lock().await = None;
    // Clear the media-server token so URLs the locked session minted
    // stop resolving. The cache files stay (encrypted at rest) — a
    // subsequent unlock under the same `db_key` can still read them.
    *state.media_server_token.lock().await = None;
    state.unload_user_db().await;
    Ok(())
}

/// Snapshot of auth/unlock state for the frontend. Never blocks on
/// keystore reads for secrets — just reads `accounts.json` and the
/// cheap `pin_meta` blob.
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

// ── Tests ────────────────────────────────────────────────────────────

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

    // ── #951: finishing #194's cutover off the unwrapped legacy slots ───
    //
    // These drive `install_wrapped_slots` — the real function `set_pin`
    // calls — rather than a restatement of it, so the ordering they pin is
    // the ordering production runs. `set_pin` itself needs a full
    // `AppState`; the flows harness covers that seam
    // (`flows::auth::pin_set_lock_unlock_roundtrip`).

    /// A keystore that accepts a write for one slot and then does not have
    /// it — the exact failure `install_wrapped_slots`' verify step exists to
    /// catch, and the one a `store` return code cannot distinguish from
    /// success. Modelled on a Secret Service that reports a stored
    /// credential it will not hand back (#184's failure class).
    struct DropsWritesTo {
        inner: InMemoryKeystore,
        slot_prefix: &'static str,
        /// When false the write is refused outright instead of silently
        /// vanishing. Both must be non-destructive; only the loud one is
        /// caught without the read-back.
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

    /// `(kek, meta)` at cheap Argon2 settings — the real 64 MiB parameters
    /// would make these tests glacial and nothing here depends on the cost.
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

    /// The migration itself: a pre-#194 upgrader's raw slots become
    /// PIN-wrapped ones, the wrapped copies really do carry the same key
    /// material, and only then do the unwrapped originals go away.
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

        // The unwrapped slots are gone — the point of the ticket.
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

        // And the key material survived, reachable only through the PIN.
        let unlocked = unlock_inner(&*ks, uid, "1357").await.unwrap();
        assert_eq!(&*unlocked.db_key, &LEGACY_DB_KEY);
        assert_eq!(&*unlocked.account_id_key, &LEGACY_ACCOUNT_KEY);
        assert!(
            unlock_inner(&*ks, uid, "2468").await.is_err(),
            "the migrated key must now be behind the PIN, not beside it"
        );
    }

    /// The property that makes the migration safe to ship: an interruption
    /// anywhere before the wrapped copies are proven readable must leave the
    /// legacy slots — the only other copy of an unrecoverable key — exactly
    /// as they were.
    ///
    /// Both failure shapes are covered. The loud one (`store` errors) is
    /// caught by `?` alone; the quiet one (`store` returns `Ok(())` and the
    /// value is simply not there afterwards) is caught ONLY by the read-back
    /// in `verify_wrapped_slots`, and is the case that used to delete a
    /// user's account identity key.
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

                // And the retry heals: the same call against a healthy
                // keystore completes, so the failure is a pause, not a
                // dead end.
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

    /// The overwhelmingly common case — a post-#194 user who never had an
    /// unwrapped slot — must be untouched by any of this: the wrap still
    /// happens, the absent legacy slots are a no-op rather than an error,
    /// and no OTHER user's slots are collateral.
    #[tokio::test]
    async fn a_user_with_no_legacy_slot_is_unaffected() {
        let ks: StdArc<dyn crate::keystore::Keystore> = StdArc::new(InMemoryKeystore::new());
        let modern = "modern_01";
        let bystander = "upgrader_03";

        // A second account on the same device that HAS legacy slots and is
        // not the one being set up.
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

        // The bystander's slots are namespaced per user and must be
        // untouched — the delete is `delete_for_user`, not a sweep.
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
