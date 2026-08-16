//! #917 — the Delivery Service's authorization for `POST /v1/join-requests/create`.
//!
//! `apply_create_join_request` used to carry a comment saying group-existence
//! and not-already-a-member "checks stay client-side", which is a description
//! of an unenforced rule: the DS is the only authoritative writer, and the
//! client is explicitly untrusted, so a check that lives only in the client is
//! not a check. These tests pin the three conditions the endpoint now
//! re-derives server-side, and each fails if its guard is removed:
//!
//!   * `a_join_request_for_an_unknown_group_is_refused`
//!   * `an_existing_member_cannot_request_to_join`
//!   * `an_admin_cannot_request_to_join_their_own_group`
//!
//! plus the two that pin what must NOT regress — the whole point of the
//! endpoint is that a stranger can use it:
//!
//!   * `a_non_member_may_request_to_join` — the allowed case.
//!   * `a_rejected_requester_may_apply_again` — the upsert path.
//!
//! Driven at the `apply_*` level against a local libsql DB, mirroring
//! `invite_links.rs`. Note `foreign_keys=OFF`, which is what production Turso
//! does: the schema's `group_join_request.group_id REFERENCES groups(id)` is
//! therefore inert, and is exactly why the existence check has to be explicit.

use pollis_delivery::db::Db;
use pollis_delivery::groups::{apply_create_join_request, CreateJoinRequestBody};
use pollis_delivery::writes::WriteOutcome;
use std::sync::Arc;

const GROUP: &str = "grp-1";
const ADMIN: &str = "admin-1";
const MEMBER: &str = "member-1";
const STRANGER: &str = "stranger-1";

async fn fresh() -> Arc<Db> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.db");
    std::mem::forget(dir);
    let db = Db::connect_local(path.to_str().unwrap())
        .await
        .expect("local db");
    let conn = db.conn().unwrap();
    // Production is Turso, where foreign-key enforcement is off; libsql's LOCAL
    // backend turns it ON by default. Match production explicitly — with FKs on,
    // the unknown-group test would pass for the wrong reason.
    conn.execute_batch("PRAGMA foreign_keys=OFF;").await.expect("pragma");
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    conn.execute_batch(
        "INSERT INTO users (id, email, username) VALUES \
           ('admin-1','admin@x','admin'),('member-1','member@x','member'),\
           ('stranger-1','stranger@x','stranger');\
         INSERT INTO groups (id, name, owner_id) VALUES ('grp-1','Group One','admin-1');\
         INSERT INTO group_member (group_id, user_id, role) VALUES \
           ('grp-1','admin-1','admin'),('grp-1','member-1','member');",
    )
    .await
    .expect("seed");
    Arc::new(db)
}

fn body(id: &str, group_id: &str, requester: &str) -> CreateJoinRequestBody {
    CreateJoinRequestBody {
        id: id.to_string(),
        group_id: group_id.to_string(),
        requester_id: Some(requester.to_string()),
    }
}

async fn request_rows(db: &Db) -> i64 {
    let conn = db.conn().unwrap();
    let mut rows = conn
        .query("SELECT COUNT(*) FROM group_join_request", ())
        .await
        .expect("count");
    rows.next().await.expect("row").expect("row").get(0).expect("count")
}

/// The core exposure: with `foreign_keys=OFF`, nothing in the schema stopped a
/// client from inserting join-request rows keyed by group ids that do not
/// exist. Combined with slug enumeration, that is a write primitive pointed at
/// arbitrary ids.
#[tokio::test]
async fn a_join_request_for_an_unknown_group_is_refused() {
    let db = fresh().await;
    let conn = db.conn().unwrap();

    let outcome = apply_create_join_request(
        &conn,
        Some(STRANGER),
        &body("req-1", "grp-does-not-exist", STRANGER),
    )
    .await
    .expect("apply");

    assert!(
        matches!(outcome, WriteOutcome::Forbidden),
        "a request naming a nonexistent group must be refused, got {outcome:?}"
    );
    assert_eq!(
        request_rows(&db).await,
        0,
        "no row may be written for a group that does not exist"
    );
}

/// A member asking to join what they are already in is an invalid state: the
/// row is approvable, and approving it re-runs the member-add for someone who
/// is already a member.
#[tokio::test]
async fn an_existing_member_cannot_request_to_join() {
    let db = fresh().await;
    let conn = db.conn().unwrap();

    let outcome = apply_create_join_request(&conn, Some(MEMBER), &body("req-1", GROUP, MEMBER))
        .await
        .expect("apply");

    assert!(
        matches!(outcome, WriteOutcome::Forbidden),
        "a current member must not be able to request to join, got {outcome:?}"
    );
    assert_eq!(request_rows(&db).await, 0, "no row may be written for a member");
}

/// The same rule, via the role that most obviously already has access. Kept
/// separate from the plain-member case because "admin" is a different row in
/// `group_member` and a guard written against `role = 'member'` would pass the
/// test above while leaking here.
#[tokio::test]
async fn an_admin_cannot_request_to_join_their_own_group() {
    let db = fresh().await;
    let conn = db.conn().unwrap();

    let outcome = apply_create_join_request(&conn, Some(ADMIN), &body("req-1", GROUP, ADMIN))
        .await
        .expect("apply");

    assert!(
        matches!(outcome, WriteOutcome::Forbidden),
        "an admin must not be able to request to join their own group, got {outcome:?}"
    );
    assert_eq!(request_rows(&db).await, 0, "no row may be written for an admin");
}

/// The allowed case, and the reason none of the above may be implemented as a
/// blanket membership requirement: a join request comes from a non-member by
/// definition. If this test ever fails, the fix has broken the feature.
#[tokio::test]
async fn a_non_member_may_request_to_join() {
    let db = fresh().await;
    let conn = db.conn().unwrap();

    let outcome =
        apply_create_join_request(&conn, Some(STRANGER), &body("req-1", GROUP, STRANGER))
            .await
            .expect("apply");

    assert!(
        matches!(outcome, WriteOutcome::Ok),
        "a non-member must be able to request to join, got {outcome:?}"
    );

    let mut rows = conn
        .query(
            "SELECT requester_id, status FROM group_join_request WHERE group_id = ?1",
            libsql::params![GROUP],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("row").expect("one row");
    assert_eq!(row.get::<String>(0).unwrap(), STRANGER);
    assert_eq!(row.get::<String>(1).unwrap(), "pending");
}

/// The upsert path still resets a prior decision back to pending. Guards
/// against an over-eager fix that treats "a row already exists" as the invalid
/// state — it is not; only "the requester is already a member" is.
#[tokio::test]
async fn a_rejected_requester_may_apply_again() {
    let db = fresh().await;
    let conn = db.conn().unwrap();

    apply_create_join_request(&conn, Some(STRANGER), &body("req-1", GROUP, STRANGER))
        .await
        .expect("apply");
    conn.execute(
        "UPDATE group_join_request SET status = 'rejected' WHERE id = 'req-1'",
        (),
    )
    .await
    .expect("reject");

    let outcome =
        apply_create_join_request(&conn, Some(STRANGER), &body("req-2", GROUP, STRANGER))
            .await
            .expect("apply");

    assert!(
        matches!(outcome, WriteOutcome::Ok),
        "a previously rejected requester must be able to re-apply, got {outcome:?}"
    );
    assert_eq!(
        request_rows(&db).await,
        1,
        "re-applying updates the existing row rather than adding a second"
    );
    let mut rows = conn
        .query(
            "SELECT status FROM group_join_request WHERE group_id = ?1",
            libsql::params![GROUP],
        )
        .await
        .expect("query");
    let row = rows.next().await.expect("row").expect("one row");
    assert_eq!(row.get::<String>(0).unwrap(), "pending");
}
