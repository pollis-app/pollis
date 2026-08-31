//! The Vault (#107) — what the DS may and may not do with entries it cannot
//! read, and how vault attachment references hold shared objects alive.
//!
//! The same four properties as `read_cursor_sync.rs`, per entry instead of
//! per account — byte-exact custody, ownership, bounds, absence-is-empty —
//! plus the two vault-specific ones:
//!
//!   * **Cross-user id collision is a permission problem.** The upsert's
//!     conflict target is the bare primary key, so the owner predicate on the
//!     update arm is the only thing standing between "same ULID" and "silent
//!     cross-user overwrite". It answers Forbidden, never success.
//!   * **A vault reference is a live reference.** An attachment object whose
//!     only reference is a vault entry must survive the collect that
//!     `/v1/attachments/delete` runs — and become collectable the moment the
//!     entry is deleted. A personal drive that loses files whenever a
//!     stranger deletes a same-hash attachment is not storage.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use http_body_util::BodyExt as _;
use pollis_api::messages::AttachmentDeleteBody;
use pollis_api::vault::{
    DeleteVaultMessageBody, SaveVaultMessageBody, VaultMessagesBody, VaultMessagesResponse,
};
use pollis_api::DsRequest;
use pollis_delivery::messages::apply_delete_attachment;
use pollis_delivery::vault::{
    apply_delete_vault_message, apply_save_vault_message, VaultOutcome,
};
use pollis_delivery::writes::WriteOutcome;
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("vault.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap())
        .await
        .expect("schema");
    db
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Stands in for AES-GCM output: not UTF-8, not JSON.
const CIPHERTEXT: &[u8] = &[
    0xF5, 0x00, 0xDE, 0xAD, 0xBE, 0xEF, 0x00, 0xFF, 0x91, 0x7A, 0x00, 0x00, 0xC3, 0x28,
];

const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn save(user: &str, id: &str, blob: &[u8], hashes: &[&str]) -> SaveVaultMessageBody {
    SaveVaultMessageBody {
        id: id.to_string(),
        user_id: user.to_string(),
        nonce: b64(&[7u8; 12]),
        blob: b64(blob),
        created_at: "2026-01-01T00:00:00Z".to_string(),
        attachment_hashes: hashes.iter().map(|h| h.to_string()).collect(),
    }
}

async fn read_vault(db: &common::TempDb, actor: &str) -> VaultMessagesResponse {
    let app = build_router_with_state(AppState::new(db.arc(), false));
    let body = serde_json::to_vec(&VaultMessagesBody {
        user_id: actor.to_string(),
    })
    .unwrap();
    let req = Request::builder()
        .method("POST")
        .uri(<VaultMessagesBody as DsRequest>::PATH)
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap();
    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

async fn ref_count(db: &common::TempDb, entry_id: &str) -> i64 {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM vault_attachment_ref WHERE vault_message_id = ?1",
            libsql::params![entry_id],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

async fn object_exists(db: &common::TempDb, hash: &str) -> bool {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT 1 FROM attachment_object WHERE content_hash = ?1",
            libsql::params![hash],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().is_some()
}

// ── Custody + ownership ──────────────────────────────────────────────────────

#[tokio::test]
async fn an_entry_comes_back_byte_for_byte() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let out = apply_save_vault_message(&conn, Some("alice"), &save("alice", "v1", CIPHERTEXT, &[]))
        .await
        .unwrap();
    assert!(matches!(out, VaultOutcome::Ok));

    let resp = read_vault(&db, "alice").await;
    assert_eq!(resp.messages.len(), 1);
    assert_eq!(resp.messages[0].blob, b64(CIPHERTEXT));
    assert_eq!(resp.messages[0].nonce, b64(&[7u8; 12]));
    assert_eq!(resp.messages[0].created_at, "2026-01-01T00:00:00Z");
}

/// An edit rewrites the blob but keeps `created_at` — the entry must not jump
/// position in the list because it was edited.
#[tokio::test]
async fn a_resave_replaces_the_blob_and_keeps_created_at() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    apply_save_vault_message(&conn, Some("alice"), &save("alice", "v1", CIPHERTEXT, &[]))
        .await
        .unwrap();
    let mut edited = save("alice", "v1", &[0x11, 0x22], &[]);
    edited.created_at = "2026-06-06T06:06:06Z".to_string();
    apply_save_vault_message(&conn, Some("alice"), &edited)
        .await
        .unwrap();

    let resp = read_vault(&db, "alice").await;
    assert_eq!(resp.messages.len(), 1, "one row per entry, not an append log");
    assert_eq!(resp.messages[0].blob, b64(&[0x11, 0x22]));
    assert_eq!(
        resp.messages[0].created_at, "2026-01-01T00:00:00Z",
        "created_at is kept from the original row on an edit"
    );
}

/// The cross-user overwrite: mallory saves under alice's ULID. The row must
/// stand untouched and the answer must be Forbidden.
#[tokio::test]
async fn a_colliding_id_cannot_overwrite_another_users_entry() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    apply_save_vault_message(&conn, Some("alice"), &save("alice", "v1", CIPHERTEXT, &[]))
        .await
        .unwrap();
    let out = apply_save_vault_message(&conn, Some("mallory"), &save("mallory", "v1", &[0xEE], &[]))
        .await
        .unwrap();
    assert!(
        matches!(out, VaultOutcome::Forbidden),
        "an id collision across users is a permission problem, not an upsert"
    );
    let resp = read_vault(&db, "alice").await;
    assert_eq!(resp.messages[0].blob, b64(CIPHERTEXT), "alice's bytes must stand");
    assert!(read_vault(&db, "mallory").await.messages.is_empty());
}

/// You save only your OWN vault: a signed identity that differs from the
/// body's owner is refused.
#[tokio::test]
async fn one_account_cannot_write_into_anothers_vault() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let out = apply_save_vault_message(&conn, Some("mallory"), &save("alice", "v2", &[0x01], &[]))
        .await
        .unwrap();
    assert!(matches!(out, VaultOutcome::Forbidden));
    assert!(read_vault(&db, "alice").await.messages.is_empty());
}

/// Deleting someone else's entry is a no-op — the owner predicate is in the
/// DELETE itself.
#[tokio::test]
async fn a_delete_is_owner_bound() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    apply_save_vault_message(&conn, Some("alice"), &save("alice", "v1", CIPHERTEXT, &[HASH_A]))
        .await
        .unwrap();
    let out = apply_delete_vault_message(
        &conn,
        Some("mallory"),
        &DeleteVaultMessageBody {
            id: "v1".into(),
            user_id: "mallory".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok), "idempotent surface, no oracle");
    assert_eq!(read_vault(&db, "alice").await.messages.len(), 1);
    assert_eq!(ref_count(&db, "v1").await, 1, "the refs must survive a stranger's delete");
}

/// Bounds: the envelope is validated even though the contents cannot be.
#[tokio::test]
async fn a_malformed_envelope_is_a_client_error() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    let mut bad = save("alice", "v1", CIPHERTEXT, &[]);
    bad.nonce = b64(&[1u8; 11]);
    assert!(matches!(
        apply_save_vault_message(&conn, Some("alice"), &bad).await.unwrap(),
        VaultOutcome::Invalid(_)
    ));

    let mut bad = save("alice", "v1", CIPHERTEXT, &[]);
    bad.blob = "not base64!!!".into();
    assert!(matches!(
        apply_save_vault_message(&conn, Some("alice"), &bad).await.unwrap(),
        VaultOutcome::Invalid(_)
    ));

    let bad = save("alice", "v1", &[0u8; 300 * 1024], &[]);
    assert!(matches!(
        apply_save_vault_message(&conn, Some("alice"), &bad).await.unwrap(),
        VaultOutcome::Invalid(_)
    ));

    let bad = save("alice", "v1", CIPHERTEXT, &["not-a-hash"]);
    assert!(matches!(
        apply_save_vault_message(&conn, Some("alice"), &bad).await.unwrap(),
        VaultOutcome::Invalid(_)
    ));
}

#[tokio::test]
async fn an_empty_vault_reads_as_an_empty_list() {
    let db = fresh().await;
    assert!(read_vault(&db, "alice").await.messages.is_empty());
}

// ── Attachment reference liveness ────────────────────────────────────────────

/// THE STORAGE-SERVICE PROPERTY: an object whose only reference is a vault
/// entry survives the unreferenced-object collect; deleting the entry
/// releases it.
#[tokio::test]
async fn a_vault_reference_keeps_the_shared_object_alive() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT INTO attachment_object (content_hash, r2_key) VALUES (?1, ?2)",
        libsql::params![HASH_A, format!("media/{HASH_A}.enc")],
    )
    .await
    .unwrap();

    apply_save_vault_message(&conn, Some("alice"), &save("alice", "v1", CIPHERTEXT, &[HASH_A]))
        .await
        .unwrap();

    // Anyone may run the collect (that is its contract) — the vault ref must
    // hold the object.
    apply_delete_attachment(
        &conn,
        Some("stranger"),
        &AttachmentDeleteBody {
            content_hash: HASH_A.to_string(),
            message_id: None,
        },
    )
    .await
    .unwrap();
    assert!(
        object_exists(&db, HASH_A).await,
        "an object referenced only by a vault entry was collected — the vault lost a file"
    );

    // Delete the entry: refs go with it, and the object becomes collectable.
    apply_delete_vault_message(
        &conn,
        Some("alice"),
        &DeleteVaultMessageBody {
            id: "v1".into(),
            user_id: "alice".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(ref_count(&db, "v1").await, 0);
    apply_delete_attachment(
        &conn,
        Some("stranger"),
        &AttachmentDeleteBody {
            content_hash: HASH_A.to_string(),
            message_id: None,
        },
    )
    .await
    .unwrap();
    assert!(
        !object_exists(&db, HASH_A).await,
        "a fully released object must be collectable"
    );
}

/// A re-save REPLACES the reference set: an edit that drops a file releases
/// it.
#[tokio::test]
async fn a_resave_replaces_the_reference_set() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    apply_save_vault_message(&conn, Some("alice"), &save("alice", "v1", CIPHERTEXT, &[HASH_A]))
        .await
        .unwrap();
    assert_eq!(ref_count(&db, "v1").await, 1);

    apply_save_vault_message(&conn, Some("alice"), &save("alice", "v1", &[0x01], &[]))
        .await
        .unwrap();
    assert_eq!(ref_count(&db, "v1").await, 0, "the dropped file must be released");
}

/// A refused save (cross-user collision) must not touch the standing entry's
/// references either.
#[tokio::test]
async fn a_forbidden_save_leaves_the_references_alone() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    apply_save_vault_message(&conn, Some("alice"), &save("alice", "v1", CIPHERTEXT, &[HASH_A]))
        .await
        .unwrap();
    apply_save_vault_message(&conn, Some("mallory"), &save("mallory", "v1", &[0xEE], &[]))
        .await
        .unwrap();
    assert_eq!(
        ref_count(&db, "v1").await,
        1,
        "a stranger's refused save stripped the entry's references"
    );
}
