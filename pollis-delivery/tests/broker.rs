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

// #1845: the DS signs JWTs itself (hmac + sha2) instead of via `jsonwebtoken`.
// These are the exact tokens `jsonwebtoken` 10.3 minted for the same inputs, so
// any drift in header bytes, claim order, JSON escaping or base64 shows up here.
const GOLDEN_PARTICIPANT: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJpc3MiOiJBUElleGFtcGxlS2V5Iiwic3ViIjoiYWxpY2UiLCJpYXQiOjE3MDAwMDAwMDAsIm5iZiI6MTcwMDAwMDAwMCwiZXhwIjoxNzAwMDAwOTAwLCJuYW1lIjoiIiwidmlkZW8iOnsicm9vbSI6InJvb20tMSIsInJvb21Kb2luIjp0cnVlLCJjYW5QdWJsaXNoIjp0cnVlLCJjYW5TdWJzY3JpYmUiOnRydWUsImNhblB1Ymxpc2hEYXRhIjp0cnVlfX0.tlPm2YKjgX_TI_Qa6tZu21xAtqI0fqX2YvZ_cNoUfPs";
const GOLDEN_VIEW: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJpc3MiOiJBUElleGFtcGxlS2V5Iiwic3ViIjoiYWxpY2U6dmlldyIsImlhdCI6MTcwMDAwMDAwMCwibmJmIjoxNzAwMDAwMDAwLCJleHAiOjE3MDAwMDA5MDAsIm5hbWUiOiIiLCJ2aWRlbyI6eyJyb29tIjoicm9vbS0xIiwicm9vbUpvaW4iOnRydWUsImNhblB1Ymxpc2giOnRydWUsImNhblN1YnNjcmliZSI6dHJ1ZSwiY2FuUHVibGlzaERhdGEiOmZhbHNlfX0.LGSitJocmDnHiNCqjK_pfDoCRSUvnpZirIX8NnO0FPw";
const GOLDEN_ESCAPED_ROOM: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJpc3MiOiJBUElleGFtcGxlS2V5Iiwic3ViIjoieCIsImlhdCI6MTcwMDAwMDAwMCwibmJmIjoxNzAwMDAwMDAwLCJleHAiOjE3MDAwMDA5MDAsIm5hbWUiOiIiLCJ2aWRlbyI6eyJyb29tIjoiZG0tXCJxXCJcXMOpLzw-JiIsInJvb21Kb2luIjp0cnVlLCJjYW5QdWJsaXNoIjp0cnVlLCJjYW5TdWJzY3JpYmUiOnRydWUsImNhblB1Ymxpc2hEYXRhIjp0cnVlfX0.gdhkCvoqvBSPSVke4At388QtAdBIqgRNR2JWDT_5-r0";
const GOLDEN_ADMIN: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJpc3MiOiJBUElleGFtcGxlS2V5Iiwic3ViIjoicG9sbGlzLWRzIiwiaWF0IjoxNzAwMDAwMDAwLCJuYmYiOjE3MDAwMDAwMDAsImV4cCI6MTcwMDAwMDMwMCwidmlkZW8iOnsicm9vbUFkbWluIjp0cnVlLCJyb29tTGlzdCI6dHJ1ZSwicm9vbSI6ImluYm94LXVfMTIzIn19.sdBn6YplkAJTvoeIfMV42gblkjjOHBToqgP4vNo2VkM";
const GOLDEN_ADMIN_LIST: &str = "eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJpc3MiOiJBUElleGFtcGxlS2V5Iiwic3ViIjoicG9sbGlzLWRzIiwiaWF0IjoxNzAwMDAwMDAwLCJuYmYiOjE3MDAwMDAwMDAsImV4cCI6MTcwMDAwMDMwMCwidmlkZW8iOnsicm9vbUFkbWluIjp0cnVlLCJyb29tTGlzdCI6dHJ1ZSwicm9vbSI6IiJ9fQ.gUxDesquDbBScicD7YlnUzeRh5zlqVeFr6cgNe3sfgE";

#[test]
fn livekit_tokens_are_byte_identical_to_the_jsonwebtoken_output() {
    let now = 1_700_000_000u64;
    let mint = |room: &str, identity: &str, data: bool| {
        sign_livekit_token(LK_KEY, LK_SECRET, room, identity, data, now).unwrap()
    };
    assert_eq!(mint("room-1", "alice", true), GOLDEN_PARTICIPANT);
    assert_eq!(mint("room-1", "alice:view", false), GOLDEN_VIEW);
    assert_eq!(mint("dm-\"q\"\\\u{e9}/<>&", "x", true), GOLDEN_ESCAPED_ROOM);
    assert_eq!(
        sign_livekit_admin_token(LK_KEY, LK_SECRET, "inbox-u_123", now).unwrap(),
        GOLDEN_ADMIN
    );
    assert_eq!(
        sign_livekit_admin_token(LK_KEY, LK_SECRET, "", now).unwrap(),
        GOLDEN_ADMIN_LIST
    );
}

/// Sign, then verify and decode the way a LiveKit server does: HS256 over the
/// first two segments with the shared secret, and the claims read back intact.
#[test]
fn livekit_token_round_trips_through_an_hs256_verifier() {
    let now = 1_700_000_000u64;
    let room = "dm-\"q\"\\\u{e9}/<>&";
    let token = sign_livekit_token(LK_KEY, LK_SECRET, room, "x", true, now).unwrap();
    assert_hs256_signature(&token, LK_SECRET);
    let (header, payload, signature) = split_jwt(&token);
    assert_eq!(header, serde_json::json!({ "typ": "JWT", "alg": "HS256" }));
    assert_eq!(payload["video"]["room"], room);
    assert_eq!(payload["sub"], "x");

    // A different secret must not verify.
    let last_dot = token.rfind('.').unwrap();
    let mut mac = Hmac::<Sha256>::new_from_slice(b"another-secret").unwrap();
    mac.update(&token.as_bytes()[..last_dot]);
    let wrong = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(mac.finalize().into_bytes());
    assert_ne!(signature, wrong);
}

/// Live round trip against a real LiveKit server: the participant token passes
/// `/rtc/validate` and the admin token is accepted by `RoomService/ListRooms`,
/// while one signed under the wrong secret is refused. Needs `LIVEKIT_URL` /
/// `LIVEKIT_API_KEY` / `LIVEKIT_API_SECRET` (e.g. a `livekit-server --dev`), so
/// it is ignored by default:
/// `cargo test -p pollis-delivery --test broker -- --ignored live_livekit`.
#[tokio::test]
#[ignore = "needs a reachable LiveKit server and its key/secret in env"]
async fn live_livekit_accepts_minted_tokens() {
    let url = std::env::var("LIVEKIT_URL").expect("LIVEKIT_URL");
    let key = std::env::var("LIVEKIT_API_KEY").expect("LIVEKIT_API_KEY");
    let secret = std::env::var("LIVEKIT_API_SECRET").expect("LIVEKIT_API_SECRET");
    let base = url
        .trim_end_matches('/')
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let http = reqwest::Client::new();
    let list_rooms = |token: String| {
        http.post(format!("{base}/twirp/livekit.RoomService/ListRooms"))
            .bearer_auth(token)
            .header("content-type", "application/json")
            .body("{}")
            .send()
    };

    let participant =
        sign_livekit_token(&key, &secret, "pollis-roundtrip", "roundtrip", false, now).unwrap();
    let resp = http
        .get(format!("{base}/rtc/validate"))
        .query(&[("access_token", participant.as_str())])
        .send()
        .await
        .expect("validate request");
    assert!(resp.status().is_success(), "participant token rejected: {}", resp.status());

    let admin = sign_livekit_admin_token(&key, &secret, "", now).unwrap();
    let resp = list_rooms(admin).await.expect("ListRooms request");
    assert!(resp.status().is_success(), "admin token rejected: {}", resp.status());

    let forged = sign_livekit_admin_token(&key, "not-the-secret", "", now).unwrap();
    let resp = list_rooms(forged).await.expect("ListRooms request");
    assert_eq!(resp.status().as_u16(), 401, "a token under the wrong secret must be refused");
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
