//! The per-envelope deletion capability (#1086).
//!
//! Sealed sender (#607) hides whose envelope a row is, so without this any
//! member could delete another's envelope (or pending edit) before slower
//! recipients fetched it: an attacker-controlled message loss. The sender
//! stores `SHA-256(token)` with the envelope and presents the preimage to
//! remove it; the DS compares a hash and learns no identity.
//!
//! Derived from the account identity key rather than random, so the author's
//! other devices can delete too, without giving the capability to every member
//! (as putting it inside the ciphertext would).
//!
//! ```text
//! delete_key = HKDF-SHA256(ikm = account identity key seed, info = DELETE_KEY_INFO)
//! token      = HMAC-SHA256(delete_key, message_id)
//! stored     = SHA-256(token)
//! ```
//!
//! Two layers: what the DS stores is not itself the capability, and the HMAC
//! means seeing tokens does not let the DS mint new ones. `stored` is
//! unlinkable per message. Not retained across `reset_identity` (the MLS
//! redaction path still works), and envelopes written before #1086 have a NULL
//! hash and fall back to the DS's membership-only check.

use base64::Engine as _;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// HKDF `info` for the delete key (domain separation).
const DELETE_KEY_INFO: &[u8] = b"pollis/envelope-delete/v1";

type HmacSha256 = Hmac<Sha256>;

/// The delete key for an account identity key seed.
fn delete_key(account_id_seed: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, account_id_seed);
    let mut out = [0u8; 32];
    hk.expand(DELETE_KEY_INFO, &mut out)
        .expect("HKDF expand 32 bytes is infallible");
    out
}

/// The deletion capability for `message_id`, base64 — what a delete presents.
pub fn delete_token(account_id_seed: &[u8], message_id: &str) -> String {
    let key = delete_key(account_id_seed);
    let mut mac = HmacSha256::new_from_slice(&key).expect("HMAC accepts any key length");
    mac.update(message_id.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// What the DS stores: `SHA-256(token)`, base64.
pub fn delete_token_hash_of(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    base64::engine::general_purpose::STANDARD.encode(digest)
}

/// The hash to send at send time, for `message_id`.
pub fn delete_token_hash(account_id_seed: &[u8], message_id: &str) -> String {
    delete_token_hash_of(&delete_token(account_id_seed, message_id))
}

/// This device's account identity key seed, from the unlock state. Needs no
/// `user_id`: the capability is only computed for envelopes this device writes
/// or deletes, and `emit_receipt` is deliberately identity-free.
async fn unlocked_account_seed(
    state: &std::sync::Arc<crate::state::AppState>,
) -> Option<Vec<u8>> {
    let guard = state.unlock.lock().await;
    guard.as_ref().map(|u| u.account_id_key.to_vec())
}

/// The capability hash to attach to an envelope this device is about to write.
/// `None` (locked) still sends, like an older client's envelope: a send never
/// fails over a capability.
pub(super) async fn envelope_delete_token_hash(
    state: &std::sync::Arc<crate::state::AppState>,
    envelope_id: &str,
) -> Option<String> {
    let seed = unlocked_account_seed(state).await;
    match seed {
        Some(seed) => Some(delete_token_hash(&seed, envelope_id)),
        None => {
            crate::diag!(
                "[messages] no delete capability for envelope {envelope_id} (locked); \
                 it falls back to the pre-#1086 membership check"
            );
            None
        }
    }
}

/// The capability to present when deleting `message_id`, if this device can
/// compute one. `None` leaves the DS on its legacy path.
pub(super) async fn envelope_delete_token(
    state: &std::sync::Arc<crate::state::AppState>,
    message_id: &str,
) -> Option<String> {
    let seed = unlocked_account_seed(state).await?;
    Some(delete_token(&seed, message_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED_A: &[u8] = &[7u8; 32];
    const SEED_B: &[u8] = &[9u8; 32];

    #[test]
    fn a_token_is_deterministic_for_one_account_and_message() {
        // A sibling device recomputes the same token from the message id alone.
        assert_eq!(delete_token(SEED_A, "msg-1"), delete_token(SEED_A, "msg-1"));
    }

    #[test]
    fn a_token_authorizes_exactly_one_message() {
        assert_ne!(delete_token(SEED_A, "msg-1"), delete_token(SEED_A, "msg-2"));
    }

    #[test]
    fn another_account_cannot_produce_the_token() {
        assert_ne!(delete_token(SEED_A, "msg-1"), delete_token(SEED_B, "msg-1"));
        assert_ne!(
            delete_token_hash(SEED_A, "msg-1"),
            delete_token_hash(SEED_B, "msg-1")
        );
    }

    #[test]
    fn the_stored_hash_is_not_the_capability() {
        // The stored hash must not be replayable as the preimage.
        let token = delete_token(SEED_A, "msg-1");
        let stored = delete_token_hash(SEED_A, "msg-1");
        assert_ne!(token, stored);
        assert_eq!(delete_token_hash_of(&token), stored);
        assert_ne!(delete_token_hash_of(&stored), stored);
    }

    #[test]
    fn the_hash_reveals_no_grouping() {
        // One author's messages must not be linkable by their stored hashes.
        let one = delete_token_hash(SEED_A, "msg-1");
        let two = delete_token_hash(SEED_A, "msg-2");
        assert_ne!(one, two);
        let decode = |s: &str| base64::engine::general_purpose::STANDARD.decode(s).unwrap();
        assert_eq!(decode(&one).len(), 32);
        assert_eq!(decode(&two).len(), 32);
        assert_ne!(decode(&one)[..8], decode(&two)[..8]);
    }
}
