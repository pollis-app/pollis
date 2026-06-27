//! Device-certificate verification — a verbatim port of pollis-core's
//! `account_identity::{device_cert_signed_payload, verify_device_cert}`.
//!
//! The first device-cert publish (`POST /v1/auth/publish-device-cert`) is the
//! PIVOT write that establishes the very `mls_signature_pub` the device-signature
//! gate ([`crate::auth`]) verifies against — so it cannot be device-signed. Its
//! gate is instead **OTP-session + cert-validity**: the DS re-verifies the cert's
//! Ed25519 signature against the account's stored `account_id_pub` here, exactly
//! as every other client does before accepting a device into an MLS group. The
//! signed payload layout and domain separator MUST stay byte-for-byte identical
//! to pollis-core, or a cert the client signed would fail to verify here.

use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// Domain separator baked into every device cert signature. Identical to
/// pollis-core's `DEVICE_CERT_DOMAIN` — bump in lock-step if the format changes.
const DEVICE_CERT_DOMAIN: &[u8] = b"pollis-device-cert-v1\x00";

/// Build the canonical byte string a device cert signs over. Mirrors
/// pollis-core's `device_cert_signed_payload` exactly:
///
///   domain_separator (22 bytes, trailing NUL included)
///   u8  device_id_len     ||  device_id bytes
///   u8  mls_sig_pub_len   ||  mls_sig_pub bytes
///   u32 identity_version  (big-endian)
///   u64 issued_at         (big-endian, unix seconds)
fn device_cert_signed_payload(
    device_id: &str,
    mls_signature_pub: &[u8],
    identity_version: u32,
    issued_at: u64,
) -> Option<Vec<u8>> {
    if device_id.len() > u8::MAX as usize || mls_signature_pub.len() > u8::MAX as usize {
        return None;
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
    Some(out)
}

/// Verify a device cert against a user's published `account_id_pub`. Returns
/// `true` iff the 64-byte Ed25519 signature is valid over the canonical payload.
/// Any malformed input (wrong key/sig length, undecodable key) is `false` — we
/// never accept on error.
pub fn verify_device_cert(
    account_id_pub: &[u8],
    device_id: &str,
    mls_signature_pub: &[u8],
    identity_version: u32,
    issued_at: u64,
    cert_bytes: &[u8],
) -> bool {
    let pk_arr: [u8; 32] = match account_id_pub.try_into() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let sig_arr: [u8; 64] = match cert_bytes.try_into() {
        Ok(a) => a,
        Err(_) => return false,
    };
    let verifying_key = match VerifyingKey::from_bytes(&pk_arr) {
        Ok(vk) => vk,
        Err(_) => return false,
    };
    let signature = Signature::from_bytes(&sig_arr);
    let payload =
        match device_cert_signed_payload(device_id, mls_signature_pub, identity_version, issued_at) {
            Some(p) => p,
            None => return false,
        };
    verifying_key.verify(&payload, &signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand_core::{OsRng, RngCore as _};

    fn key() -> SigningKey {
        let mut s = [0u8; 32];
        OsRng.fill_bytes(&mut s);
        SigningKey::from_bytes(&s)
    }

    #[test]
    fn valid_cert_verifies() {
        let account = key();
        let device_id = "01HXABCDEFGHJKMNPQRSTVWXYZ";
        let mut mls_pub = [0u8; 32];
        OsRng.fill_bytes(&mut mls_pub);
        let payload = device_cert_signed_payload(device_id, &mls_pub, 1, 1_700_000_000).unwrap();
        let sig = account.sign(&payload);
        assert!(verify_device_cert(
            &account.verifying_key().to_bytes(),
            device_id,
            &mls_pub,
            1,
            1_700_000_000,
            &sig.to_bytes(),
        ));
    }

    #[test]
    fn wrong_account_key_rejected() {
        let legit = key();
        let attacker = key();
        let device_id = "01HXABCDEFGHJKMNPQRSTVWXYZ";
        let mut mls_pub = [0u8; 32];
        OsRng.fill_bytes(&mut mls_pub);
        let payload = device_cert_signed_payload(device_id, &mls_pub, 1, 1_700_000_000).unwrap();
        let sig = attacker.sign(&payload);
        assert!(!verify_device_cert(
            &legit.verifying_key().to_bytes(),
            device_id,
            &mls_pub,
            1,
            1_700_000_000,
            &sig.to_bytes(),
        ));
    }
}
