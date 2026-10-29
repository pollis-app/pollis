//! Pseudonymous LiveKit participant identities (#836), the sibling of
//! [`crate::room_id`]: `{prefix}{base64url(siv ‖ ct ‖ tag)}`, keyed per logical
//! room so one user's identities in two rooms are unrelated.
//!
//! Encrypted rather than MAC'd because identity is matched against (roster,
//! speaking indicators, self-hear filter), and [`resolve_participant`] must
//! recover the user with no candidate list. The key stays server-only. See
//! `.codesight/wiki/secrets-broker.md` → "Participant identities are pseudonymous too".
//!
//! ```text
//! root  = HMAC(api_secret, LABEL ‖ len(room) ‖ room)      # per logical room
//! k_siv = HMAC(root, "siv")   k_enc = HMAC(root, "enc")   k_tag = HMAC(root, "tag")
//!
//! siv   = HMAC(k_siv, kind ‖ lp(user_id) ‖ lp(device_id))[..8]
//! ks    = HMAC(k_enc, siv ‖ be32(counter))                # keystream, HKDF-Expand shaped
//! ct    = user_id ⊕ ks
//! tag   = HMAC(k_tag, kind ‖ siv ‖ ct)[..4]
//! ```
//!
//! `siv` is a PRF of the plaintext plus the device, so equal inputs give one
//! stable identity and unequal inputs never share a keystream; folding
//! `device_id` into it keeps a user's devices distinct (#140). `tag` makes a
//! hostile SFU's fabricated identity fail to resolve instead of decrypting to a
//! garbage user id.

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Domain separation from [`crate::room_id`], which derives from the same secret.
const PARTICIPANT_LABEL: &[u8] = b"pollis-livekit-participant-pseudonym-v1";

/// A collision makes two of one user's devices evict each other; 2^-64 per pair.
const SIV_LEN: usize = 8;

/// Deliberately short: the tag only rejects forgeries (2^-32 per attempt); the
/// keystream provides confidentiality.
const TAG_LEN: usize = 4;

/// The capability class of a participant, the one thing left legible to LiveKit
/// (the 2-character prefix) because both ends route on it: `view` is filtered
/// out of rosters, and voice shares a room with realtime connections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticipantKind {
    /// Data-only realtime / inbox connection.
    Realtime,
    Voice,
    /// Screenshare-receive client; no data channel.
    View,
}

impl ParticipantKind {
    /// Parse the `kind` field of a token request. Unknown/absent → `Realtime`.
    pub fn from_wire(kind: Option<&str>) -> Self {
        match kind {
            Some("voice") => Self::Voice,
            Some("view") => Self::View,
            _ => Self::Realtime,
        }
    }

    /// Fixed width so the split back out is unambiguous even though `-` is in
    /// the base64url alphabet.
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Realtime => "d-",
            Self::Voice => "v-",
            Self::View => "w-",
        }
    }

    fn from_prefix(prefix: &str) -> Option<Self> {
        match prefix {
            "d-" => Some(Self::Realtime),
            "v-" => Some(Self::Voice),
            "w-" => Some(Self::View),
            _ => None,
        }
    }

    /// Mixed into `siv` and `tag` so one user's kinds in a room are independent
    /// strings, not re-prefixings of each other.
    fn label(self) -> u8 {
        match self {
            Self::Realtime => 1,
            Self::Voice => 2,
            Self::View => 3,
        }
    }
}

/// Internal LiveKit participants the DS connects as. Never pseudonymised: both
/// ends filter on these exact strings.
pub const INTERNAL_IDENTITIES: [&str; 3] = ["server", "pollis-backend", "pollis-ds"];

fn mac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut m = HmacSha256::new_from_slice(key).expect("HMAC accepts a key of any length");
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

/// Length-prefix so no two `(user, device)` splits share a MAC input.
fn lp(out: &mut Vec<u8>, field: &[u8]) {
    out.extend_from_slice(&(field.len() as u64).to_be_bytes());
    out.extend_from_slice(field);
}

/// Per-logical-room keys: this scoping is what makes identities unlinkable
/// across rooms.
fn room_keys(api_secret: &str, logical_room: &str) -> ([u8; 32], [u8; 32], [u8; 32]) {
    let mut input = Vec::with_capacity(PARTICIPANT_LABEL.len() + 8 + logical_room.len());
    input.extend_from_slice(PARTICIPANT_LABEL);
    lp(&mut input, logical_room.as_bytes());
    let root = mac(api_secret.as_bytes(), &[&input]);
    (
        mac(&root, &[b"siv"]),
        mac(&root, &[b"enc"]),
        mac(&root, &[b"tag"]),
    )
}

fn keystream(k_enc: &[u8; 32], siv: &[u8], len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len + 32);
    let mut counter: u32 = 0;
    while out.len() < len {
        out.extend_from_slice(&mac(k_enc, &[siv, &counter.to_be_bytes()]));
        counter += 1;
    }
    out.truncate(len);
    out
}

/// Build the opaque identity LiveKit sees for `(user_id, device_id)` in
/// `logical_room`. `device_id` may be empty (legacy no-device path).
///
/// Deterministic, so a reconnect lands on the participant LiveKit already knows
/// instead of doubling it.
pub fn participant_pseudonym(
    api_secret: &str,
    logical_room: &str,
    user_id: &str,
    device_id: &str,
    kind: ParticipantKind,
) -> String {
    let (k_siv, k_enc, k_tag) = room_keys(api_secret, logical_room);

    let mut siv_input = vec![kind.label()];
    lp(&mut siv_input, user_id.as_bytes());
    lp(&mut siv_input, device_id.as_bytes());
    let siv = &mac(&k_siv, &[&siv_input])[..SIV_LEN];

    let ks = keystream(&k_enc, siv, user_id.len());
    let ct: Vec<u8> = user_id.as_bytes().iter().zip(ks).map(|(b, k)| b ^ k).collect();

    let tag = &mac(&k_tag, &[&[kind.label()], siv, &ct])[..TAG_LEN];

    let mut body = Vec::with_capacity(SIV_LEN + ct.len() + TAG_LEN);
    body.extend_from_slice(siv);
    body.extend_from_slice(&ct);
    body.extend_from_slice(tag);

    format!(
        "{}{}",
        kind.prefix(),
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&body)
    )
}

/// Recover `(user_id, kind)` from an identity minted for `logical_room`.
///
/// `None` for anything this key did not produce (internal participant, other
/// room, malformed, forged); callers leave such a participant unattributed.
pub fn resolve_participant(
    api_secret: &str,
    logical_room: &str,
    identity: &str,
) -> Option<(String, ParticipantKind)> {
    if identity.len() < 2 || !identity.is_char_boundary(2) {
        return None;
    }
    let (prefix, blob) = identity.split_at(2);
    let kind = ParticipantKind::from_prefix(prefix)?;

    let body = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(blob)
        .ok()?;
    if body.len() < SIV_LEN + TAG_LEN {
        return None;
    }
    let (siv, rest) = body.split_at(SIV_LEN);
    let (ct, tag) = rest.split_at(rest.len() - TAG_LEN);

    let (_, k_enc, k_tag) = room_keys(api_secret, logical_room);

    let expected = &mac(&k_tag, &[&[kind.label()], siv, ct])[..TAG_LEN];
    if expected.ct_eq(tag).unwrap_u8() != 1 {
        return None;
    }

    let ks = keystream(&k_enc, siv, ct.len());
    let plain: Vec<u8> = ct.iter().zip(ks).map(|(b, k)| b ^ k).collect();
    let user_id = String::from_utf8(plain).ok()?;
    Some((user_id, kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    const K: &str = "test-api-secret";
    const ROOM: &str = "01KZVDFR5Z9DKJ27YN91S3KS6N";
    const USER: &str = "01KZT7RW4RXQGT943Q05C7NW8A";
    const DEV: &str = "01KZT7RW4RXQGT943Q05C7NW8B";

    fn v(room: &str, user: &str, device: &str) -> String {
        participant_pseudonym(K, room, user, device, ParticipantKind::Voice)
    }

    /// The acceptance criterion from #836: no raw user id and no raw device id
    /// ever reaches LiveKit as a participant identity, for ANY kind.
    #[test]
    fn never_leaks_the_user_or_device_id() {
        for kind in [
            ParticipantKind::Realtime,
            ParticipantKind::Voice,
            ParticipantKind::View,
        ] {
            let id = participant_pseudonym(K, ROOM, USER, DEV, kind);
            assert!(!id.contains(USER), "identity must not embed the user id: {id}");
            assert!(!id.contains(DEV), "identity must not embed the device id: {id}");
            assert!(!id.contains(ROOM), "identity must not embed the room: {id}");
            assert!(id.starts_with(kind.prefix()));
            // Short enough to stay a comfortable LiveKit identity: 2 + b64(8+26+4).
            assert!(id.len() <= 64, "identity unexpectedly long ({}): {id}", id.len());
        }
    }

    /// Round-trips, or the roster cannot attribute anyone.
    #[test]
    fn resolves_back_to_the_user_and_kind() {
        for kind in [
            ParticipantKind::Realtime,
            ParticipantKind::Voice,
            ParticipantKind::View,
        ] {
            let id = participant_pseudonym(K, ROOM, USER, DEV, kind);
            assert_eq!(
                resolve_participant(K, ROOM, &id),
                Some((USER.to_string(), kind))
            );
        }
    }

    /// Stable for a given secret — a reconnect must land on the identity LiveKit
    /// already has, or the stale participant lingers beside the new one.
    #[test]
    fn stable_within_a_room() {
        assert_eq!(v(ROOM, USER, DEV), v(ROOM, USER, DEV));
    }

    /// The point of the ticket: one user's identity in two rooms must not be
    /// relatable, or co-membership clustering survives.
    #[test]
    fn unlinkable_across_rooms() {
        let a = v("room-a", USER, DEV);
        let b = v("room-b", USER, DEV);
        assert_ne!(a, b);
        // And room A's key must not resolve room B's identity.
        assert_eq!(resolve_participant(K, "room-a", &b), None);
    }

    /// #140: a user's two devices must remain distinct participants, or the
    /// second one evicts the first on the SFU.
    #[test]
    fn devices_of_one_user_stay_distinct() {
        let a = v(ROOM, USER, "dev-a");
        let b = v(ROOM, USER, "dev-b");
        assert_ne!(a, b);
        // …while still resolving to the same person.
        assert_eq!(resolve_participant(K, ROOM, &a).unwrap().0, USER);
        assert_eq!(resolve_participant(K, ROOM, &b).unwrap().0, USER);
    }

    /// The three kinds must not be re-prefixings of one another, or the SFU can
    /// link a user's voice and data connections by comparing the tail.
    #[test]
    fn kinds_are_independent_strings() {
        let voice = participant_pseudonym(K, ROOM, USER, DEV, ParticipantKind::Voice);
        let realtime = participant_pseudonym(K, ROOM, USER, DEV, ParticipantKind::Realtime);
        let view = participant_pseudonym(K, ROOM, USER, DEV, ParticipantKind::View);
        assert_ne!(voice[2..], realtime[2..]);
        assert_ne!(voice[2..], view[2..]);
        assert_ne!(realtime[2..], view[2..]);
    }

    /// Distinct users must not collide, or their audio is attributed to one tile.
    #[test]
    fn distinct_users_do_not_collide() {
        assert_ne!(v(ROOM, "u1", DEV), v(ROOM, "u2", DEV));
        // Length-prefixing guards the concatenation ambiguity: ("ab","c") and
        // ("a","bc") must not share a MAC input.
        assert_ne!(v(ROOM, "ab", "c"), v(ROOM, "a", "bc"));
    }

    /// Rotating the LiveKit API secret re-keys identities — the documented,
    /// self-healing consequence it shares with room names.
    #[test]
    fn rekeys_with_the_secret() {
        assert_ne!(
            participant_pseudonym(K, ROOM, USER, DEV, ParticipantKind::Voice),
            participant_pseudonym("other-secret", ROOM, USER, DEV, ParticipantKind::Voice)
        );
    }

    /// A hostile SFU inventing a participant must fail to resolve rather than
    /// decrypt to a plausible user id that gets used as a database key.
    #[test]
    fn rejects_forged_and_malformed_identities() {
        let good = v(ROOM, USER, DEV);
        // Flip a ciphertext byte.
        let mut bytes = good.clone().into_bytes();
        let last = bytes.len() - 1;
        bytes[last] = if bytes[last] == b'A' { b'B' } else { b'A' };
        let tampered = String::from_utf8(bytes).unwrap();
        assert_eq!(resolve_participant(K, ROOM, &tampered), None);

        for bad in [
            "",
            "v-",
            "v-!!!!",
            "x-abcdefgh",
            "server",
            "pollis-ds",
            USER,
            &format!("voice-{USER}:{DEV}"),
        ] {
            assert_eq!(resolve_participant(K, ROOM, bad), None, "accepted {bad:?}");
        }
    }

    /// The legacy no-device path still round-trips, and does not collide with
    /// the same user on a real device.
    #[test]
    fn empty_device_is_supported_and_distinct() {
        let none = v(ROOM, USER, "");
        assert_eq!(resolve_participant(K, ROOM, &none).unwrap().0, USER);
        assert_ne!(none, v(ROOM, USER, DEV));
    }
}
