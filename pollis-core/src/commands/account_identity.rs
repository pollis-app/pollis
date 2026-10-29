//! Account identity key management.
//!
//! Each user has one long-lived ML-DSA-44 account identity key (#668), distinct
//! from per-device MLS keys: it represents the human and cross-signs every
//! device. Its private half is a 32-byte seed held on each device and, wrapped
//! under a user-held Secret Key (shown once at signup, never sent), in the
//! server's `account_recovery` row. Losing the Secret Key and every device makes
//! the account unrecoverable by design.

use std::sync::Arc;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit};
use hkdf::Hkdf;
use ml_dsa::{Keypair, MlDsa44, Signer, SigningKey};
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::commands::pin::UnlockState;
use crate::error::{Error, Result};
use crate::state::AppState;

const DB_KEY_LEN: usize = 32;

/// Keystore slot for this device's copy of the account identity private key.
const ACCOUNT_ID_KEY_KEYSTORE_SLOT: &str = "account_id_key";

/// Keystore slot for the account identity public key the DS last reported at
/// `verify-otp`. Public, but the keystore is the one store a not-yet-enrolled
/// device has, and it survives restarts mid-enrollment.
const ACCOUNT_ID_PUB_SLOT: &str = "account_id_pub";

/// Keystore slot marking that the DS says this account's identity is no longer
/// the one this device holds. Holds the claimed pub key.
const ACCOUNT_IDENTITY_SUPERSEDED_SLOT: &str = "account_identity_superseded";

/// Crockford base32 (no I/L/O/U).
const SECRET_KEY_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Secret Key body length: 30 chars × 5 bits = 150 bits.
const SECRET_KEY_BODY_CHARS: usize = 30;

/// Secret Key format version prefix; bump if the derivation or alphabet changes.
const SECRET_KEY_VERSION: &str = "A3";

/// HKDF info binding the wrap key to this use only.
const HKDF_INFO: &[u8] = b"pollis-account-key-wrap-v1";

const SALT_LEN: usize = 32;
const NONCE_LEN: usize = 12;

/// Account identity private key length: an ML-DSA-44 seed, the same size the
/// Ed25519 key had, so wrap/recovery/transfer formats are unchanged.
const ACCOUNT_SEED_LEN: usize = 32;

pub type AccountSigningKey = SigningKey<MlDsa44>;

/// A fresh random Secret Key in display form:
/// `A3-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX`.
pub fn generate_secret_key_string() -> String {
    let mut rng = OsRng;
    // The body is the secret: `Zeroizing`, pre-sized so `push` never reallocates
    // and strands an unwiped copy (#1141).
    let mut body = Zeroizing::new(String::with_capacity(SECRET_KEY_BODY_CHARS));
    for _ in 0..SECRET_KEY_BODY_CHARS {
        // 32 divides 256, so masking to 5 bits is unbiased.
        let mut buf = [0u8; 1];
        rng.fill_bytes(&mut buf);
        let idx = (buf[0] & 0x1f) as usize;
        body.push(SECRET_KEY_ALPHABET[idx] as char);
    }

    let mut out = String::with_capacity(SECRET_KEY_VERSION.len() + 1 + SECRET_KEY_BODY_CHARS + 5);
    out.push_str(SECRET_KEY_VERSION);
    for (i, chunk) in body.as_bytes().chunks(5).enumerate() {
        out.push('-');
        out.push_str(std::str::from_utf8(chunk).unwrap());
        let _ = i;
    }
    out
}

/// Normalize a typed Secret Key (whitespace, case, prefix, dashes) to its raw
/// body, the HKDF IKM, validating length and alphabet.
pub fn normalize_secret_key(input: &str) -> Result<Zeroizing<String>> {
    // Both copies carry the secret, so both are wiped (#1141).
    let filtered = Zeroizing::new(
        input.chars().filter(|c| !c.is_whitespace()).collect::<String>(),
    );
    let cleaned = Zeroizing::new(filtered.to_uppercase());

    let body_with_dashes = cleaned.strip_prefix(&format!("{SECRET_KEY_VERSION}-")).ok_or_else(|| {
        Error::Crypto(format!(
            "Secret Key must start with \"{SECRET_KEY_VERSION}-\""
        ))
    })?;

    let body = Zeroizing::new(
        body_with_dashes.chars().filter(|c| *c != '-').collect::<String>(),
    );

    if body.len() != SECRET_KEY_BODY_CHARS {
        return Err(Error::Crypto(format!(
            "Secret Key must contain exactly {} alphanumeric characters (got {})",
            SECRET_KEY_BODY_CHARS,
            body.len()
        )));
    }

    for c in body.chars() {
        if !SECRET_KEY_ALPHABET.contains(&(c as u8)) {
            return Err(Error::Crypto(format!(
                "Secret Key contains invalid character '{c}'"
            )));
        }
    }

    Ok(body)
}

fn derive_wrap_key(secret_key_body: &str, salt: &[u8]) -> Zeroizing<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(Some(salt), secret_key_body.as_bytes());
    let mut out = Zeroizing::new([0u8; 32]);
    hk.expand(HKDF_INFO, &mut *out)
        .expect("HKDF-SHA256 expand 32 bytes is always valid");
    out
}

fn aes_gcm_encrypt(key: &[u8; 32], nonce: &[u8; NONCE_LEN], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key));
    cipher
        .encrypt(GenericArray::from_slice(nonce), plaintext)
        .map_err(|e| Error::Crypto(format!("aes-256-gcm encrypt: {e}")))
}

/// `Zeroizing`: the plaintext here is always the account seed (#1141).
fn aes_gcm_decrypt(
    key: &[u8; 32],
    nonce: &[u8; NONCE_LEN],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key));
    cipher
        .decrypt(GenericArray::from_slice(nonce), ciphertext)
        .map(Zeroizing::new)
        .map_err(|e| Error::Crypto(format!("aes-256-gcm decrypt: {e}")))
}

/// Unwrap an `account_recovery` row with a typed Secret Key, returning the
/// 32-byte account seed. Used by Secret Key recovery.
pub fn unwrap_recovery_blob(
    secret_key_input: &str,
    salt: &[u8],
    nonce: &[u8],
    wrapped_key: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    let body = normalize_secret_key(secret_key_input)?;
    let wrap_key = derive_wrap_key(&body, salt);

    if nonce.len() != NONCE_LEN {
        return Err(Error::Crypto(format!(
            "recovery blob nonce has wrong length: {} (expected {NONCE_LEN})",
            nonce.len()
        )));
    }
    let mut nonce_arr = [0u8; NONCE_LEN];
    nonce_arr.copy_from_slice(nonce);

    let plaintext = aes_gcm_decrypt(&wrap_key, &nonce_arr, wrapped_key)?;
    if plaintext.len() != ACCOUNT_SEED_LEN {
        return Err(Error::Crypto(format!(
            "unwrapped account_id_key has wrong length: {} (expected {ACCOUNT_SEED_LEN})",
            plaintext.len()
        )));
    }
    Ok(plaintext)
}

/// What the server persists for a fresh account identity. Safe to send: the
/// private key is only ever there as `wrapped_key`, sealed under the Secret Key.
pub struct AccountIdentityMaterial {
    /// ML-DSA-44 public key (1312 bytes).
    pub account_id_pub: Vec<u8>,
    /// HKDF salt for the Secret-Key-derived wrap key.
    pub salt: [u8; SALT_LEN],
    pub nonce: [u8; NONCE_LEN],
    pub wrapped_key: Vec<u8>,
}

/// Generate an account identity with no server write: the keypair and a
/// one-time Secret Key, the private key wrapped under it, and the key plus a
/// fresh `db_key` installed in `AppState.unlock`.
///
/// Returns the Secret Key to show once and the material the caller POSTs to
/// `/v1/auth/establish-identity`. The caller ensures no identity exists yet.
pub async fn generate_account_identity_material(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<(String, AccountIdentityMaterial)> {
    let mut rng = OsRng;

    let (private_bytes, public_bytes) = generate_account_keypair(&mut rng);

    let secret_key_display = generate_secret_key_string();
    let secret_key_body = normalize_secret_key(&secret_key_display)
        .expect("freshly-generated secret key must normalize");

    let mut salt = [0u8; SALT_LEN];
    rng.fill_bytes(&mut salt);
    let mut nonce = [0u8; NONCE_LEN];
    rng.fill_bytes(&mut nonce);

    let wrap_key = derive_wrap_key(&secret_key_body, &salt);
    let wrapped = aes_gcm_encrypt(&wrap_key, &nonce, &*private_bytes)?;

    *state.unlock.lock().await = Some(unlock_state_with_fresh_db_key(user_id, &*private_bytes));

    // Pin the public half locally so a later unwrap here can be checked against it.
    if let Err(e) = remember_published_account_id_pub(state, user_id, &public_bytes).await {
        crate::diag!("[identity] could not record the published account_id_pub: {e}");
    }

    Ok((
        secret_key_display,
        AccountIdentityMaterial {
            account_id_pub: public_bytes,
            salt,
            nonce,
            wrapped_key: wrapped,
        },
    ))
}

/// A fresh account keypair: `(32-byte seed, 1312-byte public key)`. The seed is
/// what every storage and transfer path carries.
fn generate_account_keypair(rng: &mut OsRng) -> (Zeroizing<[u8; ACCOUNT_SEED_LEN]>, Vec<u8>) {
    let mut seed = Zeroizing::new([0u8; ACCOUNT_SEED_LEN]);
    rng.fill_bytes(&mut *seed);
    let signing_key = AccountSigningKey::from_seed(&(*seed).into());
    let public_bytes = signing_key.verifying_key().encode().to_vec();
    (seed, public_bytes)
}

/// An `UnlockState` with the given account key and a fresh `db_key`, for paths
/// that just produced key material with no PIN yet.
pub(crate) fn unlock_state_with_fresh_db_key(
    user_id: &str,
    account_id_private: &[u8],
) -> UnlockState {
    let mut db_key = vec![0u8; DB_KEY_LEN];
    OsRng.fill_bytes(&mut db_key);
    UnlockState {
        user_id: user_id.to_string(),
        db_key: Zeroizing::new(db_key),
        account_id_key: Zeroizing::new(account_id_private.to_vec()),
    }
}

/// Mirrors `commands::pin::ACCOUNT_ID_KEY_WRAPPED_SLOT`.
const ACCOUNT_ID_KEY_WRAPPED_SLOT: &str = "account_id_key_wrapped";

/// True if this device holds the account key for `user_id` in any form: in
/// `AppState.unlock`, PIN-wrapped, or in the legacy unwrapped slot.
pub async fn has_local_account_identity(state: &AppState, user_id: &str) -> Result<bool> {
    {
        let guard = state.unlock.lock().await;
        if let Some(u) = guard.as_ref() {
            if u.user_id == user_id {
                return Ok(true);
            }
        }
    }
    if state
        .keystore
        .load_for_user(ACCOUNT_ID_KEY_WRAPPED_SLOT, user_id)
        .await?
        .is_some()
    {
        return Ok(true);
    }
    Ok(state
        .keystore
        .load_for_user(ACCOUNT_ID_KEY_KEYSTORE_SLOT, user_id)
        .await?
        .is_some())
}

/// True if the local account key's public half matches `remote_pub`. When only
/// the PIN-wrapped copy exists (locked), returns `Ok(true)`: the check is
/// deferred to `unlock`. No local key is `Ok(false)`.
pub async fn has_matching_local_account_identity(
    state: &AppState,
    user_id: &str,
    remote_pub: &[u8],
) -> Result<bool> {
    {
        let guard = state.unlock.lock().await;
        if let Some(u) = guard.as_ref() {
            if u.user_id == user_id {
                return Ok(account_id_pub_matches(&u.account_id_key, remote_pub));
            }
        }
    }
    if state
        .keystore
        .load_for_user(ACCOUNT_ID_KEY_WRAPPED_SLOT, user_id)
        .await?
        .is_some()
    {
        return Ok(true);
    }
    let Some(local_bytes) = state
        .keystore
        .load_for_user(ACCOUNT_ID_KEY_KEYSTORE_SLOT, user_id)
        .await?
    else {
        return Ok(false);
    };
    Ok(account_id_pub_matches(&local_bytes, remote_pub))
}

pub(crate) fn account_id_pub_matches(private_bytes: &[u8], remote_pub: &[u8]) -> bool {
    let Ok(signing_key) = signing_key_from_bytes(private_bytes) else {
        return false;
    };
    signing_key.verifying_key().encode().as_slice() == remote_pub
}

/// Record the account identity public key the DS published, so a later unwrap
/// can be checked against it. A missing record makes
/// [`adopt_recovered_account_identity`] refuse.
pub(crate) async fn remember_published_account_id_pub(
    state: &AppState,
    user_id: &str,
    account_id_pub: &[u8],
) -> Result<()> {
    state
        .keystore
        .store_for_user(ACCOUNT_ID_PUB_SLOT, user_id, account_id_pub)
        .await
}

pub(crate) async fn published_account_id_pub(
    state: &AppState,
    user_id: &str,
) -> Result<Option<Vec<u8>>> {
    state
        .keystore
        .load_for_user(ACCOUNT_ID_PUB_SLOT, user_id)
        .await
}

/// Mark that the server's account identity isn't the one this device holds
/// (reset elsewhere, or a lying server).
///
/// Marks instead of deleting: the claim is unauthenticated, and deleting the
/// only local copy would let any hostile DS strand a user on their Secret Key in
/// one response. Marking is reversible.
pub(crate) async fn mark_account_identity_superseded(
    state: &AppState,
    user_id: &str,
    claimed_pub: &[u8],
) -> Result<()> {
    state
        .keystore
        .store_for_user(ACCOUNT_IDENTITY_SUPERSEDED_SLOT, user_id, claimed_pub)
        .await
}

pub(crate) async fn clear_account_identity_superseded(
    state: &AppState,
    user_id: &str,
) -> Result<()> {
    state
        .keystore
        .delete_for_user(ACCOUNT_IDENTITY_SUPERSEDED_SLOT, user_id)
        .await
}

/// True if this device was told its identity was superseded and hasn't
/// re-enrolled; drives `enrollment_required` with the old key kept intact.
pub(crate) async fn account_identity_superseded(
    state: &AppState,
    user_id: &str,
) -> Result<bool> {
    Ok(state
        .keystore
        .load_for_user(ACCOUNT_IDENTITY_SUPERSEDED_SLOT, user_id)
        .await?
        .is_some())
}

/// Install an account seed this device just unwrapped, from a sibling's approval
/// or the Secret Key recovery blob, only if it matches the account's published
/// identity key.
///
/// Closes both a forged approval blob (an attacker-chosen seed, see
/// `device_enrollment::unwrap_account_key`) and a replayed superseded recovery
/// row. To pass, a hostile DS must publish the forged key to everyone (bumping
/// `identity_version`, appending to the transparency-backed `account_key_log`,
/// invalidating every device cert) rather than aim one blob at one enrollment.
/// Fails closed with no published key on record.
pub(crate) async fn adopt_recovered_account_identity(
    state: &Arc<AppState>,
    user_id: &str,
    seed: &[u8],
) -> Result<()> {
    let published = published_account_id_pub(state, user_id)
        .await?
        .ok_or_else(|| {
            Error::Crypto(format!(
                "refusing to install an account identity for {user_id}: this device has no \
                 record of the account's published identity key to check it against"
            ))
        })?;
    if !account_id_pub_matches(seed, &published) {
        return Err(Error::Crypto(format!(
            "refusing to install an account identity for {user_id}: the unwrapped key is not \
             the account's published identity key"
        )));
    }

    // The user has acted, so drop the stale key and the marker together.
    if let Err(e) = wipe_local_account_identity(state, user_id).await {
        crate::diag!("[enrollment] clearing the superseded local key (non-fatal): {e}");
    }
    if let Err(e) = clear_account_identity_superseded(state, user_id).await {
        crate::diag!("[enrollment] clearing the superseded marker (non-fatal): {e}");
    }

    *state.unlock.lock().await = Some(unlock_state_with_fresh_db_key(user_id, seed));
    Ok(())
}

/// Delete every local copy of the account key: `AppState.unlock`, the wrapped
/// slot and the legacy slot.
pub async fn wipe_local_account_identity(state: &AppState, user_id: &str) -> Result<()> {
    {
        let mut guard = state.unlock.lock().await;
        let matches = guard.as_ref().is_some_and(|u| u.user_id == user_id);
        if matches {
            *guard = None;
        }
    }
    let _ = state
        .keystore
        .delete_for_user(ACCOUNT_ID_KEY_WRAPPED_SLOT, user_id)
        .await;
    state
        .keystore
        .delete_for_user(ACCOUNT_ID_KEY_KEYSTORE_SLOT, user_id)
        .await
}

/// Soft recovery: a fresh account identity under a new Secret Key, bumping
/// `identity_version`. For a user with neither their Secret Key nor a sibling
/// device; the caller authenticates them by email OTP first.
///
/// The DS writes the `identity_rotated` audit row in the rotation's own
/// transaction. A device-signed call is a plain rotation; an OTP-session-only
/// call is also the `reset-recover` wipe, atomically, so an email OTP alone can
/// never mint a key that inherits memberships and devices.
///
/// Returns the new Secret Key to show once.
pub async fn reset_identity(state: &Arc<AppState>, user_id: &str) -> Result<String> {
    let mut rng = OsRng;

    let (private_bytes, public_bytes) = generate_account_keypair(&mut rng);

    let secret_key_display = generate_secret_key_string();
    let secret_key_body =
        normalize_secret_key(&secret_key_display).expect("just-generated key must normalize");

    let mut salt = [0u8; SALT_LEN];
    rng.fill_bytes(&mut salt);
    let mut nonce = [0u8; NONCE_LEN];
    rng.fill_bytes(&mut nonce);
    let wrap_key = derive_wrap_key(&secret_key_body, &salt);
    let wrapped = aes_gcm_encrypt(&wrap_key, &nonce, &*private_bytes)?;

    // The version bump, `account_key_log` append and recovery rewrap must be one
    // CAS-guarded unit (#419) so concurrent rotations can't fork the transparency
    // history. The DS, sole writer, does it in one transaction; we send the
    // expected version.
    let based_on_version: i64 = crate::commands::ds_reads::account_status(state, user_id)
        .await?
        .map(|a| a.identity_version)
        .ok_or_else(|| {
            Error::Other(anyhow::anyhow!(
                "user {user_id} not found during reset_identity"
            ))
        })?;

    let new_version: i64 = {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::STANDARD;
        let body = pollis_api::account::RotateIdentityBody {
            based_on_version,
            account_id_pub: b64.encode(&public_bytes),
            salt: b64.encode(salt),
            nonce: b64.encode(nonce),
            wrapped_key: b64.encode(&wrapped),
            // Auth on: must equal the signed user. Auth off: this is the actor, and a body
            // without it is refused.
            user_id: Some(user_id.to_string()),
        };
        let resp =
            crate::commands::mls::ds_post_signed_or_session(state, &body)
                .await?;
        let status = resp.status();
        if status.as_u16() == 409 {
            // A concurrent rotation won this version: refuse rather than fork the log.
            return Err(Error::Other(anyhow::anyhow!(
                "identity rotation lost a concurrent race (account_key_log CAS); retry"
            )));
        }
        if !status.is_success() {
            let txt = resp.text().await.unwrap_or_default();
            return Err(Error::Other(anyhow::anyhow!(
                "DS rotate-identity {status}: {txt}"
            )));
        }
        // Typed (#922): the version is the transparency log's leaf index, never guessed.
        let pollis_api::account::RotateIdentityResponse::Ok { identity_version } =
            crate::commands::mls::decode_response::<pollis_api::account::RotateIdentityBody>(resp)
                .await?;
        identity_version
    };

    // The key never touches the keystore unwrapped; `set_pin` wraps it.
    *state.unlock.lock().await = Some(unlock_state_with_fresh_db_key(user_id, &*private_bytes));

    // This device now is the account's identity: re-pin and clear any marker.
    if let Err(e) = remember_published_account_id_pub(state, user_id, &public_bytes).await {
        crate::diag!("[reset] could not record the rotated account_id_pub: {e}");
    }
    if let Err(e) = clear_account_identity_superseded(state, user_id).await {
        crate::diag!("[reset] could not clear the superseded marker: {e}");
    }

    // Re-sign every `user_device` row under the rotated key, or other clients can't
    // verify their certs until each device next runs `ensure_device_cert`.
    // Best-effort.
    if let Err(e) = crate::commands::mls::resign_stale_device_certs(state, user_id).await {
        crate::diag!(
            "[reset] resign_stale_device_certs failed (non-fatal): {e}"
        );
    }

    crate::diag!("[reset] identity rotated to version {new_version} for {user_id}");

    Ok(secret_key_display)
}

/// Load the account identity signing key: from `AppState.unlock`, else the
/// legacy unwrapped slot. `Error::Crypto("locked")` when only the wrapped slot
/// exists, so the frontend can prompt for the PIN.
pub async fn load_account_id_key(state: &AppState, user_id: &str) -> Result<AccountSigningKey> {
    {
        let guard = state.unlock.lock().await;
        if let Some(u) = guard.as_ref() {
            if u.user_id == user_id {
                return signing_key_from_bytes(&u.account_id_key);
            }
        }
    }

    if state
        .keystore
        .load_for_user(ACCOUNT_ID_KEY_WRAPPED_SLOT, user_id)
        .await?
        .is_some()
    {
        return Err(Error::Crypto(
            "account_id_key locked: enter PIN to unlock".into(),
        ));
    }

    let bytes = state
        .keystore
        .load_for_user(ACCOUNT_ID_KEY_KEYSTORE_SLOT, user_id)
        .await?
        .ok_or_else(|| {
            Error::Crypto(format!("account_id_key not in keystore for user {user_id}"))
        })?;
    signing_key_from_bytes(&bytes)
}

fn signing_key_from_bytes(bytes: &[u8]) -> Result<AccountSigningKey> {
    if bytes.len() != ACCOUNT_SEED_LEN {
        return Err(Error::Crypto(format!(
            "account_id_key has wrong length: {} (expected {})",
            bytes.len(),
            ACCOUNT_SEED_LEN
        )));
    }
    let mut arr = [0u8; ACCOUNT_SEED_LEN];
    arr.copy_from_slice(bytes);
    Ok(AccountSigningKey::from_seed(&arr.into()))
}

// The cert payload format and verification live in `pollis-device-cert` so
// `pollis-relay` can verify certs without a dependency cycle. Signing stays
// here (it needs the keystore) over the same shared payload, so signer and
// verifier can't drift.
pub use pollis_device_cert::{
    device_cert_signed_payload, verify_device_cert, DEVICE_CERT_DOMAIN, MLDSA44_PUB_LEN,
    MLDSA44_SIG_LEN,
};

/// Sign a device cert with the account identity key. One cert covers both of
/// the device's MLS leaf keys, Ed25519 and ML-DSA-44 (#668). The caller persists
/// the result in `user_device`.
pub async fn sign_device_cert(
    state: &AppState,
    user_id: &str,
    device_id: &str,
    ed25519_device_pub: &[u8],
    mldsa_device_pub: &[u8],
    identity_version: u32,
    issued_at: u64,
) -> Result<Vec<u8>> {
    let signing_key = load_account_id_key(state, user_id).await?;
    let payload = device_cert_signed_payload(
        device_id,
        ed25519_device_pub,
        mldsa_device_pub,
        identity_version,
        issued_at,
    )?;
    Ok(signing_key.sign(&payload).encode().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_key_roundtrip_format() {
        for _ in 0..100 {
            let display = generate_secret_key_string();
            assert!(display.starts_with("A3-"), "should start with version prefix");
            let body = normalize_secret_key(&display).unwrap();
            assert_eq!(body.len(), SECRET_KEY_BODY_CHARS);
            for c in body.chars() {
                assert!(SECRET_KEY_ALPHABET.contains(&(c as u8)));
            }
        }
    }

    #[test]
    fn normalize_accepts_lowercase_and_whitespace() {
        let display = generate_secret_key_string();
        let noisy = format!(" {}  ", display.to_lowercase());
        let body = normalize_secret_key(&noisy).unwrap();
        let canonical = normalize_secret_key(&display).unwrap();
        assert_eq!(body, canonical);
    }

    #[test]
    fn normalize_rejects_wrong_prefix() {
        let err = normalize_secret_key("B1-0123456789ABCDEFGHJKMNPQRSTV").unwrap_err();
        assert!(format!("{err}").contains("A3-"));
    }

    #[test]
    fn normalize_rejects_wrong_length() {
        let err = normalize_secret_key("A3-SHORT").unwrap_err();
        assert!(format!("{err}").contains("30"));
    }

    #[test]
    fn normalize_rejects_ambiguous_characters() {
        let err = normalize_secret_key("A3-IIIIIIIIIIIIIIIIIIIIIIIIIIIIII").unwrap_err();
        assert!(format!("{err}").contains("invalid"));
    }

    #[test]
    fn wrap_unwrap_roundtrip() {
        let mut rng = OsRng;
        let mut salt = [0u8; SALT_LEN];
        rng.fill_bytes(&mut salt);
        let mut nonce = [0u8; NONCE_LEN];
        rng.fill_bytes(&mut nonce);

        let sk = generate_secret_key_string();
        let body = normalize_secret_key(&sk).unwrap();
        let wrap_key = derive_wrap_key(&body, &salt);

        let plaintext = b"32-byte-account-key-seed-0000000";
        let ct = aes_gcm_encrypt(&wrap_key, &nonce, plaintext).unwrap();
        let pt = aes_gcm_decrypt(&wrap_key, &nonce, &ct).unwrap();
        assert_eq!(&*pt, plaintext);
    }

    fn device_pubs(seed: u8) -> ([u8; 32], Vec<u8>) {
        let mldsa = AccountSigningKey::from_seed(&[seed; 32].into())
            .verifying_key()
            .encode()
            .to_vec();
        ([seed; 32], mldsa)
    }

    /// The mint format must match the golden vector `pollis-device-cert` freezes and
    /// `pollis-relay` verifies; if this and `golden_vector_is_frozen` disagree, mint
    /// and verify have drifted.
    #[test]
    fn sign_device_cert_matches_shared_golden_vector() {
        let account = AccountSigningKey::from_seed(&[42u8; 32].into());
        let device_id = "01HXGOLDENVECTORDEVICE0001";
        let ed_pub = [7u8; 32];
        let pq_pub = AccountSigningKey::from_seed(&[8u8; 32].into())
            .verifying_key()
            .encode()
            .to_vec();

        let payload =
            device_cert_signed_payload(device_id, &ed_pub, &pq_pub, 3, 1_800_000_000).unwrap();
        let cert = account.sign(&payload).encode().to_vec();

        // Same digest `golden_vector_is_frozen` pins (ML-DSA blobs are kilobytes).
        assert_eq!(
            fnv1a_128_hex(&cert),
            "6a63116689699b97746e47352a07d857",
            "pollis-core mint format drifted from the shared golden vector"
        );

        verify_device_cert(
            &account.verifying_key().encode(),
            device_id,
            &ed_pub,
            &pq_pub,
            3,
            1_800_000_000,
            &cert,
        )
        .expect("golden cert must verify under the shared verifier");
    }

    /// Mirror of `pollis_device_cert`'s test fingerprint, duplicated on purpose so
    /// the two crates reach the constant independently.
    fn fnv1a_128_hex(bytes: &[u8]) -> String {
        const OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
        const PRIME: u128 = 0x0000000001000000000000000000013b;
        let mut hash = OFFSET;
        for b in bytes {
            hash ^= u128::from(*b);
            hash = hash.wrapping_mul(PRIME);
        }
        hash.to_be_bytes().iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn device_cert_roundtrip() {
        let signing = AccountSigningKey::from_seed(&rand::random::<[u8; 32]>().into());
        let account_pub = signing.verifying_key().encode();

        let device_id = "01HXABCDEFGHJKMNPQRSTVWXYZ";
        let (ed_pub, pq_pub) = device_pubs(9);
        let payload =
            device_cert_signed_payload(device_id, &ed_pub, &pq_pub, 1, 1_700_000_000).unwrap();
        let sig = signing.sign(&payload).encode();

        verify_device_cert(
            &account_pub,
            device_id,
            &ed_pub,
            &pq_pub,
            1,
            1_700_000_000,
            &sig,
        )
        .expect("cert should verify with matching inputs");
    }

    #[test]
    fn device_cert_rejects_tampered_fields() {
        let signing = AccountSigningKey::from_seed(&rand::random::<[u8; 32]>().into());
        let account_pub = signing.verifying_key().encode();

        let device_id = "01HXABCDEFGHJKMNPQRSTVWXYZ";
        let (ed_pub, pq_pub) = device_pubs(9);
        let payload =
            device_cert_signed_payload(device_id, &ed_pub, &pq_pub, 1, 1_700_000_000).unwrap();
        let sig_bytes = signing.sign(&payload).encode();

        assert!(verify_device_cert(
            &account_pub,
            "01HXZZZZZZZZZZZZZZZZZZZZZZ",
            &ed_pub,
            &pq_pub,
            1,
            1_700_000_000,
            &sig_bytes,
        )
        .is_err());

        let (other_ed, other_pq) = device_pubs(11);
        assert!(verify_device_cert(
            &account_pub,
            device_id,
            &other_ed,
            &pq_pub,
            1,
            1_700_000_000,
            &sig_bytes,
        )
        .is_err());

        assert!(verify_device_cert(
            &account_pub,
            device_id,
            &ed_pub,
            &other_pq,
            1,
            1_700_000_000,
            &sig_bytes,
        )
        .is_err());

        assert!(verify_device_cert(
            &account_pub,
            device_id,
            &ed_pub,
            &pq_pub,
            2,
            1_700_000_000,
            &sig_bytes,
        )
        .is_err());

        assert!(verify_device_cert(
            &account_pub,
            device_id,
            &ed_pub,
            &pq_pub,
            1,
            1_700_000_001,
            &sig_bytes,
        )
        .is_err());
    }

    #[test]
    fn device_cert_rejects_wrong_account_key() {
        let legitimate = AccountSigningKey::from_seed(&rand::random::<[u8; 32]>().into());
        let attacker = AccountSigningKey::from_seed(&rand::random::<[u8; 32]>().into());

        let device_id = "01HXABCDEFGHJKMNPQRSTVWXYZ";
        let (ed_pub, pq_pub) = device_pubs(9);
        let payload =
            device_cert_signed_payload(device_id, &ed_pub, &pq_pub, 1, 1_700_000_000).unwrap();
        let sig = attacker.sign(&payload).encode();

        let res = verify_device_cert(
            &legitimate.verifying_key().encode(),
            device_id,
            &ed_pub,
            &pq_pub,
            1,
            1_700_000_000,
            &sig,
        );
        assert!(res.is_err());
    }

    /// The private key must stay a 32-byte seed; every at-rest format depends on it.
    #[test]
    fn account_private_key_is_still_a_32_byte_seed() {
        let mut rng = OsRng;
        let (seed, public_bytes) = generate_account_keypair(&mut rng);
        assert_eq!(seed.len(), 32);
        assert_eq!(public_bytes.len(), MLDSA44_PUB_LEN);
        assert!(account_id_pub_matches(&*seed, &public_bytes));
    }

    #[test]
    fn unwrap_recovery_blob_roundtrip() {
        let sk_display = generate_secret_key_string();
        let body = normalize_secret_key(&sk_display).unwrap();

        let mut rng = OsRng;
        let mut salt = [0u8; SALT_LEN];
        rng.fill_bytes(&mut salt);
        let mut nonce = [0u8; NONCE_LEN];
        rng.fill_bytes(&mut nonce);

        let wrap_key = derive_wrap_key(&body, &salt);
        let private = [0x17u8; ACCOUNT_SEED_LEN];
        let wrapped = aes_gcm_encrypt(&wrap_key, &nonce, &private).unwrap();

        let noisy = format!(" {}\n", sk_display.to_lowercase());
        let unwrapped = unwrap_recovery_blob(&noisy, &salt, &nonce, &wrapped).unwrap();
        assert_eq!(*unwrapped, private.to_vec());
    }

    #[test]
    fn unwrap_recovery_blob_wrong_key_fails() {
        let sk1 = generate_secret_key_string();
        let sk2 = generate_secret_key_string();

        let mut rng = OsRng;
        let mut salt = [0u8; SALT_LEN];
        rng.fill_bytes(&mut salt);
        let mut nonce = [0u8; NONCE_LEN];
        rng.fill_bytes(&mut nonce);

        let wrap_key = derive_wrap_key(&normalize_secret_key(&sk1).unwrap(), &salt);
        let private = [0xaau8; ACCOUNT_SEED_LEN];
        let wrapped = aes_gcm_encrypt(&wrap_key, &nonce, &private).unwrap();

        assert!(unwrap_recovery_blob(&sk2, &salt, &nonce, &wrapped).is_err());
    }

    #[test]
    fn wrap_with_wrong_key_fails() {
        let mut rng = OsRng;
        let mut salt = [0u8; SALT_LEN];
        rng.fill_bytes(&mut salt);
        let mut nonce = [0u8; NONCE_LEN];
        rng.fill_bytes(&mut nonce);

        let sk1 = generate_secret_key_string();
        let sk2 = generate_secret_key_string();
        let wrap1 = derive_wrap_key(&normalize_secret_key(&sk1).unwrap(), &salt);
        let wrap2 = derive_wrap_key(&normalize_secret_key(&sk2).unwrap(), &salt);

        let ct = aes_gcm_encrypt(&wrap1, &nonce, b"secret").unwrap();
        assert!(aes_gcm_decrypt(&wrap2, &nonce, &ct).is_err());
    }

    // Schema tests for the `account_key_log` table (migration 000005): backfill and
    // UNIQUE constraint.
    mod account_key_log {
        use rusqlite::Connection;

        use crate::db::BASELINE_SQL as BASELINE;

        /// Pulled from the runner's list, so this also fails if 000005 isn't registered.
        fn migration_sql() -> &'static str {
            crate::db::POST_BASELINE_MIGRATIONS
                .iter()
                .find(|(version, _, _)| *version == 5)
                .expect("migration 5 (account_key_log) must be registered")
                .2
        }

        fn baseline_db() -> Connection {
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
            conn.execute_batch(BASELINE).unwrap();
            conn
        }

        fn insert_user(conn: &Connection, id: &str) {
            conn.execute(
                "INSERT INTO users (id, email, username) VALUES (?1, ?2, ?3)",
                rusqlite::params![id, format!("{id}@x.com"), id],
            )
            .unwrap();
        }

        fn log_rows(conn: &Connection, user_id: &str) -> Vec<(i64, i64, Vec<u8>)> {
            let mut stmt = conn
                .prepare(
                    "SELECT seq, identity_version, account_id_pub \
                     FROM account_key_log WHERE user_id = ?1 ORDER BY seq",
                )
                .unwrap();
            stmt.query_map(rusqlite::params![user_id], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
        }

        // Seeds a version-1 identity: the users row plus its first log row.
        fn signup_dual_write(conn: &Connection, user_id: &str, pubkey: &[u8]) {
            conn.execute(
                "UPDATE users SET account_id_pub = ?1, identity_version = 1 WHERE id = ?2",
                rusqlite::params![pubkey, user_id],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO account_key_log (user_id, account_id_pub, identity_version) \
                 VALUES (?1, ?2, 1)",
                rusqlite::params![user_id, pubkey],
            )
            .unwrap();
        }

        #[test]
        fn migration_backfills_existing_users_with_identity() {
            let conn = baseline_db();

            insert_user(&conn, "alice");
            let alice_key = vec![0xAAu8; 32];
            conn.execute(
                "UPDATE users SET account_id_pub = ?1, identity_version = 3 WHERE id = 'alice'",
                rusqlite::params![alice_key],
            )
            .unwrap();
            insert_user(&conn, "bob");

            conn.execute_batch(migration_sql()).unwrap();

            let alice = log_rows(&conn, "alice");
            assert_eq!(alice.len(), 1);
            assert_eq!(alice[0].1, 3);
            assert_eq!(alice[0].2, alice_key);
            assert!(log_rows(&conn, "bob").is_empty());
        }

        #[test]
        fn unique_constraint_rejects_duplicate_version() {
            let conn = baseline_db();
            conn.execute_batch(migration_sql()).unwrap();

            insert_user(&conn, "erin");
            let key = vec![0x33u8; 32];
            signup_dual_write(&conn, "erin", &key);

            let dup = conn.execute(
                "INSERT INTO account_key_log (user_id, account_id_pub, identity_version) \
                 VALUES (?1, ?2, 1)",
                rusqlite::params!["erin", key],
            );
            assert!(dup.is_err(), "duplicate (user_id, identity_version) must be rejected");
            assert_eq!(log_rows(&conn, "erin").len(), 1);
        }
    }

    mod adopting_and_superseding {
        use super::*;

        fn test_state() -> Arc<AppState> {
            Arc::new(AppState::new_with_parts(
                crate::config::Config::for_test().expect("test config"),
                Arc::new(crate::keystore::InMemoryKeystore::new()),
            ))
        }

        fn identity(seed_byte: u8) -> ([u8; ACCOUNT_SEED_LEN], Vec<u8>) {
            let seed = [seed_byte; ACCOUNT_SEED_LEN];
            let pub_bytes = AccountSigningKey::from_seed(&seed.into())
                .verifying_key()
                .encode()
                .to_vec();
            (seed, pub_bytes)
        }

        #[tokio::test]
        async fn the_accounts_own_key_is_adopted() {
            let state = test_state();
            let (seed, published) = identity(11);
            remember_published_account_id_pub(&state, "u1", &published)
                .await
                .unwrap();

            adopt_recovered_account_identity(&state, "u1", &seed)
                .await
                .expect("the published key must still be installable");

            let guard = state.unlock.lock().await;
            let unlocked = guard.as_ref().expect("unlock populated");
            assert_eq!(unlocked.user_id, "u1");
            assert_eq!(&**unlocked.account_id_key, &seed[..]);
        }

        /// M1: a seed that isn't the published identity (forged approval blob, or a
        /// replayed superseded recovery row) is refused.
        #[tokio::test]
        async fn a_seed_that_is_not_the_published_identity_is_refused() {
            let state = test_state();
            let (_current_seed, published) = identity(11);
            let (attacker_seed, _) = identity(99);
            remember_published_account_id_pub(&state, "u1", &published)
                .await
                .unwrap();

            let err = adopt_recovered_account_identity(&state, "u1", &attacker_seed)
                .await
                .expect_err("a seed that is not the account's identity must be refused");
            assert!(
                format!("{err}").contains("published identity key"),
                "refused for the wrong reason: {err}"
            );
            assert!(
                state.unlock.lock().await.is_none(),
                "nothing may be installed when the check fails"
            );
        }

        /// Fails closed: withholding `account_id_pub` can't turn the check off.
        #[tokio::test]
        async fn an_unwrapped_key_is_refused_when_no_published_key_is_known() {
            let state = test_state();
            let (seed, _) = identity(11);
            let err = adopt_recovered_account_identity(&state, "u1", &seed)
                .await
                .expect_err("with nothing to check against, installing must be refused");
            assert!(format!("{err}").contains("no record"), "got: {err}");
            assert!(state.unlock.lock().await.is_none());
        }

        /// M2: being told the identity changed marks the device instead of deleting its
        /// key.
        #[tokio::test]
        async fn being_told_the_identity_changed_keeps_the_local_key() {
            let state = test_state();
            let (seed, _published) = identity(11);
            let (_, someone_elses_pub) = identity(99);
            state
                .keystore
                .store_for_user(ACCOUNT_ID_KEY_KEYSTORE_SLOT, "u1", &seed)
                .await
                .unwrap();

            mark_account_identity_superseded(&state, "u1", &someone_elses_pub)
                .await
                .unwrap();

            assert!(
                account_identity_superseded(&state, "u1").await.unwrap(),
                "the device must know it needs to re-enroll"
            );
            assert!(
                has_local_account_identity(&state, "u1").await.unwrap(),
                "the key must survive an unauthenticated server claim"
            );
            assert_eq!(
                state
                    .keystore
                    .load_for_user(ACCOUNT_ID_KEY_KEYSTORE_SLOT, "u1")
                    .await
                    .unwrap()
                    .as_deref(),
                Some(&seed[..]),
                "and it must be the same bytes, not a re-derived stand-in"
            );
        }

        /// Re-enrolling clears the marker and retires the stale key.
        #[tokio::test]
        async fn the_marker_clears_when_the_user_re_enrolls() {
            let state = test_state();
            let (old_seed, _) = identity(11);
            let (new_seed, new_published) = identity(12);
            state
                .keystore
                .store_for_user(ACCOUNT_ID_KEY_KEYSTORE_SLOT, "u1", &old_seed)
                .await
                .unwrap();
            mark_account_identity_superseded(&state, "u1", &new_published)
                .await
                .unwrap();
            remember_published_account_id_pub(&state, "u1", &new_published)
                .await
                .unwrap();

            adopt_recovered_account_identity(&state, "u1", &new_seed)
                .await
                .unwrap();

            assert!(
                !account_identity_superseded(&state, "u1").await.unwrap(),
                "re-enrolling clears the marker"
            );
            assert!(
                state
                    .keystore
                    .load_for_user(ACCOUNT_ID_KEY_KEYSTORE_SLOT, "u1")
                    .await
                    .unwrap()
                    .is_none(),
                "and the stale key goes with it, now that the user has acted"
            );
        }

        #[tokio::test]
        async fn clearing_the_marker_is_idempotent() {
            let state = test_state();
            clear_account_identity_superseded(&state, "u1")
                .await
                .unwrap();
            assert!(!account_identity_superseded(&state, "u1").await.unwrap());
        }
    }
}
