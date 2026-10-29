//! #1548: `/v1/read/security-events` pages by keyset cursor and filters by kind.
//!
//! Driven through the real router at the real schema, so the wire body, the
//! clamp and the migration's indexes are what a client actually meets.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pollis_api::account_reads::{
    SecurityEventCursor, SecurityEventsBody, SecurityEventsResponse, SECURITY_EVENTS_MAX_KINDS,
};
use pollis_api::DsRequest;
use pollis_delivery::account_reads::security_events_query;
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;

async fn fixture() -> (axum::Router, common::TempDb) {
    let db = common::TempDb::open("db.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap())
        .await
        .expect("schema");
    db.conn()
        .await
        .unwrap()
        .execute(
            "INSERT INTO users (id, email, username) VALUES ('alice', 'alice@x', 'alice'), \
             ('bob', 'bob@x', 'bob')",
            (),
        )
        .await
        .unwrap();
    // Auth off: the actor is the body's `user_id`. Owner scoping under real
    // signatures is pinned in `cross_account_authz.rs`.
    (build_router_with_state(AppState::new(db.arc(), false)), db)
}

async fn seed(db: &common::TempDb, user: &str, id: &str, kind: &str, created_at: &str) {
    db.conn()
        .await
        .unwrap()
        .execute(
            "INSERT INTO security_event (id, user_id, kind, created_at) VALUES (?1, ?2, ?3, ?4)",
            libsql::params![id, user, kind, created_at],
        )
        .await
        .unwrap();
}

async fn post_raw(app: &axum::Router, body: &serde_json::Value) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri(SecurityEventsBody::PATH)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(body).unwrap()))
        .unwrap();
    let resp = app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

async fn page(
    app: &axum::Router,
    limit: i64,
    before: Option<SecurityEventCursor>,
    kinds: Option<Vec<&str>>,
) -> SecurityEventsResponse {
    let body = SecurityEventsBody {
        user_id: Some("alice".into()),
        limit: Some(limit),
        before,
        kinds: kinds.map(|k| k.into_iter().map(String::from).collect()),
    };
    let (status, json) = post_raw(app, &serde_json::to_value(&body).unwrap()).await;
    assert_eq!(status, StatusCode::OK, "{json}");
    serde_json::from_value(json).unwrap()
}

fn ids(resp: &SecurityEventsResponse) -> Vec<String> {
    resp.events.iter().map(|e| e.id.clone()).collect()
}

/// Walking the whole log two rows at a time returns every row exactly once, in
/// `(created_at DESC, id DESC)` order — including a run of five rows that share
/// one `created_at`, which is the case a `created_at`-only cursor gets wrong
/// (it would skip or repeat whichever rows straddle a page boundary).
#[tokio::test]
async fn the_cursor_is_stable_across_rows_with_equal_created_at() {
    let (app, db) = fixture().await;
    let same = "2026-10-01 12:00:00";
    for id in ["e1", "e2", "e3", "e4", "e5"] {
        seed(&db, "alice", id, "device_enrolled", same).await;
    }
    seed(&db, "alice", "z-new", "device_revoked", "2026-10-02 08:00:00").await;
    seed(&db, "alice", "a-old", "email_changed", "2026-09-30 08:00:00").await;
    // Another account's rows must never appear on alice's pages.
    seed(&db, "bob", "e3b", "device_enrolled", same).await;

    let mut seen = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let resp = page(&app, 2, cursor.clone(), None).await;
        pages += 1;
        assert!(resp.events.len() <= 2);
        seen.extend(ids(&resp));
        match resp.next_cursor {
            Some(c) => {
                let last = resp.events.last().unwrap();
                assert_eq!(c, SecurityEventCursor { created_at: last.created_at.clone(), id: last.id.clone() });
                cursor = Some(c);
            }
            None => break,
        }
        assert!(pages < 10, "pagination did not terminate");
    }
    assert_eq!(seen, ["z-new", "e5", "e4", "e3", "e2", "e1", "a-old"]);
    assert_eq!(pages, 4);
}

/// The last page carries no cursor, even when it ends exactly on the limit —
/// the DS looks one row past the page rather than guessing from a full page.
#[tokio::test]
async fn an_exactly_full_last_page_has_no_next_cursor() {
    let (app, db) = fixture().await;
    seed(&db, "alice", "a", "device_enrolled", "2026-10-01 12:00:00").await;
    seed(&db, "alice", "b", "device_enrolled", "2026-10-01 12:00:01").await;
    let resp = page(&app, 2, None, None).await;
    assert_eq!(ids(&resp), ["b", "a"]);
    assert!(resp.next_cursor.is_none());
}

/// `kinds` narrows the log, and the cursor keeps working inside the filter.
#[tokio::test]
async fn the_kind_filter_returns_only_those_kinds_and_pages_within_them() {
    let (app, db) = fixture().await;
    seed(&db, "alice", "d1", "device_enrolled", "2026-10-01 10:00:00").await;
    seed(&db, "alice", "i1", "identity_rotated", "2026-10-01 11:00:00").await;
    seed(&db, "alice", "d2", "device_revoked", "2026-10-01 12:00:00").await;
    seed(&db, "alice", "m1", "email_changed", "2026-10-01 13:00:00").await;
    seed(&db, "alice", "d3", "device_rejected", "2026-10-01 14:00:00").await;
    seed(&db, "alice", "r1", "recovery_blob_fetched", "2026-10-01 15:00:00").await;

    let devices = Some(vec!["device_enrolled", "device_rejected", "device_revoked"]);
    let first = page(&app, 2, None, devices.clone()).await;
    assert_eq!(ids(&first), ["d3", "d2"]);
    let second = page(&app, 2, first.next_cursor.clone(), devices).await;
    assert_eq!(ids(&second), ["d1"]);
    assert!(second.next_cursor.is_none());

    let keys = page(&app, 50, None, Some(vec!["identity_rotated", "recovery_blob_fetched"])).await;
    assert_eq!(ids(&keys), ["r1", "i1"]);

    // An empty list is "every kind", not "no kind".
    let all = page(&app, 50, None, Some(vec![])).await;
    assert_eq!(all.events.len(), 6);

    // Too many kinds is a 400, not a statement SQLite refuses to prepare.
    let many: Vec<String> = (0..=SECURITY_EVENTS_MAX_KINDS).map(|i| format!("k{i}")).collect();
    let (status, _) = post_raw(
        &app,
        &serde_json::json!({ "user_id": "alice", "kinds": many }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// `limit` is clamped to `1..=500` server-side whatever the body asks for.
#[tokio::test]
async fn the_limit_is_clamped() {
    let (app, db) = fixture().await;
    let conn = db.conn().await.unwrap();
    conn.execute("BEGIN", ()).await.unwrap();
    for i in 0..510 {
        conn.execute(
            "INSERT INTO security_event (id, user_id, kind, created_at) VALUES (?1, 'alice', 'device_enrolled', ?2)",
            libsql::params![format!("ev-{i:04}"), format!("2026-10-01 00:{:02}:{:02}", i / 60, i % 60)],
        )
        .await
        .unwrap();
    }
    conn.execute("COMMIT", ()).await.unwrap();

    let huge = page(&app, 100_000, None, None).await;
    assert_eq!(huge.events.len(), 500);
    assert!(huge.next_cursor.is_some());

    let zero = page(&app, 0, None, None).await;
    assert_eq!(zero.events.len(), 1);

    let negative = page(&app, -5, None, None).await;
    assert_eq!(negative.events.len(), 1);
}

/// A pre-#1548 client sends `{user_id, limit}` (or less) and parses
/// `{events}`. It must get the same rows as before — the newest `limit`, every
/// kind — and the body a new client builds without a cursor or filter must be
/// byte-identical to the old one.
#[tokio::test]
async fn an_old_client_body_gets_the_old_answer() {
    let (app, db) = fixture().await;
    for i in 0..5 {
        seed(&db, "alice", &format!("ev-{i}"), "device_enrolled", &format!("2026-10-01 00:00:0{i}")).await;
    }

    let (status, json) = post_raw(&app, &serde_json::json!({ "user_id": "alice", "limit": 3 })).await;
    assert_eq!(status, StatusCode::OK);
    let events = json["events"].as_array().unwrap();
    let got: Vec<&str> = events.iter().map(|e| e["id"].as_str().unwrap()).collect();
    assert_eq!(got, ["ev-4", "ev-3", "ev-2"]);
    // Every field the old row type read is still there, with the old names.
    for key in ["id", "kind", "device_id", "created_at", "metadata"] {
        assert!(events[0].as_object().unwrap().contains_key(key), "missing {key}");
    }

    // No `limit` either: the old default of 100, so all five rows and no cursor.
    let (status, json) = post_raw(&app, &serde_json::json!({ "user_id": "alice" })).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(json["events"].as_array().unwrap().len(), 5);
    assert!(json.get("next_cursor").is_none());

    // The new client's first-page body serializes to exactly the old shape.
    let body = SecurityEventsBody {
        user_id: Some("alice".into()),
        limit: Some(3),
        before: None,
        kinds: None,
    };
    assert_eq!(
        serde_json::to_value(&body).unwrap(),
        serde_json::json!({ "user_id": "alice", "limit": 3 })
    );
}

/// The page query is an index search on the new indexes, not a scan plus sort.
#[tokio::test]
async fn the_page_query_is_served_by_the_new_indexes() {
    let (_app, db) = fixture().await;
    let conn = db.conn().await.unwrap();
    let cursor = SecurityEventCursor {
        created_at: "2026-10-01 00:00:00".into(),
        id: "x".into(),
    };
    let plan = |sql: String, params: Vec<libsql::Value>| {
        let conn = conn.clone();
        async move {
            let mut rows = conn
                .query(&format!("EXPLAIN QUERY PLAN {sql}"), params)
                .await
                .unwrap();
            let mut out = Vec::new();
            while let Some(row) = rows.next().await.unwrap() {
                out.push(row.get::<String>(3).unwrap());
            }
            out.join(" | ")
        }
    };

    let (sql, params) = security_events_query("alice", Some(&cursor), &[], 51);
    let p = plan(sql, params).await;
    assert!(p.contains("idx_security_event_user_created_id"), "unfiltered plan: {p}");
    assert!(!p.contains("TEMP B-TREE"), "unfiltered plan sorts: {p}");

    let (sql, params) = security_events_query("alice", Some(&cursor), &["email_changed".to_string()], 51);
    let p = plan(sql, params).await;
    assert!(p.contains("idx_security_event_user_kind"), "single-kind plan: {p}");
    assert!(!p.contains("TEMP B-TREE"), "single-kind plan sorts: {p}");
}
