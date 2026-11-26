//! Shared attachment objects (#2109): each distinct file is stored once,
//! across every account.
//!
//! Convergent encryption with separated derivations:
//!
//! - `h = SHA-256(plaintext)`, carried only inside the end-to-end encrypted
//!   message or Vault entry (the `_att` `sha` field).
//! - The file key and base nonce come from `h` by HKDF under their own labels
//!   (`pollis-file-key-v2`, `pollis-file-nonce-v2`). Identical files give
//!   identical ciphertext, so they are stored once.
//! - The object is named `v2-` + `SHA-256(ciphertext)`. The bucket can check
//!   bytes against that name (the presigned PUT signs it as a checksum), and
//!   the name is a hash of ciphertext, so it yields nothing about the key.
//!
//! The Delivery Service and the bucket see the name and the ciphertext and
//! nothing else; neither yields `h` or the key. Who holds the same file can
//! compute its name and so test whether it is stored: the one property
//! convergent storage gives up, stated in the security whitepaper.
//!
//! Replaced, for new uploads, the per-conversation key ring of #1382, which is
//! kept for reading the objects stored under it. Unlike the convergent scheme
//! #1382 removed (key and name both `SHA-256(plaintext)`), nothing here is
//! derivable from what the server stores.

use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::error::{Error, Result};

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

/// HKDF salt, `info` for the key and `info` for the base nonce. Three labels,
/// so the key, the nonce and the object name are separate derivations.
const SALT: &[u8] = b"pollis-shared-file-v2";
const KEY_INFO: &[u8] = b"pollis-file-key-v2";
const NONCE_INFO: &[u8] = b"pollis-file-nonce-v2";

/// SHA-256 of `data`.
pub(crate) fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// The key and base nonce a file with plaintext digest `h` is sealed under.
///
/// The nonce being a function of the key is safe here: a key is a function
/// of the whole plaintext, so one key only ever seals one plaintext, and the
/// chunk index keeps every chunk's nonce distinct within it.
pub(crate) fn content_key(h: &[u8; 32]) -> (Zeroizing<[u8; KEY_LEN]>, [u8; NONCE_LEN]) {
    let hk = Hkdf::<Sha256>::new(Some(SALT), h);
    let mut key = Zeroizing::new([0u8; KEY_LEN]);
    hk.expand(KEY_INFO, key.as_mut())
        .expect("HKDF expand for a 32-byte key cannot fail");
    let mut nonce = [0u8; NONCE_LEN];
    hk.expand(NONCE_INFO, &mut nonce)
        .expect("HKDF expand for a 12-byte nonce cannot fail");
    (key, nonce)
}

/// The shared object id of stored bytes `ciphertext`.
pub(crate) fn object_id(ciphertext: &[u8]) -> String {
    format!(
        "{}{}",
        pollis_api::messages::SHARED_OBJECT_PREFIX,
        hex::encode(sha256(ciphertext))
    )
}

/// Seal `plaintext` (whose digest is `h`): the object id and the bytes to store.
pub(crate) fn seal(plaintext: &[u8], h: &[u8; 32]) -> (String, Vec<u8>) {
    let (key, nonce) = content_key(h);
    let ciphertext = crate::commands::r2::encrypt_chunked(plaintext, &key, &nonce);
    (object_id(&ciphertext), ciphertext)
}

/// Open the stored bytes of shared object `id`, given the plaintext digest `h`
/// from the end-to-end encrypted reference. Refuses bytes that are not the
/// object named, bytes that do not open under `h`'s key, and a plaintext whose
/// digest is not `h`.
pub(crate) fn open(id: &str, ciphertext: &[u8], h: &[u8; 32]) -> Result<Vec<u8>> {
    if object_id(ciphertext) != id {
        return Err(Error::Other(anyhow::anyhow!(
            "attachment {id}: the stored bytes are not the object named"
        )));
    }
    let (key, nonce) = content_key(h);
    let plaintext = crate::commands::r2::decrypt_chunked(ciphertext, &key, &nonce)?;
    if sha256(&plaintext) != *h {
        return Err(Error::Other(anyhow::anyhow!(
            "attachment {id} does not match the digest in its reference"
        )));
    }
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(n: usize, fill: u8) -> Vec<u8> {
        (0..n).map(|i| fill.wrapping_add((i % 251) as u8)).collect()
    }

    #[test]
    fn identical_files_seal_to_identical_objects() {
        let a = file(9 * 1024 * 1024 + 17, 3);
        let b = a.clone();
        let (id_a, ct_a) = seal(&a, &sha256(&a));
        let (id_b, ct_b) = seal(&b, &sha256(&b));
        assert_eq!(id_a, id_b, "same file, same object");
        assert_eq!(ct_a, ct_b, "deterministic, so it is stored once");
        assert!(pollis_api::messages::is_shared_object_id(&id_a));
        assert_eq!(open(&id_a, &ct_a, &sha256(&a)).unwrap(), a);
    }

    #[test]
    fn different_files_seal_to_different_objects() {
        let a = file(1000, 1);
        let mut b = a.clone();
        b[999] ^= 1;
        assert_ne!(seal(&a, &sha256(&a)).0, seal(&b, &sha256(&b)).0);
    }

    #[test]
    fn the_four_mib_chunking_is_kept() {
        let a = file(4 * 1024 * 1024 * 2 + 5, 9);
        let (_, ct) = seal(&a, &sha256(&a));
        // Three chunks, one 16-byte tag each.
        assert_eq!(ct.len(), a.len() + 3 * 16);
    }

    /// The invariant #2109 rests on: nothing the Delivery Service or the bucket
    /// stores — the object id, its key, the ciphertext — yields the file key.
    /// Every key a server could compute from what it holds is tried, and none
    /// opens the object; the real key appears nowhere in what it holds.
    #[test]
    fn nothing_the_server_stores_yields_the_file_key() {
        let plaintext = file(70_000, 42);
        let h = sha256(&plaintext);
        let (id, ciphertext) = seal(&plaintext, &h);
        let (key, nonce) = content_key(&h);

        let digest = pollis_api::messages::shared_object_digest(&id).unwrap();
        let r2_key = pollis_api::messages::shared_object_key(&id);
        let server_holds: Vec<Vec<u8>> = vec![
            id.as_bytes().to_vec(),
            r2_key.as_bytes().to_vec(),
            digest.to_vec(),
            ciphertext.clone(),
        ];
        for blob in &server_holds {
            assert!(
                !blob.windows(KEY_LEN).any(|w| w == key.as_slice()),
                "the file key must not appear in anything the server stores"
            );
            assert!(!blob.windows(32).any(|w| w == h), "the plaintext digest must not either");
        }

        // Candidate keys from server-side data: the id digest itself, our own
        // derivation run on it (as if it were `h`), and a hash of the ciphertext.
        let mut candidates: Vec<([u8; 32], [u8; 12])> = Vec::new();
        let (k, n) = content_key(&digest);
        candidates.push((*k, n));
        candidates.push((digest, nonce));
        candidates.push((sha256(&ciphertext), nonce));
        candidates.push((sha256(r2_key.as_bytes()), nonce));
        for (k, n) in candidates {
            assert!(
                crate::commands::r2::decrypt_chunked(&ciphertext, &k, &n).is_err(),
                "a key computed from server-side data opened the object"
            );
        }
        // Only `h`, which lives inside the end-to-end encrypted reference, does.
        assert_eq!(open(&id, &ciphertext, &h).unwrap(), plaintext);
    }

    #[test]
    fn substituted_bytes_are_refused() {
        let a = file(5000, 7);
        let h = sha256(&a);
        let (id, mut ct) = seal(&a, &h);
        ct[10] ^= 1;
        assert!(open(&id, &ct, &h).is_err(), "bytes that are not the object named");
        let other = file(5000, 8);
        let (other_id, other_ct) = seal(&other, &sha256(&other));
        assert!(open(&other_id, &other_ct, &h).is_err(), "another file under this reference");
    }
}
