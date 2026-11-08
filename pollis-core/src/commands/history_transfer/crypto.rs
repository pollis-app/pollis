//! The cryptography of a history transfer (#1735). Pure: no state, no I/O.
//!
//! - **Device authentication.** Each side signs its half of the transcript
//!   with its ML-DSA-44 device key, under a domain string; the peer checks the
//!   signature against the key the PEER's device cert certifies, and the cert
//!   against this account's own `account_id_pub` (never the DS's copy).
//! - **Key agreement.** Hybrid: ML-KEM-768 and X25519. The receiver generates
//!   both key pairs (the request carries the public halves); the source
//!   encapsulates / does an ephemeral DH. The transfer key is HKDF-SHA256 over
//!   BOTH shared secrets, salted with the transfer id, with both transcript
//!   hashes in `info` — and the transcripts carry both public keys and both
//!   ciphertexts, so the combiner binds every input (the X-Wing shape).
//! - **Chunks.** XChaCha20-Poly1305 under that key with a random 192-bit nonce
//!   per seal, so a re-sealed index can never reuse a nonce; the AAD binds the
//!   domain, the transfer id, the index and the final flag.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use ml_dsa::{EncodedSignature, EncodedVerifyingKey, MlDsa44, Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use ml_kem::{Decapsulate as _, KeyExport as _, TryKeyInit as _};
use rand_core::{OsRng, RngCore as _};
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

pub(crate) const REQ_DOMAIN: &[u8] = b"pollis-history-transfer-req-v1";
pub(crate) const ACC_DOMAIN: &[u8] = b"pollis-history-transfer-acc-v1";
pub(crate) const KEY_INFO: &[u8] = b"pollis-history-transfer-key-v1";
pub(crate) const CHUNK_AAD_DOMAIN: &[u8] = b"pollis-history-transfer-chunk-v1";

/// The sealed-chunk format version (first byte).
pub(crate) const SEAL_VERSION: u8 = 1;
/// `version ‖ is_final ‖ nonce`.
pub(crate) const SEAL_HEADER_LEN: usize = 2 + 24;
pub(crate) const X25519_LEN: usize = 32;
pub(crate) const MLKEM_SEED_LEN: usize = 64;
pub(crate) const MLKEM_PUB_LEN: usize = 1184;
pub(crate) const MLKEM_CT_LEN: usize = 1088;
/// The receiver's stored secret: X25519 secret ‖ ML-KEM seed.
pub(crate) const KEM_SECRET_LEN: usize = X25519_LEN + MLKEM_SEED_LEN;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CryptoError {
    #[error("bad key or ciphertext length")]
    Length,
    #[error("invalid public key")]
    Key,
    #[error("non-contributory key agreement")]
    NonContributory,
    #[error("signature does not verify")]
    Signature,
    #[error("chunk does not open")]
    Open,
    #[error("unknown sealed-chunk version")]
    Version,
}

/// The receiver's half: secrets (stored, zeroized on drop) and public keys (in the request).
pub(crate) struct ReceiverKeys {
    pub secret: Zeroizing<Vec<u8>>,
    pub x25519_pub: [u8; X25519_LEN],
    pub mlkem_pub: Vec<u8>,
}

/// Fresh receiver key pairs.
pub(crate) fn receiver_keys() -> ReceiverKeys {
    let mut secret = Zeroizing::new(vec![0u8; KEM_SECRET_LEN]);
    OsRng.fill_bytes(&mut secret);
    let (x25519_pub, mlkem_pub) = receiver_publics(&secret).expect("fresh secret has the right length");
    ReceiverKeys { secret, x25519_pub, mlkem_pub }
}

fn receiver_publics(secret: &[u8]) -> Result<([u8; X25519_LEN], Vec<u8>), CryptoError> {
    if secret.len() != KEM_SECRET_LEN {
        return Err(CryptoError::Length);
    }
    let x: [u8; X25519_LEN] = secret[..X25519_LEN].try_into().map_err(|_| CryptoError::Length)?;
    let x_secret = x25519_dalek::StaticSecret::from(x);
    let x_pub = x25519_dalek::PublicKey::from(&x_secret).to_bytes();
    let seed = ml_kem::Seed::try_from(&secret[X25519_LEN..]).map_err(|_| CryptoError::Length)?;
    let dk = ml_kem::ml_kem_768::DecapsulationKey::from_seed(seed);
    let ek = dk.encapsulation_key().to_bytes();
    Ok((x_pub, ek.as_slice().to_vec()))
}

/// The source's encapsulation to the receiver's public keys: the shared
/// secrets (to derive the key) and the ciphertexts (for the acceptance).
pub(crate) struct Encapsulated {
    pub shared: Zeroizing<Vec<u8>>,
    pub x25519_ct: [u8; X25519_LEN],
    pub mlkem_ct: Vec<u8>,
}

pub(crate) fn encapsulate(x25519_pub: &[u8], mlkem_pub: &[u8]) -> Result<Encapsulated, CryptoError> {
    let x_pub: [u8; X25519_LEN] = x25519_pub.try_into().map_err(|_| CryptoError::Length)?;
    if mlkem_pub.len() != MLKEM_PUB_LEN {
        return Err(CryptoError::Length);
    }
    let ek = ml_kem::ml_kem_768::EncapsulationKey::new_from_slice(mlkem_pub).map_err(|_| CryptoError::Key)?;
    let mut m = Zeroizing::new([0u8; 32]);
    OsRng.fill_bytes(m.as_mut());
    let m_arr = ml_kem::B32::try_from(&m[..]).map_err(|_| CryptoError::Length)?;
    let (ct, ss_kem) = ek.encapsulate_deterministic(&m_arr);

    let eph = x25519_dalek::StaticSecret::random_from_rng(OsRng);
    let eph_pub = x25519_dalek::PublicKey::from(&eph).to_bytes();
    let ss_x = eph.diffie_hellman(&x25519_dalek::PublicKey::from(x_pub));
    if !ss_x.was_contributory() {
        return Err(CryptoError::NonContributory);
    }
    let mut shared = Zeroizing::new(Vec::with_capacity(64));
    shared.extend_from_slice(ss_kem.as_slice());
    shared.extend_from_slice(ss_x.as_bytes());
    Ok(Encapsulated { shared, x25519_ct: eph_pub, mlkem_ct: ct.as_slice().to_vec() })
}

/// The receiver's side of [`encapsulate`].
pub(crate) fn decapsulate(
    secret: &[u8],
    x25519_ct: &[u8],
    mlkem_ct: &[u8],
) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if secret.len() != KEM_SECRET_LEN || mlkem_ct.len() != MLKEM_CT_LEN {
        return Err(CryptoError::Length);
    }
    let eph: [u8; X25519_LEN] = x25519_ct.try_into().map_err(|_| CryptoError::Length)?;
    let seed = ml_kem::Seed::try_from(&secret[X25519_LEN..]).map_err(|_| CryptoError::Length)?;
    let dk = ml_kem::ml_kem_768::DecapsulationKey::from_seed(seed);
    let ss_kem = dk.decapsulate_slice(mlkem_ct).map_err(|_| CryptoError::Length)?;
    let x: [u8; X25519_LEN] = secret[..X25519_LEN].try_into().map_err(|_| CryptoError::Length)?;
    let ss_x = x25519_dalek::StaticSecret::from(x).diffie_hellman(&x25519_dalek::PublicKey::from(eph));
    if !ss_x.was_contributory() {
        return Err(CryptoError::NonContributory);
    }
    let mut shared = Zeroizing::new(Vec::with_capacity(64));
    shared.extend_from_slice(ss_kem.as_slice());
    shared.extend_from_slice(ss_x.as_bytes());
    Ok(shared)
}

/// The transfer key: HKDF-SHA256(salt = transfer id, ikm = ss_kem ‖ ss_x,
/// info = domain ‖ H(req) ‖ H(acc)).
pub(crate) fn derive_key(
    shared: &[u8],
    transfer_id: &str,
    req: &[u8],
    acc: &[u8],
) -> Zeroizing<[u8; 32]> {
    let mut info = Vec::with_capacity(KEY_INFO.len() + 64);
    info.extend_from_slice(KEY_INFO);
    info.extend_from_slice(&sha256(req));
    info.extend_from_slice(&sha256(acc));
    let hk = Hkdf::<Sha256>::new(Some(transfer_id.as_bytes()), shared);
    let mut okm = Zeroizing::new([0u8; 32]);
    hk.expand(&info, okm.as_mut()).expect("32 bytes is a valid HKDF-SHA256 length");
    okm
}

pub(crate) fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn domain_message(domain: &[u8], bytes: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(domain.len() + bytes.len());
    m.extend_from_slice(domain);
    m.extend_from_slice(bytes);
    m
}

/// Sign `bytes` under `domain` with this device's ML-DSA-44 key.
pub(crate) fn sign(key: &SigningKey<MlDsa44>, domain: &[u8], bytes: &[u8]) -> Vec<u8> {
    key.sign(&domain_message(domain, bytes)).encode().to_vec()
}

/// Verify a peer's signature over `domain ‖ bytes` with its certified ML-DSA-44 key.
pub(crate) fn verify(pq_pub: &[u8], domain: &[u8], bytes: &[u8], sig: &[u8]) -> Result<(), CryptoError> {
    let encoded_key: &EncodedVerifyingKey<MlDsa44> = pq_pub.try_into().map_err(|_| CryptoError::Key)?;
    let encoded_sig: &EncodedSignature<MlDsa44> = sig.try_into().map_err(|_| CryptoError::Signature)?;
    let vk = VerifyingKey::<MlDsa44>::decode(encoded_key);
    let signature = Signature::<MlDsa44>::decode(encoded_sig).ok_or(CryptoError::Signature)?;
    vk.verify(&domain_message(domain, bytes), &signature).map_err(|_| CryptoError::Signature)
}

fn aad(transfer_id: &str, index: u32, is_final: bool) -> Vec<u8> {
    let mut a = Vec::with_capacity(CHUNK_AAD_DOMAIN.len() + transfer_id.len() + 5);
    a.extend_from_slice(CHUNK_AAD_DOMAIN);
    a.extend_from_slice(transfer_id.as_bytes());
    a.extend_from_slice(&index.to_be_bytes());
    a.push(u8::from(is_final));
    a
}

/// Seal one chunk's plaintext: `version ‖ is_final ‖ nonce(24) ‖ ciphertext`.
pub(crate) fn seal(key: &[u8; 32], transfer_id: &str, index: u32, is_final: bool, plain: &[u8]) -> Vec<u8> {
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut nonce = [0u8; 24];
    OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plain, aad: &aad(transfer_id, index, is_final) })
        .expect("XChaCha20-Poly1305 encryption does not fail for in-memory buffers");
    let mut out = Vec::with_capacity(SEAL_HEADER_LEN + ct.len());
    out.push(SEAL_VERSION);
    out.push(u8::from(is_final));
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

/// Open a sealed chunk expected at `index` of `transfer_id`. Returns the
/// plaintext and the (authenticated) final flag.
pub fn open(key: &[u8; 32], transfer_id: &str, index: u32, sealed: &[u8]) -> Result<(Vec<u8>, bool), CryptoError> {
    if sealed.len() < SEAL_HEADER_LEN + 16 {
        return Err(CryptoError::Open);
    }
    if sealed[0] != SEAL_VERSION {
        return Err(CryptoError::Version);
    }
    let is_final = match sealed[1] {
        0 => false,
        1 => true,
        _ => return Err(CryptoError::Open),
    };
    let cipher = XChaCha20Poly1305::new(key.into());
    let plain = cipher
        .decrypt(
            XNonce::from_slice(&sealed[2..SEAL_HEADER_LEN]),
            Payload { msg: &sealed[SEAL_HEADER_LEN..], aad: &aad(transfer_id, index, is_final) },
        )
        .map_err(|_| CryptoError::Open)?;
    Ok((plain, is_final))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ml_dsa::Keypair as _;

    #[test]
    fn hybrid_agreement_round_trips_and_binds_both_secrets() {
        let r = receiver_keys();
        let e = encapsulate(&r.x25519_pub, &r.mlkem_pub).unwrap();
        let got = decapsulate(&r.secret, &e.x25519_ct, &e.mlkem_ct).unwrap();
        assert_eq!(*got, *e.shared);
        // A tampered ML-KEM ciphertext yields a different (implicitly rejected) secret.
        let mut bad = e.mlkem_ct.clone();
        bad[10] ^= 1;
        assert_ne!(*decapsulate(&r.secret, &e.x25519_ct, &bad).unwrap(), *e.shared);
        // A substituted X25519 share does too.
        let other = receiver_keys();
        assert_ne!(*decapsulate(&r.secret, &other.x25519_pub, &e.mlkem_ct).unwrap(), *e.shared);
        // The all-zero (low-order) point is refused outright.
        assert_eq!(encapsulate(&[0u8; 32], &r.mlkem_pub).err(), Some(CryptoError::NonContributory));
        assert_eq!(decapsulate(&r.secret, &[0u8; 32], &e.mlkem_ct).err(), Some(CryptoError::NonContributory));
    }

    #[test]
    fn the_key_depends_on_every_transcript_input() {
        let k = derive_key(&[1u8; 64], "T", b"req", b"acc");
        assert_ne!(*k, *derive_key(&[2u8; 64], "T", b"req", b"acc"));
        assert_ne!(*k, *derive_key(&[1u8; 64], "U", b"req", b"acc"));
        assert_ne!(*k, *derive_key(&[1u8; 64], "T", b"reQ", b"acc"));
        assert_ne!(*k, *derive_key(&[1u8; 64], "T", b"req", b"acC"));
    }

    #[test]
    fn a_chunk_opens_only_at_its_own_place() {
        let key = [7u8; 32];
        let sealed = seal(&key, "T", 3, false, b"rows");
        assert_eq!(open(&key, "T", 3, &sealed).unwrap(), (b"rows".to_vec(), false));
        // Another index, another transfer, another key: refused.
        assert!(open(&key, "T", 4, &sealed).is_err());
        assert!(open(&key, "U", 3, &sealed).is_err());
        assert!(open(&[8u8; 32], "T", 3, &sealed).is_err());
        // Flipping the final flag or any byte: refused.
        let mut flipped = sealed.clone();
        flipped[1] = 1;
        assert!(open(&key, "T", 3, &flipped).is_err());
        for i in 2..sealed.len() {
            let mut b = sealed.clone();
            b[i] ^= 0x40;
            assert!(open(&key, "T", 3, &b).is_err(), "byte {i}");
        }
        // Truncated, empty, wrong version.
        assert!(open(&key, "T", 3, &sealed[..sealed.len() - 1]).is_err());
        assert!(open(&key, "T", 3, &[]).is_err());
        let mut v = sealed.clone();
        v[0] = 2;
        assert_eq!(open(&key, "T", 3, &v), Err(CryptoError::Version));
        // Two seals of the same plaintext never share a nonce.
        assert_ne!(seal(&key, "T", 3, false, b"rows")[2..26], sealed[2..26]);
    }

    #[test]
    fn signatures_are_domain_separated() {
        let key = SigningKey::<MlDsa44>::from_seed(&[5u8; 32].into());
        let pq = key.verifying_key().encode().to_vec();
        let sig = sign(&key, REQ_DOMAIN, b"bytes");
        assert!(verify(&pq, REQ_DOMAIN, b"bytes", &sig).is_ok());
        assert!(verify(&pq, ACC_DOMAIN, b"bytes", &sig).is_err(), "a request signature is not an acceptance");
        assert!(verify(&pq, REQ_DOMAIN, b"bytez", &sig).is_err());
        let other = SigningKey::<MlDsa44>::from_seed(&[6u8; 32].into()).verifying_key().encode().to_vec();
        assert!(verify(&other, REQ_DOMAIN, b"bytes", &sig).is_err());
        assert!(verify(&pq, REQ_DOMAIN, b"bytes", &sig[..10]).is_err());
    }
}
