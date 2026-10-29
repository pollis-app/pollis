//! Server-side attachment reference counting (#690).

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

async fn add_envelope(db: &Db, msg: &str) {
    db.conn().await
        .unwrap()
        .execute(
            // Every NOT NULL column is supplied, or `OR IGNORE` silently inserts nothing.
            "INSERT OR IGNORE INTO message_envelope \
               (id, conversation_id, sender_id, ciphertext, sent_at) \
             VALUES (?1, 'conv', 'sender', 'ct', '2026-01-01T00:00:00Z')",
            libsql::params![msg.to_string()],
        )
        .await
        .unwrap();
}

/// The only way a reference is released.
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

async fn register_ref(db: &Db, hash: &str, key: &str, msg: &str) {
    add_envelope(db, msg).await;
    let body = AttachmentRegisterBody::ForMessage {
        content_hash: hash.into(),
        r2_key: key.into(),
        message_id: msg.into(),
        delete_token: None,
    };
    let out = apply_register_attachment(&db.conn().await.unwrap(), Some("sender"), &body)
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
}

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

/// Deleting the first referencing message must not collect the shared object.
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

/// A double-count would keep a single delete from reaching zero.
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

/// A forged delete naming another's live message must release nothing.
#[tokio::test]
async fn a_forged_attachment_delete_cannot_release_a_live_reference() {
    let db = fresh().await;
    let hash = "victimhash";
    register_ref(&db, hash, "media/victimhash/pic.enc", "m_victim").await;

    // `_authed` is unused: release happens via the envelope, not this endpoint.
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

/// Deleting the envelope alone releases the reference (GC path is tested in `messages.rs`).
#[tokio::test]
async fn a_reference_cannot_outlive_its_message() {
    let db = fresh().await;
    let hash = "gone";
    register_ref(&db, hash, "media/gone/f.enc", "m1").await;
    assert!(referenced(&db, hash).await, "referenced while m1 lives");

    delete_envelope(&db, "m1").await;
    assert!(
        !referenced(&db, hash).await,
        "with m1's envelope gone the reference no longer counts — a reference must \
         not outlive its message (#690 Blocking 2)"
    );

    apply_delete_attachment(
        &db.conn().await.unwrap(),
        Some("u1"),
        &AttachmentDeleteBody { content_hash: hash.into(), message_id: None },
    )
    .await
    .unwrap();
    assert!(!object_exists(&db, hash).await, "the orphaned object is collectable");
}

/// A pre-#690 delete with no `message_id` releases nothing and stays conditional.
#[tokio::test]
async fn old_client_delete_cannot_strand_a_referenced_object() {
    let db = fresh().await;
    register_ref(&db, "h", "media/h/f.enc", "m_new").await;

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

/// Secrets present so the handler reaches the reference gate; signatures go unused.
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

/// A refcount that guards the row but not the R2 object is no fix (#690).
#[tokio::test]
async fn r2_presign_delete_is_refused_while_referenced() {
    let db = fresh().await;
    let key = "media/hhhh/pic.enc";
    register_ref(&db, "hhhh", key, "m_p").await;
    register_ref(&db, "hhhh", key, "m_q").await;

    let state = AppState::new(Arc::clone(&db), false).with_broker_config(r2_broker());
    let router = build_router_with_state(state);

    assert_eq!(
        presign_delete(&router, key).await,
        StatusCode::FORBIDDEN,
        "a delete presign must be REFUSED while a reference remains"
    );

    release_ref(&db, "hhhh", "m_p").await;
    assert_eq!(
        presign_delete(&router, key).await,
        StatusCode::FORBIDDEN,
        "still refused while conversation Q's reference remains"
    );

    release_ref(&db, "hhhh", "m_q").await;
    assert_eq!(
        presign_delete(&router, key).await,
        StatusCode::OK,
        "with no reference left the delete presign must be minted"
    );
}

/// Non-media keys pass the gate so avatar/icon cleanup keeps working.
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

/// `put` is gated: keys are shared via convergent dedup and the AEAD key derives from the hash.
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
            let body = serde_json::json!({
                "operation": op,
                "key": key,
                "user_id": "u1",
                "content_length": 1024,
            });
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

    // Proves the gate isn't "refuse every put".
    let fresh_hash = "c".repeat(64);
    let put_new = presign("put", format!("media/{fresh_hash}.enc")).await;
    assert_eq!(
        put_new.status(),
        StatusCode::OK,
        "a first upload of an unreferenced hash must still be allowed"
    );
}

/// M5: a declaration is bound to `message_envelope.sender_id`.
#[tokio::test(flavor = "multi_thread")]
async fn a_reference_cannot_be_declared_for_another_users_message() {
    let db = fresh().await;
    let hash = "f".repeat(64);
    add_envelope(&db, "m-owned-by-sender").await;

    let body = AttachmentRegisterBody::ForMessage {
        content_hash: hash.clone(),
        r2_key: format!("media/{hash}.enc"),
        message_id: "m-owned-by-sender".into(),
        delete_token: None,
    };
    let out = apply_register_attachment(&db.conn().await.unwrap(), Some("mallory"), &body)
        .await
        .unwrap();
    assert!(
        matches!(out, WriteOutcome::Forbidden),
        "a declaration naming another account's message must be refused"
    );
    assert!(
        !referenced(&db, &hash).await,
        "the forged declaration must not pin the object"
    );
    assert!(
        !object_exists(&db, &hash).await,
        "a refused registration writes nothing at all"
    );

    let out = apply_register_attachment(&db.conn().await.unwrap(), Some("sender"), &body)
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
    assert!(referenced(&db, &hash).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reference_cannot_be_declared_for_a_message_that_does_not_exist() {
    let db = fresh().await;
    let hash = "e".repeat(64);
    let body = AttachmentRegisterBody::ForMessage {
        content_hash: hash.clone(),
        r2_key: format!("media/{hash}.enc"),
        message_id: "no-such-message".into(),
        delete_token: None,
    };
    let out = apply_register_attachment(&db.conn().await.unwrap(), Some("sender"), &body)
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Forbidden));
    assert!(!referenced(&db, &hash).await);
}

/// Sealed envelopes name no sender, so the author proves itself with the delete token (#1086).
#[tokio::test(flavor = "multi_thread")]
async fn the_author_of_a_sealed_message_declares_its_reference_with_the_capability() {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};

    let db = fresh().await;
    let hash = "c".repeat(64);
    let token = "the-author-only-capability";
    let stored = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(token.as_bytes()));
    db.conn()
        .await
        .unwrap()
        .execute(
            "INSERT INTO message_envelope \
               (id, conversation_id, sender_id, ciphertext, sent_at, sealed, delete_token_hash) \
             VALUES ('m-sealed', 'conv', 'sealed', 'ct', '2026-01-01T00:00:00Z', 1, ?1)",
            libsql::params![stored],
        )
        .await
        .unwrap();
    let body = |delete_token: Option<&str>| AttachmentRegisterBody::ForMessage {
        content_hash: hash.clone(),
        r2_key: format!("media/{hash}.enc"),
        message_id: "m-sealed".into(),
        delete_token: delete_token.map(str::to_string),
    };

    for (label, presented) in [("no token", None), ("a wrong token", Some("guess"))] {
        let out = apply_register_attachment(&db.conn().await.unwrap(), Some("mallory"), &body(presented))
            .await
            .unwrap();
        assert!(
            matches!(out, WriteOutcome::Forbidden),
            "{label} must not declare a reference on a sealed message"
        );
        assert!(!referenced(&db, &hash).await, "{label} must not pin the object");
    }

    let out = apply_register_attachment(&db.conn().await.unwrap(), Some("alice"), &body(Some(token)))
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Ok), "the author's capability must be accepted");
    assert!(
        referenced(&db, &hash).await,
        "the sealed message's attachment must now be counted"
    );
}

/// Older clients omit `delete_token`; it is skipped on the wire when absent.
#[test]
fn the_capability_is_optional_on_the_wire() {
    let old: AttachmentRegisterBody =
        serde_json::from_str(r#"{"content_hash":"h","r2_key":"media/h.enc","message_id":"m"}"#).unwrap();
    assert_eq!(old.message_id(), Some("m"));
    assert_eq!(old.delete_token(), None);
    let json = serde_json::to_string(&old).unwrap();
    assert!(!json.contains("delete_token"), "{json}");
}

/// A messageless registration carries no reference, so any device may make it.
#[tokio::test(flavor = "multi_thread")]
async fn an_object_only_registration_stays_ungated() {
    let db = fresh().await;
    let hash = "d".repeat(64);
    let body = AttachmentRegisterBody::ObjectOnly {
        content_hash: hash.clone(),
        r2_key: format!("media/{hash}.enc"),
    };
    let out = apply_register_attachment(&db.conn().await.unwrap(), Some("anyone"), &body)
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
    assert!(object_exists(&db, &hash).await);
    assert!(
        !referenced(&db, &hash).await,
        "an object with no message reference is not pinned"
    );
}

async fn add_envelope_from(db: &Db, msg: &str, sender: &str) {
    db.conn().await
        .unwrap()
        .execute(
            "INSERT OR IGNORE INTO message_envelope \
               (id, conversation_id, sender_id, ciphertext, sent_at) \
             VALUES (?1, 'conv', ?2, 'ct', '2026-01-01T00:00:00Z')",
            libsql::params![msg.to_string(), sender.to_string()],
        )
        .await
        .unwrap();
}

async fn register_ref_as(db: &Db, hash: &str, key: &str, msg: &str, actor: &str) {
    add_envelope_from(db, msg, actor).await;
    let body = AttachmentRegisterBody::ForMessage {
        content_hash: hash.into(),
        r2_key: key.into(),
        message_id: msg.into(),
        delete_token: None,
    };
    let out = apply_register_attachment(&db.conn().await.unwrap(), Some(actor), &body)
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
}

async fn collect_as(db: &Db, hash: &str, actor: &str) {
    let out = apply_delete_attachment(
        &db.conn().await.unwrap(),
        Some(actor),
        &AttachmentDeleteBody { content_hash: hash.into(), message_id: None },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
}

async fn presign_delete_as(router: &axum::Router, key: &str, user: &str) -> StatusCode {
    let body = serde_json::json!({ "operation": "delete", "key": key, "user_id": user });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/r2/presign")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    router.clone().oneshot(req).await.unwrap().status()
}

/// Two accounts dedup onto one blob; the first sender's delete must leave both row and R2 object.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_senders_attachment_survives_the_first_senders_delete() {
    let db = fresh().await;
    let hash = "a".repeat(64);
    let key = format!("media/{hash}.enc");

    register_ref_as(&db, &hash, &key, "m-alice", "alice").await;
    register_ref_as(&db, &hash, &key, "m-bob", "bob").await;

    delete_envelope(&db, "m-alice").await;
    collect_as(&db, &hash, "alice").await;

    assert!(
        object_exists(&db, &hash).await,
        "bob's live message still carries this file — collecting the shared row \
         here would 404 his attachment"
    );
    assert!(
        referenced(&db, &hash).await,
        "bob's reference is live, so the object is still referenced"
    );

    let state = AppState::new(Arc::clone(&db), false).with_broker_config(r2_broker());
    let router = build_router_with_state(state);
    for user in ["alice", "bob", "carol"] {
        assert_eq!(
            presign_delete_as(&router, &key, user).await,
            StatusCode::FORBIDDEN,
            "no account — the first sender included — may destroy bytes another \
             account's live message depends on"
        );
    }

    delete_envelope(&db, "m-bob").await;
    assert_eq!(
        presign_delete_as(&router, &key, "alice").await,
        StatusCode::OK,
        "with the last reference gone the delete presign must be minted"
    );
    collect_as(&db, &hash, "alice").await;
    assert!(!object_exists(&db, &hash).await, "the unreferenced object is collected");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_reference_blocks_every_actor() {
    let db = fresh().await;
    let hash = "b".repeat(64);
    let key = format!("media/{hash}.enc");

    register_ref_as(&db, &hash, &key, "m-alice", "alice").await;
    register_ref_as(&db, &hash, &key, "m-bob", "bob").await;
    delete_envelope(&db, "m-alice").await;

    for actor in ["alice", "bob", "carol"] {
        collect_as(&db, &hash, actor).await;
        assert!(
            object_exists(&db, &hash).await,
            "{actor} must not collect an object bob's live message references"
        );
    }

    let out = apply_delete_attachment(
        &db.conn().await.unwrap(),
        None,
        &AttachmentDeleteBody { content_hash: hash.clone(), message_id: None },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));
    assert!(object_exists(&db, &hash).await, "the no-auth path keeps the same rule");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_senders_own_second_message_still_blocks_their_delete() {
    let db = fresh().await;
    let hash = "c".repeat(64);
    let key = format!("media/{hash}.enc");

    register_ref_as(&db, &hash, &key, "m-one", "alice").await;
    register_ref_as(&db, &hash, &key, "m-two", "alice").await;

    delete_envelope(&db, "m-one").await;
    collect_as(&db, &hash, "alice").await;
    assert!(
        object_exists(&db, &hash).await,
        "alice's second message still carries the file"
    );

    delete_envelope(&db, "m-two").await;
    collect_as(&db, &hash, "alice").await;
    assert!(!object_exists(&db, &hash).await, "with her last reference gone it collects");
}

/// Migration 000031's attribution columns are dead (000032): never written.
#[tokio::test(flavor = "multi_thread")]
async fn registration_records_no_ownership_attribution() {
    let db = fresh().await;
    let hash = "f".repeat(64);
    let key = format!("media/{hash}.enc");
    register_ref_as(&db, &hash, &key, "m-alice", "alice").await;

    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT (SELECT COUNT(*) FROM attachment_object WHERE uploaded_by IS NOT NULL), \
                    (SELECT COUNT(*) FROM attachment_ref WHERE registered_by IS NOT NULL)",
            (),
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().expect("one row");
    assert_eq!(row.get::<i64>(0).unwrap(), 0, "no uploader is recorded");
    assert_eq!(row.get::<i64>(1).unwrap(), 0, "no registrar is recorded");
}

/// No account, first uploader included, may substitute bytes under a live object.
#[tokio::test(flavor = "multi_thread")]
async fn the_put_substitution_gate_binds_the_first_uploader_too() {
    let db = fresh().await;
    let hash = "e".repeat(64);
    let key = format!("media/{hash}.enc");

    register_ref_as(&db, &hash, &key, "m-alice", "alice").await;
    register_ref_as(&db, &hash, &key, "m-mallory", "mallory").await;
    delete_envelope(&db, "m-alice").await;

    let state = AppState::new(Arc::clone(&db), false).with_broker_config(r2_broker());
    let router = build_router_with_state(state);

    let body = serde_json::json!({
        "operation": "put",
        "key": key,
        "user_id": "alice",
        "content_length": 64,
    });
    let req = Request::builder()
        .method("POST")
        .uri("/v1/r2/presign")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    assert_eq!(
        router.oneshot(req).await.unwrap().status(),
        StatusCode::FORBIDDEN,
        "nobody may overwrite an object others still hold"
    );
}
