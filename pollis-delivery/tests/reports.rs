//! Abuse reports and account suspension (#1213).
//!
//! The invariants, each proven where it is enforced:
//!   - a report holds ids and a reason only (the table has no content column);
//!   - you cannot report yourself, file as someone else, use a reason outside
//!     the set, give a message without its conversation, or point at a
//!     conversation you are not in;
//!   - one account cannot flood the operator (a rolling daily cap);
//!   - a suspended account authenticates nothing, from any device.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine as _;
use http_body_util::BodyExt as _;
use ml_dsa::{Keypair, MlDsa44, Signer, SigningKey};
use pollis_api::reports::{ReportReason, ReportUserBody};
use pollis_delivery::auth::canonical_message;
use pollis_delivery::reports::{apply_report_user, ReportOutcome, REPORTS_PER_DAY};
use pollis_delivery::{build_router_with_state, AppState};
use rand_core::{OsRng, RngCore as _};
use tower::ServiceExt as _;

mod common;

async fn fresh_db() -> common::TempDb {
    let db = common::TempDb::open("reports.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap()).await.expect("schema");
    for u in ["alice", "bob", "carol"] {
        db.conn()
            .await
            .unwrap()
            .execute(
                "INSERT INTO users (id, email, username) VALUES (?1, ?1 || '@x', ?1)",
                libsql::params![u],
            )
            .await
            .unwrap();
    }
    db
}

fn body(reporter: &str, reported: &str) -> ReportUserBody {
    ReportUserBody {
        reporter_id: reporter.into(),
        reported_id: reported.into(),
        reason: ReportReason::Harassment,
        conversation_id: None,
        message_id: None,
    }
}

async fn report_count(db: &common::TempDb) -> i64 {
    let conn = db.conn().await.unwrap();
    let mut rows = conn.query("SELECT COUNT(*) FROM user_report", ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_report_stores_ids_and_a_reason_and_nothing_else() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let outcome = apply_report_user(&conn, Some("alice"), &body("alice", "bob")).await.unwrap();
    assert!(matches!(outcome, ReportOutcome::Stored { prior_reports_against: 0, .. }));

    // The table's columns are the whole of what a report can hold.
    let mut cols = Vec::new();
    let mut rows = conn.query("PRAGMA table_info(user_report)", ()).await.unwrap();
    while let Some(r) = rows.next().await.unwrap() {
        cols.push(r.get::<String>(1).unwrap());
    }
    assert_eq!(
        cols,
        ["id", "reporter_id", "reported_id", "reason", "conversation_id", "message_id", "created_at"],
        "a new column on user_report needs the same no-content scrutiny as the wire type"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn you_cannot_report_yourself_or_file_as_someone_else() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    assert_eq!(
        apply_report_user(&conn, Some("alice"), &body("alice", "alice")).await.unwrap(),
        ReportOutcome::Forbidden
    );
    assert_eq!(
        apply_report_user(&conn, Some("alice"), &body("carol", "bob")).await.unwrap(),
        ReportOutcome::Forbidden
    );
    assert_eq!(report_count(&db).await, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_table_refuses_invalid_reports_even_without_the_ds_checks() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    for (sql, what) in [
        (
            "INSERT INTO user_report (id, reporter_id, reported_id, reason) VALUES ('r1', 'alice', 'alice', 'spam')",
            "a self-report",
        ),
        (
            "INSERT INTO user_report (id, reporter_id, reported_id, reason) VALUES ('r2', 'alice', 'bob', 'rude')",
            "a reason outside the set",
        ),
        (
            "INSERT INTO user_report (id, reporter_id, reported_id, reason, message_id) VALUES ('r3', 'alice', 'bob', 'spam', 'm1')",
            "a message id with no conversation",
        ),
        (
            "INSERT INTO account_suspension (user_id, reason) VALUES ('bob', '   ')",
            "a suspension with no reason",
        ),
    ] {
        assert!(conn.execute(sql, ()).await.is_err(), "the schema accepted {what}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unknown_reason_does_not_parse() {
    let raw = serde_json::json!({
        "reporter_id": "alice", "reported_id": "bob", "reason": "rude",
        "conversation_id": null, "message_id": null,
    });
    assert!(serde_json::from_value::<ReportUserBody>(raw).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_conversation_must_be_one_the_reporter_is_in() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob')",
        (),
    )
    .await
    .unwrap();
    let mut b = body("alice", "bob");
    b.conversation_id = Some("g1".into());
    b.message_id = Some("m1".into());
    assert_eq!(apply_report_user(&conn, Some("alice"), &b).await.unwrap(), ReportOutcome::Forbidden);

    conn.execute(
        "INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice')",
        (),
    )
    .await
    .unwrap();
    assert!(matches!(
        apply_report_user(&conn, Some("alice"), &b).await.unwrap(),
        ReportOutcome::Stored { .. }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_message_without_its_conversation_is_refused() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    let mut b = body("alice", "bob");
    b.message_id = Some("m1".into());
    assert_eq!(apply_report_user(&conn, Some("alice"), &b).await.unwrap(), ReportOutcome::Forbidden);
}

#[tokio::test(flavor = "multi_thread")]
async fn reporting_an_account_that_does_not_exist_is_refused() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    assert_eq!(
        apply_report_user(&conn, Some("alice"), &body("alice", "nobody")).await.unwrap(),
        ReportOutcome::UnknownAccount
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn one_account_cannot_flood_the_operator() {
    let db = fresh_db().await;
    let conn = db.conn().await.unwrap();
    for _ in 0..REPORTS_PER_DAY {
        assert!(matches!(
            apply_report_user(&conn, Some("alice"), &body("alice", "bob")).await.unwrap(),
            ReportOutcome::Stored { .. }
        ));
    }
    assert_eq!(
        apply_report_user(&conn, Some("alice"), &body("alice", "bob")).await.unwrap(),
        ReportOutcome::RateLimited
    );
    // The cap is per reporter: someone else can still report.
    assert!(matches!(
        apply_report_user(&conn, Some("carol"), &body("carol", "bob")).await.unwrap(),
        ReportOutcome::Stored { prior_reports_against: 1, .. }
    ));
}

// ── Over HTTP, with auth enforced ────────────────────────────────────────────

fn signing_key() -> SigningKey<MlDsa44> {
    let mut seed = [0u8; 32];
    OsRng.fill_bytes(&mut seed);
    SigningKey::<MlDsa44>::from_seed(&seed.into())
}

async fn seed_device(db: &common::TempDb, user: &str, device: &str, sk: &SigningKey<MlDsa44>) {
    db.conn()
        .await
        .unwrap()
        .execute(
            "INSERT INTO user_device (device_id, user_id, mls_signature_pub_pq) VALUES (?1, ?2, ?3)",
            libsql::params![device, user, sk.verifying_key().encode().to_vec()],
        )
        .await
        .unwrap();
}

fn signed(path: &str, user: &str, device: &str, sk: &SigningKey<MlDsa44>, body: &[u8]) -> Request<Body> {
    let ts = pollis_delivery::util::now_unix() as i64;
    let sig = sk.sign(&canonical_message("POST", path, ts, body)).encode();
    Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("X-Pollis-User", user)
        .header("X-Pollis-Device", device)
        .header("X-Pollis-Timestamp", ts.to_string())
        .header("X-Pollis-Signature", base64::engine::general_purpose::STANDARD.encode(sig))
        .body(Body::from(body.to_vec()))
        .unwrap()
}

async fn status(state: &AppState, req: Request<Body>) -> StatusCode {
    let resp = build_router_with_state(state.clone()).oneshot(req).await.unwrap();
    let s = resp.status();
    let _ = resp.into_body().collect().await;
    s
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signed_report_is_accepted_and_bound_to_the_signer() {
    let db = fresh_db().await;
    let sk = signing_key();
    seed_device(&db, "alice", "dev-a", &sk).await;
    let state = AppState::new(db.arc(), true);

    let ok = serde_json::to_vec(&body("alice", "bob")).unwrap();
    assert_eq!(status(&state, signed("/v1/reports", "alice", "dev-a", &sk, &ok)).await, StatusCode::OK);

    let forged = serde_json::to_vec(&body("carol", "bob")).unwrap();
    assert_eq!(
        status(&state, signed("/v1/reports", "alice", "dev-a", &sk, &forged)).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(report_count(&db).await, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_suspended_account_authenticates_nothing_from_any_device() {
    let db = fresh_db().await;
    let (sk1, sk2) = (signing_key(), signing_key());
    seed_device(&db, "bob", "dev-b1", &sk1).await;
    seed_device(&db, "bob", "dev-b2", &sk2).await;
    db.conn()
        .await
        .unwrap()
        .execute(
            "INSERT INTO account_suspension (user_id, reason) VALUES ('bob', 'spam campaign')",
            (),
        )
        .await
        .unwrap();
    // Fresh state, so no device key is cached from before the suspension.
    let state = AppState::new(db.arc(), true);

    let b = serde_json::to_vec(&body("bob", "alice")).unwrap();
    for (device, sk) in [("dev-b1", &sk1), ("dev-b2", &sk2)] {
        assert_eq!(
            status(&state, signed("/v1/reports", "bob", device, sk, &b)).await,
            StatusCode::UNAUTHORIZED,
            "{device} of a suspended account was let in"
        );
    }

    // Lifting the suspension (deleting the row) restores access.
    db.conn()
        .await
        .unwrap()
        .execute("DELETE FROM account_suspension WHERE user_id = 'bob'", ())
        .await
        .unwrap();
    let state = AppState::new(db.arc(), true);
    assert_eq!(status(&state, signed("/v1/reports", "bob", "dev-b1", &sk1, &b)).await, StatusCode::OK);
}

