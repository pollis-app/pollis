//! `POST /v1/r2/presign` mints a credential-free URL for whatever key it is
//! handed, so the key, the method and the size are the whole of the
//! authorization.
//!
//! Before these tests it checked almost none of that. Any authenticated device
//! could:
//!
//!   * name **any key at all** — a prefix the product does not use, a key with
//!     `..` in it, a key invented purely to park bytes on someone else's bucket;
//!   * PUT **any number of bytes**, because `content_length` was optional
//!     outside the emoji path and only R2 ever counts them;
//!   * overwrite or delete **another user's avatar** or **another group's icon**,
//!     which were not gated at all (the reference check only looks at media and
//!     emoji, and an avatar has no reference count);
//!   * overwrite or delete **any media object**, because the hash extractor
//!     handled only the pre-#762 `media/<hash>/<name>` shape and returned
//!     `"<hash>.enc"` for today's `media/<hash>.enc` — a value matching no stored
//!     content hash, so the #690 integrity gate never fired for any object
//!     written since.
//!
//! Every test below fails against the pre-fix handler.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pollis_delivery::broker::{
    parse_r2_key, BrokerConfig, R2Family, R2_MEDIA_MAX_BYTES, R2_PUBLIC_IMAGE_MAX_BYTES,
};
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;

const HASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

/// alice is an admin of group `g1`; bob is a plain member; mallory is neither.
async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("r2.db").await;
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    conn.execute_batch(
        "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin');
         INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob');",
    )
    .await
    .unwrap();
    db
}

fn broker() -> BrokerConfig {
    BrokerConfig {
        r2_endpoint: Some("https://acct.r2.cloudflarestorage.com".to_string()),
        r2_region: "auto".to_string(),
        r2_bucket: Some("pollis-media".to_string()),
        r2_access_key_id: Some("AKIAIOSFODNN7EXAMPLE".to_string()),
        r2_secret_access_key: Some("wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string()),
        ..Default::default()
    }
}

/// Auth OFF, so the acting user is the body's `user_id` — the same identity the
/// handler derives from a verified signature when auth is on.
async fn presign(
    db: &common::TempDb,
    user: &str,
    operation: &str,
    key: &str,
    content_length: Option<u64>,
) -> StatusCode {
    let state = AppState::new(db.arc(), false).with_broker_config(broker());
    let app = build_router_with_state(state);
    let mut body = serde_json::json!({
        "operation": operation,
        "key": key,
        "user_id": user,
    });
    if let Some(n) = content_length {
        body["content_length"] = serde_json::json!(n);
    }
    let req = Request::builder()
        .method("POST")
        .uri("/v1/r2/presign")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    app.oneshot(req).await.unwrap().status()
}

// ── The key allow-list ───────────────────────────────────────────────────────

/// Every shape the product writes parses, including the two legacy ones that
/// must stay READABLE.
#[test]
fn the_four_families_parse() {
    let key = format!("media/{HASH_A}.enc");
    let media = parse_r2_key(&key).unwrap();
    assert_eq!(media.family, R2Family::Media);
    assert_eq!(media.content_hash, Some(HASH_A));

    let key = format!("media/{HASH_A}/report.pdf.enc");
    let legacy_media = parse_r2_key(&key).unwrap();
    assert_eq!(legacy_media.content_hash, Some(HASH_A));

    let key = format!("emoji/{HASH_A}.webp");
    let emoji = parse_r2_key(&key).unwrap();
    assert_eq!(emoji.family, R2Family::Emoji);
    assert_eq!(emoji.content_hash, Some(HASH_A));

    let key = format!("avatars/alice/{HASH_A}.png");
    let avatar = parse_r2_key(&key).unwrap();
    assert_eq!(avatar.family, R2Family::Avatar);
    assert_eq!(avatar.owner, Some("alice"));

    // Legacy, pre-#874: mutable key, no digest. Readable, never writable.
    let legacy_avatar = parse_r2_key("avatars/alice").unwrap();
    assert_eq!(legacy_avatar.owner, Some("alice"));
    assert!(!legacy_avatar.content_addressed);

    let key = format!("group-icons/g1/{HASH_A}.png");
    let icon = parse_r2_key(&key).unwrap();
    assert_eq!(icon.family, R2Family::GroupIcon);
    assert_eq!(icon.owner, Some("g1"));
}

/// Anything else is not an object this service stores.
#[test]
fn keys_outside_the_allow_list_do_not_parse() {
    for key in [
        "",
        "secrets/prod.env",
        "media",
        "media/",
        "/media/x",
        "media//x",
        "media/../secrets/prod.env",
        "avatars/../../etc/passwd",
        "emoji/x?y",
        "media/x y.enc",
        "backups/2026-01-01.tar",
    ] {
        assert!(
            parse_r2_key(key).is_none(),
            "the DS must not sign a URL for {key:?}"
        );
    }
}

#[tokio::test]
async fn an_arbitrary_key_is_refused_on_every_operation() {
    let db = fresh().await;
    for op in ["get", "put", "delete"] {
        assert_eq!(
            presign(&db, "alice", op, "backups/prod.tar", Some(10)).await,
            StatusCode::BAD_REQUEST,
            "{op} on an unknown prefix must be refused"
        );
    }
}

// ── PUT: content-addressed, and exactly this many bytes ──────────────────────

#[tokio::test]
async fn a_put_without_a_content_length_is_refused() {
    let db = fresh().await;
    assert_eq!(
        presign(&db, "alice", "put", &format!("media/{HASH_A}.enc"), None).await,
        StatusCode::BAD_REQUEST,
        "an unbounded put is permission to write an object of any size"
    );
}

#[tokio::test]
async fn a_put_above_the_cap_is_refused_and_at_the_cap_is_signed() {
    let db = fresh().await;
    let key = format!("media/{HASH_A}.enc");
    assert_eq!(
        presign(&db, "alice", "put", &key, Some(R2_MEDIA_MAX_BYTES + 1)).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        presign(&db, "alice", "put", &key, Some(0)).await,
        StatusCode::BAD_REQUEST,
        "a zero-length put writes an object that decrypts to nothing"
    );
    assert_eq!(
        presign(&db, "alice", "put", &key, Some(R2_MEDIA_MAX_BYTES)).await,
        StatusCode::OK
    );
}

/// A public image gets the much smaller ceiling: nothing renders 100 MiB of
/// avatar.
#[tokio::test]
async fn an_avatar_put_uses_the_public_image_cap() {
    let db = fresh().await;
    let key = format!("avatars/alice/{HASH_A}.png");
    assert_eq!(
        presign(&db, "alice", "put", &key, Some(R2_PUBLIC_IMAGE_MAX_BYTES + 1)).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        presign(&db, "alice", "put", &key, Some(R2_PUBLIC_IMAGE_MAX_BYTES)).await,
        StatusCode::OK
    );
}

/// A legacy mutable key stays readable and stops being writable — overwriting
/// `avatars/<uid>` replaces that user's picture everywhere, and the key says
/// nothing about what the bytes should be.
#[tokio::test]
async fn a_legacy_key_is_readable_but_not_writable() {
    let db = fresh().await;
    assert_eq!(
        presign(&db, "alice", "get", "avatars/alice", None).await,
        StatusCode::OK
    );
    assert_eq!(
        presign(&db, "alice", "put", "avatars/alice", Some(1024)).await,
        StatusCode::BAD_REQUEST
    );
}

// ── Owned objects ────────────────────────────────────────────────────────────

#[tokio::test]
async fn nobody_may_write_or_delete_another_users_avatar() {
    let db = fresh().await;
    let alices = format!("avatars/alice/{HASH_A}.png");

    assert_eq!(
        presign(&db, "mallory", "put", &alices, Some(1024)).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        presign(&db, "mallory", "delete", &alices, None).await,
        StatusCode::FORBIDDEN
    );
    // Reading someone's avatar is the point of an avatar.
    assert_eq!(
        presign(&db, "mallory", "get", &alices, None).await,
        StatusCode::OK
    );
    // And alice may still manage her own.
    assert_eq!(
        presign(&db, "alice", "put", &alices, Some(1024)).await,
        StatusCode::OK
    );
    assert_eq!(
        presign(&db, "alice", "delete", &alices, None).await,
        StatusCode::OK
    );
}

#[tokio::test]
async fn only_a_group_admin_may_write_or_delete_its_icon() {
    let db = fresh().await;
    let icon = format!("group-icons/g1/{HASH_A}.png");

    assert_eq!(
        presign(&db, "bob", "put", &icon, Some(1024)).await,
        StatusCode::FORBIDDEN,
        "a plain member is not the group's identity"
    );
    assert_eq!(
        presign(&db, "mallory", "delete", &icon, None).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        presign(&db, "bob", "get", &icon, None).await,
        StatusCode::OK
    );
    assert_eq!(
        presign(&db, "alice", "put", &icon, Some(1024)).await,
        StatusCode::OK
    );
}

// ── Shared objects ───────────────────────────────────────────────────────────

/// THE DEAD GATE. `media/<hash>.enc` is what every upload since #762 writes, and
/// the reference check never saw it, so any authenticated user could overwrite
/// or delete any attachment in the bucket. The key derives from the hash, so a
/// substituted blob decrypts cleanly for every recipient.
#[tokio::test]
async fn a_referenced_media_object_can_be_neither_overwritten_nor_deleted() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    conn.execute_batch(&format!(
        "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) \
             VALUES ('m1', 'g1', 'alice', 'x', '2026-01-01T00:00:00.000000000+00:00');
         INSERT INTO attachment_object (content_hash, r2_key) \
             VALUES ('{HASH_A}', 'media/{HASH_A}.enc');
         INSERT INTO attachment_ref (content_hash, message_id) VALUES ('{HASH_A}', 'm1');"
    ))
    .await
    .unwrap();

    let key = format!("media/{HASH_A}.enc");
    assert_eq!(
        presign(&db, "mallory", "put", &key, Some(1024)).await,
        StatusCode::FORBIDDEN,
        "an overwrite of a live attachment is chosen plaintext for every recipient"
    );
    assert_eq!(
        presign(&db, "mallory", "delete", &key, None).await,
        StatusCode::FORBIDDEN,
        "deleting a live attachment 404s it for every conversation holding it"
    );

    // An UNreferenced object is still collectable — that is the cleanup path
    // after the last message carrying it is deleted.
    assert_eq!(
        presign(&db, "alice", "delete", &format!("media/{HASH_B}.enc"), None).await,
        StatusCode::OK
    );
}
