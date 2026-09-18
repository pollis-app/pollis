//! **Cross-account refusal is a property, not an accident.**
//!
//! Every endpoint in this service that acts on "the caller's own" resource is
//! bound to the authenticated signer. Those bindings were all correct — and
//! none of them were encoded by a test, so nothing would have noticed one going
//! away. C1 is what that costs: `/v1/commits` checked only that the SUBMITTER
//! was a member and wrote the bundle's Welcome rows with a recipient taken
//! verbatim from the request body, so any authenticated user could park a
//! forged MLS Welcome on any device of any account — and the sibling path
//! (`/v1/welcomes/resubmit`) had the exact check that was missing, with a doc
//! comment naming the exact attack.
//!
//! So this file is deliberately shaped as "user B cannot act on user A's
//! resource", one test per family, driven through the REAL router with REAL
//! ML-DSA-44 signatures and `require_auth = true`. A binding that regresses
//! fails here rather than in an audit.

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

// ── signing fixtures (the client contract, mirrored from `auth.rs`) ──────────

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

/// One signed-in account: a user row, a live device row, and the signing key
/// that device's public key belongs to.
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

/// Put `user` in `group_id`'s roster with `role`. A group id is a valid MLS
/// conversation id (a group's text channels share one MLS group keyed by it).
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

/// A device-signed POST, exactly as `pollis-core`'s `ds_post` builds one.
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

// ── C1: a commit bundle's Welcomes are checked against the roster ────────────

/// A commit bundle carrying a Welcome for a recipient/device pair.
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

/// **C1.** Mallory makes a group of her own, so she passes `/v1/commits`'
/// submitter-membership gate, and submits a commit whose Welcome is addressed to
/// a device belonging to an account that is not in it. `/v1/welcomes/fetch` is
/// scoped by `recipient_id` alone, so that row would be handed to the victim's
/// device — inviting it into a group nobody added it to, and (with the MLS
/// GroupId inside the blob set to the victim's real conversation) replacing the
/// real group with the attacker's.
///
/// Before the fix this returned 200 and wrote the row. Now the whole bundle is
/// refused: not the Welcome, not the commit, not the GroupInfo.
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

/// The control: the same bundle, for a recipient who IS a member, still lands.
/// Without this the test above would pass just as well if `/v1/commits` were
/// broken outright.
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

/// The gate is the DESIRED roster, not `is_member`. An invitee's devices are
/// added to the MLS tree and Welcomed at INVITE time — that is what makes
/// accepting independent of the inviter being online — so a pending
/// `group_invite` row is enough, and a gate that demanded membership would
/// refuse the staged Welcome the shipped invite flow exists to produce.
#[tokio::test(flavor = "multi_thread")]
async fn a_welcome_to_a_pending_invitee_is_admitted() {
    let db = fresh_db().await;
    let alice = seed_account(&db, "alice").await;
    let _dave = seed_account(&db, "dave").await;
    seed_member(&db, "grp", "alice", "admin").await;
    // Dave is invited but has NOT accepted — no `group_member` row.
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

/// The blob bound `/v1/welcomes/resubmit` has always had, now on the primary
/// path too: a Welcome is bounded by `WELCOME_MAX_BYTES`, so the commit bundle
/// is not an unbounded write primitive for any member.
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

/// The `submitted_by` gate, ported from the resubmit path: the
/// `(conversation, recipient, device)` tuple is UNIQUE, so writing it IS an
/// overwrite. Bob may not replace the PENDING Welcome Alice published for
/// Dave — that Welcome is the one Dave needs to join, and swapping it for
/// Bob's bytes keeps Dave out.
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
    // Bob is at the head (epoch 1), so his commit would otherwise win the CAS.
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

/// The escape hatch the resubmit path also has: an ADMIN of the conversation's
/// group may re-drive a Welcome another member published. Without this, a
/// genuinely stuck join could only be repaired by the member that first
/// attempted it.
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

/// **C1's client half needs a field to compare against.** `/v1/welcomes/fetch`
/// returned `{id, welcome}` and nothing else, so a client had no way to check
/// the `GroupId` inside the (server-opaque) Welcome blob against the
/// conversation the row was filed under — `join_from_welcome`'s guard had
/// nothing to bind to. The row's `conversation_id` now rides along.
///
/// Additive: the field is `Option` with `#[serde(default)]`, so an older client
/// ignores it and an older server decodes as `None`.
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

// ── cross-account 403s: enrollment ───────────────────────────────────────────

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

/// Approving somebody else's pending enrollment would hand the attacker's own
/// device key an account it does not own. Bob names Alice's request id.
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

/// Rejecting somebody else's pending enrollment is a denial of service against
/// their new device, and is refused by the same `WHERE user_id = actor`.
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

/// A signed request may not *declare* another account in the body either — the
/// signer and the claimed `user_id` must agree before anything is read.
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

    // And with no claim at all, Bob sees only his own (empty) set — never
    // Alice's request leaking through an unscoped query.
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

/// Revoking another account's device locks them out of their own messages. The
/// revoke is scoped `WHERE device_id = ? AND user_id = actor`, so Bob naming
/// Alice's device must change nothing.
#[tokio::test(flavor = "multi_thread")]
async fn b_cannot_revoke_as_device() {
    let db = fresh_db().await;
    let alice = seed_account(&db, "alice").await;
    let bob = seed_account(&db, "bob").await;

    // Declaring Alice's user_id is refused outright.
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
    // And naming only the device id silently scopes to Bob's own account, so
    // Alice's device is untouched.
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

/// Logout deletes a `user_device` row, so it is the same shape of hazard.
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

// ── cross-account 403s: the security-event audit log ─────────────────────────

/// The audit log is the evidence a user reads after a compromise. Writing a row
/// into somebody else's log — to fabricate or to bury — is refused, and reading
/// somebody else's is refused.
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

    // An unclaimed write lands on the SIGNER's log, never the named account's.
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

/// **L3 — flooding evicts evidence.** `/v1/read/security-events` is
/// `ORDER BY created_at DESC LIMIT n` with `n` clamped to 500 and no cursor, so
/// the log is a fixed-size window onto the newest rows. Appending was unbounded,
/// which made eviction an available move for a device that had just done
/// something the log records. The append is now budgeted per account per hour.
#[tokio::test(flavor = "multi_thread")]
async fn a_flood_of_security_events_is_refused_before_it_can_evict() {
    let db = fresh_db().await;
    let bob = seed_account(&db, "bob").await;

    // The DS-authored evidence a flood would try to bury.
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

/// The control: an ordinary client's handful of events is nowhere near the
/// bound, so the cap never costs a real report.
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

// ── cross-account 403s: directory reads ──────────────────────────────────────

/// The conversation directory is the map of who talks to whom. Asking for
/// another account's is refused; asking for your own returns only your own.
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
