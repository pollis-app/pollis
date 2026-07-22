//! The Pollis **offline device-certificate** primitive.
//!
//! A device cert is a 64-byte Ed25519 signature, made by a user's long-lived
//! *account identity key*, that binds a specific device's MLS signing public key
//! to that account. Verifying it proves — with **zero I/O**, no database, no
//! network — that "this device's signing key belongs to the account claiming
//! `account_id_pub`".
//!
//! This crate is intentionally tiny (deps: `ed25519-dalek` + std only) so that
//! the two very different tiers that both need the format share exactly one
//! source of truth:
//!
//! - [`pollis-core`](../pollis_core/index.html) mints certs (it holds the account
//!   key in the OS keystore) and re-exports [`verify_device_cert`] so every
//!   existing MLS/identity caller is unchanged.
//! - `pollis-relay` verifies certs at its connection handshake. Because the check
//!   is offline, the relay tier stays **out of the Turso metadata plane** — it
//!   holds no DB credentials and makes no per-connection lookup (design
//!   `docs/relay-overlay-design.md` §11.1). That operational separation is the
//!   whole point of pulling the primitive into a shared, cycle-free crate:
//!   `pollis-core` already depends on `pollis-relay`, so `pollis-relay` cannot
//!   depend back on `pollis-core`.
//!
//! # Wire format (canonical, do not reimplement elsewhere)
//!
//! [`device_cert_signed_payload`] is the exact byte string the account key signs:
//!
//! ```text
//! DEVICE_CERT_DOMAIN            (22 bytes, trailing NUL included)
//! u8  device_id_len            || device_id bytes            (UTF-8)
//! u8  mls_sig_pub_len          || mls_signature_pub bytes
//! u32 identity_version         (big-endian)
//! u64 issued_at                (big-endian, unix seconds)
//! ```
//!
//! All length prefixes are `u8`: device_ids are ULIDs (26 bytes) and Ed25519
//! public keys are 32 bytes, both well under 255.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// Domain separator baked into every device cert signature. Bump the suffix if
/// the signed payload format ever changes so old signatures cannot be
/// reinterpreted under a new schema.
pub const DEVICE_CERT_DOMAIN: &[u8] = b"pollis-device-cert-v1\x00";

/// Errors from building or verifying a device cert. Kept dependency-free (a plain
/// `std::error::Error`) so this crate needs nothing beyond `ed25519-dalek`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceCertError {
    /// A length-prefixed field exceeds the `u8` prefix it must fit in.
    FieldTooLong {
        field: &'static str,
        len: usize,
    },
    /// `account_id_pub` was not 32 bytes.
    BadAccountKeyLen(usize),
    /// `cert_bytes` was not 64 bytes.
    BadCertLen(usize),
    /// `account_id_pub` bytes are not a valid Ed25519 point.
    BadAccountKey(String),
    /// The signature did not verify against `account_id_pub` over the payload.
    SignatureInvalid,
}

impl std::fmt::Display for DeviceCertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceCertError::FieldTooLong { field, len } => {
                write!(f, "{field} too long for cert payload ({len} > 255)")
            }
            DeviceCertError::BadAccountKeyLen(n) => {
                write!(f, "account_id_pub has wrong length: {n} (expected 32)")
            }
            DeviceCertError::BadCertLen(n) => {
                write!(f, "device_cert has wrong length: {n} (expected 64)")
            }
            DeviceCertError::BadAccountKey(e) => write!(f, "bad account_id_pub: {e}"),
            DeviceCertError::SignatureInvalid => write!(f, "device cert signature invalid"),
        }
    }
}

impl std::error::Error for DeviceCertError {}

/// Build the canonical byte string that a device cert signs over. See the module
/// docs for the layout. This is the single definition of the format; every
/// signer and verifier — in `pollis-core` and `pollis-relay` alike — routes
/// through it so the two can never drift.
pub fn device_cert_signed_payload(
    device_id: &str,
    mls_signature_pub: &[u8],
    identity_version: u32,
    issued_at: u64,
) -> Result<Vec<u8>, DeviceCertError> {
    if device_id.len() > u8::MAX as usize {
        return Err(DeviceCertError::FieldTooLong {
            field: "device_id",
            len: device_id.len(),
        });
    }
    if mls_signature_pub.len() > u8::MAX as usize {
        return Err(DeviceCertError::FieldTooLong {
            field: "mls_signature_pub",
            len: mls_signature_pub.len(),
        });
    }

    let mut out = Vec::with_capacity(
        DEVICE_CERT_DOMAIN.len() + 1 + device_id.len() + 1 + mls_signature_pub.len() + 4 + 8,
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

/// Verify a device cert against a user's published `account_id_pub`.
///
/// Returns `Ok(())` if the 64-byte Ed25519 `cert_bytes` is a valid signature by
/// `account_id_pub` over the canonical payload for `(device_id,
/// mls_signature_pub, identity_version, issued_at)` — i.e. the account key
/// certified that `mls_signature_pub` is a device of that account. `Err`
/// otherwise. Pure and offline: no clock, no I/O.
pub fn verify_device_cert(
    account_id_pub: &[u8],
    device_id: &str,
    mls_signature_pub: &[u8],
    identity_version: u32,
    issued_at: u64,
    cert_bytes: &[u8],
) -> Result<(), DeviceCertError> {
    if account_id_pub.len() != 32 {
        return Err(DeviceCertError::BadAccountKeyLen(account_id_pub.len()));
    }
    if cert_bytes.len() != 64 {
        return Err(DeviceCertError::BadCertLen(cert_bytes.len()));
    }

    let mut pk_arr = [0u8; 32];
    pk_arr.copy_from_slice(account_id_pub);
    let verifying_key = VerifyingKey::from_bytes(&pk_arr)
        .map_err(|e| DeviceCertError::BadAccountKey(e.to_string()))?;

    let mut sig_arr = [0u8; 64];
    sig_arr.copy_from_slice(cert_bytes);
    let signature = Signature::from_bytes(&sig_arr);

    let payload =
        device_cert_signed_payload(device_id, mls_signature_pub, identity_version, issued_at)?;

    verifying_key
        .verify(&payload, &signature)
        .map_err(|_| DeviceCertError::SignatureInvalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn account_key(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    #[test]
    fn roundtrip_verifies() {
        let acct = account_key(3);
        let device_id = "01HXABCDEFGHJKMNPQRSTVWXYZ";
        let mls_sig_pub = [9u8; 32];
        let payload = device_cert_signed_payload(device_id, &mls_sig_pub, 1, 1_700_000_000).unwrap();
        let sig = acct.sign(&payload);
        verify_device_cert(
            &acct.verifying_key().to_bytes(),
            device_id,
            &mls_sig_pub,
            1,
            1_700_000_000,
            &sig.to_bytes(),
        )
        .expect("must verify with matching inputs");
    }

    #[test]
    fn wrong_account_key_rejected() {
        let acct = account_key(3);
        let attacker = account_key(4);
        let device_id = "01HXABCDEFGHJKMNPQRSTVWXYZ";
        let mls_sig_pub = [9u8; 32];
        let payload = device_cert_signed_payload(device_id, &mls_sig_pub, 1, 1).unwrap();
        let sig = attacker.sign(&payload);
        assert_eq!(
            verify_device_cert(
                &acct.verifying_key().to_bytes(),
                device_id,
                &mls_sig_pub,
                1,
                1,
                &sig.to_bytes()
            ),
            Err(DeviceCertError::SignatureInvalid)
        );
    }

    #[test]
    fn tampered_fields_rejected() {
        let acct = account_key(3);
        let device_id = "01HXABCDEFGHJKMNPQRSTVWXYZ";
        let mls_sig_pub = [9u8; 32];
        let payload = device_cert_signed_payload(device_id, &mls_sig_pub, 1, 1_700_000_000).unwrap();
        let sig = acct.sign(&payload).to_bytes();
        let acct_pub = acct.verifying_key().to_bytes();

        // Wrong identity_version.
        assert!(verify_device_cert(&acct_pub, device_id, &mls_sig_pub, 2, 1_700_000_000, &sig).is_err());
        // Wrong issued_at.
        assert!(verify_device_cert(&acct_pub, device_id, &mls_sig_pub, 1, 1_700_000_001, &sig).is_err());
        // Wrong device_id.
        assert!(verify_device_cert(&acct_pub, "other", &mls_sig_pub, 1, 1_700_000_000, &sig).is_err());
    }

    /// GOLDEN VECTOR — freezes the exact wire format so neither `pollis-core`
    /// (which mints certs) nor `pollis-relay` (which verifies them at its
    /// handshake) can silently drift. Both route through THIS crate, so this one
    /// vector is the cross-crate consistency guard: change the payload layout and
    /// this test breaks, forcing a deliberate `DEVICE_CERT_DOMAIN` bump. The
    /// signature is deterministic (Ed25519 over a fixed seed + fixed inputs).
    #[test]
    fn golden_vector_is_frozen() {
        let account = SigningKey::from_bytes(&[42u8; 32]);
        let device_id = "01HXGOLDENVECTORDEVICE0001";
        let device_pub = SigningKey::from_bytes(&[7u8; 32]).verifying_key().to_bytes();
        let identity_version = 3;
        let issued_at = 1_800_000_000;

        let payload =
            device_cert_signed_payload(device_id, &device_pub, identity_version, issued_at).unwrap();
        assert_eq!(payload.len(), 94, "payload layout changed — bump the domain");

        let expected_cert = hex_decode(
            "af1b7e28940c1bdc67201d9d91bd9263c02f456929a7bc2f6ce35a7398d72561\
             6842eed1734cd16a41836edc45b7a722c670b018f0786a573ec42bdf0cd4cd03",
        );
        let sig = account.sign(&payload);
        assert_eq!(
            sig.to_bytes().to_vec(),
            expected_cert,
            "signature over the canonical payload changed — format drift"
        );

        // And the frozen cert verifies (the whole point).
        verify_device_cert(
            &account.verifying_key().to_bytes(),
            device_id,
            &device_pub,
            identity_version,
            issued_at,
            &expected_cert,
        )
        .expect("golden cert must verify");
    }

    fn hex_decode(s: &str) -> Vec<u8> {
        let clean: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        clean
            .chunks(2)
            .map(|c| u8::from_str_radix(std::str::from_utf8(c).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn bad_lengths_rejected() {
        assert_eq!(
            verify_device_cert(&[0u8; 31], "d", &[0u8; 32], 1, 1, &[0u8; 64]),
            Err(DeviceCertError::BadAccountKeyLen(31))
        );
        assert_eq!(
            verify_device_cert(&[0u8; 32], "d", &[0u8; 32], 1, 1, &[0u8; 63]),
            Err(DeviceCertError::BadCertLen(63))
        );
    }
}
