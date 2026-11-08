//! #1735: the history-transfer relay. Each invariant is tried from the side
//! that would break it — the wrong party, another account, an expired
//! transfer, a chunk outside the window, a rewritten chunk — not only the happy
//! path. Database-level rules are also hit with raw SQL, bypassing the handlers.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use http_body_util::BodyExt as _;
use ml_dsa::{Keypair, MlDsa44, Signer, SigningKey};
use pollis_delivery::auth::canonical_message;
use pollis_delivery::db::Db;
use pollis_delivery::history_transfer::{self as ht, Outcome};
use pollis_delivery::{build_router_with_state, AppState};
use rand_core::{OsRng, RngCore as _};
use tower::ServiceExt as _;

mod common;

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

struct Dev {
    user_id: String,
    device_id: String,
    key: SigningKey<MlDsa44>,
}

async fn seed_device(db: &Db, user_id: &str, device_id: &str) -> Dev {
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    let key = SigningKey::<MlDsa44>::from_seed(&seed.into());
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO users (id, email, username) VALUES (?1, ?1 || '@x', ?1)",
        libsql::params![user_id],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT INTO user_device (device_id, user_id, mls_signature_pub_pq) VALUES (?1, ?2, ?3)",
        libsql::params![device_id, user_id, key.verifying_key().encode().to_vec()],
    )
    .await
    .unwrap();
    Dev { user_id: user_id.into(), device_id: device_id.into(), key }
}

fn signed(path: &str, who: &Dev, body: &serde_json::Value) -> Request<Body> {
    let bytes = serde_json::to_vec(body).unwrap();
    signed_raw(path, who, bytes)
}

fn signed_raw(path: &str, who: &Dev, bytes: Vec<u8>) -> Request<Body> {
    let ts = pollis_delivery::util::now_unix() as i64;
    let sig = b64(&who.key.sign(&canonical_message("POST", path, ts, &bytes)).encode());
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("X-Pollis-User", who.user_id.clone())
        .header("X-Pollis-Device", who.device_id.clone())
        .header("X-Pollis-Timestamp", ts.to_string())
        .header("X-Pollis-Signature", sig)
        .body(Body::from(bytes))
        .unwrap()
}

fn router(db: &common::TempDb) -> axum::Router {
    build_router_with_state(AppState::new(Arc::clone(db), true))
}

async fn call(db: &common::TempDb, who: &Dev, path: &str, body: serde_json::Value) -> (StatusCode, serde_json::Value) {
    let resp = router(db).oneshot(signed(path, who, &body)).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
}

const P_REQUEST: &str = "/v1/history-transfer/request";
const P_PENDING: &str = "/v1/history-transfer/pending";
const P_STATUS: &str = "/v1/history-transfer/status";
const P_ACCEPT: &str = "/v1/history-transfer/accept";
const P_DECLINE: &str = "/v1/history-transfer/decline";
const P_CANCEL: &str = "/v1/history-transfer/cancel";
const P_COMPLETE: &str = "/v1/history-transfer/complete";
const P_PUT: &str = "/v1/history-transfer/chunk/put";
const P_GET: &str = "/v1/history-transfer/chunk/get";
const P_ACK: &str = "/v1/history-transfer/chunk/ack";

fn request_body(transfer_id: &str, src: &Dev) -> serde_json::Value {
    serde_json::json!({
        "transfer_id": transfer_id,
        "src_device_id": src.device_id,
        "via": "sibling",
        "req": b64(b"opaque request"),
        "req_sig": b64(&[7u8; 2420]),
    })
}

fn accept_body(transfer_id: &str) -> serde_json::Value {
    serde_json::json!({ "transfer_id": transfer_id, "acc": b64(b"opaque acc"), "acc_sig": b64(&[9u8; 2420]) })
}

fn put_body(transfer_id: &str, idx: i64, bytes: &[u8]) -> serde_json::Value {
    serde_json::json!({ "transfer_id": transfer_id, "idx": idx, "sealed": b64(bytes) })
}

/// Account `alice` with three devices: `src` (has history), `dst` (wants it),
/// `other` (an uninvolved sibling). Plus `mallory`, another account.
struct World {
    db: common::TempDb,
    src: Dev,
    dst: Dev,
    other: Dev,
    mallory: Dev,
}

async fn world() -> World {
    let db = fresh_db().await;
    let src = seed_device(&db, "alice", "alice-src").await;
    let dst = seed_device(&db, "alice", "alice-dst").await;
    let other = seed_device(&db, "alice", "alice-other").await;
    let mallory = seed_device(&db, "mallory", "mallory-dev").await;
    World { db, src, dst, other, mallory }
}

/// Request + accept through the router; returns the transfer id.
async fn accepted(w: &World, id: &str) -> String {
    let (s, _) = call(&w.db, &w.dst, P_REQUEST, request_body(id, &w.src)).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(&w.db, &w.src, P_ACCEPT, accept_body(id)).await;
    assert_eq!(s, StatusCode::OK);
    id.to_string()
}

async fn count(db: &Db, sql: &str, params: impl libsql::params::IntoParams) -> i64 {
    let conn = db.conn().await.unwrap();
    let mut rows = conn.query(sql, params).await.unwrap();
    rows.next().await.unwrap().map(|r| r.get::<i64>(0).unwrap()).unwrap_or(0)
}

#[tokio::test]
async fn happy_path_request_accept_chunks_ack_complete() {
    let w = world().await;
    let (s, _) = call(&w.db, &w.dst, P_REQUEST, request_body("T1", &w.src)).await;
    assert_eq!(s, StatusCode::OK);

    // The source sees it pending, with the signed bytes exactly as filed.
    let (s, pend) = call(&w.db, &w.src, P_PENDING, serde_json::json!({})).await;
    assert_eq!(s, StatusCode::OK);
    let reqs = pend["requests"].as_array().unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0]["dst_device_id"], "alice-dst");
    assert_eq!(reqs[0]["req"], b64(b"opaque request"));
    // The receiver is not the source: nothing pending for it.
    let (_, pend) = call(&w.db, &w.dst, P_PENDING, serde_json::json!({})).await;
    assert!(pend["requests"].as_array().unwrap().is_empty());

    let (s, _) = call(&w.db, &w.src, P_ACCEPT, accept_body("T1")).await;
    assert_eq!(s, StatusCode::OK);
    for i in 0..3 {
        let (s, r) = call(&w.db, &w.src, P_PUT, put_body("T1", i, &[i as u8 + 1; 100])).await;
        assert_eq!(s, StatusCode::OK, "{r}");
    }
    let (s, st) = call(&w.db, &w.dst, P_STATUS, serde_json::json!({ "transfer_id": "T1" })).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(st["state"], "accepted");
    assert_eq!(st["stored_through"], 2);
    assert_eq!(st["acked_through"], -1);
    assert_eq!(st["acc"], b64(b"opaque acc"));
    for i in 0..3 {
        let (s, g) = call(&w.db, &w.dst, P_GET, serde_json::json!({ "transfer_id": "T1", "idx": i })).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(g["sealed"], b64(&[i as u8 + 1; 100]));
        let (s, a) = call(&w.db, &w.dst, P_ACK, serde_json::json!({ "transfer_id": "T1", "idx": i })).await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(a["acked_through"], i);
    }
    // Acked chunks are gone from the DS.
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer_chunk", ()).await, 0);
    let (s, _) = call(&w.db, &w.dst, P_COMPLETE, serde_json::json!({ "transfer_id": "T1" })).await;
    assert_eq!(s, StatusCode::OK);
    // The source learns it completed (kept for an hour), and completing twice is a no-op.
    let (_, st) = call(&w.db, &w.src, P_STATUS, serde_json::json!({ "transfer_id": "T1" })).await;
    assert_eq!(st["state"], "completed");
    let (s, _) = call(&w.db, &w.dst, P_COMPLETE, serde_json::json!({ "transfer_id": "T1" })).await;
    assert_eq!(s, StatusCode::OK);
}

/// Another account, an uninvolved sibling, and an unknown id all get the same
/// 404 with the same body on every endpoint, so none of them learns the
/// transfer exists.
#[tokio::test]
async fn strangers_and_siblings_get_the_same_404_as_an_unknown_id() {
    let w = world().await;
    let id = accepted(&w, "T2").await;
    call(&w.db, &w.src, P_PUT, put_body(&id, 0, b"c0")).await;

    let probes: Vec<(&str, serde_json::Value)> = vec![
        (P_STATUS, serde_json::json!({ "transfer_id": id })),
        (P_ACCEPT, accept_body(&id)),
        (P_DECLINE, serde_json::json!({ "transfer_id": id })),
        (P_CANCEL, serde_json::json!({ "transfer_id": id })),
        (P_COMPLETE, serde_json::json!({ "transfer_id": id })),
        (P_PUT, put_body(&id, 1, b"c1")),
        (P_GET, serde_json::json!({ "transfer_id": id, "idx": 0 })),
        (P_ACK, serde_json::json!({ "transfer_id": id, "idx": 0 })),
    ];
    for (path, body) in &probes {
        let mut unknown = body.clone();
        unknown["transfer_id"] = serde_json::json!("NOPE");
        let (s_unknown, b_unknown) = call(&w.db, &w.src, path, unknown).await;
        for who in [&w.mallory, &w.other] {
            let (s, b) = call(&w.db, who, path, body.clone()).await;
            assert_eq!(s, StatusCode::NOT_FOUND, "{path} by {}", who.device_id);
            assert_eq!((s, &b), (s_unknown, &b_unknown), "{path}: a stranger's answer differs from an unknown id's");
        }
    }
    // Nothing they did changed it.
    let (_, st) = call(&w.db, &w.dst, P_STATUS, serde_json::json!({ "transfer_id": id })).await;
    assert_eq!(st["state"], "accepted");
    assert_eq!(st["stored_through"], 0);
    assert_eq!(st["acked_through"], -1);
}

/// Each party may only make its own moves: the receiver cannot approve its own
/// request or upload chunks; the source cannot read, ack or complete.
#[tokio::test]
async fn each_party_is_held_to_its_role() {
    let w = world().await;
    call(&w.db, &w.dst, P_REQUEST, request_body("T3", &w.src)).await;
    // Receiver approving its own request: refused, same 404.
    let (s, _) = call(&w.db, &w.dst, P_ACCEPT, accept_body("T3")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(&w.db, &w.dst, P_DECLINE, serde_json::json!({ "transfer_id": "T3" })).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Nothing moves before approval.
    let (s, _) = call(&w.db, &w.src, P_PUT, put_body("T3", 0, b"x")).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(&w.db, &w.dst, P_GET, serde_json::json!({ "transfer_id": "T3", "idx": 0 })).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer_chunk", ()).await, 0);

    call(&w.db, &w.src, P_ACCEPT, accept_body("T3")).await;
    let (s, _) = call(&w.db, &w.dst, P_PUT, put_body("T3", 0, b"x")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    call(&w.db, &w.src, P_PUT, put_body("T3", 0, b"x")).await;
    let (s, _) = call(&w.db, &w.src, P_GET, serde_json::json!({ "transfer_id": "T3", "idx": 0 })).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(&w.db, &w.src, P_ACK, serde_json::json!({ "transfer_id": "T3", "idx": 0 })).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = call(&w.db, &w.src, P_COMPLETE, serde_json::json!({ "transfer_id": "T3" })).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_request_names_a_live_sibling_of_the_same_account() {
    let w = world().await;
    // Another account's device.
    let (s, _) = call(&w.db, &w.dst, P_REQUEST, request_body("T4", &w.mallory)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Itself.
    let (s, _) = call(&w.db, &w.dst, P_REQUEST, request_body("T4", &w.dst)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    // A revoked sibling.
    w.db.conn()
        .await
        .unwrap()
        .execute("UPDATE user_device SET revoked_at = datetime('now') WHERE device_id = 'alice-other'", ())
        .await
        .unwrap();
    let (s, _) = call(&w.db, &w.dst, P_REQUEST, request_body("T4", &w.other)).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer", ()).await, 0);
    // A bad `via` is refused.
    let mut b = request_body("T4", &w.src);
    b["via"] = serde_json::json!("backup");
    let (s, _) = call(&w.db, &w.dst, P_REQUEST, b).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

/// At most 8 un-acknowledged chunks; the 9th waits for an ack.
#[tokio::test]
async fn the_window_holds_eight_unacked_chunks() {
    let w = world().await;
    let id = accepted(&w, "T5").await;
    for i in 0..8 {
        let (s, _) = call(&w.db, &w.src, P_PUT, put_body(&id, i, &[i as u8; 10])).await;
        assert_eq!(s, StatusCode::OK);
    }
    let (s, b) = call(&w.db, &w.src, P_PUT, put_body(&id, 8, b"nine")).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(b["error"], "window_full");
    call(&w.db, &w.dst, P_ACK, serde_json::json!({ "transfer_id": id, "idx": 0 })).await;
    let (s, _) = call(&w.db, &w.src, P_PUT, put_body(&id, 8, b"nine")).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn a_re_put_is_idempotent_and_a_rewrite_is_refused() {
    let w = world().await;
    let id = accepted(&w, "T6").await;
    assert_eq!(call(&w.db, &w.src, P_PUT, put_body(&id, 0, b"zero")).await.0, StatusCode::OK);
    // The same bytes again: a no-op.
    assert_eq!(call(&w.db, &w.src, P_PUT, put_body(&id, 0, b"zero")).await.0, StatusCode::OK);
    // Different bytes at a stored index: 409, and the stored bytes stand.
    let (s, b) = call(&w.db, &w.src, P_PUT, put_body(&id, 0, b"ZERO")).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(b["error"], "chunk index holds different bytes");
    let (_, g) = call(&w.db, &w.dst, P_GET, serde_json::json!({ "transfer_id": id, "idx": 0 })).await;
    assert_eq!(g["sealed"], b64(b"zero"));
    // Once acknowledged, a re-put (any bytes) is answered with the ack, stores nothing.
    call(&w.db, &w.dst, P_ACK, serde_json::json!({ "transfer_id": id, "idx": 0 })).await;
    let (s, b) = call(&w.db, &w.src, P_PUT, put_body(&id, 0, b"zero")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["acked_through"], 0);
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer_chunk", ()).await, 0);
    // A gap is refused.
    let (s, _) = call(&w.db, &w.src, P_PUT, put_body(&id, 2, b"two")).await;
    assert_eq!(s, StatusCode::CONFLICT);
}

#[tokio::test]
async fn acks_move_only_forward_and_only_over_stored_chunks() {
    let w = world().await;
    let id = accepted(&w, "T7").await;
    // Nothing stored: an ack of 0 is refused.
    assert_eq!(call(&w.db, &w.dst, P_ACK, serde_json::json!({ "transfer_id": id, "idx": 0 })).await.0, StatusCode::CONFLICT);
    call(&w.db, &w.src, P_PUT, put_body(&id, 0, b"a")).await;
    call(&w.db, &w.src, P_PUT, put_body(&id, 1, b"b")).await;
    // Skipping ahead: refused.
    assert_eq!(call(&w.db, &w.dst, P_ACK, serde_json::json!({ "transfer_id": id, "idx": 1 })).await.0, StatusCode::CONFLICT);
    assert_eq!(call(&w.db, &w.dst, P_ACK, serde_json::json!({ "transfer_id": id, "idx": 0 })).await.0, StatusCode::OK);
    // A repeated ack is a no-op answered with the current position.
    let (s, b) = call(&w.db, &w.dst, P_ACK, serde_json::json!({ "transfer_id": id, "idx": 0 })).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["acked_through"], 0);
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer_chunk", ()).await, 1);
}

#[tokio::test]
async fn sealed_chunks_are_capped_at_one_mebibyte_and_the_route_has_a_body_limit() {
    let w = world().await;
    let id = accepted(&w, "T8").await;
    let max = vec![1u8; ht::MAX_SEALED_BYTES];
    assert_eq!(call(&w.db, &w.src, P_PUT, put_body(&id, 0, &max)).await.0, StatusCode::OK);
    let over = vec![1u8; ht::MAX_SEALED_BYTES + 1];
    assert_eq!(call(&w.db, &w.src, P_PUT, put_body(&id, 1, &over)).await.0, StatusCode::BAD_REQUEST);
    // A body past the route's explicit limit never reaches the handler.
    let huge = vec![b'a'; ht::PUT_BODY_LIMIT + 1024];
    let mut body = b"{\"transfer_id\":\"T8\",\"idx\":1,\"sealed\":\"".to_vec();
    body.extend_from_slice(&huge);
    body.extend_from_slice(b"\"}");
    let resp = router(&w.db).oneshot(signed_raw(P_PUT, &w.src, body)).await.unwrap();
    assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    // An empty chunk is not a chunk.
    assert_eq!(call(&w.db, &w.src, P_PUT, put_body(&id, 1, b"")).await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn decline_and_cancel_end_it_and_both_sides_learn_how() {
    let w = world().await;
    call(&w.db, &w.dst, P_REQUEST, request_body("T9", &w.src)).await;
    assert_eq!(call(&w.db, &w.src, P_DECLINE, serde_json::json!({ "transfer_id": "T9" })).await.0, StatusCode::OK);
    let (_, st) = call(&w.db, &w.dst, P_STATUS, serde_json::json!({ "transfer_id": "T9" })).await;
    assert_eq!(st["state"], "declined");
    // A declined transfer cannot be accepted afterwards.
    assert_eq!(call(&w.db, &w.src, P_ACCEPT, accept_body("T9")).await.0, StatusCode::CONFLICT);

    let id = accepted(&w, "T10").await;
    call(&w.db, &w.src, P_PUT, put_body(&id, 0, b"a")).await;
    assert_eq!(call(&w.db, &w.dst, P_CANCEL, serde_json::json!({ "transfer_id": id })).await.0, StatusCode::OK);
    let (_, st) = call(&w.db, &w.src, P_STATUS, serde_json::json!({ "transfer_id": id })).await;
    assert_eq!(st["state"], "cancelled");
    // Cancelling drops its chunks at once and nothing more can be stored.
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer_chunk", ()).await, 0);
    assert_eq!(call(&w.db, &w.src, P_PUT, put_body(&id, 0, b"a")).await.0, StatusCode::CONFLICT);
    assert_eq!(call(&w.db, &w.dst, P_COMPLETE, serde_json::json!({ "transfer_id": id })).await.0, StatusCode::CONFLICT);
}

#[tokio::test]
async fn a_new_request_replaces_the_receivers_own_old_one() {
    let w = world().await;
    let id = accepted(&w, "T11").await;
    call(&w.db, &w.src, P_PUT, put_body(&id, 0, b"a")).await;
    assert_eq!(call(&w.db, &w.dst, P_REQUEST, request_body("T12", &w.other)).await.0, StatusCode::OK);
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer WHERE transfer_id = 'T11'", ()).await, 0);
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer_chunk", ()).await, 0);
    // A transfer id is never reused across receivers.
    assert_eq!(call(&w.db, &w.other, P_REQUEST, request_body("T12", &w.src)).await.0, StatusCode::CONFLICT);
}

#[tokio::test]
async fn the_request_budget_is_per_account_and_durable() {
    let w = world().await;
    let conn = w.db.conn().await.unwrap();
    let now = 1_000_000;
    for i in 0..ht::REQUESTS_PER_WINDOW {
        let body: pollis_api::history_transfer::RequestTransferBody =
            serde_json::from_value(request_body(&format!("B{i}"), &w.src)).unwrap();
        let out = ht::apply_request(&conn, "alice", "alice-dst", &body, now).await.unwrap();
        assert!(matches!(out, Outcome::Ok(_)), "{i}: {out:?}");
    }
    // The other device of the same account shares the budget.
    let body: pollis_api::history_transfer::RequestTransferBody =
        serde_json::from_value(request_body("B-over", &w.src)).unwrap();
    let out = ht::apply_request(&conn, "alice", "alice-other", &body, now + 10).await.unwrap();
    assert!(matches!(out, Outcome::TooMany), "{out:?}");
    // Another account is unaffected.
    let m2 = seed_device(&w.db, "mallory", "mallory-2").await;
    let body: pollis_api::history_transfer::RequestTransferBody =
        serde_json::from_value(request_body("M1", &w.mallory)).unwrap();
    let out = ht::apply_request(&conn, "mallory", &m2.device_id, &body, now).await.unwrap();
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
    // A new window restores it.
    let out = ht::apply_request(&conn, "alice", "alice-other", &body_for("B-later", &w.src), now + ht::BUDGET_WINDOW_SECS).await.unwrap();
    assert!(matches!(out, Outcome::Ok(_)), "{out:?}");
}

fn body_for(id: &str, src: &Dev) -> pollis_api::history_transfer::RequestTransferBody {
    serde_json::from_value(request_body(id, src)).unwrap()
}

#[tokio::test]
async fn expiry_is_enforced_on_read_and_swept_with_its_chunks() {
    let w = world().await;
    let conn = w.db.conn().await.unwrap();
    let now = 2_000_000;
    let out = ht::apply_request(&conn, "alice", "alice-dst", &body_for("E1", &w.src), now).await.unwrap();
    assert!(matches!(out, Outcome::Ok(_)));
    // An unanswered request dies after 15 minutes.
    let st = |t| pollis_api::history_transfer::TransferStatusBody { transfer_id: t };
    let late = now + ht::REQUEST_TTL_SECS;
    assert!(matches!(ht::apply_status(&conn, "alice", "alice-dst", &st("E1".into()), late).await.unwrap(), Outcome::NotFound));
    assert!(matches!(ht::apply_pending(&conn, "alice", "alice-src", late).await.unwrap(), Outcome::Ok(r) if r.requests.is_empty()));
    let acc = pollis_api::history_transfer::AcceptTransferBody {
        transfer_id: "E1".into(),
        acc: b64(b"a"),
        acc_sig: b64(b"s"),
    };
    assert!(matches!(ht::apply_accept(&conn, "alice", "alice-src", &acc, late).await.unwrap(), Outcome::NotFound));

    // Accepted: alive 24h after the last activity, which every chunk extends.
    let out = ht::apply_request(&conn, "alice", "alice-dst", &body_for("E2", &w.src), now).await.unwrap();
    assert!(matches!(out, Outcome::Ok(_)));
    let acc = pollis_api::history_transfer::AcceptTransferBody { transfer_id: "E2".into(), ..acc };
    assert!(matches!(ht::apply_accept(&conn, "alice", "alice-src", &acc, now).await.unwrap(), Outcome::Ok(_)));
    let put = pollis_api::history_transfer::PutChunkBody { transfer_id: "E2".into(), idx: 0, sealed: b64(b"x") };
    let t1 = now + ht::ACCEPTED_TTL_SECS - 10;
    assert!(matches!(ht::apply_put_chunk(&conn, "alice", "alice-src", &put, t1).await.unwrap(), Outcome::Ok(_)));
    // Past the original deadline, still alive because of the chunk.
    assert!(matches!(ht::apply_status(&conn, "alice", "alice-dst", &st("E2".into()), now + ht::ACCEPTED_TTL_SECS + 5).await.unwrap(), Outcome::Ok(_)));
    // The sweep leaves it alone before its deadline and removes it, chunks too, after.
    // E1 was replaced by E2 (same receiver), so nothing else is there to sweep.
    assert_eq!(ht::sweep_expired(&conn, t1 + 1).await.unwrap(), 0);
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer_chunk", ()).await, 1);
    assert_eq!(ht::sweep_expired(&conn, t1 + ht::ACCEPTED_TTL_SECS).await.unwrap(), 1);
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer", ()).await, 0);
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer_chunk", ()).await, 0);
}

/// Revoking or logging out either party, resetting the identity, or deleting
/// the account removes the transfer and every sealed chunk.
#[tokio::test]
async fn revocation_logout_reset_and_teardown_purge_transfers() {
    use pollis_api::account::{LogoutDeviceBody, ResetRecoverBody, RevokeDeviceBody};
    use pollis_delivery::account::{apply_logout_device, apply_reset_recover, apply_revoke_device};

    async fn staged(w: &World, id: &str) {
        accepted(w, id).await;
        let (s, _) = call(&w.db, &w.src, P_PUT, put_body(id, 0, b"a")).await;
        assert_eq!(s, StatusCode::OK);
    }
    let w = world().await;
    let conn = w.db.conn().await.unwrap();

    staged(&w, "R1").await;
    apply_revoke_device(&conn, Some("alice"), &RevokeDeviceBody { device_id: "alice-src".into(), user_id: None })
        .await
        .unwrap();
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer", ()).await, 0);
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer_chunk", ()).await, 0);

    let w = world().await;
    let conn = w.db.conn().await.unwrap();
    staged(&w, "R2").await;
    apply_logout_device(&conn, Some("alice"), &LogoutDeviceBody { device_id: "alice-dst".into(), user_id: None })
        .await
        .unwrap();
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer", ()).await, 0);
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer_chunk", ()).await, 0);

    let w = world().await;
    let conn = w.db.conn().await.unwrap();
    staged(&w, "R3").await;
    apply_reset_recover(
        &conn,
        Some("alice"),
        &ResetRecoverBody { current_device_id: Some("alice-other".into()), user_id: None },
    )
    .await
    .unwrap();
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer", ()).await, 0);
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer_chunk", ()).await, 0);

    let w = world().await;
    let conn = w.db.conn().await.unwrap();
    staged(&w, "R4").await;
    pollis_delivery::teardown::purge_user_rows(&conn, "alice").await.unwrap();
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer", ()).await, 0);
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer_chunk", ()).await, 0);
    assert_eq!(count(&w.db, "SELECT COUNT(*) FROM history_transfer_budget", ()).await, 0);
}

// ---------------------------------------------------------------------------
// The database refuses the invalid states itself (handlers bypassed).
// ---------------------------------------------------------------------------

async fn raw_transfer(conn: &libsql::Connection, id: &str, dst: &str, state: &str) -> Result<u64, libsql::Error> {
    let acc: Option<Vec<u8>> = if state == "requested" { None } else { Some(b"acc".to_vec()) };
    conn.execute(
        "INSERT INTO history_transfer (transfer_id, user_id, src_device_id, dst_device_id, via, state, \
             req, req_sig, acc, acc_sig, created_at, updated_at, expires_at) \
         VALUES (?1, 'alice', 'alice-src', ?2, 'sibling', ?3, X'01', X'02', ?4, ?4, 0, 0, 9999999999)",
        libsql::params![id, dst, state, acc],
    )
    .await
}

async fn raw_chunk(conn: &libsql::Connection, id: &str, idx: i64) -> Result<u64, libsql::Error> {
    conn.execute(
        "INSERT INTO history_transfer_chunk (transfer_id, idx, sealed, created_at) VALUES (?1, ?2, X'AA', 0)",
        libsql::params![id, idx],
    )
    .await
}

#[tokio::test]
async fn the_schema_refuses_invalid_transfers() {
    let w = world().await;
    let conn = w.db.conn().await.unwrap();
    // src == dst.
    assert!(raw_transfer(&conn, "X1", "alice-src", "requested").await.is_err());
    // A requested transfer with an acceptance; an accepted one without.
    assert!(conn
        .execute(
            "INSERT INTO history_transfer (transfer_id, user_id, src_device_id, dst_device_id, via, state, \
                 req, req_sig, acc, acc_sig, created_at, updated_at, expires_at) \
             VALUES ('X2', 'alice', 'alice-src', 'alice-dst', 'sibling', 'requested', X'01', X'02', X'03', X'04', 0, 0, 1)",
            (),
        )
        .await
        .is_err());
    assert!(conn
        .execute(
            "INSERT INTO history_transfer (transfer_id, user_id, src_device_id, dst_device_id, via, state, \
                 req, req_sig, created_at, updated_at, expires_at) \
             VALUES ('X3', 'alice', 'alice-src', 'alice-dst', 'sibling', 'accepted', X'01', X'02', 0, 0, 1)",
            (),
        )
        .await
        .is_err());
    // Two open transfers for one receiver.
    raw_transfer(&conn, "X4", "alice-dst", "requested").await.unwrap();
    assert!(raw_transfer(&conn, "X5", "alice-dst", "accepted").await.is_err());
    // ...but an ended one does not block a new one.
    conn.execute("UPDATE history_transfer SET state = 'cancelled', acc = X'01', acc_sig = X'01' WHERE transfer_id = 'X4'", ())
        .await
        .unwrap();
    raw_transfer(&conn, "X5", "alice-dst", "accepted").await.unwrap();
    // An unknown state or via.
    assert!(conn
        .execute("UPDATE history_transfer SET state = 'paused' WHERE transfer_id = 'X5'", ())
        .await
        .is_err());
    assert!(conn
        .execute("UPDATE history_transfer SET via = 'backup' WHERE transfer_id = 'X5'", ())
        .await
        .is_err());
}

#[tokio::test]
async fn the_schema_refuses_chunks_outside_the_window_or_out_of_order() {
    let w = world().await;
    let conn = w.db.conn().await.unwrap();
    raw_transfer(&conn, "C1", "alice-dst", "requested").await.unwrap();
    // Not accepted: no chunk.
    assert!(raw_chunk(&conn, "C1", 0).await.is_err());
    // No transfer at all: no chunk.
    assert!(raw_chunk(&conn, "nope", 0).await.is_err());
    conn.execute("UPDATE history_transfer SET state = 'accepted', acc = X'01', acc_sig = X'01' WHERE transfer_id = 'C1'", ())
        .await
        .unwrap();
    // Not contiguous.
    assert!(raw_chunk(&conn, "C1", 1).await.is_err());
    for i in 0..8 {
        raw_chunk(&conn, "C1", i).await.unwrap();
    }
    // The 9th un-acked chunk.
    assert!(raw_chunk(&conn, "C1", 8).await.is_err());
    // A stored chunk cannot be rewritten.
    assert!(conn
        .execute("UPDATE history_transfer_chunk SET sealed = X'BB' WHERE transfer_id = 'C1' AND idx = 0", ())
        .await
        .is_err());
    // Ack 0 (delete + move); then an index at or below the ack is refused.
    conn.execute("DELETE FROM history_transfer_chunk WHERE transfer_id = 'C1' AND idx = 0", ()).await.unwrap();
    conn.execute("UPDATE history_transfer SET acked_through = 0 WHERE transfer_id = 'C1'", ()).await.unwrap();
    assert!(raw_chunk(&conn, "C1", 0).await.is_err());
    raw_chunk(&conn, "C1", 8).await.unwrap();
    // The ack never moves back.
    assert!(conn
        .execute("UPDATE history_transfer SET acked_through = -1 WHERE transfer_id = 'C1'", ())
        .await
        .is_err());
    // Oversize and empty chunks.
    conn.execute("DELETE FROM history_transfer_chunk WHERE transfer_id = 'C1' AND idx = 1", ()).await.unwrap();
    conn.execute("UPDATE history_transfer SET acked_through = 1 WHERE transfer_id = 'C1'", ()).await.unwrap();
    assert!(conn
        .execute(
            "INSERT INTO history_transfer_chunk (transfer_id, idx, sealed, created_at) VALUES ('C1', 9, zeroblob(1048577), 0)",
            (),
        )
        .await
        .is_err());
    assert!(conn
        .execute("INSERT INTO history_transfer_chunk (transfer_id, idx, sealed, created_at) VALUES ('C1', 9, X'', 0)", ())
        .await
        .is_err());
}

/// The nudge names the transfer and the step, nothing else: no device, no size, no content.
#[test]
fn nudges_are_content_free() {
    let p = ht::nudge_payload("T1", "chunk");
    let obj = p.as_object().unwrap();
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["event", "transfer_id", "type"]);
    assert_eq!(p["type"], "history_transfer");
}
