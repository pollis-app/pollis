//! Known-answer tests for the broker's pure signing functions (#393), with an injected clock.

use base64::Engine as _;
use hmac::{Hmac, Mac};
use pollis_delivery::broker::{
    presign_r2_url, sign_livekit_admin_token, sign_livekit_token, LIVEKIT_TOKEN_TTL_SECS,
};
use sha2::Sha256;

const LK_KEY: &str = "APIexampleKey";
const LK_SECRET: &str = "livekit-signing-secret-do-not-log";

fn b64url_decode(s: &str) -> Vec<u8> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(s)
        .expect("valid base64url")
}

fn split_jwt(token: &str) -> (serde_json::Value, serde_json::Value, String) {
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(parts.len(), 3, "JWT has three dot-separated parts");
    let header: serde_json::Value =
        serde_json::from_slice(&b64url_decode(parts[0])).expect("header json");
    let payload: serde_json::Value =
        serde_json::from_slice(&b64url_decode(parts[1])).expect("payload json");
    (header, payload, parts[2].to_string())
}

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

    assert_eq!(payload["iss"], LK_KEY);
    assert_eq!(payload["sub"], "alice");
    // #836: no display name; the username would name the account to the SFU.
    assert_eq!(payload["name"], "");
    assert_eq!(payload["iat"], now);
    assert_eq!(payload["nbf"], now);
    assert_eq!(payload["exp"], now + LIVEKIT_TOKEN_TTL_SECS);

    let v = &payload["video"];
    assert_eq!(v["room"], "room-1");
    assert_eq!(v["roomJoin"], true);
    assert_eq!(v["canPublish"], true);
    assert_eq!(v["canSubscribe"], true);
    assert_eq!(v["canPublishData"], true);

    assert_hs256_signature(&token, LK_SECRET);
}

/// Membership is checked only at mint, so the TTL bounds re-entry after removal.
#[test]
fn a_participant_token_expires_in_minutes_not_hours() {
    let ttl = LIVEKIT_TOKEN_TTL_SECS;
    assert!(
        ttl <= 15 * 60,
        "a {LIVEKIT_TOKEN_TTL_SECS}s participant token leaves that long a window \
         for an ex-member to rejoin the room with a token minted before they lost access"
    );
    assert!(
        ttl >= 5 * 60,
        "a {LIVEKIT_TOKEN_TTL_SECS}s token is short enough that an ordinary \
         reconnect races its own expiry"
    );
    let now = 1_700_000_000u64;
    let token = sign_livekit_token(LK_KEY, LK_SECRET, "room-1", "alice", true, now).unwrap();
    let (_, payload, _) = split_jwt(&token);
    assert_eq!(
        payload["exp"], now + LIVEKIT_TOKEN_TTL_SECS,
        "the minted claim must carry the declared TTL, not a hardcoded one"
    );
}

#[test]
fn livekit_view_variant_disables_publish_data() {
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
    assert_eq!(payload["video"]["roomJoin"], true);
    assert_eq!(payload["video"]["canPublish"], true);
    assert_eq!(payload["video"]["canSubscribe"], true);
    assert_hs256_signature(&token, LK_SECRET);
}

#[test]
fn livekit_admin_token_grants_room_admin_and_verifies() {
    // `sub` is the internal DS identity, filtered out of rosters.
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
    assert!(payload["video"].get("roomJoin").is_none());
    assert_hs256_signature(&token, LK_SECRET);
}

// R2 SigV4 known-answer: path-style, so the signature is not AWS's documented one.

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
    // Locks URI-encoding of the canonical path: slash kept, space as %20.
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
    // The method is part of the canonical request: DELETE must sign differently from GET.
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
    assert_eq!(
        del.split_once("&X-Amz-Signature=").unwrap().0,
        get.split_once("&X-Amz-Signature=").unwrap().0,
    );
}

#[test]
fn presign_canonical_query_is_sorted() {
    // SigV4 sorts query params; X-Amz-Signature is appended after signing.
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
    assert!(query.split('&').next_back().unwrap().starts_with("X-Amz-Signature="));
}

/// Asserted on the decoded JWT: the grant is what LiveKit reads.
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

use pollis_delivery::participant_id::{
    participant_pseudonym, resolve_participant, ParticipantKind,
};

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
        assert!(!token.contains(user));
        assert!(!token.contains(device));
    }
}

/// The identity derives from room, verified signer and device: no client input moves it.
#[test]
fn identity_is_determined_by_the_signer_not_the_request() {
    let room = "01KZVDFR5Z9DKJ27YN91S3KS6N";
    let alice = participant_pseudonym(LK_SECRET, room, "alice", "dev-a", ParticipantKind::Voice);
    let bob = participant_pseudonym(LK_SECRET, room, "bob", "dev-a", ParticipantKind::Voice);
    assert_ne!(alice, bob);
    for kind in [ParticipantKind::Realtime, ParticipantKind::Voice, ParticipantKind::View] {
        let id = participant_pseudonym(LK_SECRET, room, "alice", "dev-a", kind);
        assert_ne!(id, bob);
        assert_eq!(resolve_participant(LK_SECRET, room, &id).unwrap().0, "alice");
    }
}
