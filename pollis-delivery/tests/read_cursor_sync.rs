//! Cross-device read-cursor sync (#844) — what the DS may and may not do with a
//! blob it cannot read.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use http_body_util::BodyExt as _;
use pollis_api::account_reads::ReadCursorsResponse;
use pollis_api::DsRequest;
use pollis_delivery::profile::{apply_save_read_cursors, ReadCursorOutcome, SaveReadCursorsBody};
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap())
        .await
        .expect("schema");
    db
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// A well-formed push; the blob is deliberately neither JSON nor UTF-8.
fn push(user: &str, blob: &[u8]) -> SaveReadCursorsBody {
    SaveReadCursorsBody {
        user_id: user.to_string(),
        nonce: b64(&[7u8; 12]),
        blob: b64(blob),
    }
}

const CIPHERTEXT: &[u8] = &[
    0xF5, 0x00, 0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0xFF, 0x91, 0x7A, 0x00, 0x00, 0xC3, 0x28,
];

async fn read_via_router(db: &common::TempDb, actor: &str) -> ReadCursorsResponse {
    // Auth off: the handler must still scope the SELECT to the body's user.
    let app = build_router_with_state(AppState::new(db.arc(), false));
    let req = Request::builder()
        .method("POST")
        .uri(<pollis_api::account_reads::ReadCursorsBody as DsRequest>::PATH)
        .header("content-type", "application/json")
        .body(Body::from(format!(r#"{{"user_id":"{actor}"}}"#)))
        .expect("request");
    let resp = app.oneshot(req).await.expect("oneshot");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.expect("body").to_bytes();
    serde_json::from_slice(&bytes).expect("decode")
}

/// Any normalisation of the blob would break every other device's AES-GCM tag check.
#[tokio::test]
async fn the_blob_comes_back_byte_for_byte() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let out = apply_save_read_cursors(&conn, Some("alice"), &push("alice", CIPHERTEXT))
        .await
        .unwrap();
    assert!(matches!(out, ReadCursorOutcome::Ok));

    let resp = read_via_router(&db, "alice").await;
    let stored = resp.cursors.expect("a blob was pushed");
    assert_eq!(stored.blob, b64(CIPHERTEXT));
    assert_eq!(stored.nonce, b64(&[7u8; 12]));
}

/// Last-writer-wins is deliberate: the merge happens client-side.
#[tokio::test]
async fn a_second_push_replaces_the_first() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    apply_save_read_cursors(&conn, Some("alice"), &push("alice", CIPHERTEXT))
        .await
        .unwrap();
    let newer = &[0x11, 0x22, 0x33][..];
    apply_save_read_cursors(&conn, Some("alice"), &push("alice", newer))
        .await
        .unwrap();

    let stored = read_via_router(&db, "alice").await.cursors.expect("blob");
    assert_eq!(stored.blob, b64(newer));

    let mut rows = conn
        .query("SELECT COUNT(*) FROM read_cursor_sync", ())
        .await
        .unwrap();
    let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(n, 1, "one row per account, not an append-only log");
}

#[tokio::test]
async fn one_account_cannot_push_into_anothers_row() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let out = apply_save_read_cursors(&conn, Some("mallory"), &push("alice", CIPHERTEXT))
        .await
        .unwrap();
    assert!(
        matches!(out, ReadCursorOutcome::Forbidden),
        "a push naming another user must be refused, not silently re-attributed"
    );
    assert!(read_via_router(&db, "alice").await.cursors.is_none());
}

/// Serving another's blob would leak its size and change rate.
#[tokio::test]
async fn one_account_cannot_read_anothers_blob() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    apply_save_read_cursors(&conn, Some("alice"), &push("alice", CIPHERTEXT))
        .await
        .unwrap();

    assert!(
        read_via_router(&db, "mallory").await.cursors.is_none(),
        "the read must be scoped to the caller, not to the row that exists"
    );
}

#[tokio::test]
async fn an_account_that_never_pushed_reads_as_absent() {
    let db = fresh().await;
    assert!(read_via_router(&db, "alice").await.cursors.is_none());
}

#[tokio::test]
async fn a_malformed_envelope_is_refused() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    let cases: Vec<(&str, SaveReadCursorsBody)> = vec![
        (
            "nonce is not base64",
            SaveReadCursorsBody {
                user_id: "alice".into(),
                nonce: "not base64!!".into(),
                blob: b64(CIPHERTEXT),
            },
        ),
        (
            "nonce is the wrong length for AES-GCM",
            SaveReadCursorsBody {
                user_id: "alice".into(),
                nonce: b64(&[7u8; 11]),
                blob: b64(CIPHERTEXT),
            },
        ),
        (
            "blob is not base64",
            SaveReadCursorsBody {
                user_id: "alice".into(),
                nonce: b64(&[7u8; 12]),
                blob: "not base64!!".into(),
            },
        ),
        ("blob is empty", push("alice", &[])),
        ("blob is over the ceiling", push("alice", &vec![0u8; 256 * 1024 + 1])),
    ];

    for (why, body) in cases {
        let out = apply_save_read_cursors(&conn, Some("alice"), &body)
            .await
            .unwrap();
        assert!(
            matches!(out, ReadCursorOutcome::Invalid(_)),
            "accepted a push where {why}"
        );
    }

    assert!(
        read_via_router(&db, "alice").await.cursors.is_none(),
        "a refused push must leave no row behind"
    );
}

#[tokio::test]
async fn a_blob_at_the_ceiling_is_accepted() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let out = apply_save_read_cursors(&conn, Some("alice"), &push("alice", &vec![0u8; 256 * 1024]))
        .await
        .unwrap();
    assert!(matches!(out, ReadCursorOutcome::Ok));
}

/// The column must stay BLOB: TEXT would corrupt ciphertext.
#[tokio::test]
async fn the_stored_column_holds_raw_bytes_not_text() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    apply_save_read_cursors(&conn, Some("alice"), &push("alice", CIPHERTEXT))
        .await
        .unwrap();

    let mut rows = conn
        .query(
            "SELECT typeof(blob), blob FROM read_cursor_sync WHERE user_id = 'alice'",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    assert_eq!(row.get::<String>(0).unwrap(), "blob");
    assert_eq!(row.get::<Vec<u8>>(1).unwrap(), CIPHERTEXT);
}
