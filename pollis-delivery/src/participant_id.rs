//! Pseudonymous LiveKit participant identities (#836).
//!
//! The sibling of [`crate::room_id`], and the half it deliberately left behind.
//!
//! #828/#835 stopped LiveKit being able to name a room. It did nothing about
//! *who is in it*: participant identity was still the raw `{user_id}:{device_id}`
//! (`voice-` / `:view` variants per kind), so the SFU operator kept a list of
//! which Pollis accounts share a room — and, because a user carries the same
//! identity into every room they join, could stitch those lists into the social
//! graph by co-membership. Opaque room names do not help against that: you do
//! not need to know a room is "#design" to learn that A, B and C talk.
//!
//! Every identity is now `{prefix}{base64url(siv ‖ ct ‖ tag)}`, keyed per
//! LOGICAL ROOM. Two rooms give the same user two unrelated identities.
//!
//! ## Why encryption and not a MAC
//!
//! [`crate::room_id`] is a one-way HMAC because nothing ever needs to invert it:
//! the room travels inside the JWT grant and no client reads it back. Identity is
//! different — it is *matched against*. The voice roster, the speaking
//! indicators, presence and the self-hear filter all have to get from a
//! participant identity back to a Pollis user.
//!
//! A MAC would force the resolver to guess: enumerate every candidate
//! `(user, device)` pair, derive, and compare. That works only where the
//! candidate set is known and current, and it is neither for `call-<ulid>` rooms
//! (no membership rows exist — the ULID *is* the capability) nor for a device
//! enrolled since the resolver last synced. So the identity is *encrypted*
//! rather than hashed, and [`resolve_participant`] recovers the user with no
//! candidate list at all.
//!
//! ## Where the key lives — still server-only
//!
//! Clients resolve by asking the DS (`POST /v1/livekit/identities`), which
//! answers only for rooms the caller is already authorized to join. No client
//! holds the key, which is the same property #828 protected and for the same
//! reason: a key shipped in a release binary can be extracted, and one that
//! resolves identities in *every* room would hand the whole graph back.
//!
//! ## Construction
//!
//! Deterministic authenticated encryption in the SIV shape, built from
//! HMAC-SHA256 only (no new dependency; the same primitive already keying
//! [`crate::room_id`]):
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
//! The synthetic IV is what makes it deterministic *without* the catastrophe of
//! a reused nonce: `siv` is a PRF of the plaintext plus the device, so equal
//! inputs give one stable identity and unequal inputs never share a keystream.
//! Folding `device_id` into `siv` — rather than into the ciphertext — is what
//! keeps a user's two devices distinct on the SFU (#140) without spending wire
//! length on a second identifier the client has no use for.
//!
//! `tag` is not load-bearing for confidentiality; it exists so a fabricated
//! identity from a hostile SFU is *rejected* rather than decrypted into a
//! plausible-looking garbage user id that then gets used as a database key.
//!
//! ## What this buys, stated exactly
//!
//! After this change the SFU sees, per participant, an opaque 53-character
//! string whose only structure is a 2-character capability prefix. It CANNOT:
//!
//!   - map a participant to a Pollis account, username, or device;
//!   - tell that a participant in room A is the same person as one in room B —
//!     which is precisely the co-membership clustering #836 was filed for;
//!   - tell that two rooms have overlapping membership at all.
//!
//! It still CAN:
//!
//!   - see how many participants share a room, and their traffic patterns;
//!   - recognise a returning participant **within one room**, because — exactly
//!     as with room names in #828 — these are stable pseudonyms, not unlinkable
//!     ones. For 1:1 calls that is already per-call: `call-<ulid>` rooms are
//!     ephemeral, so a fresh room key means a fresh identity every call. For a
//!     persistent voice channel it is not, and the SFU can count "this opaque
//!     participant joined this opaque room 14 times".
//!
//! Closing that last gap needs a per-call epoch in the key. The only epoch the
//! DS could observe without inventing state is the LiveKit room's creation time,
//! which costs a `ListRooms` round trip on the join hot path and, worse, breaks
//! LiveKit's duplicate-identity eviction: a mid-call reconnect that lands on a
//! new epoch leaves the stale participant to time out beside the new one — a
//! duplicate tile where today there is none. That is an availability/UX trade
//! that wants its own decision, so it is *not* taken here, matching the same
//! call made for room names in #828.
//!
//! ## Keying
//!
//! Derived from `LIVEKIT_API_SECRET` under its own domain-separation label, for
//! the reason given in [`crate::room_id`]: rotating it re-keys identities, and
//! rotation already invalidates every outstanding LiveKit token, so clients
//! re-request one and pick up the new scheme in a round trip they were making
//! anyway.

use base64::Engine;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

/// Domain separation from [`crate::room_id`], which derives from the same
/// secret. Distinct labels keep the two derivations from ever colliding.
const PARTICIPANT_LABEL: &[u8] = b"pollis-livekit-participant-pseudonym-v1";

/// Synthetic-IV width in bytes. 64 bits of PRF output over
/// `(kind, user_id, device_id)`: a collision would make two of one user's
/// devices share an identity and evict each other on the SFU, which at 2^-64 per
/// pair is not a failure mode worth spending wire length on.
const SIV_LEN: usize = 8;

/// Authentication-tag width in bytes. Deliberately short: the tag only has to
/// make a hostile SFU's fabricated identity fail to resolve, and 2^-32 forgery
/// odds per attempt does that. It is not protecting confidentiality — the
/// keystream is.
const TAG_LEN: usize = 4;

/// The capability class of a participant. This is the ONE thing that stays
/// legible to LiveKit, encoded as the identity's 2-character prefix, because
/// both ends have to route on it: the `view` variant is filtered out of rosters
/// and tile grids, and voice participants share a room with the data-only
/// realtime connections of the same conversation.
///
/// Capability was already visible before this change (`voice-` prefix, `:view`
/// suffix); nothing new is disclosed by keeping it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticipantKind {
    /// Data-only realtime / inbox connection. Was `{user}:{device}`.
    Realtime,
    /// Voice participant. Was `voice-{user}:{device}`.
    Voice,
    /// Screenshare-receive client; no data channel. Was `{user}:{device}:view`.
    View,
}

impl ParticipantKind {
    /// Parse the `kind` field of a token request. Unknown/absent → `Realtime`,
    /// matching the endpoint's pre-existing default.
    pub fn from_wire(kind: Option<&str>) -> Self {
        match kind {
            Some("voice") => Self::Voice,
            Some("view") => Self::View,
            _ => Self::Realtime,
        }
    }

    /// The 2-character identity prefix. Fixed width so the split back out is
    /// unambiguous even though `-` is in the base64url alphabet.
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

    /// Domain-separating byte mixed into `siv` and `tag`, so one user's voice
    /// and realtime identities in a room are independent strings rather than
    /// obvious re-prefixings of each other.
    fn label(self) -> u8 {
        match self {
            Self::Realtime => 1,
            Self::Voice => 2,
            Self::View => 3,
        }
    }
}

/// Internal LiveKit participants the DS itself connects as. Never pseudonymised
/// — they are not people, and both ends filter on these exact strings.
pub const INTERNAL_IDENTITIES: [&str; 3] = ["server", "pollis-backend", "pollis-ds"];

fn mac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut m = HmacSha256::new_from_slice(key).expect("HMAC accepts a key of any length");
    for p in parts {
        m.update(p);
    }
    m.finalize().into_bytes().into()
}

/// Length-prefix a field so no two distinct `(user, device)` splits can produce
/// the same MAC input by concatenation — the same guard `room_id` applies to the
/// logical room name.
fn lp(out: &mut Vec<u8>, field: &[u8]) {
    out.extend_from_slice(&(field.len() as u64).to_be_bytes());
    out.extend_from_slice(field);
}

/// Per-logical-room key material. Scoping here is what makes identities
/// unlinkable ACROSS rooms: the same user in two rooms shares no key, so nothing
/// relates their two identities.
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

/// HMAC-as-PRG keystream, the HKDF-Expand shape: successive counter blocks under
/// `k_enc`, seeded by the synthetic IV.
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
/// `logical_room`.
///
/// `device_id` may be empty — the legacy no-device path — in which case the
/// identity is still stable and still resolves, it just does not distinguish
/// devices (there is only one to distinguish).
///
/// Deterministic: the same inputs always give the same identity for a given
/// secret, which is what lets a reconnect land on the participant LiveKit
/// already knows instead of doubling it.
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
/// `None` for anything this key did not produce: an internal participant, a
/// truncated or re-prefixed string, an identity from a different room, or a
/// forgery. Callers treat `None` as "not a Pollis user" and leave the
/// participant unattributed rather than guessing.
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
