//! `POST /v1/welcomes/resubmit` (issue #430 P2), driven through the real axum
//! router with `tower::oneshot` against a local libsql DB. The resubmit path
//! re-drives a missing Welcome so recovery does not depend solely on the client's
//! external-join fallback, and is idempotent on the UNIQUE recipient tuple.

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

/// A resubmit body carrying an explicit suite generation.
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

/// A resubmit body with NO generation field — byte-for-byte what a pre-P4
/// desktop app sends.
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

/// The COUNT + newest welcome_data + generation for a (conversation, recipient,
/// device) tuple.
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

/// A resubmit re-drives a Welcome for a recipient/device that had none — the row
/// is (re)inserted with the supplied blob, armed for delivery.
#[tokio::test(flavor = "multi_thread")]
async fn resubmit_re_drives_a_missing_welcome() {
    let db = fresh_db().await;
    // Auth off: the no-auth path skips the membership gate (as submit does).
    let router = build_router_with_state(AppState::new(Arc::clone(&db), false));

    let resp = router
        .oneshot(resubmit_req("conv1", "alice", "dev1", b"welcome-blob"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let (count, data, _) = welcome_row(&db, "conv1", "alice", "dev1").await;
    assert_eq!(count, 1, "resubmit must insert exactly one Welcome");
    assert_eq!(data, b"welcome-blob");
}

/// Two resubmits for the same (conversation, recipient, device) are idempotent:
/// the second refreshes the blob in place on the UNIQUE tuple — no error, no
/// duplicate row.
#[tokio::test(flavor = "multi_thread")]
async fn resubmit_is_idempotent_and_refreshes_blob() {
    let db = fresh_db().await;
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

/// The pre-hybrid compatibility contract (#454 P4): a shipped desktop app sends
/// no `generation` field, and generation 0 is exactly the lineage that app is in.
#[tokio::test(flavor = "multi_thread")]
async fn an_omitted_generation_is_the_classic_lineage() {
    let db = fresh_db().await;
    let router = build_router_with_state(AppState::new(Arc::clone(&db), false));

    let resp = router
        .oneshot(resubmit_req("conv1", "alice", "dev1", b"welcome-blob"))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let (_, _, generation) = welcome_row(&db, "conv1", "alice", "dev1").await;
    assert_eq!(generation, 0, "a pre-P4 client's Welcome is generation 0");
}

/// Re-driving a Welcome for a conversation that has since migrated must land the
/// SUCCESSOR's Welcome, not leave the recipient pointed at the retired lineage.
/// The recipient decides when to adopt by reading this generation, so a stale
/// value is how a device gets stranded on a closed suite.
#[tokio::test(flavor = "multi_thread")]
async fn a_resubmit_refreshes_the_generation_too() {
    let db = fresh_db().await;
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

/// Acking more Welcome ids than SQLite will bind must still work (#916).
///
/// `ack_welcomes` builds `id IN (…)` from `AckBody::welcome_ids`, a list that
/// arrives in the REQUEST BODY — so its length is chosen by the caller, not
/// sized by anything on the server. Past `SQLITE_MAX_VARIABLE_NUMBER` the
/// statement does not run slowly, it fails to prepare, and an ack that 500s
/// leaves the Welcomes marked undelivered and redelivered forever.
///
/// The ids do not need to exist for this to bite: the limit applies to the
/// statement, not the matching rows. So this drives one real Welcome plus a pile
/// of ids that match nothing, which is both the cheap way to exceed the ceiling
/// and a fair model of a client acking a long backlog.
///
/// Without the chunking this is an `Err` from `prepare`; with it, the real
/// Welcome is acked and the rest are simply no-ops.
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

    // Comfortably past the bundled build's 32766 ceiling, so the test does not
    // quietly stop testing anything if that limit is raised again.
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

/// Chunking must not let an ack cross the recipient scope (#916 + W5).
///
/// The recipient is bound as `?1` in every chunk. Rebuilding the statement per
/// chunk is exactly where that binding could be dropped, and losing it would
/// turn an ack into "mark any Welcome with this id delivered, whoever owns it".
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

    // Long enough to span several chunks, and it names BOTH users' Welcomes.
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
