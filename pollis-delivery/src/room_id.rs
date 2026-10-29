//! Pseudonymous LiveKit room names (#828).
//!
//! Raw ids as room names would hand the SFU operator a map of who is in which conversation,
//! so the DS authorizes the logical name and only the pseudonym reaches LiveKit.
//! Keyed from `LIVEKIT_API_SECRET`; rotating it re-keys rooms, which token rotation forces anyway.
//! Stable, not rotated. Participant identities: [`crate::participant_id`].

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Domain separation: this key is derived from the same secret that signs
/// LiveKit JWTs, so the label keeps the two uses from ever colliding.
const ROOM_LABEL: &[u8] = b"pollis-livekit-room-pseudonym-v1";

/// Truncation width in hex characters (128 bits).
const ROOM_HEX_LEN: usize = 32;

/// Map a **logical** room name to the opaque name LiveKit actually sees.
///
/// Authorization happens on the logical name; only the return value crosses to LiveKit.
pub fn room_pseudonym(api_secret: &str, logical_room: &str) -> String {
    let mut mac = HmacSha256::new_from_slice(api_secret.as_bytes())
        .expect("HMAC accepts a key of any length");
    mac.update(ROOM_LABEL);
    // Length-prefix the label/input boundary so no two distinct logical names can
    // ever produce the same MAC input by concatenation.
    mac.update(&(logical_room.len() as u64).to_be_bytes());
    mac.update(logical_room.as_bytes());
    let digest = mac.finalize().into_bytes();

    let mut out = String::with_capacity(2 + ROOM_HEX_LEN);
    out.push_str("r-");
    for b in digest.iter() {
        if out.len() >= 2 + ROOM_HEX_LEN {
            break;
        }
        out.push_str(&format!("{b:02x}"));
    }
    out.truncate(2 + ROOM_HEX_LEN);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: &str = "test-api-secret";

    /// The acceptance criterion from #828: no raw conversation id, and no user id,
    /// ever reaches LiveKit as a room name.
    #[test]
    fn never_leaks_the_logical_name() {
        let conv = "01KZVDFR5Z9DKJ27YN91S3KS6N";
        let inbox = "inbox-01KZT7RW4RXQGT943Q05C7NW8A";
        for logical in [conv, inbox, "call-01KZVDFRKW2957R7VK81MY0T2G"] {
            let room = room_pseudonym(K, logical);
            assert!(!room.contains(logical), "pseudonym must not embed the logical name");
            // Also reject the bare id inside a prefixed logical name (`inbox-<id>`).
            let bare = logical.rsplit('-').next().unwrap();
            assert!(!room.contains(bare), "pseudonym must not embed the raw id");
            assert!(room.starts_with("r-"));
            assert_eq!(room.len(), 2 + ROOM_HEX_LEN);
            assert!(room[2..].chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    /// Deterministic, or two members of one conversation land in different rooms.
    #[test]
    fn stable_for_a_given_secret() {
        assert_eq!(room_pseudonym(K, "conv-a"), room_pseudonym(K, "conv-a"));
    }

    /// Distinct conversations must not collide, or their traffic merges.
    #[test]
    fn distinct_logical_rooms_do_not_collide() {
        assert_ne!(room_pseudonym(K, "conv-a"), room_pseudonym(K, "conv-b"));
        // The length-prefix guards the concatenation ambiguity: without it
        // ("ab","c") and ("a","bc") could share a MAC input.
        assert_ne!(room_pseudonym(K, "ab"), room_pseudonym(K, "a\u{0}b"));
    }

    /// Re-keying changes every name.
    #[test]
    fn rekeys_with_the_secret() {
        assert_ne!(room_pseudonym(K, "conv-a"), room_pseudonym("other-secret", "conv-a"));
    }

    /// A group id and the inbox of a user with the same ULID must not collide.
    #[test]
    fn prefixes_are_distinguished() {
        let id = "01KZVDFR5Z9DKJ27YN91S3KS6N";
        assert_ne!(room_pseudonym(K, id), room_pseudonym(K, &format!("inbox-{id}")));
    }
}
