//! Known-answer tests for the authorized-secrets broker's pure signing
//! functions (#393). Both take an injected clock/timestamp precisely so the
//! output is deterministic and lockable:
//!
//!   - `sign_livekit_token` — decode the HS256 JWT, assert header + claim shape,
//!     and re-verify the signature against the secret.
//!   - `presign_r2_url` — a SigV4 golden test: every input pinned, the exact
//!     resulting URL string asserted, so any drift in canonical-request
//!     construction, encoding, or the signature breaks the test.
//!
//! No DB / router here — these are the pure functions, tested directly (the
//! request-path auth is covered by `auth.rs`).

use base64::Engine as _;
use hmac::{Hmac, Mac};
use pollis_delivery::broker::{presign_r2_url, sign_livekit_admin_token, sign_livekit_token};
use sha2::Sha256;

// ── LiveKit JWT ────────────────────────────────────────────────────────────

const LK_KEY: &str = "APIexampleKey";
const LK_SECRET: &str = "livekit-signing-secret-do-not-log";

fn b64url_decode(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .expect("valid base64url")
}

/// Split a JWT into (header_json, payload_json, signature_b64url).
fn split_jwt(token: &str) -> (serde_json::Value, serde_json::Value, String) {
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3, "JWT has three dot-separated parts");
    let header: serde_json::Value =
        serde_json::from_slice(&b64url_decode(parts[0])).expect("header json");
    let payload: serde_json::Value =
        serde_json::from_slice(&b64url_decode(parts[1])).expect("payload json");
    (header, payload, parts[2].to_string())
}

/// Recompute the HS256 signature over `header.payload` and assert it matches the
/// token's third segment — proves the token verifies against `secret`.
fn assert_hs256_signature(token: &str, secret: &str) {
    let last_dot = token.rfind('.').expect("has signature segment");
    let signing_input = &token[..last_dot];
    let provided_sig = &token[last_dot + 1..];

    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(signing_input.as_bytes());
    let expected = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(mac.finalize().into_bytes());

    assert_eq!(provided_sig, expected, "HS256 signature must verify");
}

#[test]
fn livekit_token_header_is_hs256_typ_jwt() {
    let token =
        sign_livekit_token(LK_KEY, LK_SECRET, "room-1", "alice", true, 1_700_000_000).unwrap();
    let (header, _, _) = split_jwt(&token);
    assert_eq!(header["alg"], "HS256");
    assert_eq!(header["typ"], "JWT");
}

#[test]
fn livekit_token_claim_shape_and_signature() {
    let now = 1_700_000_000u64;
    let token =
        sign_livekit_token(LK_KEY, LK_SECRET, "room-1", "alice", true, now).unwrap();
    let (_, payload, _) = split_jwt(&token);

    // iss = api key; sub = identity; times pinned off the injected clock.
    assert_eq!(payload["iss"], LK_KEY);
    assert_eq!(payload["sub"], "alice");
    // #836: the display name is gone. It used to be the user's Pollis username,
    // which handed the SFU the account behind every participant regardless of
    // what the identity said.
    assert_eq!(payload["name"], "");
    assert_eq!(payload["iat"], now);
    assert_eq!(payload["nbf"], now);
    assert_eq!(payload["exp"], now + 3600);

    // Video grants — a normal publisher.
    let v = &payload["video"];
    assert_eq!(v["room"], "room-1");
    assert_eq!(v["roomJoin"], true);
    assert_eq!(v["canPublish"], true);
    assert_eq!(v["canSubscribe"], true);
    assert_eq!(v["canPublishData"], true);

    assert_hs256_signature(&token, LK_SECRET);
}

#[test]
fn livekit_view_variant_disables_publish_data() {
    // The screenshare `:view` participant: can_publish_data = false.
    let token = sign_livekit_token(
        LK_KEY,
        LK_SECRET,
        "room-1",
        "alice:view",
        false,
        1_700_000_000,
    )
    .unwrap();
    let (_, payload, _) = split_jwt(&token);
    assert_eq!(payload["sub"], "alice:view");
    assert_eq!(payload["video"]["canPublishData"], false);
    // Everything else stays a full grant.
    assert_eq!(payload["video"]["roomJoin"], true);
    assert_eq!(payload["video"]["canPublish"], true);
    assert_eq!(payload["video"]["canSubscribe"], true);
    assert_hs256_signature(&token, LK_SECRET);
}

#[test]
fn livekit_admin_token_grants_room_admin_and_verifies() {
    // The admin token (SendData / ListParticipants) must carry roomAdmin +
    // roomList scoped to the room, a short (+300s) expiry, and verify against the
    // secret. `sub` is the internal DS identity (filtered out of rosters).
    let now = 1_700_000_000u64;
    let token = sign_livekit_admin_token(LK_KEY, LK_SECRET, "inbox-u_123", now).unwrap();
    let (header, payload, _) = split_jwt(&token);
    assert_eq!(header["alg"], "HS256");
    assert_eq!(payload["iss"], LK_KEY);
    assert_eq!(payload["sub"], "pollis-ds");
    assert_eq!(payload["exp"], now + 300);
    assert_eq!(payload["video"]["roomAdmin"], true);
    assert_eq!(payload["video"]["roomList"], true);
    assert_eq!(payload["video"]["room"], "inbox-u_123");
    // Must NOT carry participant grants (it never joins as a participant).
    assert!(payload["video"].get("roomJoin").is_none());
    assert_hs256_signature(&token, LK_SECRET);
}

// ── R2 SigV4 presign (known-answer) ─────────────────────────────────────────
//
// Every input pinned so the signature is reproducible. Endpoint/bucket/key/
// region/keys/datetime/expires are all fixed; the resulting URL string is
// asserted byte-for-byte. AWS's documented example secret key is reused for
// familiarity, but this is path-style (host = endpoint, `/bucket/key`), so the
// signature is specific to THIS canonical request, not AWS's virtual-hosted one.

const R2_ENDPOINT: &str = "https://accountid.r2.cloudflarestorage.com";
const R2_BUCKET: &str = "pollis-media";
const R2_REGION: &str = "auto";
const R2_ACCESS: &str = "AKIAIOSFODNN7EXAMPLE";
const R2_SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";
const R2_DATE: &str = "20250101T000000Z";

#[test]
fn presign_get_is_locked() {
    let url = presign_r2_url(
        R2_ENDPOINT,
        R2_BUCKET,
        R2_REGION,
        R2_ACCESS,
        R2_SECRET,
        "GET",
        "media/abc123/file.enc",
        900,
        R2_DATE,
    );
    // Signature cross-checked against an independent SigV4 implementation.
    assert_eq!(
        url,
        "https://accountid.r2.cloudflarestorage.com/pollis-media/media/abc123/file.enc\
?X-Amz-Algorithm=AWS4-HMAC-SHA256\
&X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20250101%2Fauto%2Fs3%2Faws4_request\
&X-Amz-Date=20250101T000000Z\
&X-Amz-Expires=900\
&X-Amz-SignedHeaders=host\
&X-Amz-Signature=e3af94429533e259ea797800c59479575d8780ccaad213de20c2954970e3903c"
    );
}

#[test]
fn presign_put_encodes_key_slash_and_space() {
    // PUT + a key with a slash (preserved) and a space (percent-encoded) —
    // locks the URI-encoding of the canonical path.
    let url = presign_r2_url(
        R2_ENDPOINT,
        R2_BUCKET,
        R2_REGION,
        R2_ACCESS,
        R2_SECRET,
        "PUT",
        "media/sub dir/my file.enc",
        3600,
        R2_DATE,
    );
    // Slash preserved in the path; space becomes %20. Expires differs (3600) so
    // the signature is distinct from the GET case.
    assert_eq!(
        url,
        "https://accountid.r2.cloudflarestorage.com/pollis-media/media/sub%20dir/my%20file.enc\
?X-Amz-Algorithm=AWS4-HMAC-SHA256\
&X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20250101%2Fauto%2Fs3%2Faws4_request\
&X-Amz-Date=20250101T000000Z\
&X-Amz-Expires=3600\
&X-Amz-SignedHeaders=host\
&X-Amz-Signature=faa8c97256a7f4a8a79ac5481e9f46233e1511fd1a4ce181dc0f6859dee14afb"
    );
}

#[test]
fn presign_delete_signature_binds_the_method() {
    // DELETE (attachment cleanup) presign — the HTTP method is part of the SigV4
    // canonical request, so a DELETE URL must carry a signature distinct from an
    // otherwise-identical GET. Guards against the handler ever mapping delete to
    // the wrong verb.
    let common = |method| {
        presign_r2_url(
            R2_ENDPOINT, R2_BUCKET, R2_REGION, R2_ACCESS, R2_SECRET, method,
            "media/abc123/file.enc", 900, R2_DATE,
        )
    };
    let del = common("DELETE");
    let get = common("GET");
    let sig = |u: &str| u.rsplit_once("X-Amz-Signature=").unwrap().1.to_string();
    assert_ne!(sig(&del), sig(&get), "DELETE must not reuse the GET signature");
    // Everything up to the signature (path + canonical query) is method-agnostic,
    // so the two URLs are identical there.
    assert_eq!(
        del.split_once("&X-Amz-Signature=").unwrap().0,
        get.split_once("&X-Amz-Signature=").unwrap().0,
    );
}

#[test]
fn presign_canonical_query_is_sorted() {
    // SigV4 requires the canonical query params sorted by name. X-Amz-Signature
    // is appended AFTER signing, so it's the only param allowed out of order.
    let url = presign_r2_url(
        R2_ENDPOINT, R2_BUCKET, R2_REGION, R2_ACCESS, R2_SECRET, "GET", "k", 900, R2_DATE,
    );
    let query = url.split_once('?').expect("has query").1;
    let names: Vec<&str> = query
        .split('&')
        .map(|p| p.split('=').next().unwrap())
        .filter(|n| *n != "X-Amz-Signature")
        .collect();
    let mut sorted = names.clone();
    sorted.sort_unstable();
    assert_eq!(names, sorted, "canonical query params must be name-sorted");
    // And the signature is genuinely last (never part of what was signed).
    assert!(query.split('&').next_back().unwrap().starts_with("X-Amz-Signature="));
}

// ─── #828: pseudonymous room names ───────────────────────────────────────────

/// The acceptance criterion: a token minted for a conversation must not carry
/// that conversation's id in its grant. This is asserted on the *decoded JWT*,
/// not on the helper, because the grant is the thing LiveKit actually reads —
/// a mapping applied anywhere short of it would still leak.
#[test]
fn minted_grant_never_carries_the_raw_conversation_id() {
    let conv = "01KZVDFR5Z9DKJ27YN91S3KS6N";
    let wire = pollis_delivery::room_id::room_pseudonym(LK_SECRET, conv);

    let token =
        sign_livekit_token(LK_KEY, LK_SECRET, &wire, "alice", true, 1_700_000_000).unwrap();
    let (_h, payload, _s) = split_jwt(&token);
    let granted = payload["video"]["room"].as_str().expect("room grant");

    assert_ne!(granted, conv, "the grant must not be the conversation id");
    assert!(!granted.contains(conv), "the grant must not embed the conversation id");
    assert!(granted.starts_with("r-"), "grant should be an opaque pseudonym, got {granted}");
}

/// Two members of one conversation must resolve to the SAME room, or they join
/// different rooms and never see each other's traffic — the failure mode that
/// would make this change silently break messaging rather than loudly break it.
#[test]
fn all_members_of_a_conversation_resolve_to_one_room() {
    let conv = "01KZVDFR5Z9DKJ27YN91S3KS6N";
    let a = pollis_delivery::room_id::room_pseudonym(LK_SECRET, conv);
    let b = pollis_delivery::room_id::room_pseudonym(LK_SECRET, conv);
    assert_eq!(a, b);
}

/// An inbox room must not expose the owner's user id either — otherwise the SFU
/// still learns exactly who is online, which is half of what #828 removes.
#[test]
fn inbox_rooms_do_not_expose_the_user_id() {
    let uid = "01KZT7RW4RXQGT943Q05C7NW8A";
    let wire = pollis_delivery::room_id::room_pseudonym(LK_SECRET, &format!("inbox-{uid}"));
    assert!(!wire.contains(uid), "inbox pseudonym must not embed the user id");
}

// ─── #836: pseudonymous participant identities ───────────────────────────────

use pollis_delivery::participant_id::{
    participant_pseudonym, resolve_participant, ParticipantKind,
};

/// The acceptance criterion, asserted on the **decoded JWT** rather than the
/// helper — the `sub` claim is the thing LiveKit actually reads, so a
/// pseudonym applied anywhere short of it would still leak the account.
#[test]
fn minted_sub_never_carries_the_raw_user_or_device_id() {
    let user = "01KZT7RW4RXQGT943Q05C7NW8A";
    let device = "01KZT7RW4RXQGT943Q05C7NW8B";
    let conv = "01KZVDFR5Z9DKJ27YN91S3KS6N";

    for kind in [ParticipantKind::Realtime, ParticipantKind::Voice, ParticipantKind::View] {
        let identity = participant_pseudonym(LK_SECRET, conv, user, device, kind);
        let wire = pollis_delivery::room_id::room_pseudonym(LK_SECRET, conv);
        let token =
            sign_livekit_token(LK_KEY, LK_SECRET, &wire, &identity, true, 1_700_000_000).unwrap();
        let (_h, payload, _s) = split_jwt(&token);
        let sub = payload["sub"].as_str().expect("sub claim");

        assert!(!sub.contains(user), "sub must not embed the user id: {sub}");
        assert!(!sub.contains(device), "sub must not embed the device id: {sub}");
        assert!(!sub.contains(conv), "sub must not embed the room: {sub}");
        assert!(sub.starts_with(kind.prefix()), "sub should keep its capability prefix: {sub}");
        // And the whole token — header, claims, signature — carries neither id.
        assert!(!token.contains(user));
        assert!(!token.contains(device));
    }
}

/// The residual #836 was filed for: the SFU could cluster the social graph by
/// co-membership because one user carried one identity into every room. Two
/// rooms must now give unrelated identities, and one room's key must not open
/// the other's.
#[test]
fn one_user_is_unlinkable_across_two_rooms() {
    let user = "01KZT7RW4RXQGT943Q05C7NW8A";
    let device = "01KZT7RW4RXQGT943Q05C7NW8B";
    let a = participant_pseudonym(LK_SECRET, "conv-a", user, device, ParticipantKind::Voice);
    let b = participant_pseudonym(LK_SECRET, "conv-b", user, device, ParticipantKind::Voice);

    assert_ne!(a, b, "the same user must not reuse an identity across rooms");
    assert_eq!(resolve_participant(LK_SECRET, "conv-a", &b), None);
    assert_eq!(
        resolve_participant(LK_SECRET, "conv-a", &a).map(|(u, _)| u),
        Some(user.to_string()),
    );
}

/// #140 must survive: a user's two devices stay distinct participants (or the
/// second evicts the first on the SFU) while still resolving to one person.
#[test]
fn two_devices_of_one_user_coexist_and_resolve_to_the_same_user() {
    let user = "01KZT7RW4RXQGT943Q05C7NW8A";
    let room = "01KZVDFR5Z9DKJ27YN91S3KS6N";
    let a = participant_pseudonym(LK_SECRET, room, user, "dev-a", ParticipantKind::Voice);
    let b = participant_pseudonym(LK_SECRET, room, user, "dev-b", ParticipantKind::Voice);

    assert_ne!(a, b);
    assert_eq!(resolve_participant(LK_SECRET, room, &a).unwrap().0, user);
    assert_eq!(resolve_participant(LK_SECRET, room, &b).unwrap().0, user);
}

/// A client cannot obtain a token identifying as another user. The identity is
/// a pure function of the room, the VERIFIED signer and the signer's device —
/// there is no input a caller controls that moves it — so the property is
/// pinned here as "nothing a client sends can produce Bob's identity except
/// being Bob".
#[test]
fn identity_is_determined_by_the_signer_not_the_request() {
    let room = "01KZVDFR5Z9DKJ27YN91S3KS6N";
    let alice = participant_pseudonym(LK_SECRET, room, "alice", "dev-a", ParticipantKind::Voice);
    let bob = participant_pseudonym(LK_SECRET, room, "bob", "dev-a", ParticipantKind::Voice);
    assert_ne!(alice, bob);
    // Alice cannot reach Bob's identity by varying the one thing she supplies
    // (`kind`) — every kind she can ask for still resolves to Alice.
    for kind in [ParticipantKind::Realtime, ParticipantKind::Voice, ParticipantKind::View] {
        let id = participant_pseudonym(LK_SECRET, room, "alice", "dev-a", kind);
        assert_ne!(id, bob);
        assert_eq!(resolve_participant(LK_SECRET, room, &id).unwrap().0, "alice");
    }
}
