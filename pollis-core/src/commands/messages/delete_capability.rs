//! The per-envelope deletion capability (#1086).
//!
//! Sealed sender (#607) blinds `message_envelope.sender_id`, so the Delivery
//! Service cannot tell whose envelope a row is. Its self-delete branch therefore
//! trusted the caller's own `msg_sender_id` hint — and any member could delete
//! any envelope in a conversation before slower recipients fetched it, with no
//! tombstone, or clobber another author's pending edit the same way. CLAUDE.md
//! allows exactly three message losses; that was a fourth, and unlike the others
//! it was attacker-controlled rather than a bound on storage.
//!
//! The capability does not re-identify the sender — that is what sealing exists
//! to prevent. The sender stores `SHA-256(token)` with the envelope and must
//! present the preimage to remove it. The DS authorizes nobody; it compares a
//! hash.
//!
//! ## Why the token is derived rather than stored
//!
//! A token generated at random on the sending device would live only in that
//! device's local database, and a second device of the same person — which never
//! saw the send, only the ingest — could not delete its own user's message.
//! Deriving it from the account identity key, which every enrolled device of that
//! user holds and nobody else does, keeps multi-device delete working without
//! handing the capability to the rest of the conversation (putting it inside the
//! ciphertext would do exactly that: every member could then delete).
//!
//! ```text
//! delete_key = HKDF-SHA256(ikm = account identity key seed, info = DELETE_KEY_INFO)
//! token      = HMAC-SHA256(delete_key, message_id)
//! stored     = SHA-256(token)
//! ```
//!
//! Two layers on purpose. The DS stores `stored`, so what it holds is not itself
//! the capability and cannot be replayed as one; and `token` is an HMAC, so
//! seeing any number of tokens does not let the DS mint one for a message it has
//! not been given.
//!
//! ## What it does and does not protect
//!
//! * Unlinkable — `stored` is a fresh-looking 32 bytes per message. It groups
//!   nothing and names no one, so it gives the DS nothing sealing took away.
//! * Scoped to the message id, so a token for one envelope authorizes only that
//!   envelope.
//! * **Not** retained across an identity rotation: `reset_identity` replaces the
//!   account key, so tokens for messages sent under the old one can no longer be
//!   recomputed and those envelopes can no longer be self-deleted at the DS. The
//!   MLS-authenticated redaction path still works, and a reset already orphans
//!   every device and drops group membership, so this is consistent with what a
//!   reset means rather than a new sharp edge.
//! * **Not** a defence for envelopes written before this shipped: those rows have
//!   a NULL hash and the DS falls back to the old membership-only check. The
//!   capability can only be *required* once clients that produce one have reached
//!   the fleet.

use base64::Engine as _;
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};

/// HKDF `info` for the delete key. Domain-separated so the same account key
/// cannot produce a value that is meaningful in another context.
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

/// This device's account identity key seed, read straight off the unlock state.
///
/// Not `account_identity::load_account_id_key`, which needs the caller to name a
/// user: the capability is only ever computed for envelopes THIS device is
/// writing or deleting, so the acting user is by definition the unlocked one.
/// Reading it here keeps the helpers below callable from paths that do not carry
/// a `user_id` — `emit_receipt` in particular, which is deliberately identity-free.
async fn unlocked_account_seed(
    state: &std::sync::Arc<crate::state::AppState>,
) -> Option<Vec<u8>> {
    let guard = state.unlock.lock().await;
    guard.as_ref().map(|u| u.account_id_key.to_vec())
}

/// The capability hash to attach to an envelope this device is about to write.
///
/// `None` when the account identity key is unavailable — the envelope is still
/// sent, just without a capability, exactly like one an older client wrote. A
/// send must never fail because a capability could not be computed: "messages
/// must work" outranks it, and in practice the key is present whenever a send is
/// possible, because sending needs the local DB and the same PIN unlock opens
/// both.
pub(super) async fn envelope_delete_token_hash(
    state: &std::sync::Arc<crate::state::AppState>,
    envelope_id: &str,
) -> Option<String> {
    let seed = unlocked_account_seed(state).await;
    match seed {
        Some(seed) => Some(delete_token_hash(&seed, envelope_id)),
        None => {
            eprintln!(
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
        // The property multi-device delete rests on: a sibling device, holding
        // the same account key and knowing only the message id, recomputes the
        // same token without ever having seen the send.
        assert_eq!(delete_token(SEED_A, "msg-1"), delete_token(SEED_A, "msg-1"));
    }

    #[test]
    fn a_token_authorizes_exactly_one_message() {
        assert_ne!(delete_token(SEED_A, "msg-1"), delete_token(SEED_A, "msg-2"));
    }

    #[test]
    fn another_account_cannot_produce_the_token() {
        // The whole point: a different member of the conversation holds a
        // different account key, so they cannot mint the capability.
        assert_ne!(delete_token(SEED_A, "msg-1"), delete_token(SEED_B, "msg-1"));
        assert_ne!(
            delete_token_hash(SEED_A, "msg-1"),
            delete_token_hash(SEED_B, "msg-1")
        );
    }

    #[test]
    fn the_stored_hash_is_not_the_capability() {
        // What the DS holds must not be replayable as the preimage, or storing
        // it would hand every reader of the table the ability to delete.
        let token = delete_token(SEED_A, "msg-1");
        let stored = delete_token_hash(SEED_A, "msg-1");
        assert_ne!(token, stored);
        assert_eq!(delete_token_hash_of(&token), stored);
        assert_ne!(delete_token_hash_of(&stored), stored);
    }

    #[test]
    fn the_hash_reveals_no_grouping() {
        // Two messages from ONE author must not be linkable by their stored
        // hashes — otherwise the capability would hand back the sender grouping
        // sealed sender removes.
        let one = delete_token_hash(SEED_A, "msg-1");
        let two = delete_token_hash(SEED_A, "msg-2");
        assert_ne!(one, two);
        // Both are full-width digests, so neither carries a shared prefix a
        // server could bucket on.
        let decode = |s: &str| base64::engine::general_purpose::STANDARD.decode(s).unwrap();
        assert_eq!(decode(&one).len(), 32);
        assert_eq!(decode(&two).len(), 32);
        assert_ne!(decode(&one)[..8], decode(&two)[..8]);
    }
}
