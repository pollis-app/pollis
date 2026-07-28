//! Device-certificate verification — a thin `bool`-returning wrapper over the
//! shared [`pollis_device_cert`] primitive.
//!
//! The first device-cert publish (`POST /v1/auth/publish-device-cert`) is the
//! PIVOT write that establishes the very `mls_signature_pub` the device-signature
//! gate ([`crate::auth`]) verifies against — so it cannot be device-signed. Its
//! gate is instead **OTP-session + cert-validity**: the DS re-verifies the cert's
//! ML-DSA-44 signature against the account's stored `account_id_pub` here,
//! exactly as every other client does before accepting a device into an MLS
//! group.
//!
//! This used to be a hand-copied port of the payload layout, kept in sync with
//! pollis-core by comment alone. It is now a delegation: the cert format lives in
//! exactly one crate, which both the client that mints certs and the two servers
//! that verify them depend on, so the layout cannot drift. (No crate cycle —
//! `pollis-device-cert` has no dependency on either side.)

/// Verify a device cert against a user's published `account_id_pub`. Returns
/// `true` iff the ML-DSA-44 signature is valid over the canonical payload
/// binding BOTH of the device's leaf signing keys. Any malformed input (wrong
/// key/sig length, over-long field) is `false` — we never accept on error.
pub fn verify_device_cert(
    account_id_pub: &[u8],
    device_id: &str,
    mls_signature_pub: &[u8],
    mls_signature_pub_pq: &[u8],
    identity_version: u32,
    issued_at: u64,
    cert_bytes: &[u8],
) -> bool {
    pollis_device_cert::verify_device_cert(
        account_id_pub,
        device_id,
        mls_signature_pub,
        mls_signature_pub_pq,
        identity_version,
        issued_at,
        cert_bytes,
    )
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ml_dsa::{Keypair, MlDsa44, Signer, SigningKey};
    use rand_core::{OsRng, RngCore as _};

    fn key() -> SigningKey<MlDsa44> {
        let mut s = [0u8; 32];
        OsRng.fill_bytes(&mut s);
        SigningKey::<MlDsa44>::from_seed(&s.into())
    }

    /// The two leaf keys the cert binds: a 32-byte classic one and a real
    /// ML-DSA-44 verifying key.
    fn device_pubs() -> ([u8; 32], Vec<u8>) {
        let mut ed_pub = [0u8; 32];
        OsRng.fill_bytes(&mut ed_pub);
        (ed_pub, key().verifying_key().encode().to_vec())
    }

    #[test]
    fn valid_cert_verifies() {
        let account = key();
        let device_id = "01HXABCDEFGHJKMNPQRSTVWXYZ";
        let (ed_pub, pq_pub) = device_pubs();
        let payload = pollis_device_cert::device_cert_signed_payload(
            device_id,
            &ed_pub,
            &pq_pub,
            1,
            1_700_000_000,
        )
        .unwrap();
        let sig = account.sign(&payload);
        assert!(verify_device_cert(
            &account.verifying_key().encode(),
            device_id,
            &ed_pub,
            &pq_pub,
            1,
            1_700_000_000,
            &sig.encode(),
        ));
    }

    #[test]
    fn wrong_account_key_rejected() {
        let legit = key();
        let attacker = key();
        let device_id = "01HXABCDEFGHJKMNPQRSTVWXYZ";
        let (ed_pub, pq_pub) = device_pubs();
        let payload = pollis_device_cert::device_cert_signed_payload(
            device_id,
            &ed_pub,
            &pq_pub,
            1,
            1_700_000_000,
        )
        .unwrap();
        let sig = attacker.sign(&payload);
        assert!(!verify_device_cert(
            &legit.verifying_key().encode(),
            device_id,
            &ed_pub,
            &pq_pub,
            1,
            1_700_000_000,
            &sig.encode(),
        ));
    }

    #[test]
    fn swapped_leaf_key_rejected() {
        // Cert v2 binds both leaf keys; substituting either one must fail.
        let account = key();
        let device_id = "01HXABCDEFGHJKMNPQRSTVWXYZ";
        let (ed_pub, pq_pub) = device_pubs();
        let (other_ed_pub, other_pq_pub) = device_pubs();
        let payload = pollis_device_cert::device_cert_signed_payload(
            device_id,
            &ed_pub,
            &pq_pub,
            1,
            1_700_000_000,
        )
        .unwrap();
        let sig = account.sign(&payload).encode();
        let account_pub = account.verifying_key().encode();
        assert!(!verify_device_cert(
            &account_pub,
            device_id,
            &other_ed_pub,
            &pq_pub,
            1,
            1_700_000_000,
            &sig,
        ));
        assert!(!verify_device_cert(
            &account_pub,
            device_id,
            &ed_pub,
            &other_pq_pub,
            1,
            1_700_000_000,
            &sig,
        ));
    }
}
