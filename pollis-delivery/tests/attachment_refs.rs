//! Server-side attachment reference counting (#690, resolving the second
//! `TODO(#419)`).
//!
//! Attachments are convergently encrypted and deduped by `content_hash`: one
//! `attachment_object` row and one R2 blob back EVERY message that carries the
//! same file, across conversations and users. The old delete was unconditional —
//! deleting one referencing message removed the shared row (and R2 object) out
//! from under every other, 404'ing their attachment.
//!
//! The count is **derived**: a `(content_hash, message_id)` row in
//! `attachment_ref` only counts while a `message_envelope` with that id still
//! exists (see `messages::object_is_referenced`). So releasing a reference is not
//! an action any endpoint performs — it is the deletion of the message envelope,
//! through the already-authorized delete/GC paths. These tests pin the fix:
//!
//!   * `stranding_cannot_happen` — two conversations reference one hash; deleting
//!     the first message keeps the object alive for the second, and only the
//!     second's delete collects it (the Definition-of-Done scenario).
//!   * `a_forged_attachment_delete_cannot_release_a_live_reference` (#690
//!     Blocking 1) — an unauthorized `/v1/attachments/delete` naming another
//!     member's still-live message must NOT strand the object. **FAILS against
//!     the pre-change code**, which deleted the `attachment_ref` row here and then
//!     collected the object.
//!   * `a_reference_cannot_outlive_its_message` (#690 Blocking 2, apply level) —
//!     deleting the envelope makes the object collectable with no attachment-side
//!     call at all. (The GC-path proof lives in `messages.rs`, where the roster
//!     fixtures are — it drives the real `sweep_envelope_gc`.)
//!   * `r2_presign_delete_is_refused_while_referenced` — the R2 chokepoint: the
//!     real `/v1/r2/presign` handler refuses to mint a `delete` for an object
//!     that still has a live reference, and mints one once the last is gone.
//!   * plus the legacy/backfill and idempotency edges.
//!
//! Driven against a local libsql DB exactly as the DS runs, mirroring
//! `envelope_retention.rs` (apply-fn level) and `auth.rs` (real router via
//! `tower::oneshot`).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use pollis_delivery::db::Db;
use pollis_delivery::messages::{
    apply_delete_attachment, apply_register_attachment, object_is_referenced,
    AttachmentDeleteBody, AttachmentRegisterBody,
};
use pollis_delivery::writes::WriteOutcome;
use pollis_delivery::{build_router_with_state, AppState};
use std::sync::Arc;
use tower::ServiceExt as _;

mod common;


async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap()).await.expect("schema");
    db
}

/// Insert the message envelope `msg` (the thing that makes a declaration count).
async fn add_envelope(db: &Db, msg: &str) {
    db.conn().await
        .unwrap()
        .execute(
            // Every NOT NULL column is supplied. The old fixture inserted `id`
            // alone under `OR IGNORE`, which against the real schema is not an
            // insert at all — the constraint violation is swallowed and the
            // envelope never exists, so the reference the test is about would
            // never be held.
            "INSERT OR IGNORE INTO message_envelope \
               (id, conversation_id, sender_id, ciphertext, sent_at) \
             VALUES (?1, 'conv', 'sender', 'ct', '2026-01-01T00:00:00Z')",
            libsql::params![msg.to_string()],
        )
        .await
        .unwrap();
}

/// Delete the message envelope `msg` — the ONLY way a reference is released. This
/// is what `/v1/messages/delete`, envelope GC, retention, and account/group
/// teardown each do to the envelope.
async fn delete_envelope(db: &Db, msg: &str) {
    db.conn().await
        .unwrap()
        .execute(
            "DELETE FROM message_envelope WHERE id = ?1",
            libsql::params![msg.to_string()],
        )
        .await
        .unwrap();
}

/// Register attachment `hash` (at R2 key `key`) as referenced by message `msg`,
/// creating the envelope so the declaration actually counts.
async fn register_ref(db: &Db, hash: &str, key: &str, msg: &str) {
    add_envelope(db, msg).await;
    let body = AttachmentRegisterBody {
        content_hash: hash.into(),
        r2_key: key.into(),
        message_id: Some(msg.into()),
    };
    let out = apply_register_attachment(&db.conn().await.unwrap(), Some("u1"), &body)
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
}

/// The message-delete path: remove the envelope (releases the reference), then
/// call `/v1/attachments/delete` to conditionally collect the shared object.
async fn release_ref(db: &Db, hash: &str, msg: &str) {
    delete_envelope(db, msg).await;
    let body = AttachmentDeleteBody {
        content_hash: hash.into(),
        message_id: Some(msg.into()),
    };
    let out = apply_delete_attachment(&db.conn().await.unwrap(), Some("u1"), &body)
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
}

async fn object_exists(db: &Db, hash: &str) -> bool {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT 1 FROM attachment_object WHERE content_hash = ?1",
            libsql::params![hash.to_string()],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().is_some()
}

async fn referenced(db: &Db, hash: &str) -> bool {
    object_is_referenced(&db.conn().await.unwrap(), hash).await.unwrap()
}

// ── The stranding invariant ───────────────────────────────────────────────────

/// Two conversations reference the same convergent file. Deleting the first's
/// message must NOT collect the shared object; only deleting the last reference
/// does. This is the Definition-of-Done scenario.
///
/// **Fails against the original unconditional delete.** That delete ran
/// `DELETE FROM attachment_object WHERE content_hash = ?` with no predicate, so
/// the assertion "the object survives the first delete" (`object_exists` == true
/// below) was false — the row was gone and conversation Q's attachment 404'd.
#[tokio::test]
async fn stranding_cannot_happen() {
    let db = fresh().await;
    let hash = "hhhh";
    let key = "media/hhhh/pic.enc";

    // Conversation P's message m_p and conversation Q's message m_q both carry
    // the same file. The object row exists once; two live messages reference it.
    register_ref(&db, hash, key, "m_p").await;
    register_ref(&db, hash, key, "m_q").await;
    assert!(object_exists(&db, hash).await, "object must exist after registration");
    assert!(referenced(&db, hash).await, "both messages reference the hash");

    // Conversation P deletes its message. The reference is released (its envelope
    // is gone), but the shared object must SURVIVE — conversation Q still
    // references it.
    release_ref(&db, hash, "m_p").await;
    assert!(
        object_exists(&db, hash).await,
        "the shared attachment_object must survive while conversation Q still \
         references it — the old UNCONDITIONAL delete removed it here, stranding Q \
         (#690)"
    );
    assert!(
        referenced(&db, hash).await,
        "the object is still referenced (m_q lives), so the R2 delete-presign gate \
         must refuse"
    );

    // Conversation Q now deletes its message — the last reference. The object is
    // collected, and the R2 gate opens.
    release_ref(&db, hash, "m_q").await;
    assert!(
        !object_exists(&db, hash).await,
        "with the last reference gone the object must be collected"
    );
    assert!(
        !referenced(&db, hash).await,
        "no references remain, so the R2 delete-presign gate must now permit the delete"
    );
}

/// Registration is idempotent: a retried send (same `content_hash`, same
/// `message_id`) must not double-count, or a single delete could never bring the
/// count to zero.
#[tokio::test]
async fn registration_is_idempotent() {
    let db = fresh().await;
    register_ref(&db, "h", "media/h/f.enc", "m1").await;
    register_ref(&db, "h", "media/h/f.enc", "m1").await;
    assert!(referenced(&db, "h").await, "the hash is referenced by m1");

    release_ref(&db, "h", "m1").await;
    assert!(
        !object_exists(&db, "h").await,
        "deleting the single message clears the idempotent ref and collects the object"
    );
}

// ── Blocking 1: releasing a reference is not an unauthorized action ────────────

/// The deliberate-strand attack this ticket exists to eliminate: an authenticated
/// device POSTs `/v1/attachments/delete` with a `(content_hash, message_id)` it
/// does not own, while that message is still LIVE. It must NOT release anything —
/// the object stays referenced and present.
///
/// **Fails against the pre-change code**, which deleted `attachment_ref
/// (content_hash, message_id)` in `apply_delete_attachment` and then collected the
/// now-unreferenced object. Here the endpoint no longer touches `attachment_ref`;
/// release is the deletion of the envelope, which this forged call cannot do.
#[tokio::test]
async fn a_forged_attachment_delete_cannot_release_a_live_reference() {
    let db = fresh().await;
    let hash = "victimhash";
    // A victim in some other conversation sent a message carrying the file.
    register_ref(&db, hash, "media/victimhash/pic.enc", "m_victim").await;

    // An attacker who is not authorized to delete m_victim calls the delete
    // endpoint naming it. `_authed` is unused by design — this proves the endpoint
    // is not the release chokepoint, so no authz on it can be the fix.
    let out = apply_delete_attachment(
        &db.conn().await.unwrap(),
        Some("attacker"),
        &AttachmentDeleteBody { content_hash: hash.into(), message_id: Some("m_victim".into()) },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));

    assert!(
        object_exists(&db, hash).await,
        "a forged delete must not collect the object — the pre-change code deleted \
         the ref row and collected it here, stranding the victim (#690 Blocking 1)"
    );
    assert!(
        referenced(&db, hash).await,
        "the victim's message still exists, so the object is still referenced and \
         the R2 delete-presign gate must refuse"
    );
}

// ── Blocking 2: a reference cannot outlive its message (apply level) ───────────

/// Deleting the envelope releases the reference by itself — no
/// `/v1/attachments/delete` needed for the release. Once it is gone the object is
/// collectable, closing the "reference outlives the message forever" leak.
///
/// The GC-path proof (driving the real `sweep_envelope_gc`) lives in `messages.rs`
/// where the watermark roster fixtures are; this pins the underlying property at
/// the apply level. **Fails against the pre-change code**, whose
/// `object_is_referenced` read the raw `attachment_ref` row and so stayed `true`
/// after the message was gone.
#[tokio::test]
async fn a_reference_cannot_outlive_its_message() {
    let db = fresh().await;
    let hash = "gone";
    register_ref(&db, hash, "media/gone/f.enc", "m1").await;
    assert!(referenced(&db, hash).await, "referenced while m1 lives");

    // The message is deleted/GC'd — nothing releases the attachment reference
    // explicitly.
    delete_envelope(&db, "m1").await;
    assert!(
        !referenced(&db, hash).await,
        "with m1's envelope gone the reference no longer counts — a reference must \
         not outlive its message (#690 Blocking 2)"
    );

    // The object is now collectable by the ordinary conditional collect.
    apply_delete_attachment(
        &db.conn().await.unwrap(),
        Some("u1"),
        &AttachmentDeleteBody { content_hash: hash.into(), message_id: None },
    )
    .await
    .unwrap();
    assert!(!object_exists(&db, hash).await, "the orphaned object is collectable");
}

/// A pre-#690 client sends no `message_id` on delete. It releases nothing, but
/// the collect is STILL conditional — so it can no longer strand a hash a newer
/// client referenced. A legacy zero-reference object it deletes is collected
/// exactly as before (no worse than today).
#[tokio::test]
async fn old_client_delete_cannot_strand_a_referenced_object() {
    let db = fresh().await;
    // A newer client referenced this hash for its (live) message.
    register_ref(&db, "h", "media/h/f.enc", "m_new").await;

    // An old client (no message_id) tries to delete the object unconditionally.
    let out = apply_delete_attachment(
        &db.conn().await.unwrap(),
        Some("u2"),
        &AttachmentDeleteBody { content_hash: "h".into(), message_id: None },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
    assert!(
        object_exists(&db, "h").await,
        "an old client's message_id-less delete must NOT collect an object a newer \
         client still references — the conditional predicate protects it (#690)"
    );

    // A legacy object with no references at all is collectable, as before.
    db.conn().await
        .unwrap()
        .execute(
            "INSERT INTO attachment_object (content_hash, r2_key) VALUES ('legacy', 'media/legacy/f.enc')",
            (),
        )
        .await
        .unwrap();
    apply_delete_attachment(
        &db.conn().await.unwrap(),
        Some("u2"),
        &AttachmentDeleteBody { content_hash: "legacy".into(), message_id: None },
    )
    .await
    .unwrap();
    assert!(
        !object_exists(&db, "legacy").await,
        "a zero-reference legacy row stays collectable — no worse than today"
    );
}

// ── The R2 presign delete gate (through the real router) ──────────────────────

/// An R2 config whose secrets are present so `broker.r2_ready()` is `Some` and
/// the presign handler proceeds to the reference gate. The signature these mint
/// is never used — the test only asserts the gate's allow/refuse decision.
fn r2_broker() -> pollis_delivery::broker::BrokerConfig {
    pollis_delivery::broker::BrokerConfig {
        r2_endpoint: Some("https://acct.r2.cloudflarestorage.com".into()),
        r2_region: "auto".into(),
        r2_bucket: Some("pollis-media".into()),
        r2_access_key_id: Some("AKIAEXAMPLE".into()),
        r2_secret_access_key: Some("secret-do-not-log".into()),
        ..Default::default()
    }
}

async fn presign_delete(router: &axum::Router, key: &str) -> StatusCode {
    let body = serde_json::json!({ "operation": "delete", "key": key, "user_id": "u1" });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/r2/presign")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    router.clone().oneshot(req).await.unwrap().status()
}

/// The R2 chokepoint: the DS must refuse to mint a `delete` presign for an object
/// that still has a reference, and mint one once the last reference is released.
/// A refcount that protects the Turso row but lets the client blow away the R2
/// object is not a fix (#690).
#[tokio::test]
async fn r2_presign_delete_is_refused_while_referenced() {
    let db = fresh().await;
    let key = "media/hhhh/pic.enc";
    register_ref(&db, "hhhh", key, "m_p").await;
    register_ref(&db, "hhhh", key, "m_q").await;

    // Auth OFF (no signing needed); broker configured so the handler reaches the
    // gate rather than 503'ing.
    let state = AppState::new(Arc::clone(&db), false).with_broker_config(r2_broker());
    let router = build_router_with_state(state);

    assert_eq!(
        presign_delete(&router, key).await,
        StatusCode::FORBIDDEN,
        "a delete presign must be REFUSED while a reference remains"
    );

    // Release one of the two references — one still remains, still refused.
    release_ref(&db, "hhhh", "m_p").await;
    assert_eq!(
        presign_delete(&router, key).await,
        StatusCode::FORBIDDEN,
        "still refused while conversation Q's reference remains"
    );

    // Release the last reference — the object is now collectable, gate opens.
    release_ref(&db, "hhhh", "m_q").await;
    assert_eq!(
        presign_delete(&router, key).await,
        StatusCode::OK,
        "with no reference left the delete presign must be minted"
    );
}

/// A non-media key (no `media/<hash>/…` shape) carries no reference-counted
/// object, so the delete gate passes it through — avatar/icon cleanup must keep
/// working.
#[tokio::test]
async fn r2_presign_delete_passes_non_media_keys() {
    let db = fresh().await;
    let state = AppState::new(Arc::clone(&db), false).with_broker_config(r2_broker());
    let router = build_router_with_state(state);
    assert_eq!(
        presign_delete(&router, "avatars/u1/pic.png").await,
        StatusCode::OK,
        "a non-media key has no reference to consult and must pass the gate"
    );
}

/// `get` is never reference-gated: a referenced object must stay freely
/// fetchable by everyone holding it.
///
/// `put` USED to be ungated too, and that was the bug. These objects are a
/// global convergent dedup, so one `media/<hash>/…` key is shared by every
/// conversation holding that attachment — an ungated `put` let any
/// authenticated user overwrite anyone else's. The AEAD key is derived from the
/// content hash, which every recipient knows, so the replacement decrypted
/// cleanly and recipients rendered chosen plaintext as genuine.
#[tokio::test]
async fn r2_presign_get_is_not_gated_but_put_of_a_referenced_object_is() {
    let db = fresh().await;
    let key = "media/hhhh/pic.enc";
    register_ref(&db, "hhhh", key, "m_p").await;
    let state = AppState::new(Arc::clone(&db), false).with_broker_config(r2_broker());
    let router = build_router_with_state(state);

    let presign = |op: &'static str, key: String| {
        let router = router.clone();
        async move {
            let body = serde_json::json!({ "operation": op, "key": key, "user_id": "u1" });
            let req = Request::builder()
                .method("POST")
                .uri("/v1/r2/presign")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap();
            router.oneshot(req).await.unwrap()
        }
    };

    let get = presign("get", key.to_string()).await;
    assert_eq!(
        get.status(),
        StatusCode::OK,
        "get must stay ungated — everyone holding the attachment fetches this object"
    );
    let bytes = get.into_body().collect().await.unwrap().to_bytes();
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(json.get("url").and_then(|u| u.as_str()).is_some(), "presign returns a url");

    let put_existing = presign("put", key.to_string()).await;
    assert_eq!(
        put_existing.status(),
        StatusCode::FORBIDDEN,
        "put over an ALREADY-REFERENCED object must be refused — that overwrite is \
         the attachment-substitution attack"
    );

    // The legitimate upload still works: a hash nobody references yet. Without
    // this the gate could be "refuse every put" and the assertion above would
    // still pass, while breaking all uploads.
    let put_new = presign("put", "media/brandnewhash/pic.enc".to_string()).await;
    assert_eq!(
        put_new.status(),
        StatusCode::OK,
        "a first upload of an unreferenced hash must still be allowed"
    );
}
