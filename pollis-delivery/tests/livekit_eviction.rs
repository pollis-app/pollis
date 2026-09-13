//! Losing access ends the LiveKit session you already hold.
//!
//! A LiveKit token is verified ONCE, at join. Nothing that happens afterwards
//! reaches the SFU on its own, so a member who was removed, left, was blocked,
//! or whose device was revoked kept their realtime and voice connections —
//! typing indicators, presence, control nudges, the voice channel itself — until
//! they chose to disconnect. Refusing them the NEXT token (which the broker
//! already did) does nothing about the one they are using.
//!
//! These tests drive the real router with device-signed requests against a local
//! libsql DB and a fake LiveKit Twirp endpoint that records every
//! `RemoveParticipant` it is handed, so the eviction is asserted where it
//! actually has to happen — on the wire to the SFU — and per identity, since the
//! identity is a per-`(room, user, device, kind)` pseudonym and missing one of
//! them leaves that session live.

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
use pollis_delivery::participant_id::{participant_pseudonym, ParticipantKind};
use pollis_delivery::{build_router_with_state, AppState};
use rand_core::{OsRng, RngCore as _};
use tower::ServiceExt as _;

mod common;

const LK_SECRET: &str = "secret-secret-secret-secret";

// ── Fixtures ─────────────────────────────────────────────────────────────────

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

/// A user with a live device whose key signs this test's requests.
struct Actor {
    user_id: String,
    device_id: String,
    key: SigningKey<MlDsa44>,
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

/// Register one more device on an existing account, without a signing key of its
/// own — enough to be enumerated by the eviction.
async fn seed_device(db: &Db, user_id: &str, device_id: &str) {
    let key = gen_signing_key();
    db.conn()
        .await
        .unwrap()
        .execute(
            "INSERT INTO user_device (device_id, user_id, mls_signature_pub_pq) VALUES (?1, ?2, ?3)",
            libsql::params![device_id, user_id, pq_pub(&key.verifying_key())],
        )
        .await
        .unwrap();
}

async fn seed_actor(db: &Db, user_id: &str) -> Actor {
    let key = gen_signing_key();
    seed_user(db, user_id).await;
    let device_id = format!("dev-{user_id}");
    db.conn()
        .await
        .unwrap()
        .execute(
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

/// A group with one text channel and the given members, all admins.
async fn seed_group(db: &Db, group: &str, channel: &str, members: &[&str]) {
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
    conn.execute(
        "INSERT OR IGNORE INTO conversation (id, kind) VALUES (?1, 'channel')",
        libsql::params![channel],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO channels (id, group_id, name) VALUES (?1, ?2, 'general')",
        libsql::params![channel, group],
    )
    .await
    .unwrap();
    for m in members {
        seed_user(db, m).await;
        conn.execute(
            "INSERT OR IGNORE INTO group_member (group_id, user_id, role) VALUES (?1, ?2, 'admin')",
            libsql::params![group, *m],
        )
        .await
        .unwrap();
    }
}

async fn seed_dm(db: &Db, dm: &str, a: &str, b: &str) {
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
    for m in [a, b] {
        conn.execute(
            "INSERT OR IGNORE INTO dm_channel_member (dm_channel_id, user_id, added_by, accepted_at) \
             VALUES (?1, ?2, ?3, datetime('now'))",
            libsql::params![dm, m, a],
        )
        .await
        .unwrap();
    }
}

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn signed(actor: &Actor, path: &str, body: serde_json::Value) -> Request<Body> {
    let body = serde_json::to_vec(&body).unwrap();
    let ts = now_ts();
    let msg = canonical_message("POST", path, ts, &body);
    let sig = b64(&actor.key.sign(&msg).encode());
    Request::builder()
        .method("POST")
        .uri(path.to_string())
        .header("content-type", "application/json")
        .header("X-Pollis-User", actor.user_id.as_str())
        .header("X-Pollis-Device", actor.device_id.as_str())
        .header("X-Pollis-Timestamp", ts.to_string())
        .header("X-Pollis-Signature", sig)
        .body(Body::from(body))
        .unwrap()
}

/// A stand-in LiveKit Twirp server recording every `RemoveParticipant` body.
async fn fake_livekit() -> (String, Arc<Mutex<Vec<serde_json::Value>>>) {
    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let app = Router::new().route(
        "/twirp/livekit.RoomService/RemoveParticipant",
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
        livekit_api_secret: Some(LK_SECRET.into()),
        livekit_url: Some(url.to_string()),
        ..BrokerConfig::default()
    }
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

/// Was `(room, user, device, kind)` kicked? Both halves of the wire pair are
/// derived here exactly as the DS derives them, so the assertion pins the real
/// pseudonymous addressing rather than a stringly-typed stand-in.
fn kicked(
    seen: &Arc<Mutex<Vec<serde_json::Value>>>,
    room: &str,
    user: &str,
    device: &str,
    kind: ParticipantKind,
) -> bool {
    let wire_room = pollis_delivery::room_id::room_pseudonym(LK_SECRET, room);
    let identity = participant_pseudonym(LK_SECRET, room, user, device, kind);
    seen.lock().unwrap().iter().any(|r| {
        r["room"] == serde_json::json!(wire_room) && r["identity"] == serde_json::json!(identity)
    })
}

// ── Removal ──────────────────────────────────────────────────────────────────

/// The defect: `POST /v1/members/remove` deleted the membership row and told
/// LiveKit nothing at all, so the ex-member's open connection outlived it.
///
/// Every device and every capability has to go, in the group room AND in the
/// group's channels (a voice participant joins the CHANNEL as the room), or the
/// eviction is only partial — which for the one that is missed is no eviction.
#[tokio::test(flavor = "multi_thread")]
async fn removing_a_member_kicks_all_their_identities_from_every_room_of_the_group() {
    let db = fresh_db().await;
    let admin = seed_actor(&db, "alice").await;
    let bob = seed_actor(&db, "bob").await;
    seed_device(&db, "bob", "bob-phone").await;
    seed_group(&db, "grp-1", "chan-1", &["alice", "bob"]).await;
    let h = harness(db).await;

    let status = send(
        &h,
        signed(
            &admin,
            "/v1/members/remove",
            serde_json::json!({ "group_id": "grp-1", "user_id": "bob" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    for room in ["grp-1", "chan-1"] {
        for device in [bob.device_id.as_str(), "bob-phone", ""] {
            for kind in [
                ParticipantKind::Realtime,
                ParticipantKind::Voice,
                ParticipantKind::View,
            ] {
                assert!(
                    kicked(&h.seen, room, "bob", device, kind),
                    "removed member must be evicted from {room} as {kind:?} on device {device:?}"
                );
            }
        }
    }
    // The removal is bob's alone: kicking the admin out of the room they still
    // belong to would be a self-inflicted outage.
    assert!(
        !kicked(
            &h.seen,
            "grp-1",
            "alice",
            &admin.device_id,
            ParticipantKind::Realtime
        ),
        "a remaining member must keep their session"
    );
}

/// A refused removal must not evict either — otherwise anyone who can reach the
/// endpoint can knock a member offline without being able to remove them.
#[tokio::test(flavor = "multi_thread")]
async fn a_forbidden_removal_evicts_nobody() {
    let db = fresh_db().await;
    let outsider = seed_actor(&db, "mallory").await;
    seed_actor(&db, "bob").await;
    seed_group(&db, "grp-1", "chan-1", &["alice", "bob"]).await;
    let h = harness(db).await;

    let status = send(
        &h,
        signed(
            &outsider,
            "/v1/members/remove",
            serde_json::json!({ "group_id": "grp-1", "user_id": "bob" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        h.seen.lock().unwrap().is_empty(),
        "a rejected removal must reach LiveKit not at all"
    );
}

/// Leaving is a removal the member performs themselves, and a stale or hostile
/// client that stops short of disconnecting keeps the session unless the DS
/// closes it.
#[tokio::test(flavor = "multi_thread")]
async fn leaving_a_group_kicks_the_leaver_out_of_its_rooms() {
    let db = fresh_db().await;
    let bob = seed_actor(&db, "bob").await;
    seed_actor(&db, "alice").await;
    seed_group(&db, "grp-1", "chan-1", &["alice", "bob"]).await;
    let h = harness(db).await;

    let status = send(
        &h,
        signed(&bob, "/v1/groups/leave", serde_json::json!({ "group_id": "grp-1" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    for room in ["grp-1", "chan-1"] {
        assert!(
            kicked(&h.seen, room, "bob", &bob.device_id, ParticipantKind::Realtime),
            "the leaver must be evicted from {room}"
        );
    }
}

// ── Blocks ───────────────────────────────────────────────────────────────────

/// Blocking does not delete the DM's membership rows, so the eviction has to be
/// paired with a token refusal — otherwise the blocked client reconnects within
/// its backoff and is let straight back in, and the kick was theatre.
#[tokio::test(flavor = "multi_thread")]
async fn blocking_kicks_the_blocked_peer_and_the_token_endpoint_keeps_them_out() {
    let db = fresh_db().await;
    let alice = seed_actor(&db, "alice").await;
    let bob = seed_actor(&db, "bob").await;
    seed_dm(&db, "dm-1", "alice", "bob").await;
    let h = harness(db).await;

    let status = send(
        &h,
        signed(
            &alice,
            "/v1/blocks/add",
            serde_json::json!({ "blocker_id": "alice", "blocked_id": "bob" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    assert!(
        kicked(&h.seen, "dm-1", "bob", &bob.device_id, ParticipantKind::Realtime),
        "the blocked peer must be evicted from the shared DM room"
    );
    assert!(
        !kicked(&h.seen, "dm-1", "alice", &alice.device_id, ParticipantKind::Realtime),
        "the blocker keeps their own session"
    );

    // And cannot come back.
    let status = send(
        &h,
        signed(
            &bob,
            "/v1/livekit/token",
            serde_json::json!({ "room": "dm-1", "kind": "realtime" }),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a blocked peer must not be able to re-mint a token for the DM"
    );

    // The blocker's own access to the same room is untouched — the block is
    // one-directional, and so is the refusal.
    let status = send(
        &h,
        signed(
            &alice,
            "/v1/livekit/token",
            serde_json::json!({ "room": "dm-1", "kind": "realtime" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

// ── Device revocation ────────────────────────────────────────────────────────

/// Revoking a device stops it signing new requests — including new token
/// requests — but says nothing to the SFU about the connection it is holding.
/// The account's OTHER devices must be left alone: this is a device eviction,
/// not an account one.
#[tokio::test(flavor = "multi_thread")]
async fn revoking_a_device_kicks_that_device_out_of_every_room_and_no_other() {
    let db = fresh_db().await;
    let alice = seed_actor(&db, "alice").await;
    seed_device(&db, "alice", "alice-laptop").await;
    seed_actor(&db, "bob").await;
    seed_group(&db, "grp-1", "chan-1", &["alice", "bob"]).await;
    seed_dm(&db, "dm-1", "alice", "bob").await;
    let h = harness(db).await;

    let status = send(
        &h,
        signed(
            &alice,
            "/v1/devices/revoke",
            serde_json::json!({ "device_id": "alice-laptop" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    for room in ["grp-1", "chan-1", "dm-1", "inbox-alice"] {
        assert!(
            kicked(&h.seen, room, "alice", "alice-laptop", ParticipantKind::Realtime),
            "the revoked device must be evicted from {room}"
        );
        assert!(
            !kicked(&h.seen, room, "alice", &alice.device_id, ParticipantKind::Realtime),
            "the account's surviving device must keep its session in {room}"
        );
    }
}
