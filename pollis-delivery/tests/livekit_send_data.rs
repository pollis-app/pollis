//! `POST /v1/livekit/send-data` target authorization + sender stamping.

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine as _;
use http_body_util::BodyExt as _;
use ml_dsa::{Keypair, MlDsa44, Signer, SigningKey, VerifyingKey};
use pollis_delivery::auth::canonical_message;
use pollis_delivery::broker::BrokerConfig;
use pollis_delivery::db::Db;
use pollis_delivery::{build_router_with_state, AppState};
use rand_core::{OsRng, RngCore as _};
use tower::ServiceExt as _;

mod common;

const PATH: &str = "/v1/livekit/send-data";

fn gen_signing_key() -> SigningKey<MlDsa44> {
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    SigningKey::<MlDsa44>::from_seed(&seed.into())
}

fn pq_pub(vk: &VerifyingKey<MlDsa44>) -> Vec<u8> {
    vk.encode().to_vec()
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

async fn fresh_db() -> common::TempDb {
    let db = common::TempDb::open("delivery.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap())
        .await
        .expect("schema");
    db
}

struct Actor {
    user_id: String,
    device_id: String,
    key: SigningKey<MlDsa44>,
}

async fn seed_actor(db: &Db, user_id: &str) -> Actor {
    let key = gen_signing_key();
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO users (id, email, username) VALUES (?1, ?1 || '@x', ?1 || '-name')",
        libsql::params![user_id],
    )
    .await
    .unwrap();
    let device_id = format!("dev-{user_id}");
    conn.execute(
        "INSERT INTO user_device (device_id, user_id, mls_signature_pub_pq) VALUES (?1, ?2, ?3)",
        libsql::params![device_id.clone(), user_id, pq_pub(&key.verifying_key())],
    )
    .await
    .unwrap();
    Actor {
        user_id: user_id.to_string(),
        device_id,
        key,
    }
}

async fn seed_user(db: &Db, user_id: &str) {
    db.conn()
        .await
        .unwrap()
        .execute(
            "INSERT OR IGNORE INTO users (id, email, username) VALUES (?1, ?1 || '@x', ?1 || '-name')",
            libsql::params![user_id],
        )
        .await
        .unwrap();
}

async fn seed_group(db: &Db, group: &str, members: &[&str]) {
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO conversation (id, kind) VALUES (?1, 'group')",
        libsql::params![group],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO groups (id, name, owner_id) VALUES (?1, ?1 || ' name', ?2)",
        libsql::params![group, members[0]],
    )
    .await
    .unwrap();
    for m in members {
        seed_user(db, m).await;
        conn.execute(
            "INSERT OR IGNORE INTO group_member (group_id, user_id) VALUES (?1, ?2)",
            libsql::params![group, *m],
        )
        .await
        .unwrap();
    }
}

/// `b_accepted`: whether `b` accepted `a`'s DM request yet.
async fn seed_dm(db: &Db, dm: &str, a: &str, b: &str, b_accepted: bool) {
    let conn = db.conn().await.unwrap();
    seed_user(db, a).await;
    seed_user(db, b).await;
    conn.execute(
        "INSERT OR IGNORE INTO conversation (id, kind) VALUES (?1, 'dm')",
        libsql::params![dm],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO dm_channel (id, created_by) VALUES (?1, ?2)",
        libsql::params![dm, a],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO dm_channel_member (dm_channel_id, user_id, added_by, accepted_at) \
         VALUES (?1, ?2, ?2, datetime('now'))",
        libsql::params![dm, a],
    )
    .await
    .unwrap();
    let accepted: Option<String> = b_accepted.then(|| "2026-01-01T00:00:00Z".to_string());
    conn.execute(
        "INSERT OR IGNORE INTO dm_channel_member (dm_channel_id, user_id, added_by, accepted_at) \
         VALUES (?1, ?2, ?3, ?4)",
        libsql::params![dm, b, a, accepted],
    )
    .await
    .unwrap();
}

async fn seed_block(db: &Db, blocker: &str, blocked: &str) {
    db.conn()
        .await
        .unwrap()
        .execute(
            "INSERT OR IGNORE INTO user_block (blocker_id, blocked_id) VALUES (?1, ?2)",
            libsql::params![blocker, blocked],
        )
        .await
        .unwrap();
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn signed_send(actor: &Actor, room: &str, payload: serde_json::Value) -> Request<Body> {
    let body = serde_json::to_vec(&serde_json::json!({ "room": room, "payload": payload })).unwrap();
    let ts = now_ts();
    let msg = canonical_message("POST", PATH, ts, &body);
    let sig = b64(&actor.key.sign(&msg).encode());
    Request::builder()
        .method("POST")
        .uri(PATH)
        .header("content-type", "application/json")
        .header("X-Pollis-User", actor.user_id.as_str())
        .header("X-Pollis-Device", actor.device_id.as_str())
        .header("X-Pollis-Timestamp", ts.to_string())
        .header("X-Pollis-Signature", sig)
        .body(Body::from(body))
        .unwrap()
}

/// A stand-in LiveKit recording every `SendData` body; returns its `ws://` URL.
async fn fake_livekit() -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let app = Router::new().route(
        "/twirp/livekit.RoomService/SendData",
        post(move |Json(body): Json<serde_json::Value>| {
            let sink = Arc::clone(&sink);
            async move {
                sink.lock().unwrap().push(body);
                Json(serde_json::json!({}))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("ws://{addr}"), seen)
}

fn livekit_broker(url: &str) -> BrokerConfig {
    BrokerConfig {
        livekit_api_key: Some("APIkey".into()),
        livekit_api_secret: Some("secret-secret-secret-secret".into()),
        livekit_url: Some(url.to_string()),
        ..BrokerConfig::default()
    }
}

fn broadcast_payload(recorded: &serde_json::Value) -> serde_json::Value {
    let raw = base64::engine::general_purpose::STANDARD
        .decode(recorded["data"].as_str().expect("data field"))
        .expect("base64 data");
    serde_json::from_slice(&raw).expect("payload json")
}

struct Harness {
    _db: common::TempDb,
    router: Router,
    seen: Arc<Mutex<Vec<serde_json::Value>>>,
}

async fn harness(db: common::TempDb) -> Harness {
    let (url, seen) = fake_livekit().await;
    let state = AppState::new(db.arc(), true).with_broker_config(livekit_broker(&url));
    Harness {
        _db: db,
        router: build_router_with_state(state),
        seen,
    }
}

async fn send(h: &Harness, req: Request<Body>) -> StatusCode {
    let resp = h.router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let _ = resp.into_body().collect().await;
    status
}

fn call_invite_forged(as_whom: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "call_invite",
        "call_id": "c1",
        "room_name": "call-c1",
        "caller_id": as_whom,
        "caller_username": "The CEO",
    })
}

/// A stranger cannot push into a victim's inbox (spoofed ring / prompt spam).
#[tokio::test]
async fn a_stranger_cannot_reach_another_users_inbox() {
    let db = fresh_db().await;
    let attacker = seed_actor(&db, "mallory").await;
    seed_user(&db, "victim").await;
    let h = harness(db).await;

    let status = send(&h, signed_send(&attacker, "inbox-victim", call_invite_forged("ceo"))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let status = send(
        &h,
        signed_send(
            &attacker,
            "inbox-victim",
            serde_json::json!({ "type": "new_message", "conversation_id": "any" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(h.seen.lock().unwrap().is_empty(), "a refused send must never reach LiveKit");
}

/// A block closes the inbox in both directions.
#[tokio::test]
async fn a_blocked_dm_peer_cannot_reach_the_inbox() {
    let db = fresh_db().await;
    let attacker = seed_actor(&db, "mallory").await;
    seed_dm(&db, "dm-1", "mallory", "victim", true).await;
    seed_block(&db, "victim", "mallory").await;
    let h = harness(db).await;

    let status = send(&h, signed_send(&attacker, "inbox-victim", call_invite_forged("mallory"))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(h.seen.lock().unwrap().is_empty());
}

/// An unaccepted DM request allows `dm_created` but not `call_invite`.
#[tokio::test]
async fn a_pending_dm_request_cannot_ring_but_can_badge() {
    let db = fresh_db().await;
    let requester = seed_actor(&db, "alice").await;
    seed_dm(&db, "dm-1", "alice", "bob", false).await;
    let h = harness(db).await;

    let status = send(&h, signed_send(&requester, "inbox-bob", call_invite_forged("alice"))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "unaccepted DM request must not ring");
    assert!(h.seen.lock().unwrap().is_empty());

    let status = send(
        &h,
        signed_send(
            &requester,
            "inbox-bob",
            serde_json::json!({ "type": "dm_created", "conversation_id": "dm-1" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "the DM-request badge ping is what pending is for");
    let seen = h.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let payload = broadcast_payload(&seen[0]);
    assert_eq!(payload["sender_id"], "alice");
    assert_eq!(payload["sender_username"], "alice-name");
}

#[tokio::test]
async fn a_non_member_cannot_push_into_a_conversation_room() {
    let db = fresh_db().await;
    let outsider = seed_actor(&db, "mallory").await;
    seed_group(&db, "g-1", &["alice", "bob"]).await;
    let h = harness(db).await;

    let status = send(
        &h,
        signed_send(
            &outsider,
            "g-1",
            serde_json::json!({ "type": "membership_changed", "group_id": "g-1" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(h.seen.lock().unwrap().is_empty());
}

/// Only the DS emits `enrollment_requested`; a client sending it is a prompt takeover.
#[tokio::test]
async fn enrollment_requested_is_never_client_publishable() {
    let db = fresh_db().await;
    let peer = seed_actor(&db, "alice").await;
    seed_dm(&db, "dm-1", "alice", "bob", true).await;
    let h = harness(db).await;

    let forged = serde_json::json!({
        "type": "enrollment_requested",
        "request_id": "r1",
        "new_device_id": "evil-device",
        "verification_code": "123456",
    });
    for room in ["inbox-bob", "inbox-alice", "dm-1"] {
        let status = send(&h, signed_send(&peer, room, forged.clone())).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "room {room}");
    }
    let status = send(
        &h,
        signed_send(&peer, "inbox-alice", serde_json::json!({ "type": "made_up" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(h.seen.lock().unwrap().is_empty());
}

/// Broadcasts name the verified caller; the forged caller fields are gone.
#[tokio::test]
async fn a_dm_peer_can_ring_and_the_stamped_caller_is_the_signer() {
    let db = fresh_db().await;
    let caller = seed_actor(&db, "alice").await;
    seed_dm(&db, "dm-1", "alice", "bob", true).await;
    let h = harness(db).await;

    let status = send(&h, signed_send(&caller, "inbox-bob", call_invite_forged("ceo"))).await;
    assert_eq!(status, StatusCode::OK);

    let seen = h.seen.lock().unwrap();
    assert_eq!(seen.len(), 1, "exactly one SendData reached LiveKit");
    let payload = broadcast_payload(&seen[0]);
    assert_eq!(payload["type"], "call_invite");
    assert_eq!(payload["call_id"], "c1");
    assert_eq!(payload["room_name"], "call-c1");
    assert_eq!(payload["sender_id"], "alice");
    assert_eq!(payload["sender_username"], "alice-name");
    assert_eq!(payload["caller_id"], "alice");
    assert_eq!(payload["caller_username"], "alice-name");
    let text = payload.to_string();
    assert!(!text.contains("ceo"), "forged caller_id survived: {text}");
    assert!(!text.contains("The CEO"), "forged caller_username survived: {text}");
    // The room LiveKit sees is the pseudonym, never the logical inbox name.
    assert_ne!(seen[0]["room"], "inbox-bob");
}

/// A group invite's inviter and group name are resolved server-side.
#[tokio::test]
async fn a_group_invite_ping_names_the_real_inviter_and_group() {
    let db = fresh_db().await;
    let inviter = seed_actor(&db, "alice").await;
    seed_group(&db, "g-1", &["alice"]).await;
    seed_user(&db, "carol").await;
    db.conn()
        .await
        .unwrap()
        .execute(
            "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('i1', 'g-1', 'alice', 'carol')",
            (),
        )
        .await
        .unwrap();
    let h = harness(db).await;

    let status = send(
        &h,
        signed_send(
            &inviter,
            "inbox-carol",
            serde_json::json!({
                "type": "membership_changed",
                "group_id": "g-1",
                "kind": "invite",
                "inviter_username": "Trusted Admin",
                "group_name": "Payroll (official)",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "a pending invite opens the invitee's inbox");

    let seen = h.seen.lock().unwrap();
    let payload = broadcast_payload(&seen[0]);
    assert_eq!(payload["kind"], "invite");
    assert_eq!(payload["sender_id"], "alice");
    assert_eq!(payload["inviter_username"], "alice-name");
    assert_eq!(payload["group_name"], "g-1 name");
    let text = payload.to_string();
    assert!(!text.contains("Trusted Admin"), "{text}");
    assert!(!text.contains("Payroll"), "{text}");
}

/// A shared-room broadcast stays routing-only (§5): identity fields stripped.
#[tokio::test]
async fn a_member_room_nudge_carries_no_identity_at_all() {
    let db = fresh_db().await;
    let member = seed_actor(&db, "alice").await;
    seed_group(&db, "g-1", &["alice", "bob"]).await;
    let h = harness(db).await;

    let status = send(
        &h,
        signed_send(
            &member,
            "g-1",
            serde_json::json!({
                "type": "membership_changed",
                "group_id": "g-1",
                "inviter_username": "smuggled",
                "sender_id": "smuggled",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let seen = h.seen.lock().unwrap();
    let payload = broadcast_payload(&seen[0]);
    assert_eq!(payload["type"], "membership_changed");
    assert_eq!(payload["group_id"], "g-1");
    let obj = payload.as_object().unwrap();
    for key in [
        "sender_id",
        "sender_username",
        "inviter_username",
        "caller_id",
        "caller_username",
        "user_id",
        "username",
        "group_name",
    ] {
        assert!(!obj.contains_key(key), "shared-room broadcast leaked `{key}`: {payload}");
    }
}

/// Your own inbox needs no relationship with anyone.
#[tokio::test]
async fn own_inbox_is_always_reachable() {
    let db = fresh_db().await;
    let me = seed_actor(&db, "alice").await;
    let h = harness(db).await;

    let status = send(
        &h,
        signed_send(&me, "inbox-alice", serde_json::json!({ "type": "device_revoked" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let seen = h.seen.lock().unwrap();
    let payload = broadcast_payload(&seen[0]);
    assert_eq!(payload, serde_json::json!({ "type": "device_revoked" }));
}

// #1337: an empty room is "nothing to notify", not a 502

async fn livekit_answering(status: StatusCode, body: &'static str) -> String {
    let app = Router::new().route(
        "/twirp/livekit.RoomService/SendData",
        post(move || async move {
            (
                status,
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                body,
            )
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("ws://{addr}")
}

async fn nudge_against(status: StatusCode, body: &'static str) -> StatusCode {
    let db = fresh_db().await;
    let member = seed_actor(&db, "alice").await;
    seed_group(&db, "g-1", &["alice", "bob"]).await;
    let url = livekit_answering(status, body).await;
    let state = AppState::new(db.arc(), true).with_broker_config(livekit_broker(&url));
    let router = build_router_with_state(state);
    let req = signed_send(
        &member,
        "g-1",
        serde_json::json!({ "type": "new_message", "conversation_id": "g-1" }),
    );
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    let _ = resp.into_body().collect().await;
    drop(db);
    status
}

/// LiveKit's answer for a room nobody has joined.
const NO_ROOM: &str = r#"{"code":"unavailable","msg":"no response from servers"}"#;

#[tokio::test]
async fn an_empty_room_is_nothing_to_notify_not_a_bad_gateway() {
    assert_eq!(
        nudge_against(StatusCode::SERVICE_UNAVAILABLE, NO_ROOM).await,
        StatusCode::OK
    );
}

/// Only that exact answer is forgiven; a real LiveKit failure still reads as one.
#[tokio::test]
async fn a_real_livekit_failure_is_still_a_bad_gateway() {
    let draining = r#"{"code":"unavailable","msg":"node is draining"}"#;
    assert_eq!(
        nudge_against(StatusCode::SERVICE_UNAVAILABLE, draining).await,
        StatusCode::BAD_GATEWAY
    );
    let internal = r#"{"code":"internal","msg":"no response from servers"}"#;
    assert_eq!(
        nudge_against(StatusCode::INTERNAL_SERVER_ERROR, internal).await,
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        nudge_against(StatusCode::SERVICE_UNAVAILABLE, "upstream connect error").await,
        StatusCode::BAD_GATEWAY
    );
}

#[test]
fn the_empty_room_classifier_is_narrow() {
    use pollis_delivery::broker::send_data_room_is_empty;

    assert!(send_data_room_is_empty(StatusCode::SERVICE_UNAVAILABLE, NO_ROOM));
    assert!(!send_data_room_is_empty(StatusCode::BAD_GATEWAY, NO_ROOM));
    assert!(!send_data_room_is_empty(StatusCode::INTERNAL_SERVER_ERROR, NO_ROOM));
    assert!(!send_data_room_is_empty(
        StatusCode::SERVICE_UNAVAILABLE,
        r#"{"code":"unavailable","msg":"node is draining"}"#
    ));
    assert!(!send_data_room_is_empty(
        StatusCode::SERVICE_UNAVAILABLE,
        r#"{"code":"internal","msg":"no response from servers"}"#
    ));
    assert!(!send_data_room_is_empty(StatusCode::SERVICE_UNAVAILABLE, ""));
    assert!(!send_data_room_is_empty(
        StatusCode::SERVICE_UNAVAILABLE,
        "no response from servers"
    ));
}
