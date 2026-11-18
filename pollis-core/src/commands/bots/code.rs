//! The eight-character pairing code (#1182).
//!
//! The host prints it; the admin types it. Derived from everything the admin's
//! approval acts on — the host's ephemeral X25519 key, its device id, and what
//! the request is for (the new bot's account key, or the existing bot's id) — so
//! a DS that swaps any of them changes the code the admin's client re-derives and
//! the admin's typed code no longer matches. Same alphabet, width and input rule
//! as the enrollment SAS (`device_enrollment::normalize_enrollment_sas`, #1096).
//!
//! Unsalted by a request id, unlike the enrollment SAS: the admin finds the
//! request BY the code, so nothing request-specific is known before the lookup.
//! The width still prices a targeted grind at 2^40 key generations per code.

use hkdf::Hkdf;
use sha2::{Digest, Sha256};

use pollis_api::bots::PairingTarget;

use crate::commands::device_enrollment::{normalize_enrollment_sas, ENROLLMENT_SAS_ALPHABET, ENROLLMENT_SAS_LEN};
use crate::error::{Error, Result};

/// Characters in a pairing code: the enrollment SAS width.
pub const PAIRING_CODE_LEN: usize = ENROLLMENT_SAS_LEN;

/// HKDF salt, domain-separating the pairing code from the enrollment SAS.
const CODE_SALT: &[u8] = b"pollis-bot-pairing-code-salt-v1";
/// HKDF info for the code.
const CODE_INFO: &[u8] = b"pollis-bot-pairing-code-v1";
/// Domain of the lookup hash the DS indexes requests by.
const LOOKUP_DOMAIN: &[u8] = b"pollis-bot-pairing-lookup-v1";

/// Normalize what a human typed: the enrollment SAS rule (case and separators
/// forgiven, any outside character dropped, never mapped to a lookalike).
pub fn normalize_pairing_code(raw: &str) -> String {
    normalize_enrollment_sas(raw)
}

/// The transcript the code is derived over, length-prefixed so it is injective.
fn transcript(ephemeral_pub: &[u8], device_id: &str, target: &PairingTarget) -> Result<Vec<u8>> {
    let (tag, bound): (&[u8], Vec<u8>) = match target {
        PairingTarget::NewBot { account_id_pub } => (
            b"new_bot",
            crate::commands::ds_reads::decode_b64("account_id_pub", account_id_pub)?,
        ),
        PairingTarget::ExistingBot { bot_user_id } => (b"existing_bot", bot_user_id.as_bytes().to_vec()),
    };
    let mut out = Vec::new();
    for field in [ephemeral_pub, device_id.as_bytes(), tag, &bound] {
        out.extend_from_slice(&(field.len() as u32).to_be_bytes());
        out.extend_from_slice(field);
    }
    Ok(out)
}

/// The code for one pairing request. Both sides call this: the host over what it
/// generated, the admin over what the DS served back.
pub fn derive_pairing_code(ephemeral_pub: &[u8], device_id: &str, target: &PairingTarget) -> Result<String> {
    if ephemeral_pub.len() != 32 {
        return Err(Error::Crypto(format!(
            "pairing ephemeral key has wrong length: {}",
            ephemeral_pub.len()
        )));
    }
    let ikm = transcript(ephemeral_pub, device_id, target)?;
    let hk = Hkdf::<Sha256>::new(Some(CODE_SALT), &ikm);
    let mut out = [0u8; PAIRING_CODE_LEN];
    hk.expand(CODE_INFO, &mut out)
        .expect("HKDF expand PAIRING_CODE_LEN bytes is infallible");
    let alphabet = ENROLLMENT_SAS_ALPHABET.as_bytes();
    Ok(out.iter().map(|b| alphabet[(b & 0x1f) as usize] as char).collect())
}

/// The 32-byte key the DS finds a request by: a hash of the normalized code.
/// Only a lookup key; the admin's client never trusts the request it finds
/// without re-deriving the code ([`derive_pairing_code`]).
pub fn code_lookup(code: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(LOOKUP_DOMAIN);
    h.update(normalize_pairing_code(code).as_bytes());
    h.finalize().into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;

    fn b64(b: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(b)
    }

    fn new_bot(seed: u8) -> PairingTarget {
        PairingTarget::NewBot { account_id_pub: b64(&[seed; 1312]) }
    }

    #[test]
    fn the_code_is_eight_unambiguous_characters_that_survive_normalization() {
        for seed in 0u8..32 {
            let code = derive_pairing_code(&[seed; 32], "DEV", &new_bot(1)).unwrap();
            assert_eq!(code.chars().count(), PAIRING_CODE_LEN);
            assert_eq!(normalize_pairing_code(&code), code);
            assert!(!code.contains(['I', 'L', 'O', 'U']));
        }
    }

    #[test]
    fn every_bound_input_changes_the_code() {
        let base = derive_pairing_code(&[7; 32], "DEV", &new_bot(1)).unwrap();
        assert_ne!(base, derive_pairing_code(&[8; 32], "DEV", &new_bot(1)).unwrap(), "ephemeral key");
        assert_ne!(base, derive_pairing_code(&[7; 32], "DEW", &new_bot(1)).unwrap(), "device id");
        assert_ne!(base, derive_pairing_code(&[7; 32], "DEV", &new_bot(2)).unwrap(), "account key");
        let existing = PairingTarget::ExistingBot { bot_user_id: "BOT".into() };
        let other = PairingTarget::ExistingBot { bot_user_id: "BOU".into() };
        assert_ne!(
            derive_pairing_code(&[7; 32], "DEV", &existing).unwrap(),
            derive_pairing_code(&[7; 32], "DEV", &other).unwrap(),
            "bot id"
        );
    }

    #[test]
    fn the_lookup_is_over_the_normalized_code() {
        assert_eq!(code_lookup("h7k2-pq3m"), code_lookup("H7K2PQ3M"));
        assert_ne!(code_lookup("H7K2PQ3M"), code_lookup("H7K2PQ3N"));
    }

    #[test]
    fn a_short_ephemeral_key_is_refused() {
        assert!(derive_pairing_code(&[7; 31], "DEV", &new_bot(1)).is_err());
    }
}
