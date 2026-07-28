//! The Pollis **offline device-certificate** primitive.
//!
//! A device cert is an **ML-DSA-44** signature, made by a user's long-lived
//! *account identity key*, that binds a specific device's MLS signing public
//! keys to that account. Verifying it proves — with **zero I/O**, no database,
//! no network — that "this device's signing keys belong to the account claiming
//! `account_id_pub`".
//!
//! This crate is intentionally tiny (deps: `ml-dsa` + std only) so that the two
//! very different tiers that both need the format share exactly one source of
//! truth:
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
//! - `pollis-delivery` re-verifies the cert at `POST /v1/auth/publish-device-cert`
//!   through this same crate, so the DS cannot drift from the client either.
//!
//! # Wire format (canonical, do not reimplement elsewhere)
//!
//! [`device_cert_signed_payload`] is the exact byte string the account key signs:
//!
//! ```text
//! DEVICE_CERT_DOMAIN            (22 bytes, trailing NUL included)
//! u8  device_id_len            || device_id bytes            (UTF-8)
//! u16 ed25519_pub_len          || ed25519 device pub bytes   (big-endian prefix)
//! u16 mldsa_pub_len            || ML-DSA-44 device pub bytes  (big-endian prefix)
//! u32 identity_version         (big-endian)
//! u64 issued_at                (big-endian, unix seconds)
//! ```
//!
//! # Why v2 certifies TWO device keys (#668)
//!
//! A device holds a *separate MLS leaf signing key per signature scheme*: the
//! classic suite's leaves are Ed25519, the PQ suite's are ML-DSA-44. During the
//! classic→PQ overlap a device is in groups of both suites at once, so certifying
//! only one scheme's key would leave the other leaf uncertified — and an
//! uncertified leaf is exactly what cross-signing exists to prevent. v2 therefore
//! binds **both** keys in one signature: at no point in the migration is a leaf
//! key admitted that the account key did not certify.
//!
//! The `u8` length prefixes of v1 became `u16` because an ML-DSA-44 public key is
//! 1312 bytes. `device_id` keeps its `u8` prefix (ULIDs are 26 bytes).

use ml_dsa::{EncodedSignature, EncodedVerifyingKey, MlDsa44, Signature, Verifier, VerifyingKey};

/// Domain separator baked into every device cert signature. Bump the suffix if
/// the signed payload format ever changes so old signatures cannot be
/// reinterpreted under a new schema.
pub const DEVICE_CERT_DOMAIN: &[u8] = b"pollis-device-cert-v2\x00";

/// Encoded length of an ML-DSA-44 public key — the account identity key, and the
/// device's PQ leaf key.
pub const MLDSA44_PUB_LEN: usize = 1312;

/// Encoded length of an ML-DSA-44 signature — i.e. of a device cert.
pub const MLDSA44_SIG_LEN: usize = 2420;

/// Errors from building or verifying a device cert. Kept dependency-free (a plain
/// `std::error::Error`) so this crate needs nothing beyond `ml-dsa`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceCertError {
    /// A length-prefixed field exceeds the prefix width it must fit in.
    FieldTooLong {
        field: &'static str,
        len: usize,
    },
    /// `account_id_pub` was not [`MLDSA44_PUB_LEN`] bytes.
    BadAccountKeyLen(usize),
    /// `cert_bytes` was not [`MLDSA44_SIG_LEN`] bytes.
    BadCertLen(usize),
    /// The signature did not verify against `account_id_pub` over the payload.
    SignatureInvalid,
}

impl std::fmt::Display for DeviceCertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DeviceCertError::FieldTooLong { field, len } => {
                write!(f, "{field} too long for cert payload ({len} bytes)")
            }
            DeviceCertError::BadAccountKeyLen(n) => {
                write!(f, "account_id_pub has wrong length: {n} (expected {MLDSA44_PUB_LEN})")
            }
            DeviceCertError::BadCertLen(n) => {
                write!(f, "device_cert has wrong length: {n} (expected {MLDSA44_SIG_LEN})")
            }
            DeviceCertError::SignatureInvalid => write!(f, "device cert signature invalid"),
        }
    }
}

impl std::error::Error for DeviceCertError {}

/// Build the canonical byte string that a device cert signs over. See the module
/// docs for the layout. This is the single definition of the format; every
/// signer and verifier — in `pollis-core`, `pollis-relay` and `pollis-delivery`
/// alike — routes through it so they can never drift.
///
/// `ed25519_device_pub` is the device's classic-suite MLS leaf key,
/// `mldsa_device_pub` its PQ-suite leaf key. Both are certified by one signature
/// so neither leaf is ever uncertified during the classic→PQ overlap.
pub fn device_cert_signed_payload(
    device_id: &str,
    ed25519_device_pub: &[u8],
    mldsa_device_pub: &[u8],
    identity_version: u32,
    issued_at: u64,
) -> Result<Vec<u8>, DeviceCertError> {
    if device_id.len() > u8::MAX as usize {
        return Err(DeviceCertError::FieldTooLong {
            field: "device_id",
            len: device_id.len(),
        });
    }
    if ed25519_device_pub.len() > u16::MAX as usize {
        return Err(DeviceCertError::FieldTooLong {
            field: "ed25519_device_pub",
            len: ed25519_device_pub.len(),
        });
    }
    if mldsa_device_pub.len() > u16::MAX as usize {
        return Err(DeviceCertError::FieldTooLong {
            field: "mldsa_device_pub",
            len: mldsa_device_pub.len(),
        });
    }

    let mut out = Vec::with_capacity(
        DEVICE_CERT_DOMAIN.len()
            + 1
            + device_id.len()
            + 2
            + ed25519_device_pub.len()
            + 2
            + mldsa_device_pub.len()
            + 4
            + 8,
    );
    out.extend_from_slice(DEVICE_CERT_DOMAIN);
    out.push(device_id.len() as u8);
    out.extend_from_slice(device_id.as_bytes());
    out.extend_from_slice(&(ed25519_device_pub.len() as u16).to_be_bytes());
    out.extend_from_slice(ed25519_device_pub);
    out.extend_from_slice(&(mldsa_device_pub.len() as u16).to_be_bytes());
    out.extend_from_slice(mldsa_device_pub);
    out.extend_from_slice(&identity_version.to_be_bytes());
    out.extend_from_slice(&issued_at.to_be_bytes());
    Ok(out)
}

/// Verify a device cert against a user's published `account_id_pub`.
///
/// Returns `Ok(())` if `cert_bytes` is a valid ML-DSA-44 signature by
/// `account_id_pub` over the canonical payload for `(device_id,
/// ed25519_device_pub, mldsa_device_pub, identity_version, issued_at)` — i.e. the
/// account key certified that both of those leaf keys belong to that device of
/// that account. `Err` otherwise. Pure and offline: no clock, no I/O.
pub fn verify_device_cert(
    account_id_pub: &[u8],
    device_id: &str,
    ed25519_device_pub: &[u8],
    mldsa_device_pub: &[u8],
    identity_version: u32,
    issued_at: u64,
    cert_bytes: &[u8],
) -> Result<(), DeviceCertError> {
    let encoded_key: &EncodedVerifyingKey<MlDsa44> = account_id_pub
        .try_into()
        .map_err(|_| DeviceCertError::BadAccountKeyLen(account_id_pub.len()))?;
    let encoded_sig: &EncodedSignature<MlDsa44> = cert_bytes
        .try_into()
        .map_err(|_| DeviceCertError::BadCertLen(cert_bytes.len()))?;

    // Decoding a verifying key is infallible for a correctly sized input: every
    // 1312-byte string is a well-formed (if not necessarily used) ML-DSA-44 key.
    let verifying_key = VerifyingKey::<MlDsa44>::decode(encoded_key);
    // A signature, by contrast, carries a hint that can be malformed; treat that
    // as an invalid signature rather than a distinct error.
    let signature =
        Signature::<MlDsa44>::decode(encoded_sig).ok_or(DeviceCertError::SignatureInvalid)?;

    let payload = device_cert_signed_payload(
        device_id,
        ed25519_device_pub,
        mldsa_device_pub,
        identity_version,
        issued_at,
    )?;

    verifying_key
        .verify(&payload, &signature)
        .map_err(|_| DeviceCertError::SignatureInvalid)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ml_dsa::{Keypair, Signer, SigningKey};

    fn account_key(seed: u8) -> SigningKey<MlDsa44> {
        SigningKey::<MlDsa44>::from_seed(&[seed; 32].into())
    }

    /// A stand-in device leaf key pair: 32 bytes of Ed25519 pub, 1312 of ML-DSA.
    fn device_pubs(seed: u8) -> ([u8; 32], Vec<u8>) {
        let mldsa = account_key(seed).verifying_key().encode().to_vec();
        ([seed; 32], mldsa)
    }

    #[test]
    fn encoded_lengths_match_the_ml_dsa_types() {
        assert_eq!(EncodedVerifyingKey::<MlDsa44>::default().len(), MLDSA44_PUB_LEN);
        assert_eq!(EncodedSignature::<MlDsa44>::default().len(), MLDSA44_SIG_LEN);
    }

    #[test]
    fn roundtrip_verifies() {
        let acct = account_key(3);
        let device_id = "01HXABCDEFGHJKMNPQRSTVWXYZ";
        let (ed_pub, pq_pub) = device_pubs(9);
        let payload =
            device_cert_signed_payload(device_id, &ed_pub, &pq_pub, 1, 1_700_000_000).unwrap();
        let sig = acct.sign(&payload);
        verify_device_cert(
            &acct.verifying_key().encode(),
            device_id,
            &ed_pub,
            &pq_pub,
            1,
            1_700_000_000,
            &sig.encode(),
        )
        .expect("must verify with matching inputs");
    }

    #[test]
    fn wrong_account_key_rejected() {
        let acct = account_key(3);
        let attacker = account_key(4);
        let device_id = "01HXABCDEFGHJKMNPQRSTVWXYZ";
        let (ed_pub, pq_pub) = device_pubs(9);
        let payload = device_cert_signed_payload(device_id, &ed_pub, &pq_pub, 1, 1).unwrap();
        let sig = attacker.sign(&payload);
        assert_eq!(
            verify_device_cert(
                &acct.verifying_key().encode(),
                device_id,
                &ed_pub,
                &pq_pub,
                1,
                1,
                &sig.encode()
            ),
            Err(DeviceCertError::SignatureInvalid)
        );
    }

    #[test]
    fn tampered_fields_rejected() {
        let acct = account_key(3);
        let device_id = "01HXABCDEFGHJKMNPQRSTVWXYZ";
        let (ed_pub, pq_pub) = device_pubs(9);
        let payload =
            device_cert_signed_payload(device_id, &ed_pub, &pq_pub, 1, 1_700_000_000).unwrap();
        let sig = acct.sign(&payload).encode();
        let acct_pub = acct.verifying_key().encode();

        // Wrong identity_version.
        assert!(
            verify_device_cert(&acct_pub, device_id, &ed_pub, &pq_pub, 2, 1_700_000_000, &sig)
                .is_err()
        );
        // Wrong issued_at.
        assert!(
            verify_device_cert(&acct_pub, device_id, &ed_pub, &pq_pub, 1, 1_700_000_001, &sig)
                .is_err()
        );
        // Wrong device_id.
        assert!(
            verify_device_cert(&acct_pub, "other", &ed_pub, &pq_pub, 1, 1_700_000_000, &sig)
                .is_err()
        );
        // Substituted Ed25519 leaf key — the classic-suite half of the binding.
        let (other_ed, _) = device_pubs(11);
        assert!(
            verify_device_cert(&acct_pub, device_id, &other_ed, &pq_pub, 1, 1_700_000_000, &sig)
                .is_err()
        );
        // Substituted ML-DSA leaf key — the PQ-suite half. Catching this is the
        // entire reason v2 certifies both keys.
        let (_, other_pq) = device_pubs(11);
        assert!(
            verify_device_cert(&acct_pub, device_id, &ed_pub, &other_pq, 1, 1_700_000_000, &sig)
                .is_err()
        );
    }

    /// GOLDEN VECTOR — freezes the exact wire format so none of `pollis-core`
    /// (which mints certs), `pollis-relay` (which verifies them at its handshake)
    /// or `pollis-delivery` (which re-verifies at publish) can silently drift.
    /// All three route through THIS crate, so this one vector is the cross-crate
    /// consistency guard: change the payload layout and this test breaks, forcing
    /// a deliberate `DEVICE_CERT_DOMAIN` bump. The signature is deterministic —
    /// `ml-dsa`'s `Signer` impl is the FIPS-204 deterministic variant (`rnd = 0`)
    /// over a fixed seed and fixed inputs.
    #[test]
    fn golden_vector_is_frozen() {
        let account = SigningKey::<MlDsa44>::from_seed(&[42u8; 32].into());
        let device_id = "01HXGOLDENVECTORDEVICE0001";
        let ed_pub = [7u8; 32];
        let pq_pub = SigningKey::<MlDsa44>::from_seed(&[8u8; 32].into())
            .verifying_key()
            .encode()
            .to_vec();
        let identity_version = 3;
        let issued_at = 1_800_000_000;

        let payload = device_cert_signed_payload(
            device_id,
            &ed_pub,
            &pq_pub,
            identity_version,
            issued_at,
        )
        .unwrap();
        assert_eq!(payload.len(), 1409, "payload layout changed — bump the domain");

        // Pin the payload by digest rather than by 1409 literal bytes; the
        // signature below pins it a second time, end to end.
        assert_eq!(
            hex_encode(&fnv1a_128(&payload)),
            "38079e5dfb626a4a98a6496e5f381233",
            "canonical payload bytes changed — format drift"
        );

        let sig = account.sign(&payload).encode().to_vec();
        assert_eq!(
            hex_encode(&fnv1a_128(&sig)),
            "6a63116689699b97746e47352a07d857",
            "signature over the canonical payload changed — format drift"
        );

        // And the frozen cert verifies (the whole point).
        verify_device_cert(
            &account.verifying_key().encode(),
            device_id,
            &ed_pub,
            &pq_pub,
            identity_version,
            issued_at,
            &sig,
        )
        .expect("golden cert must verify");
    }

    /// A 128-bit FNV-1a over the input. Not a security hash — just a stable,
    /// dependency-free fingerprint so the golden vector can pin multi-kilobyte
    /// ML-DSA blobs without pasting them into the source.
    fn fnv1a_128(bytes: &[u8]) -> [u8; 16] {
        const OFFSET: u128 = 0x6c62272e07bb014262b821756295c58d;
        const PRIME: u128 = 0x0000000001000000000000000000013b;
        let mut hash = OFFSET;
        for b in bytes {
            hash ^= u128::from(*b);
            hash = hash.wrapping_mul(PRIME);
        }
        hash.to_be_bytes()
    }

    fn hex_encode(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn bad_lengths_rejected() {
        assert_eq!(
            verify_device_cert(
                &[0u8; MLDSA44_PUB_LEN - 1],
                "d",
                &[0u8; 32],
                &[0u8; MLDSA44_PUB_LEN],
                1,
                1,
                &[0u8; MLDSA44_SIG_LEN]
            ),
            Err(DeviceCertError::BadAccountKeyLen(MLDSA44_PUB_LEN - 1))
        );
        assert_eq!(
            verify_device_cert(
                &[0u8; MLDSA44_PUB_LEN],
                "d",
                &[0u8; 32],
                &[0u8; MLDSA44_PUB_LEN],
                1,
                1,
                &[0u8; MLDSA44_SIG_LEN - 1]
            ),
            Err(DeviceCertError::BadCertLen(MLDSA44_SIG_LEN - 1))
        );
    }
}
