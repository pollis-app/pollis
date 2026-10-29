//! The LiveKit participant-identity grammar, in one always-compiled place.
//!
//! Two identity spaces (#836): wire = per-room DS pseudonym; internal = local key, never sent.
//! Translate inbound at every SDK boundary; never put an identity in a payload.
//! Remote device halves are synthesised from the pseudonym; only ours is real.

/// One LiveKit participant translated out of its wire pseudonym: the internal
/// identity the app keys on, and the display name to render.
pub type TranslatedIdentity = (String, String);

/// The #836 translation memo: `(logical_room, wire_identity)` → the translation.
pub type IdentityCache = std::collections::HashMap<(String, String), TranslatedIdentity>;

/// Capability class of a participant, as the DS reports it when resolving.
/// Mirrors `pollis_delivery::participant_id::ParticipantKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticipantKind {
    Realtime,
    Voice,
    View,
}

impl ParticipantKind {
    /// Parse the DS's `kind` string. Unknown → `Realtime`, matching the token
    /// endpoint's default.
    pub fn from_wire(kind: &str) -> Self {
        match kind {
            "voice" => Self::Voice,
            "view" => Self::View,
            _ => Self::Realtime,
        }
    }

    /// The kind from an unresolved identity's prefix (deliberately legible).
    pub fn from_wire_prefix(identity: &str) -> Option<Self> {
        match identity.get(..2)? {
            "d-" => Some(Self::Realtime),
            "v-" => Some(Self::Voice),
            "w-" => Some(Self::View),
            _ => None,
        }
    }
}

/// Synthetic device-token length: 12 base64url chars = 72 bits.
const DEVICE_TOKEN_LEN: usize = 12;

/// Build the per-device internal identity for a LOCAL voice participant:
/// `voice-{user_id}:{device_id}`, falling back to the legacy `voice-{user_id}`
/// when no device id is known yet.
///
/// The renderer builds the same string from `get_device_id`; keep in lockstep.
pub fn voice_identity(user_id: &str, device_id: Option<&str>) -> String {
    match device_id {
        Some(d) if !d.is_empty() => format!("voice-{user_id}:{d}"),
        _ => format!("voice-{user_id}"),
    }
}

/// Build the internal identity for a REMOTE participant whose wire pseudonym
/// resolved to `user_id`.
///
/// Device half derived deterministically from the pseudonym (see module docs).
pub fn internal_identity(user_id: &str, wire_identity: &str, kind: ParticipantKind) -> String {
    // Strip the prefix; `char_indices` since the identity is attacker-supplied.
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

/// Extract the bare `user_id` from a **voice** identity.
///
/// `voice-u1:dev-a` → `u1`; non-voice identities are returned unchanged.
pub fn user_id_from_voice_identity(identity: &str) -> &str {
    let stripped = identity.strip_prefix("voice-").unwrap_or(identity);
    match stripped.split_once(':') {
        Some((uid, _device)) => uid,
        None => stripped,
    }
}

/// Extract a `user_id` from any internal identity — voice, realtime or view.
///
/// `None` for DS participants and unresolved pseudonyms (never guess a user).
pub fn user_id_from_identity(identity: &str) -> Option<&str> {
    if INTERNAL_IDENTITIES.contains(&identity) {
        return None;
    }
    // An unresolved pseudonym (by its prefix) is not a user.
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

/// LiveKit participants that are not people. Both the DS and the client filter
/// these out of rosters by exact name.
pub const INTERNAL_IDENTITIES: [&str; 3] = ["server", "pollis-backend", "pollis-ds"];

/// Is this the screenshare-receive variant?
///
/// Checks `:view` and the `w-` prefix, so a resolve failure can't add a phantom tile.
pub fn is_view_identity(identity: &str) -> bool {
    identity.ends_with(":view")
        || ParticipantKind::from_wire_prefix(identity) == Some(ParticipantKind::View)
}

/// Read the `sub` (identity) claim out of a LiveKit JWT **we hold**.
///
/// Not a verification; lets the caller pin our own pseudonym (see `pin_local_identity`).
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

    /// Wire pseudonyms shaped like the DS mints them, for the translation tests.
    const WIRE_VOICE_A: &str = "v-AbCdEfGhIjKlMnOpQrStUvWxYz012345";
    const WIRE_VOICE_B: &str = "v-ZyXwVuTsRqPoNmLkJiHgFeDcBa543210";

    /// The translated identity must parse back to the user under the SAME
    /// helpers the rest of the app uses, or peers are attributed to nobody.
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

    /// #140: one user's two devices stay two participants, same person.
    #[test]
    fn two_pseudonyms_of_one_user_stay_distinct_participants() {
        let a = internal_identity("u1", WIRE_VOICE_A, ParticipantKind::Voice);
        let b = internal_identity("u1", WIRE_VOICE_B, ParticipantKind::Voice);
        assert_ne!(a, b);
        assert_eq!(user_id_from_voice_identity(&a), user_id_from_voice_identity(&b));
    }

    /// Deterministic — every event mentioning a peer must produce one identity,
    /// or the tile grid gains a duplicate on each mute/speak event.
    #[test]
    fn translation_is_stable_for_one_pseudonym() {
        assert_eq!(
            internal_identity("u1", WIRE_VOICE_A, ParticipantKind::Voice),
            internal_identity("u1", WIRE_VOICE_A, ParticipantKind::Voice),
        );
    }

    /// The `:view` split must survive translation, or a screenshare-receive
    /// client renders as a second tile for the same person.
    #[test]
    fn view_clients_stay_recognisable() {
        let view = internal_identity("u1", "w-AbCdEfGhIjKlMnOp", ParticipantKind::View);
        assert!(is_view_identity(&view), "{view}");
        assert!(!is_view_identity(&internal_identity(
            "u1",
            WIRE_VOICE_A,
            ParticipantKind::Voice
        )));
        // …and an UNRESOLVED view pseudonym is still filtered, by its prefix.
        assert!(is_view_identity("w-AbCdEfGhIjKlMnOp"));
        assert!(!is_view_identity("v-AbCdEfGhIjKlMnOp"));
    }

    /// An unresolved pseudonym must not be mistaken for a user id.
    #[test]
    fn unresolved_pseudonyms_are_not_users() {
        for id in [WIRE_VOICE_A, "d-AbCdEfGh", "w-AbCdEfGh"] {
            assert_eq!(user_id_from_identity(id), None, "{id}");
        }
    }

    /// Internal LiveKit participants are not people.
    #[test]
    fn internal_participants_are_not_users() {
        for id in INTERNAL_IDENTITIES {
            assert_eq!(user_id_from_identity(id), None, "{id}");
        }
    }

    /// The local identity keeps the real device id — the renderer rebuilds this
    /// exact string to recognise itself, so the two must not drift.
    #[test]
    fn local_voice_identity_shape() {
        assert_eq!(voice_identity("u1", Some("dev-a")), "voice-u1:dev-a");
        assert_eq!(voice_identity("u1", None), "voice-u1");
        assert_eq!(voice_identity("u1", Some("")), "voice-u1");
        for id in [voice_identity("u1", Some("dev-a")), voice_identity("u1", None)] {
            assert_eq!(user_id_from_voice_identity(&id), "u1");
        }
    }

    /// The self-hear filter (#140) keys on the USER, so a sibling device counts
    /// as self and another user does not — whichever space the identity is in.
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

    /// A malformed pseudonym still yields a usable, non-colliding key without panicking.
    #[test]
    fn degenerate_pseudonyms_still_produce_a_key() {
        for wire in ["", "v-", "v", "::::"] {
            let id = internal_identity("u1", wire, ParticipantKind::Voice);
            assert_eq!(user_id_from_voice_identity(&id), "u1", "{wire:?} → {id}");
            assert!(!id.ends_with(':'), "{wire:?} → {id}");
        }
    }

    /// Reading our own token's `sub` keeps the local tile on its mute state.
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

    /// Anything that is not a token with a non-empty `sub` must be `None`, so the
    /// caller falls back to resolving normally rather than pinning nonsense.
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
