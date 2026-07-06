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

// The commit-log table the resubmit path writes, plus the UNIQUE recipient index
// migration 000002 (commit-log DB) adds (the ON CONFLICT target the upsert keys on).
const SCHEMA: &str = "\
CREATE TABLE mls_welcome (\
  id TEXT PRIMARY KEY,\
  conversation_id TEXT NOT NULL,\
  recipient_id TEXT NOT NULL,\
  welcome_data BLOB NOT NULL,\
  delivered INTEGER NOT NULL DEFAULT 0,\
  created_at TEXT NOT NULL DEFAULT (datetime('now')),\
  recipient_device_id TEXT\
);\
CREATE UNIQUE INDEX idx_mls_welcome_recipient ON mls_welcome (conversation_id, recipient_id, recipient_device_id);";

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

async fn fresh_db() -> Arc<Db> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("delivery.db");
    std::mem::forget(dir);
    let db = Db::connect_local(path.to_str().unwrap()).await.expect("local db");
    db.conn().unwrap().execute_batch(SCHEMA).await.expect("schema");
    Arc::new(db)
}

fn resubmit_req(conv: &str, recipient: &str, device: &str, welcome: &[u8]) -> Request<Body> {
    let body = serde_json::to_vec(&serde_json::json!({
        "conversation_id": conv,
        "recipient_id": recipient,
        "recipient_device_id": device,
        "welcome": b64(welcome),
    }))
    .unwrap();
    Request::builder()
        .method("POST")
        .uri("/v1/welcomes/resubmit")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

/// The COUNT + newest welcome_data for a (conversation, recipient, device) tuple.
async fn welcome_row(db: &Db, conv: &str, recipient: &str, device: &str) -> (i64, Vec<u8>) {
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*), MAX(welcome_data) FROM mls_welcome \
             WHERE conversation_id = ?1 AND recipient_id = ?2 AND recipient_device_id = ?3",
            libsql::params![conv, recipient, device],
        )
        .await
        .unwrap();
    let r = rows.next().await.unwrap().unwrap();
    let count: i64 = r.get(0).unwrap();
    let data: Vec<u8> = r.get::<Option<Vec<u8>>>(1).unwrap().unwrap_or_default();
    (count, data)
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

    let (count, data) = welcome_row(&db, "conv1", "alice", "dev1").await;
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

    let (count, data) = welcome_row(&db, "conv1", "alice", "dev1").await;
    assert_eq!(count, 1, "resubmit must not duplicate the Welcome");
    assert_eq!(data, b"welcome-v2", "resubmit must refresh to the latest blob");
}
