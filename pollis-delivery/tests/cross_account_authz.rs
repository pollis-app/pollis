//! **Cross-account refusal is a property, not an accident.**
//!
//! "User B cannot act on user A's resource", one test per family, with real signatures and auth on.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use http_body_util::BodyExt as _;
use ml_dsa::{Keypair, MlDsa44, Signer, SigningKey, VerifyingKey};
use pollis_delivery::auth::canonical_message;
use pollis_delivery::db::Db;
use pollis_delivery::{build_router_with_state, AppState};
use rand_core::{OsRng, RngCore as _};
use tower::ServiceExt as _;

mod common;

fn gen_signing_key() -> SigningKey<MlDsa44> {
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    SigningKey::<MlDsa44>::from_seed(&seed.into())
}

fn b64(b: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(b)
}

fn pq_pub(vk: &VerifyingKey<MlDsa44>) -> Vec<u8> {
    vk.encode().to_vec()
}

async fn fresh_db() -> common::TempDb {
    let db = common::TempDb::open("delivery.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap())
        .await
        .expect("schema");
    db
}

struct Account {
    user_id: String,
    device_id: String,
    key: SigningKey<MlDsa44>,
}

async fn seed_account(db: &Db, user_id: &str) -> Account {
    let key = gen_signing_key();
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO users (id, email, username) VALUES (?1, ?1 || '@x', ?1)",
        libsql::params![user_id],
    )
    .await
    .unwrap();
    let device_id = format!("{user_id}-dev");
    conn.execute(
        "INSERT INTO user_device (device_id, user_id, mls_signature_pub_pq) VALUES (?1, ?2, ?3)",
        libsql::params![device_id.clone(), user_id, pq_pub(&key.verifying_key())],
    )
    .await
    .unwrap();
    Account {
        user_id: user_id.to_string(),
        device_id,
        key,
    }
}

/// Put `user` in `group_id`'s roster; a group id is a valid MLS conversation id.
async fn seed_member(db: &Db, group_id: &str, user: &str, role: &str) {
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO conversation (id, kind) VALUES (?1, 'group')",
        libsql::params![group_id],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO groups (id, name, owner_id) VALUES (?1, ?1, ?2)",
        libsql::params![group_id, user],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT OR REPLACE INTO group_member (group_id, user_id, role) VALUES (?1, ?2, ?3)",
        libsql::params![group_id, user, role],
    )
    .await
    .unwrap();
}

fn signed(path: &str, who: &Account, body: &serde_json::Value) -> Request<Body> {
    let bytes = serde_json::to_vec(body).unwrap();
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

async fn post(db: &common::TempDb, who: &Account, path: &str, body: serde_json::Value) -> StatusCode {
    let resp = router(db).oneshot(signed(path, who, &body)).await.unwrap();
    let status = resp.status();
    let _ = resp.into_body().collect().await;
    status
}

async fn post_json(
    db: &common::TempDb,
    who: &Account,
    path: &str,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let resp = router(db).oneshot(signed(path, who, &body)).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn scalar(db: &Db, sql: &str, params: impl libsql::params::IntoParams) -> Option<String> {
    let conn = db.conn().await.unwrap();
    let mut rows = conn.query(sql, params).await.unwrap();
    rows.next()
        .await
        .unwrap()
        .map(|r| r.get::<String>(0).unwrap())
}

async fn count(db: &Db, sql: &str, params: impl libsql::params::IntoParams) -> i64 {
    let conn = db.conn().await.unwrap();
    let mut rows = conn.query(sql, params).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

fn bundle(conv: &str, epoch: i64, sender: &str, recipient: &str, welcome: &[u8]) -> serde_json::Value {
    serde_json::json!({
        "conversation_id": conv,
        "based_on_epoch": epoch,
        "sender_id": sender,
        "commit": b64(format!("commit-{conv}-{epoch}-{sender}").as_bytes()),
        "group_info": b64(b"group-info"),
        "welcomes": [{
            "recipient_id": recipient,
            "recipient_device_id": format!("{recipient}-dev"),
            "welcome": b64(welcome),
        }],
    })
}

/// C1: a commit bundle's Welcome for a non-member device is refused whole.
#[tokio::test(flavor = "multi_thread")]
async fn a_commit_bundle_cannot_park_a_welcome_on_a_non_member() {
    let db = fresh_db().await;
    let mallory = seed_account(&db, "mallory").await;
    let _victim = seed_account(&db, "victim").await;
    seed_member(&db, "mallory-grp", "mallory", "admin").await;

    let status = post(
        &db,
        &mallory,
        "/v1/commits",
        bundle("mallory-grp", 0, "mallory", "victim", b"forged-welcome"),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "a Welcome to a non-member must be refused");

    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM mls_welcome WHERE recipient_id = 'victim'", ()).await,
        0,
        "no forged Welcome row may exist"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM mls_commit_log WHERE conversation_id = 'mallory-grp'",
            ()
        )
        .await,
        0,
        "the refusal is the WHOLE bundle — the commit must not land either"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM mls_group_info WHERE conversation_id = 'mallory-grp'",
            ()
        )
        .await,
        0,
        "nor the GroupInfo"
    );
}

/// The control: a member recipient still lands.
#[tokio::test(flavor = "multi_thread")]
async fn a_welcome_to_a_current_member_still_lands() {
    let db = fresh_db().await;
    let alice = seed_account(&db, "alice").await;
    let _dave = seed_account(&db, "dave").await;
    seed_member(&db, "grp", "alice", "member").await;
    seed_member(&db, "grp", "dave", "member").await;

    let status = post(&db, &alice, "/v1/commits", bundle("grp", 0, "alice", "dave", b"real-welcome")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM mls_welcome WHERE recipient_id = 'dave'", ()).await,
        1
    );
}

/// The gate is the desired roster: an invitee is Welcomed at invite time.
#[tokio::test(flavor = "multi_thread")]
async fn a_welcome_to_a_pending_invitee_is_admitted() {
    let db = fresh_db().await;
    let alice = seed_account(&db, "alice").await;
    let _dave = seed_account(&db, "dave").await;
    seed_member(&db, "grp", "alice", "admin").await;
    db.conn()
        .await
        .unwrap()
        .execute(
            "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) \
             VALUES ('inv-1', 'grp', 'alice', 'dave')",
            (),
        )
        .await
        .unwrap();

    assert_eq!(
        post(&db, &alice, "/v1/commits", bundle("grp", 0, "alice", "dave", b"staged")).await,
        StatusCode::OK,
        "a Welcome staged for a pending invitee must be admitted"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM mls_welcome WHERE recipient_id = 'dave'", ()).await,
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_oversize_welcome_is_refused() {
    let db = fresh_db().await;
    let alice = seed_account(&db, "alice").await;
    let _dave = seed_account(&db, "dave").await;
    seed_member(&db, "grp", "alice", "member").await;
    seed_member(&db, "grp", "dave", "member").await;

    let huge = vec![0u8; pollis_delivery::writes::WELCOME_MAX_BYTES + 1];
    let status = post(&db, &alice, "/v1/commits", bundle("grp", 0, "alice", "dave", &huge)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM mls_commit_log WHERE conversation_id = 'grp'", ()).await,
        0
    );
}

/// The tuple is UNIQUE, so writing it overwrites: Bob may not replace Alice's pending Welcome.
#[tokio::test(flavor = "multi_thread")]
async fn a_member_cannot_steal_another_members_pending_welcome() {
    let db = fresh_db().await;
    let alice = seed_account(&db, "alice").await;
    let bob = seed_account(&db, "bob").await;
    let _dave = seed_account(&db, "dave").await;
    for u in ["alice", "bob", "dave"] {
        seed_member(&db, "grp", u, "member").await;
    }

    assert_eq!(
        post(&db, &alice, "/v1/commits", bundle("grp", 0, "alice", "dave", b"alice-welcome")).await,
        StatusCode::OK
    );
    assert_eq!(
        post(&db, &bob, "/v1/commits", bundle("grp", 1, "bob", "dave", b"bob-welcome")).await,
        StatusCode::FORBIDDEN
    );

    let blob = {
        let conn = db.conn().await.unwrap();
        let mut rows = conn
            .query(
                "SELECT welcome_data FROM mls_welcome WHERE recipient_id = 'dave'",
                (),
            )
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<Vec<u8>>(0).unwrap()
    };
    assert_eq!(blob, b"alice-welcome", "Alice's pending Welcome must survive");
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM mls_commit_log WHERE conversation_id = 'grp'", ()).await,
        1,
        "Bob's whole bundle rolled back, so his commit did not claim epoch 1"
    );
}

/// An admin of the group may re-drive another member's Welcome.
#[tokio::test(flavor = "multi_thread")]
async fn an_admin_may_re_drive_another_members_pending_welcome() {
    let db = fresh_db().await;
    let alice = seed_account(&db, "alice").await;
    let boss = seed_account(&db, "boss").await;
    let _dave = seed_account(&db, "dave").await;
    seed_member(&db, "grp", "alice", "member").await;
    seed_member(&db, "grp", "boss", "admin").await;
    seed_member(&db, "grp", "dave", "member").await;

    assert_eq!(
        post(&db, &alice, "/v1/commits", bundle("grp", 0, "alice", "dave", b"alice-welcome")).await,
        StatusCode::OK
    );
    assert_eq!(
        post(&db, &boss, "/v1/commits", bundle("grp", 1, "boss", "dave", b"boss-welcome")).await,
        StatusCode::OK
    );
}

/// `/v1/welcomes/fetch` carries `conversation_id` so clients can check the blob's GroupId.
#[tokio::test(flavor = "multi_thread")]
async fn a_fetched_welcome_names_the_conversation_it_was_filed_under() {
    let db = fresh_db().await;
    let alice = seed_account(&db, "alice").await;
    let dave = seed_account(&db, "dave").await;
    seed_member(&db, "grp", "alice", "member").await;
    seed_member(&db, "grp", "dave", "member").await;

    assert_eq!(
        post(&db, &alice, "/v1/commits", bundle("grp", 0, "alice", "dave", b"real-welcome")).await,
        StatusCode::OK
    );

    let (status, body) = post_json(
        &db,
        &dave,
        "/v1/welcomes/fetch",
        serde_json::json!({ "device_id": dave.device_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let welcomes = body["welcomes"].as_array().expect("welcomes array");
    assert_eq!(welcomes.len(), 1);
    assert_eq!(
        welcomes[0]["conversation_id"], "grp",
        "the fetched row must name the conversation it was filed under, so the \
         client can refuse a blob whose GroupId disagrees with it"
    );
}

/// #1412: the drain reports the signing device's own unclaimed packages in the asked suite.
#[tokio::test(flavor = "multi_thread")]
async fn the_welcome_drain_reports_the_signing_devices_own_pool() {
    let db = fresh_db().await;
    let dave = seed_account(&db, "dave").await;
    let conn = db.conn().await.unwrap();
    for (i, device, suite, claimed) in [
        (0, dave.device_id.as_str(), 7, 0),
        (1, dave.device_id.as_str(), 7, 0),
        (2, dave.device_id.as_str(), 7, 1),
        (3, dave.device_id.as_str(), 1, 0),
        (4, "dave-other-device", 7, 0),
    ] {
        conn.execute(
            "INSERT INTO mls_key_package \
                 (ref_hash, user_id, key_package, device_id, ciphersuite, claimed) \
             VALUES (?1, 'dave', X'00', ?2, ?3, ?4)",
            libsql::params![format!("kp-{i}"), device, suite, claimed],
        )
        .await
        .unwrap();
    }

    let (status, body) = post_json(
        &db,
        &dave,
        "/v1/welcomes/fetch",
        serde_json::json!({ "device_id": dave.device_id, "count_key_packages_in_suite": 7 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["unclaimed_key_packages"], 2, "{body}");

    let (status, body) = post_json(
        &db,
        &dave,
        "/v1/welcomes/fetch",
        serde_json::json!({ "device_id": dave.device_id }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.get("unclaimed_key_packages").is_none(),
        "a request that does not ask must get the pre-#1412 shape: {body}"
    );
}

async fn seed_enrollment_request(db: &Db, id: &str, user_id: &str) {
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT INTO device_enrollment_request \
             (id, user_id, new_device_id, new_device_ephemeral_pub, verification_code, status, expires_at) \
         VALUES (?1, ?2, ?2 || '-new', X'00', '000000', 'pending', datetime('now', '+1 hour'))",
        libsql::params![id, user_id],
    )
    .await
    .unwrap();
}

async fn enrollment_status(db: &Db, id: &str) -> Option<String> {
    scalar(
        db,
        "SELECT status FROM device_enrollment_request WHERE id = ?1",
        libsql::params![id],
    )
    .await
}

#[tokio::test(flavor = "multi_thread")]
async fn b_cannot_approve_as_enrollment_request() {
    let db = fresh_db().await;
    let _alice = seed_account(&db, "alice").await;
    let bob = seed_account(&db, "bob").await;
    seed_enrollment_request(&db, "req-a", "alice").await;

    let status = post(
        &db,
        &bob,
        "/v1/enrollment/approve",
        serde_json::json!({
            "request_id": "req-a",
            "wrapped_account_key": b64(b"wrapped"),
            "approved_by_device_id": bob.device_id,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(enrollment_status(&db, "req-a").await.as_deref(), Some("pending"));
}

#[tokio::test(flavor = "multi_thread")]
async fn b_cannot_reject_as_enrollment_request() {
    let db = fresh_db().await;
    let _alice = seed_account(&db, "alice").await;
    let bob = seed_account(&db, "bob").await;
    seed_enrollment_request(&db, "req-a", "alice").await;

    let status = post(
        &db,
        &bob,
        "/v1/enrollment/reject",
        serde_json::json!({ "request_id": "req-a", "approved_by_device_id": bob.device_id }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(enrollment_status(&db, "req-a").await.as_deref(), Some("pending"));
}

/// The signer and the claimed `user_id` must agree before anything is read.
#[tokio::test(flavor = "multi_thread")]
async fn b_cannot_read_as_pending_enrollments() {
    let db = fresh_db().await;
    let _alice = seed_account(&db, "alice").await;
    let bob = seed_account(&db, "bob").await;
    seed_enrollment_request(&db, "req-a", "alice").await;

    let (status, _) = post_json(
        &db,
        &bob,
        "/v1/read/pending-enrollments",
        serde_json::json!({ "user_id": "alice" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, body) = post_json(
        &db,
        &bob,
        "/v1/read/pending-enrollments",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["requests"].as_array().map(Vec::len), Some(0));
}

// ── cross-account 403s: devices ──────────────────────────────────────────────

/// The revoke is scoped `WHERE device_id = ? AND user_id = actor`.
#[tokio::test(flavor = "multi_thread")]
async fn b_cannot_revoke_as_device() {
    let db = fresh_db().await;
    let alice = seed_account(&db, "alice").await;
    let bob = seed_account(&db, "bob").await;

    assert_eq!(
        post(
            &db,
            &bob,
            "/v1/devices/revoke",
            serde_json::json!({ "device_id": alice.device_id, "user_id": "alice" }),
        )
        .await,
        StatusCode::FORBIDDEN
    );
    let _ = post(
        &db,
        &bob,
        "/v1/devices/revoke",
        serde_json::json!({ "device_id": alice.device_id }),
    )
    .await;
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM user_device WHERE device_id = ?1 AND revoked_at IS NULL",
            libsql::params![alice.device_id.clone()]
        )
        .await,
        1,
        "Alice's device must still be live"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn b_cannot_log_out_as_device() {
    let db = fresh_db().await;
    let alice = seed_account(&db, "alice").await;
    let bob = seed_account(&db, "bob").await;

    let _ = post(
        &db,
        &bob,
        "/v1/auth/logout",
        serde_json::json!({ "device_id": alice.device_id }),
    )
    .await;
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM user_device WHERE device_id = ?1",
            libsql::params![alice.device_id.clone()]
        )
        .await,
        1,
        "Alice's device row must still exist"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn b_cannot_write_or_read_as_security_events() {
    let db = fresh_db().await;
    let _alice = seed_account(&db, "alice").await;
    let bob = seed_account(&db, "bob").await;

    assert_eq!(
        post(
            &db,
            &bob,
            "/v1/security-events",
            serde_json::json!({ "kind": "forged", "user_id": "alice" }),
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM security_event WHERE user_id = 'alice'",
            ()
        )
        .await,
        0
    );

    assert_eq!(
        post(&db, &bob, "/v1/security-events", serde_json::json!({ "kind": "own" })).await,
        StatusCode::OK
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM security_event WHERE user_id = 'bob'", ()).await,
        1
    );

    let (status, _) = post_json(
        &db,
        &bob,
        "/v1/read/security-events",
        serde_json::json!({ "user_id": "alice" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

/// L3: the append is budgeted per account per hour, so a flood cannot bury evidence.
#[tokio::test(flavor = "multi_thread")]
async fn a_flood_of_security_events_is_refused_before_it_can_evict() {
    let db = fresh_db().await;
    let bob = seed_account(&db, "bob").await;

    db.conn()
        .await
        .unwrap()
        .execute(
            "INSERT INTO security_event (id, user_id, kind) VALUES ('ev-0', 'bob', 'identity_rotated')",
            (),
        )
        .await
        .unwrap();

    let mut accepted = 0;
    let mut refused = false;
    for i in 0..200 {
        let status = post(
            &db,
            &bob,
            "/v1/security-events",
            serde_json::json!({ "kind": format!("noise-{i}") }),
        )
        .await;
        if status == StatusCode::FORBIDDEN {
            refused = true;
            break;
        }
        assert_eq!(status, StatusCode::OK);
        accepted += 1;
    }
    assert!(refused, "an unbounded append loop must eventually be refused");
    assert!(
        accepted < 200,
        "the flood was not bounded: {accepted} rows appended"
    );

    let total = count(&db, "SELECT COUNT(*) FROM security_event WHERE user_id = 'bob'", ()).await;
    assert!(
        total < 500,
        "the log must stay inside the read window the client can actually see, \
         got {total} rows"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM security_event WHERE id = 'ev-0'",
            ()
        )
        .await,
        1,
        "the DS-authored event must still be there"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_ordinary_run_of_security_events_is_unaffected() {
    let db = fresh_db().await;
    let bob = seed_account(&db, "bob").await;
    for kind in ["device_enrolled", "identity_rotated", "device_revoked"] {
        assert_eq!(
            post(&db, &bob, "/v1/security-events", serde_json::json!({ "kind": kind })).await,
            StatusCode::OK
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn b_cannot_read_as_conversation_directory() {
    let db = fresh_db().await;
    let _alice = seed_account(&db, "alice").await;
    let bob = seed_account(&db, "bob").await;
    seed_member(&db, "alice-grp", "alice", "admin").await;
    seed_member(&db, "bob-grp", "bob", "admin").await;

    let (status, _) = post_json(
        &db,
        &bob,
        "/v1/directory/conversations",
        serde_json::json!({ "user_id": "alice" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let (status, body) = post_json(
        &db,
        &bob,
        "/v1/directory/conversations",
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let groups: Vec<String> = serde_json::from_value(body["group_ids"].clone()).unwrap();
    assert_eq!(groups, vec!["bob-grp".to_string()]);
}
