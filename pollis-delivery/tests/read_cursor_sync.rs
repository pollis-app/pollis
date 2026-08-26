//! Cross-device read-cursor sync (#844) — what the DS may and may not do with a
//! blob it cannot read.
//!
//! The whole design rests on one claim: the server stores the cursor list
//! **opaquely**, unlike `user_preferences`, which it stores as readable JSON. A
//! server that quietly parsed, normalised, re-encoded or truncated the payload
//! would break decryption on every other device — and a server that let one
//! account read another's blob would leak exactly the metadata (size, change
//! rate) the encryption is there to blunt.
//!
//! So four properties, each with a test:
//!
//!   * **Byte-exact custody.** What comes out is what went in, bit for bit.
//!   * **Ownership.** You may write and read only your OWN row.
//!   * **Bounded.** An opaque blob with no ceiling is a free object store; the
//!     envelope (base64, nonce length, size) is validated even though the
//!     contents cannot be.
//!   * **Absence is not an error.** A first-run device gets `cursors: None`.
//!
//! Driven against a local libsql DB exactly as the DS runs, mirroring
//! `custom_emoji.rs`: `apply_*` for the write rules, the real router via
//! `tower::oneshot` for the read.

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

/// A well-formed push. The blob is deliberately NOT valid JSON and NOT valid
/// UTF-8 — it stands in for real AES-GCM output, and any handler that tried to
/// interpret it would fail here rather than in production.
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

/// Read the row back the way another device would: through the real router.
async fn read_via_router(db: &common::TempDb, actor: &str) -> ReadCursorsResponse {
    // Auth OFF, so `authed_user` falls back to the body's `user_id` — which is
    // what makes the ownership test below meaningful without minting a device
    // signature: the handler must still scope the SELECT to the named user.
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

/// The custody property: the DS is a bit bucket for this row.
///
/// If it ever normalised the base64, re-encoded the blob, or trimmed a trailing
/// byte, every other device's AES-GCM tag check would fail and the account would
/// silently stop syncing. Byte-exactness is not fussiness, it is the contract.
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

/// A push replaces the previous one. Last-writer-wins is deliberate: the merge
/// that makes concurrent pushes safe happens on the client, over decrypted
/// cursors that only move forward.
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

/// You may push only your OWN cursors. The signed identity wins over the body.
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

/// And you may read only your own. The blob is undecryptable to a stranger, but
/// serving it would still hand them its size and change rate — which is the
/// metadata this whole feature exists to blunt.
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

/// A device that has never synced is an ordinary state, not a 404.
#[tokio::test]
async fn an_account_that_never_pushed_reads_as_absent() {
    let db = fresh().await;
    assert!(read_via_router(&db, "alice").await.cursors.is_none());
}

/// The envelope is validated even though the contents cannot be. Each of these
/// is a distinct way a malformed push could otherwise land in the table and
/// break the account's sync until someone noticed.
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
        // An opaque blob with no ceiling is a free per-account object store.
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

/// The control for the case list above: a blob exactly AT the ceiling is fine,
/// so the size check is a bound rather than a blanket refusal.
#[tokio::test]
async fn a_blob_at_the_ceiling_is_accepted() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let out = apply_save_read_cursors(&conn, Some("alice"), &push("alice", &vec![0u8; 256 * 1024]))
        .await
        .unwrap();
    assert!(matches!(out, ReadCursorOutcome::Ok));
}

/// The column is a BLOB, and the bytes in it are not text. This is the test that
/// notices if someone "simplifies" the schema to TEXT the way
/// `user_preferences.preferences` is — which would corrupt any ciphertext
/// containing a byte sequence SQLite's text handling touches.
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
