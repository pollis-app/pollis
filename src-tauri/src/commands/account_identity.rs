//! Account identity key management.
//!
//! Each user has a long-lived Ed25519 "account identity key" generated once
//! at first-device signup. This key is distinct from per-device MLS signing
//! keys — it represents the human, not any specific device, and is what
//! lets every device of that user cross-sign each other into MLS groups.
//!
//! The private key is:
//!   1. Stored in the OS keystore on every device that holds a copy.
//!   2. Also stored server-side in Turso's `account_recovery` table,
//!      encrypted (wrapped) under a key derived from a user-held Secret
//!      Key via HKDF-SHA256. The Secret Key itself is never sent to the
//!      server.
//!
//! The Secret Key is shown to the user exactly once at signup. Losing it
//! and all devices holding the account identity key means the account is
//! unrecoverable (by design — see `MULTI_DEVICE_ENROLLMENT.md`).

use std::sync::Arc;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use hkdf::Hkdf;
use rand::rngs::OsRng;
use rand::RngCore;
use sha2::Sha256;

use crate::error::{Error, Result};
use crate::keystore::Keystore;
use crate::state::AppState;

// ── Constants ────────────────────────────────────────────────────────────────

/// Keystore slot for this device's copy of the account identity private key.
const ACCOUNT_ID_KEY_KEYSTORE_SLOT: &str = "account_id_key";

/// Crockford base32 alphabet (32 chars, drops I/L/O/U for visual clarity).
const SECRET_KEY_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

/// Number of base32 chars in the Secret Key body (excluding version prefix
/// and dashes). 30 chars × 5 bits = 150 bits of entropy — well above the
/// 128-bit floor for uncrackable-offline.
const SECRET_KEY_BODY_CHARS: usize = 30;

/// Current Secret Key format version prefix. Bump if the derivation or
/// alphabet ever changes so old kits can be recognized and rejected.
const SECRET_KEY_VERSION: &str = "A3";

/// HKDF info string — bind the wrap key to this specific use so the same
/// Secret Key cannot be repurposed for any other KDF output.
const HKDF_INFO: &[u8] = b"pollis-account-key-wrap-v1";

const SALT_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const ED25519_PRIVATE_LEN: usize = 32;

// ── Secret Key formatting ────────────────────────────────────────────────────

/// Generate a fresh random Secret Key and return it in display form:
/// `A3-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX-XXXXX` (6 groups of 5 Crockford base32 chars).
pub fn generate_secret_key_string() -> String {
    let mut rng = OsRng;
    let mut body = String::with_capacity(SECRET_KEY_BODY_CHARS);
    for _ in 0..SECRET_KEY_BODY_CHARS {
        // Take a fresh random byte and reduce to 5 bits — rejection is
        // unnecessary because 32 divides 256 evenly.
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

/// Normalize a user-entered Secret Key: strip whitespace, uppercase, remove
/// the version prefix and all dashes, validate the result is exactly
/// `SECRET_KEY_BODY_CHARS` characters from the Crockford alphabet.
///
/// Returned string is the raw body, suitable for feeding into HKDF as IKM.
pub fn normalize_secret_key(input: &str) -> Result<String> {
    let cleaned: String = input
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_uppercase();

    let body_with_dashes = cleaned.strip_prefix(&format!("{SECRET_KEY_VERSION}-")).ok_or_else(|| {
        Error::Crypto(format!(
            "Secret Key must start with \"{SECRET_KEY_VERSION}-\""
        ))
    })?;

    let body: String = body_with_dashes.chars().filter(|c| *c != '-').collect();

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

// ── Key derivation and AEAD ──────────────────────────────────────────────────

fn derive_wrap_key(secret_key_body: &str, salt: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(Some(salt), secret_key_body.as_bytes());
    let mut out = [0u8; 32];
    hk.expand(HKDF_INFO, &mut out)
        .expect("HKDF-SHA256 expand 32 bytes is always valid");
    out
}

fn aes_gcm_encrypt(key: &[u8; 32], nonce: &[u8; NONCE_LEN], plaintext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key));
    cipher
        .encrypt(GenericArray::from_slice(nonce), plaintext)
        .map_err(|e| Error::Crypto(format!("aes-256-gcm encrypt: {e}")))
}

fn aes_gcm_decrypt(key: &[u8; 32], nonce: &[u8; NONCE_LEN], ciphertext: &[u8]) -> Result<Vec<u8>> {
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key));
    cipher
        .decrypt(GenericArray::from_slice(nonce), ciphertext)
        .map_err(|e| Error::Crypto(format!("aes-256-gcm decrypt: {e}")))
}

/// Unwrap a server-stored `account_recovery` row using a user-entered
/// Secret Key. Returns the raw 32-byte `account_id_key` private material
/// on success. Used by the Secret Key recovery path during new-device
/// enrollment.
///
/// `secret_key_input` is the raw text the user typed (case, whitespace,
/// and dash tolerance are handled by `normalize_secret_key`).
pub fn unwrap_recovery_blob(
    secret_key_input: &str,
    salt: &[u8],
    nonce: &[u8],
    wrapped_key: &[u8],
) -> Result<Vec<u8>> {
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
    if plaintext.len() != ED25519_PRIVATE_LEN {
        return Err(Error::Crypto(format!(
            "unwrapped account_id_key has wrong length: {} (expected {ED25519_PRIVATE_LEN})",
            plaintext.len()
        )));
    }
    Ok(plaintext)
}

// ── Public API ───────────────────────────────────────────────────────────────

/// Generate a fresh account identity for `user_id`. Writes:
///   - `users.account_id_pub` and `users.identity_version = 1`
///   - a new row in `account_recovery`
///   - the private key into this device's OS keystore
///
/// Returns the formatted Secret Key to show the user exactly once.
/// Caller is responsible for ensuring the user has no existing identity
/// (`users.account_id_pub IS NULL`) before invoking this.
pub async fn generate_account_identity(state: &Arc<AppState>, user_id: &str) -> Result<String> {
    let mut rng = OsRng;

    let signing_key = SigningKey::generate(&mut rng);
    let verifying_key: VerifyingKey = signing_key.verifying_key();
    let private_bytes: [u8; ED25519_PRIVATE_LEN] = signing_key.to_bytes();
    let public_bytes: [u8; 32] = verifying_key.to_bytes();

    let secret_key_display = generate_secret_key_string();
    let secret_key_body = normalize_secret_key(&secret_key_display)
        .expect("freshly-generated secret key must normalize");

    let mut salt = [0u8; SALT_LEN];
    rng.fill_bytes(&mut salt);
    let mut nonce = [0u8; NONCE_LEN];
    rng.fill_bytes(&mut nonce);

    let wrap_key = derive_wrap_key(&secret_key_body, &salt);
    let wrapped = aes_gcm_encrypt(&wrap_key, &nonce, &private_bytes)?;

    state.keystore.store_for_user(ACCOUNT_ID_KEY_KEYSTORE_SLOT, user_id, &private_bytes).await?;

    let conn = state.remote_db.conn().await?;

    conn.execute(
        "UPDATE users SET account_id_pub = ?1, identity_version = 1 WHERE id = ?2",
        libsql::params![public_bytes.to_vec(), user_id.to_string()],
    )
    .await?;

    conn.execute(
        "INSERT INTO account_recovery \
         (user_id, identity_version, salt, nonce, wrapped_key, created_at, updated_at) \
         VALUES (?1, 1, ?2, ?3, ?4, datetime('now'), datetime('now'))",
        libsql::params![
            user_id.to_string(),
            salt.to_vec(),
            nonce.to_vec(),
            wrapped
        ],
    )
    .await?;

    Ok(secret_key_display)
}

/// True if this device holds an account identity private key for `user_id`
/// in its OS keystore.
pub async fn has_local_account_identity(keystore: &dyn Keystore, user_id: &str) -> Result<bool> {
    Ok(keystore.load_for_user(ACCOUNT_ID_KEY_KEYSTORE_SLOT, user_id)
        .await?
        .is_some())
}

/// True if the locally-stored account identity key's public half matches
/// the `remote_pub` bytes read from `users.account_id_pub`. Returns
/// `false` if the key is absent locally OR if it doesn't match (the
/// user rotated their identity via soft recovery on another device).
///
/// A `false` return means this device is orphaned and must re-enroll.
pub async fn has_matching_local_account_identity(
    keystore: &dyn Keystore,
    user_id: &str,
    remote_pub: &[u8],
) -> Result<bool> {
    let Some(local_bytes) = keystore.load_for_user(ACCOUNT_ID_KEY_KEYSTORE_SLOT, user_id).await?
    else {
        return Ok(false);
    };
    if local_bytes.len() != ED25519_PRIVATE_LEN {
        return Ok(false);
    }
    let mut arr = [0u8; ED25519_PRIVATE_LEN];
    arr.copy_from_slice(&local_bytes);
    let signing_key = SigningKey::from_bytes(&arr);
    let local_pub: [u8; 32] = signing_key.verifying_key().to_bytes();
    Ok(local_pub.as_slice() == remote_pub)
}

/// Delete the locally-stored account identity private key. Called when
/// a device discovers (via `has_matching_local_account_identity`) that
/// the user has rotated their identity on another device and this
/// device is now orphaned.
pub async fn wipe_local_account_identity(keystore: &dyn Keystore, user_id: &str) -> Result<()> {
    keystore.delete_for_user(ACCOUNT_ID_KEY_KEYSTORE_SLOT, user_id).await
}

/// Soft recovery: generate a completely fresh account identity, bumping
/// `users.identity_version`, overwriting `users.account_id_pub`, and
/// replacing the `account_recovery` blob under a new Secret Key.
///
/// Used when the user has lost their old Secret Key AND has no sibling
/// device to approve a new-device enrollment. The caller is responsible
/// for authenticating the user through email OTP first — this function
/// simply trusts that the user is authorized.
///
/// Stores the new private key in the local OS keystore so the calling
/// device immediately becomes a valid member of the reset identity.
///
/// Writes a `security_event` row (`kind = 'identity_reset'`) so the
/// user can see the reset in the Security settings page.
///
/// Returns the new formatted Secret Key to show the user once.
pub async fn reset_identity(state: &Arc<AppState>, user_id: &str) -> Result<String> {
    let mut rng = OsRng;

    // 1. Generate a new Ed25519 keypair.
    let signing_key = SigningKey::generate(&mut rng);
    let verifying_key: VerifyingKey = signing_key.verifying_key();
    let private_bytes: [u8; ED25519_PRIVATE_LEN] = signing_key.to_bytes();
    let public_bytes: [u8; 32] = verifying_key.to_bytes();

    // 2. Generate a fresh Secret Key and wrap the new private key.
    let secret_key_display = generate_secret_key_string();
    let secret_key_body =
        normalize_secret_key(&secret_key_display).expect("just-generated key must normalize");

    let mut salt = [0u8; SALT_LEN];
    rng.fill_bytes(&mut salt);
    let mut nonce = [0u8; NONCE_LEN];
    rng.fill_bytes(&mut nonce);
    let wrap_key = derive_wrap_key(&secret_key_body, &salt);
    let wrapped = aes_gcm_encrypt(&wrap_key, &nonce, &private_bytes)?;

    // 3. Atomically bump identity_version, overwrite users.account_id_pub,
    //    and REPLACE the account_recovery row. We do this in three
    //    statements because libsql doesn't expose a transaction
    //    abstraction in our current setup; failure between statements
    //    leaves a partial state that the next reset_identity call can
    //    clean up.
    let conn = state.remote_db.conn().await?;
    conn.execute(
        "UPDATE users \
         SET account_id_pub = ?1, identity_version = identity_version + 1 \
         WHERE id = ?2",
        libsql::params![public_bytes.to_vec(), user_id.to_string()],
    )
    .await?;

    let new_version: i64 = {
        let mut rows = conn
            .query(
                "SELECT identity_version FROM users WHERE id = ?1",
                libsql::params![user_id.to_string()],
            )
            .await?;
        match rows.next().await? {
            Some(row) => row.get(0)?,
            None => {
                return Err(Error::Other(anyhow::anyhow!(
                    "user {user_id} not found during reset_identity"
                )))
            }
        }
    };

    conn.execute(
        "INSERT INTO account_recovery \
         (user_id, identity_version, salt, nonce, wrapped_key, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now'), datetime('now')) \
         ON CONFLICT(user_id) DO UPDATE SET \
             identity_version = excluded.identity_version, \
             salt = excluded.salt, \
             nonce = excluded.nonce, \
             wrapped_key = excluded.wrapped_key, \
             updated_at = datetime('now')",
        libsql::params![
            user_id.to_string(),
            new_version,
            salt.to_vec(),
            nonce.to_vec(),
            wrapped
        ],
    )
    .await?;

    // 4. Install the new private key in this device's OS keystore
    //    immediately so the calling device is enrolled under the new
    //    identity.
    state.keystore.store_for_user(ACCOUNT_ID_KEY_KEYSTORE_SLOT, user_id, &private_bytes).await?;

    // 5. Record the reset in the security log. Best-effort only.
    let event_id = ulid::Ulid::new().to_string();
    let _ = conn
        .execute(
            "INSERT INTO security_event (id, user_id, kind, device_id, metadata) \
             VALUES (?1, ?2, 'identity_reset', NULL, ?3)",
            libsql::params![
                event_id,
                user_id.to_string(),
                format!("new_identity_version={new_version}")
            ],
        )
        .await;

    Ok(secret_key_display)
}

/// Load the account identity signing key for `user_id` from the OS keystore.
pub async fn load_account_id_key(keystore: &dyn Keystore, user_id: &str) -> Result<SigningKey> {
    let bytes = keystore.load_for_user(ACCOUNT_ID_KEY_KEYSTORE_SLOT, user_id)
        .await?
        .ok_or_else(|| {
            Error::Crypto(format!("account_id_key not in keystore for user {user_id}"))
        })?;
    if bytes.len() != ED25519_PRIVATE_LEN {
        return Err(Error::Crypto(format!(
            "account_id_key has wrong length: {} (expected {})",
            bytes.len(),
            ED25519_PRIVATE_LEN
        )));
    }
    let mut arr = [0u8; ED25519_PRIVATE_LEN];
    arr.copy_from_slice(&bytes);
    Ok(SigningKey::from_bytes(&arr))
}

// ── Device certificate ───────────────────────────────────────────────────────

/// Domain separator baked into every device cert signature. Bump the suffix
/// if the signed payload format ever changes so old signatures cannot be
/// reinterpreted under a new schema.
const DEVICE_CERT_DOMAIN: &[u8] = b"pollis-device-cert-v1\x00";

/// Build the canonical byte string that a device cert signs over.
///
/// Layout:
///   domain_separator (22 bytes, trailing NUL included)
///   u8  device_id_len     ||  device_id bytes
///   u8  mls_sig_pub_len   ||  mls_sig_pub bytes
///   u32 identity_version  (big-endian)
///   u64 issued_at         (big-endian, unix seconds)
///
/// All length prefixes are u8 because device_ids are ULIDs (26 bytes) and
/// Ed25519 public keys are 32 bytes — both fit comfortably.
fn device_cert_signed_payload(
    device_id: &str,
    mls_signature_pub: &[u8],
    identity_version: u32,
    issued_at: u64,
) -> Result<Vec<u8>> {
    if device_id.len() > u8::MAX as usize {
        return Err(Error::Crypto(format!(
            "device_id too long for cert payload ({} > 255)",
            device_id.len()
        )));
    }
    if mls_signature_pub.len() > u8::MAX as usize {
        return Err(Error::Crypto(format!(
            "mls_signature_pub too long for cert payload ({} > 255)",
            mls_signature_pub.len()
        )));
    }

    let mut out = Vec::with_capacity(
        DEVICE_CERT_DOMAIN.len()
            + 1
            + device_id.len()
            + 1
            + mls_signature_pub.len()
            + 4
            + 8,
    );
    out.extend_from_slice(DEVICE_CERT_DOMAIN);
    out.push(device_id.len() as u8);
    out.extend_from_slice(device_id.as_bytes());
    out.push(mls_signature_pub.len() as u8);
    out.extend_from_slice(mls_signature_pub);
    out.extend_from_slice(&identity_version.to_be_bytes());
    out.extend_from_slice(&issued_at.to_be_bytes());
    Ok(out)
}

/// Sign a device cert for this device using the loaded account identity
/// key. The caller is responsible for persisting the returned signature,
/// `issued_at`, `identity_version`, and `mls_signature_pub` in the remote
/// `user_device` table so other clients can verify.
pub async fn sign_device_cert(
    keystore: &dyn Keystore,
    user_id: &str,
    device_id: &str,
    mls_signature_pub: &[u8],
    identity_version: u32,
    issued_at: u64,
) -> Result<Vec<u8>> {
    let signing_key = load_account_id_key(keystore, user_id).await?;
    let payload = device_cert_signed_payload(
        device_id,
        mls_signature_pub,
        identity_version,
        issued_at,
    )?;
    let signature: Signature = signing_key.sign(&payload);
    Ok(signature.to_bytes().to_vec())
}

/// Verify a device cert against a user's published `account_id_pub`.
///
/// Returns `Ok(())` if the cert is valid, `Err(Error::Crypto)` otherwise.
/// Used by every client before accepting a new device into an MLS group.
pub fn verify_device_cert(
    account_id_pub: &[u8],
    device_id: &str,
    mls_signature_pub: &[u8],
    identity_version: u32,
    issued_at: u64,
    cert_bytes: &[u8],
) -> Result<()> {
    if account_id_pub.len() != 32 {
        return Err(Error::Crypto(format!(
            "account_id_pub has wrong length: {} (expected 32)",
            account_id_pub.len()
        )));
    }
    if cert_bytes.len() != 64 {
        return Err(Error::Crypto(format!(
            "device_cert has wrong length: {} (expected 64)",
            cert_bytes.len()
        )));
    }

    let mut pk_arr = [0u8; 32];
    pk_arr.copy_from_slice(account_id_pub);
    let verifying_key = VerifyingKey::from_bytes(&pk_arr)
        .map_err(|e| Error::Crypto(format!("bad account_id_pub: {e}")))?;

    let mut sig_arr = [0u8; 64];
    sig_arr.copy_from_slice(cert_bytes);
    let signature = Signature::from_bytes(&sig_arr);

    let payload = device_cert_signed_payload(
        device_id,
        mls_signature_pub,
        identity_version,
        issued_at,
    )?;

    verifying_key
        .verify(&payload, &signature)
        .map_err(|e| Error::Crypto(format!("device cert signature invalid: {e}")))
}

// ── Tests ────────────────────────────────────────────────────────────────────

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
        // 'I' is deliberately omitted from the Crockford alphabet.
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

        let plaintext = b"32-byte-ed25519-private-key-000";
        let ct = aes_gcm_encrypt(&wrap_key, &nonce, plaintext).unwrap();
        let pt = aes_gcm_decrypt(&wrap_key, &nonce, &ct).unwrap();
        assert_eq!(pt, plaintext);
    }

    #[test]
    fn device_cert_roundtrip() {
        let mut rng = OsRng;
        let signing = SigningKey::generate(&mut rng);
        let account_pub = signing.verifying_key().to_bytes();

        let device_id = "01HXABCDEFGHJKMNPQRSTVWXYZ";
        let mls_sig_pub: [u8; 32] = rand::random();
        let payload = device_cert_signed_payload(device_id, &mls_sig_pub, 1, 1_700_000_000).unwrap();
        let sig: Signature = signing.sign(&payload);

        verify_device_cert(
            &account_pub,
            device_id,
            &mls_sig_pub,
            1,
            1_700_000_000,
            &sig.to_bytes(),
        )
        .expect("cert should verify with matching inputs");
    }

    #[test]
    fn device_cert_rejects_tampered_fields() {
        let mut rng = OsRng;
        let signing = SigningKey::generate(&mut rng);
        let account_pub = signing.verifying_key().to_bytes();

        let device_id = "01HXABCDEFGHJKMNPQRSTVWXYZ";
        let mls_sig_pub: [u8; 32] = rand::random();
        let payload = device_cert_signed_payload(device_id, &mls_sig_pub, 1, 1_700_000_000).unwrap();
        let sig = signing.sign(&payload);
        let sig_bytes = sig.to_bytes();

        // Wrong device_id.
        assert!(verify_device_cert(
            &account_pub,
            "01HXZZZZZZZZZZZZZZZZZZZZZZ",
            &mls_sig_pub,
            1,
            1_700_000_000,
            &sig_bytes,
        )
        .is_err());

        // Wrong mls_sig_pub.
        let other_pub: [u8; 32] = rand::random();
        assert!(verify_device_cert(
            &account_pub,
            device_id,
            &other_pub,
            1,
            1_700_000_000,
            &sig_bytes,
        )
        .is_err());

        // Wrong identity_version.
        assert!(verify_device_cert(
            &account_pub,
            device_id,
            &mls_sig_pub,
            2,
            1_700_000_000,
            &sig_bytes,
        )
        .is_err());

        // Wrong issued_at.
        assert!(verify_device_cert(
            &account_pub,
            device_id,
            &mls_sig_pub,
            1,
            1_700_000_001,
            &sig_bytes,
        )
        .is_err());
    }

    #[test]
    fn device_cert_rejects_wrong_account_key() {
        let mut rng = OsRng;
        let legitimate = SigningKey::generate(&mut rng);
        let attacker = SigningKey::generate(&mut rng);

        let device_id = "01HXABCDEFGHJKMNPQRSTVWXYZ";
        let mls_sig_pub: [u8; 32] = rand::random();
        let payload = device_cert_signed_payload(device_id, &mls_sig_pub, 1, 1_700_000_000).unwrap();
        let sig = attacker.sign(&payload);

        let res = verify_device_cert(
            &legitimate.verifying_key().to_bytes(),
            device_id,
            &mls_sig_pub,
            1,
            1_700_000_000,
            &sig.to_bytes(),
        );
        assert!(res.is_err());
    }

    #[test]
    fn unwrap_recovery_blob_roundtrip() {
        // Full generate-then-unwrap roundtrip using the public API.
        let sk_display = generate_secret_key_string();
        let body = normalize_secret_key(&sk_display).unwrap();

        let mut rng = OsRng;
        let mut salt = [0u8; SALT_LEN];
        rng.fill_bytes(&mut salt);
        let mut nonce = [0u8; NONCE_LEN];
        rng.fill_bytes(&mut nonce);

        let wrap_key = derive_wrap_key(&body, &salt);
        let private = [0x17u8; ED25519_PRIVATE_LEN];
        let wrapped = aes_gcm_encrypt(&wrap_key, &nonce, &private).unwrap();

        // User types it back with noise — should still unwrap.
        let noisy = format!(" {}\n", sk_display.to_lowercase());
        let unwrapped = unwrap_recovery_blob(&noisy, &salt, &nonce, &wrapped).unwrap();
        assert_eq!(unwrapped, private.to_vec());
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
        let private = [0xaau8; ED25519_PRIVATE_LEN];
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
}
