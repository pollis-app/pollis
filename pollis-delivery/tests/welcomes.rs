//! `POST /v1/welcomes/resubmit` (#430 P2), driven through the real router.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use pollis_delivery::db::Db;
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;


fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

async fn fresh_db() -> common::TempDb {
    let db = common::TempDb::open("delivery.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap()).await.expect("schema");
    db
}

/// Make `recipient` a member of `conv` and open `generation`, both required on any path.
async fn seed_conversation(db: &Db, conv: &str, recipient: &str, generation: i64) {
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO group_member (group_id, user_id) VALUES (?1, ?2)",
        libsql::params![conv, recipient],
    )
    .await
    .unwrap();
    for g in 0..=generation {
        conn.execute(
            "INSERT INTO mls_commit_log \
                 (conversation_id, generation, epoch, sender_id, commit_data) \
             VALUES (?1, ?2, 0, ?3, X'00')",
            libsql::params![conv, g, recipient],
        )
        .await
        .unwrap();
    }
}

fn resubmit_req_at(
    conv: &str,
    generation: i64,
    recipient: &str,
    device: &str,
    welcome: &[u8],
) -> Request<Body> {
    req_from(serde_json::json!({
        "conversation_id": conv,
        "generation": generation,
        "recipient_id": recipient,
        "recipient_device_id": device,
        "welcome": b64(welcome),
    }))
}

/// A resubmit body with no generation field, as a pre-P4 desktop app sends.
fn resubmit_req(conv: &str, recipient: &str, device: &str, welcome: &[u8]) -> Request<Body> {
    req_from(serde_json::json!({
        "conversation_id": conv,
        "recipient_id": recipient,
        "recipient_device_id": device,
        "welcome": b64(welcome),
    }))
}

fn req_from(body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/v1/welcomes/resubmit")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap()
}

async fn welcome_row(db: &Db, conv: &str, recipient: &str, device: &str) -> (i64, Vec<u8>, i64) {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*), MAX(welcome_data), COALESCE(MAX(generation), -1) FROM mls_welcome \
             WHERE conversation_id = ?1 AND recipient_id = ?2 AND recipient_device_id = ?3",
            libsql::params![conv, recipient, device],
        )
        .await
        .unwrap();
    let r = rows.next().await.unwrap().unwrap();
    let count: i64 = r.get(0).unwrap();
    let data: Vec<u8> = r.get::<Option<Vec<u8>>>(1).unwrap().unwrap_or_default();
    let generation: i64 = r.get(2).unwrap();
    (count, data, generation)
}

/// A resubmit with no `generation` re-drives the Welcome at generation 0 (#454 P4).
#[tokio::test(flavor = "multi_thread")]
async fn resubmit_re_drives_a_missing_welcome() {
    let db = fresh_db().await;
    // Auth off skips the caller's membership gate; the recipient's is checked either way.
    seed_conversation(&db, "conv1", "alice", 0).await;
    let router = build_router_with_state(AppState::new(Arc::clone(&db), false));

    let resp = router
        .oneshot(resubmit_req("conv1", "alice", "dev1", b"welcome-blob"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let (count, data, generation) = welcome_row(&db, "conv1", "alice", "dev1").await;
    assert_eq!(count, 1, "resubmit must insert exactly one Welcome");
    assert_eq!(data, b"welcome-blob");
    assert_eq!(generation, 0, "a pre-P4 client's Welcome is generation 0");
}

#[tokio::test(flavor = "multi_thread")]
async fn resubmit_is_idempotent_and_refreshes_blob() {
    let db = fresh_db().await;
    seed_conversation(&db, "conv1", "alice", 0).await;
    let router = build_router_with_state(AppState::new(Arc::clone(&db), false));

    let r1 = router
        .clone()
        .oneshot(resubmit_req("conv1", "alice", "dev1", b"welcome-v1"))
        .await
        .unwrap();
    assert_eq!(r1.status(), StatusCode::OK);

    let r2 = router
        .oneshot(resubmit_req("conv1", "alice", "dev1", b"welcome-v2"))
        .await
        .unwrap();
    assert_eq!(r2.status(), StatusCode::OK);

    let (count, data, _) = welcome_row(&db, "conv1", "alice", "dev1").await;
    assert_eq!(count, 1, "resubmit must not duplicate the Welcome");
    assert_eq!(data, b"welcome-v2", "resubmit must refresh to the latest blob");
}

/// A migrated conversation must land the successor's Welcome, or the device is stranded.
#[tokio::test(flavor = "multi_thread")]
async fn a_resubmit_refreshes_the_generation_too() {
    let db = fresh_db().await;
    seed_conversation(&db, "conv1", "alice", 1).await;
    let router = build_router_with_state(AppState::new(Arc::clone(&db), false));

    let r1 = router
        .clone()
        .oneshot(resubmit_req_at("conv1", 0, "alice", "dev1", b"classic"))
        .await
        .unwrap();
    assert_eq!(r1.status(), StatusCode::OK);

    let r2 = router
        .oneshot(resubmit_req_at("conv1", 1, "alice", "dev1", b"hybrid"))
        .await
        .unwrap();
    assert_eq!(r2.status(), StatusCode::OK);

    let (count, data, generation) = welcome_row(&db, "conv1", "alice", "dev1").await;
    assert_eq!(count, 1);
    assert_eq!(data, b"hybrid");
    assert_eq!(generation, 1, "the upsert must carry the successor's lineage");
}

/// Acking more ids than SQLite will bind must still work (#916): an ack that 500s redelivers forever.
#[tokio::test]
async fn acking_more_ids_than_sqlite_can_bind_still_marks_them_delivered() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();

    conn.execute(
        "INSERT INTO mls_welcome \
         (id, conversation_id, generation, recipient_id, recipient_device_id, welcome_data, delivered) \
         VALUES ('w-real', 'c1', 0, 'alice', 'd1', X'00', 0)",
        (),
    )
    .await
    .expect("seed welcome");

    // Past the bundled 32766 ceiling, with margin.
    let mut ids: Vec<String> = (0..40_000).map(|i| format!("w-absent-{i}")).collect();
    ids.push("w-real".to_string());

    let updated = pollis_delivery::writes::ack_welcomes(&conn, "alice", &ids)
        .await
        .expect("an oversized ack list must not fail to prepare");
    assert_eq!(updated, 1, "exactly the one real Welcome should be acked");

    let mut rows = conn
        .query(
            "SELECT delivered FROM mls_welcome WHERE id = 'w-real'",
            (),
        )
        .await
        .unwrap();
    let delivered: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(delivered, 1, "the acked Welcome must not be redelivered");
}

/// Chunking must keep the recipient bound in every chunk (#916 + W5).
#[tokio::test]
async fn a_chunked_ack_still_cannot_touch_another_users_welcomes() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();

    for (id, who) in [("w-alice", "alice"), ("w-bob", "bob")] {
        conn.execute(
            &format!(
                "INSERT INTO mls_welcome \
                 (id, conversation_id, generation, recipient_id, recipient_device_id, welcome_data, delivered) \
                 VALUES ('{id}', 'c1', 0, '{who}', 'd1', X'00', 0)"
            ),
            (),
        )
        .await
        .unwrap();
    }

    let mut ids: Vec<String> = (0..1_200).map(|i| format!("w-absent-{i}")).collect();
    ids.push("w-alice".to_string());
    ids.push("w-bob".to_string());

    let updated = pollis_delivery::writes::ack_welcomes(&conn, "alice", &ids)
        .await
        .unwrap();
    assert_eq!(updated, 1, "only alice's Welcome is hers to ack");

    let mut rows = conn
        .query("SELECT delivered FROM mls_welcome WHERE id = 'w-bob'", ())
        .await
        .unwrap();
    let delivered: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(delivered, 0, "bob's Welcome must be untouched");
}
