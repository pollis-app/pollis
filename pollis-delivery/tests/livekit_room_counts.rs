//! `POST /v1/livekit/room-counts`: one signed call counts many voice rooms; rooms the
//! signer may not read, empty rooms and `view` receivers all count 0.

use std::sync::Arc;

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
use pollis_delivery::room_id::room_pseudonym;
use pollis_delivery::{build_router_with_state, AppState};
use rand_core::{OsRng, RngCore as _};
use tower::ServiceExt as _;

mod common;

const PATH: &str = "/v1/livekit/room-counts";
const API_SECRET: &str = "secret-secret-secret-secret";

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

fn now_ts() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

fn signed_counts(actor: &Actor, rooms: &[&str]) -> Request<Body> {
    let body = serde_json::to_vec(&serde_json::json!({ "rooms": rooms })).unwrap();
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

/// A stand-in LiveKit whose `ListParticipants` answers from a fixed table keyed by the
/// pseudonymous room name: `(wire_room -> identities)`; unknown rooms are 404.
async fn fake_livekit(table: Vec<(String, Vec<String>)>) -> String {
    let table = Arc::new(table);
    let app = Router::new().route(
        "/twirp/livekit.RoomService/ListParticipants",
        post(move |Json(body): Json<serde_json::Value>| {
            let table = Arc::clone(&table);
            async move {
                let room = body["room"].as_str().unwrap_or_default().to_string();
                match table.iter().find(|(r, _)| *r == room) {
                    Some((_, ids)) => {
                        let participants: Vec<serde_json::Value> = ids
                            .iter()
                            .map(|i| serde_json::json!({ "identity": i }))
                            .collect();
                        (
                            StatusCode::OK,
                            Json(serde_json::json!({ "participants": participants })),
                        )
                    }
                    None => (
                        StatusCode::NOT_FOUND,
                        Json(serde_json::json!({ "msg": "room not found" })),
                    ),
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    format!("ws://{addr}")
}

fn livekit_broker(url: &str) -> BrokerConfig {
    BrokerConfig {
        livekit_api_key: Some("APIkey".into()),
        livekit_api_secret: Some(API_SECRET.into()),
        livekit_url: Some(url.to_string()),
        ..BrokerConfig::default()
    }
}

async fn counts_of(router: Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let resp = router.oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn one_call_counts_every_room_and_hides_what_the_signer_may_not_read() {
    let db = fresh_db().await;
    let alice = seed_actor(&db, "alice").await;
    seed_group(&db, "g-1", &["alice", "bob", "carol"]).await;
    seed_group(&db, "g-2", &["alice", "bob"]).await;
    seed_group(&db, "g-3", &["bob", "carol"]).await;

    let voice = |room: &str, user: &str| {
        participant_pseudonym(
            API_SECRET,
            room,
            user,
            &format!("dev-{user}"),
            ParticipantKind::Voice,
        )
    };
    let view = |room: &str, user: &str| {
        participant_pseudonym(
            API_SECRET,
            room,
            user,
            &format!("dev-{user}"),
            ParticipantKind::View,
        )
    };
    let table = vec![
        // g-1: two voice participants, one screenshare receiver, one forgery.
        (
            room_pseudonym(API_SECRET, "g-1"),
            vec![
                voice("g-1", "bob"),
                voice("g-1", "carol"),
                view("g-1", "alice"),
                "not-ours".into(),
            ],
        ),
        // g-2: nobody (LiveKit has no such room → 404).
        // g-3: busy, but alice is not a member.
        (
            room_pseudonym(API_SECRET, "g-3"),
            vec![voice("g-3", "bob"), voice("g-3", "carol")],
        ),
    ];
    let url = fake_livekit(table).await;
    let state = AppState::new(db.arc(), true).with_broker_config(livekit_broker(&url));
    let router = build_router_with_state(state);

    let (status, body) =
        counts_of(router, signed_counts(&alice, &["g-1", "g-2", "g-3", "g-1"])).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["counts"],
        serde_json::json!([
            { "room": "g-1", "count": 2 },
            { "room": "g-2", "count": 0 },
            { "room": "g-3", "count": 0 },
        ]),
        "voice-only count for a member's room, 0 for an empty room, 0 for a room the signer is not in, duplicates collapsed"
    );
    drop(db);
}

#[tokio::test]
async fn an_oversized_batch_is_refused() {
    let db = fresh_db().await;
    let alice = seed_actor(&db, "alice").await;
    let url = fake_livekit(Vec::new()).await;
    let state = AppState::new(db.arc(), true).with_broker_config(livekit_broker(&url));
    let router = build_router_with_state(state);
    let rooms: Vec<String> = (0..65).map(|i| format!("g-{i}")).collect();
    let refs: Vec<&str> = rooms.iter().map(String::as_str).collect();
    let (status, _) = counts_of(router, signed_counts(&alice, &refs)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    drop(db);
}
