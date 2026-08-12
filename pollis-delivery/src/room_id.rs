//! Pseudonymous LiveKit room names (#828).
//!
//! Rooms used to be named with the raw `group.id` / `dm_channel.id`, and inboxes
//! with `inbox-<user_id>`. Room membership is visible to the SFU operator by
//! construction, so that handed LiveKit a full map of **which users belong to
//! which conversations** — a second, independent copy of the social graph living
//! outside Turso.
//!
//! `docs/security-whitepaper.md` §1.2 accepts the graph being visible *on Turso*
//! as irreducible for a store-and-forward server. It does not account for the
//! same graph also sitting on the media plane. Today that is academic — one SFU,
//! ours. It stops being academic the moment there are regional SFUs, or a
//! third-party one.
//!
//! ## Why this is server-only
//!
//! A LiveKit JWT carries the room inside its `video` grant, and `Room::connect`
//! takes no room argument — the client dials whatever room its token grants. The
//! client also tags realtime events with its own local conversation id rather
//! than reading the room name back off the wire. So the mapping can live entirely
//! in the Delivery Service: **the logical name is authorized, the pseudonym is
//! what reaches LiveKit**, and no client ships the mapping or the key.
//!
//! That property is the point. If clients derived the pseudonym, anyone with a
//! release binary could extract the key and re-link every room to its
//! conversation, which would defeat the exercise.
//!
//! ## Keying
//!
//! Derived from `LIVEKIT_API_SECRET` under a domain-separation label rather than
//! introducing a secret that has to be provisioned separately and can drift out
//! of sync. Rotating the API secret therefore re-keys room names — which is
//! self-healing, not a break: rotation already invalidates every outstanding
//! LiveKit token, so clients re-request one and receive the new name in the same
//! round trip they were going to make anyway.
//!
//! ## What this does and does not buy
//!
//! **Does:** the SFU can no longer join a room name to a row in Turso. Absent the
//! key, `r-<hex>` is opaque, and the input space (ULIDs) is far too large to
//! enumerate.
//!
//! **Does not:** LiveKit participant identity is still `{user_id}:{device_id}`,
//! so the operator continues to see *which users* share a room even when it
//! cannot tell which conversation that room is. Co-membership clustering
//! therefore survives this change. Pseudonymising identity is a larger change
//! (it is load-bearing for voice roster matching and speaking indicators) and is
//! deliberately out of scope here — see #828.
//!
//! **Not rotated.** These are stable pseudonyms, not unlinkable ones: a given
//! conversation keeps one room name for the life of the API secret. Rotating per
//! epoch would buy unlinkability over time at the cost of dropping every live
//! connection at each rotation, which is a real availability trade and wants its
//! own decision.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Domain separation: this key is derived from the same secret that signs
/// LiveKit JWTs, so the label keeps the two uses from ever colliding.
const ROOM_LABEL: &[u8] = b"pollis-livekit-room-pseudonym-v1";

/// Truncation width in hex characters. 32 hex = 128 bits — far beyond birthday
/// range for any plausible number of conversations, and short enough to stay
/// readable in LiveKit's dashboards and logs.
const ROOM_HEX_LEN: usize = 32;

/// Map a **logical** room name to the opaque name LiveKit actually sees.
///
/// The logical name is whatever the rest of the DS authorizes on — a conversation
/// id, `inbox-<user_id>`, or `call-<ulid>`. Authorization happens on the logical
/// name; only the return value crosses to LiveKit.
///
/// Deterministic: the same logical room always maps to the same pseudonym for a
/// given secret, which is what lets independent DS instances and successive
/// requests agree without coordination.
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

    /// Deterministic for a given secret — successive requests and independent DS
    /// instances must agree, or two members of one conversation land in different
    /// rooms and never see each other.
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

    /// Re-keying changes every name — the documented consequence of rotating the
    /// LiveKit API secret.
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
