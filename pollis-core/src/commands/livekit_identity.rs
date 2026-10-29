//! The LiveKit participant-identity grammar (#836), always compiled.
//!
//! Two identity spaces: the **wire** identity is an opaque per-room pseudonym
//! minted by the DS (prefix `d-` realtime, `v-` voice, `w-` view, then
//! ciphertext); the **internal** identity (`voice-{user}:{device}`,
//! `{user}:{device}`, `{user}:{device}:view`) is a local UI key. Every LiveKit
//! boundary translates inbound. Internal identities contain raw ids, so one
//! reaching a wire (a data payload, a DS body) is a bug.
//!
//! Resolving a pseudonym recovers the user, not the device, so a remote peer's
//! device half is synthesised from the pseudonym ([`internal_identity`]); only
//! the local participant keeps its real device id. See
//! `.codesight/wiki/secrets-broker.md`.

/// A participant translated out of its wire pseudonym: (internal identity,
/// display name). Not the DS response type because the local participant's entry
/// is pinned at join, not resolved.
pub type TranslatedIdentity = (String, String);

/// The #836 translation memo: `(logical_room, wire_identity)` to its translation.
pub type IdentityCache = std::collections::HashMap<(String, String), TranslatedIdentity>;

/// Capability class of a participant. Mirrors
/// `pollis_delivery::participant_id::ParticipantKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticipantKind {
    Realtime,
    Voice,
    View,
}

impl ParticipantKind {
    /// Parse the DS's `kind`. Unknown is `Realtime`, the token endpoint's default.
    pub fn from_wire(kind: &str) -> Self {
        match kind {
            "voice" => Self::Voice,
            "view" => Self::View,
            _ => Self::Realtime,
        }
    }

    /// Classify an unresolved identity by its prefix, the one part of a pseudonym
    /// that stays legible on purpose.
    pub fn from_wire_prefix(identity: &str) -> Option<Self> {
        match identity.get(..2)? {
            "d-" => Some(Self::Realtime),
            "v-" => Some(Self::Voice),
            "w-" => Some(Self::View),
            _ => None,
        }
    }
}

/// Pseudonym characters used as the synthetic device token: 72 bits of
/// per-device ciphertext, short enough to stay readable in logs.
const DEVICE_TOKEN_LEN: usize = 12;

/// The internal identity for the LOCAL voice participant: `voice-{user_id}:{device_id}`,
/// or `voice-{user_id}` with no device id yet. The suffix keeps one user's
/// devices distinct (#140). The renderer builds the same string from
/// `get_device_id`; keep the two in lockstep.
pub fn voice_identity(user_id: &str, device_id: Option<&str>) -> String {
    match device_id {
        Some(d) if !d.is_empty() => format!("voice-{user_id}:{d}"),
        _ => format!("voice-{user_id}"),
    }
}

/// The internal identity for a REMOTE participant whose pseudonym resolved to
/// `user_id`. Deterministic, so one peer keeps one identity for the whole call.
pub fn internal_identity(user_id: &str, wire_identity: &str, kind: ParticipantKind) -> String {
    // Skip the capability prefix; the ciphertext beneath differs per device. Chars,
    // not a byte slice, because the identity may be attacker-supplied.
    let blob: String = wire_identity
        .chars()
        .skip(2)
        .filter(|c| *c != ':')
        .take(DEVICE_TOKEN_LEN)
        .collect();
    let device = if blob.is_empty() { "x".to_string() } else { blob };
    match kind {
        ParticipantKind::Voice => format!("voice-{user_id}:{device}"),
        ParticipantKind::Realtime => format!("{user_id}:{device}"),
        ParticipantKind::View => format!("{user_id}:{device}:view"),
    }
}

/// The bare `user_id` of a voice identity (`voice-u1:dev-a` and `voice-u1` give
/// `u1`). Anything else is returned unchanged.
pub fn user_id_from_voice_identity(identity: &str) -> &str {
    let stripped = identity.strip_prefix("voice-").unwrap_or(identity);
    match stripped.split_once(':') {
        Some((uid, _device)) => uid,
        None => stripped,
    }
}

/// The `user_id` of any internal identity. `None` for DS-internal participants
/// and unresolved pseudonyms, rather than guessing the wrong person.
pub fn user_id_from_identity(identity: &str) -> Option<&str> {
    if INTERNAL_IDENTITIES.contains(&identity) {
        return None;
    }
    if ParticipantKind::from_wire_prefix(identity).is_some() {
        return None;
    }
    if let Some(rest) = identity.strip_prefix("voice-") {
        return Some(user_id_from_voice_identity(rest));
    }
    if let Some((user_id, _device)) = identity.split_once(':') {
        return Some(user_id);
    }
    Some(identity)
}

/// LiveKit participants that are not people; filtered from rosters by exact name.
pub const INTERNAL_IDENTITIES: [&str; 3] = ["server", "pollis-backend", "pollis-ds"];

/// Is this the screenshare-receive variant? Checks `:view` and the `w-` prefix,
/// so an unresolved view client still doesn't become a phantom duplicate tile.
pub fn is_view_identity(identity: &str) -> bool {
    identity.ends_with(":view")
        || ParticipantKind::from_wire_prefix(identity) == Some(ParticipantKind::View)
}

/// Read the `sub` claim out of a LiveKit JWT we hold. Not a verification.
///
/// Lets the local participant pin its own pseudonym to its `get_device_id`-built
/// key: LiveKit reports local events (e.g. `TrackMuted` on every push-to-talk)
/// under the wire identity, and resolving it via the DS would give a synthetic
/// device and detach the local tile from its mute state.
pub fn jwt_subject(token: &str) -> Option<String> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims
        .get("sub")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIRE_VOICE_A: &str = "v-AbCdEfGhIjKlMnOpQrStUvWxYz012345";
    const WIRE_VOICE_B: &str = "v-ZyXwVuTsRqPoNmLkJiHgFeDcBa543210";

    /// Must parse back under the app's own helpers, or peers are attributed to nobody.
    #[test]
    fn translated_identities_round_trip_to_the_user() {
        for (kind, id) in [
            (ParticipantKind::Voice, internal_identity("u1", WIRE_VOICE_A, ParticipantKind::Voice)),
            (
                ParticipantKind::Realtime,
                internal_identity("u1", WIRE_VOICE_A, ParticipantKind::Realtime),
            ),
            (ParticipantKind::View, internal_identity("u1", WIRE_VOICE_A, ParticipantKind::View)),
        ] {
            assert_eq!(user_id_from_identity(&id), Some("u1"), "{kind:?}: {id}");
        }
        let voice = internal_identity("u1", WIRE_VOICE_A, ParticipantKind::Voice);
        assert_eq!(user_id_from_voice_identity(&voice), "u1");
    }

    #[test]
    fn two_pseudonyms_of_one_user_stay_distinct_participants() {
        let a = internal_identity("u1", WIRE_VOICE_A, ParticipantKind::Voice);
        let b = internal_identity("u1", WIRE_VOICE_B, ParticipantKind::Voice);
        assert_ne!(a, b);
        assert_eq!(user_id_from_voice_identity(&a), user_id_from_voice_identity(&b));
    }

    #[test]
    fn translation_is_stable_for_one_pseudonym() {
        assert_eq!(
            internal_identity("u1", WIRE_VOICE_A, ParticipantKind::Voice),
            internal_identity("u1", WIRE_VOICE_A, ParticipantKind::Voice),
        );
    }

    #[test]
    fn view_clients_stay_recognisable() {
        let view = internal_identity("u1", "w-AbCdEfGhIjKlMnOp", ParticipantKind::View);
        assert!(is_view_identity(&view), "{view}");
        assert!(!is_view_identity(&internal_identity(
            "u1",
            WIRE_VOICE_A,
            ParticipantKind::Voice
        )));
        assert!(is_view_identity("w-AbCdEfGhIjKlMnOp"));
        assert!(!is_view_identity("v-AbCdEfGhIjKlMnOp"));
    }

    #[test]
    fn unresolved_pseudonyms_are_not_users() {
        for id in [WIRE_VOICE_A, "d-AbCdEfGh", "w-AbCdEfGh"] {
            assert_eq!(user_id_from_identity(id), None, "{id}");
        }
    }

    #[test]
    fn internal_participants_are_not_users() {
        for id in INTERNAL_IDENTITIES {
            assert_eq!(user_id_from_identity(id), None, "{id}");
        }
    }

    #[test]
    fn local_voice_identity_shape() {
        assert_eq!(voice_identity("u1", Some("dev-a")), "voice-u1:dev-a");
        assert_eq!(voice_identity("u1", None), "voice-u1");
        assert_eq!(voice_identity("u1", Some("")), "voice-u1");
        for id in [voice_identity("u1", Some("dev-a")), voice_identity("u1", None)] {
            assert_eq!(user_id_from_voice_identity(&id), "u1");
        }
    }

    /// The self-hear filter (#140) keys on the user: a sibling device counts as self.
    #[test]
    fn self_hear_predicate_matches_sibling_device_only() {
        let me = "u1";
        assert_eq!(user_id_from_voice_identity(&voice_identity("u1", Some("dev-b"))), me);
        assert_ne!(user_id_from_voice_identity(&voice_identity("u2", Some("dev-x"))), me);
        assert_eq!(
            user_id_from_voice_identity(&internal_identity("u1", WIRE_VOICE_B, ParticipantKind::Voice)),
            me
        );
        assert_ne!(
            user_id_from_voice_identity(&internal_identity("u2", WIRE_VOICE_B, ParticipantKind::Voice)),
            me
        );
    }

    /// A malformed pseudonym must still give a usable key that doesn't end in `:` and
    /// collide with every other malformed one.
    #[test]
    fn degenerate_pseudonyms_still_produce_a_key() {
        for wire in ["", "v-", "v", "::::"] {
            let id = internal_identity("u1", wire, ParticipantKind::Voice);
            assert_eq!(user_id_from_voice_identity(&id), "u1", "{wire:?} → {id}");
            assert!(!id.ends_with(':'), "{wire:?} → {id}");
        }
    }

    #[test]
    fn jwt_subject_reads_the_identity_claim() {
        use base64::Engine;
        let b64 = |v: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        let token = format!(
            "{}.{}.{}",
            b64(r#"{"alg":"HS256","typ":"JWT"}"#),
            b64(r#"{"sub":"v-AbCdEfGh","name":"","exp":1}"#),
            "not-a-real-signature"
        );
        assert_eq!(jwt_subject(&token).as_deref(), Some("v-AbCdEfGh"));
    }

    #[test]
    fn jwt_subject_rejects_malformed_tokens() {
        use base64::Engine;
        let b64 = |v: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v);
        for bad in [
            String::new(),
            "abc".to_string(),
            "a.b.c".to_string(),
            format!("x.{}.y", b64(r#"{"name":"alice"}"#)),
            format!("x.{}.y", b64(r#"{"sub":""}"#)),
        ] {
            assert_eq!(jwt_subject(&bad), None, "accepted {bad:?}");
        }
    }
}
