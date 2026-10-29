//! #917 — the Delivery Service's authorization for `POST /v1/join-requests/create`.
//!
//! Note `foreign_keys=OFF`, as on Turso: the existence check has to be explicit.

use pollis_delivery::db::Db;
use pollis_delivery::groups::{apply_create_join_request, CreateJoinRequestBody, JoinRequestOutcome};

mod common;

const GROUP: &str = "grp-1";
const ADMIN: &str = "admin-1";
const MEMBER: &str = "member-1";
const STRANGER: &str = "stranger-1";

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    // No `PRAGMA foreign_keys=OFF` here: since #919 `Db` stamps it on every connection.
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    conn.execute_batch(
        "INSERT INTO users (id, email, username) VALUES \
           ('admin-1','admin@x','admin'),('member-1','member@x','member'),\
           ('stranger-1','stranger@x','stranger');\
         INSERT INTO conversation (id, kind) VALUES ('grp-1','group');\
         INSERT INTO groups (id, name, owner_id) VALUES ('grp-1','Group One','admin-1');\
         INSERT INTO group_member (group_id, user_id, role) VALUES \
           ('grp-1','admin-1','admin'),('grp-1','member-1','member');",
    )
    .await
    .expect("seed");
    db
}

fn body(id: &str, group_id: &str, requester: &str) -> CreateJoinRequestBody {
    CreateJoinRequestBody {
        id: id.to_string(),
        group_id: group_id.to_string(),
        requester_id: Some(requester.to_string()),
    }
}

async fn request_rows(db: &Db) -> i64 {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query("SELECT COUNT(*) FROM group_join_request", ())
        .await
        .expect("count");
    rows.next().await.expect("row").expect("row").get(0).expect("count")
}

/// With `foreign_keys=OFF`, nothing in the schema stops rows for nonexistent groups.
#[tokio::test]
async fn a_join_request_for_an_unknown_group_is_refused() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    let outcome = apply_create_join_request(
        &conn,
        Some(STRANGER),
        &body("req-1", "grp-does-not-exist", STRANGER),
    )
    .await
    .expect("apply");

    assert!(
        matches!(outcome, JoinRequestOutcome::NoSuchGroup),
        "a request naming a nonexistent group must be refused, got {outcome:?}"
    );
    assert_eq!(
        request_rows(&db).await,
        0,
        "no row may be written for a group that does not exist"
    );
}

/// Approving a member's request would re-run the member-add.
#[tokio::test]
async fn an_existing_member_cannot_request_to_join() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    let outcome = apply_create_join_request(&conn, Some(MEMBER), &body("req-1", GROUP, MEMBER))
        .await
        .expect("apply");

    assert!(
        matches!(outcome, JoinRequestOutcome::AlreadyMember),
        "a current member must not be able to request to join, got {outcome:?}"
    );
    assert_eq!(request_rows(&db).await, 0, "no row may be written for a member");
}

/// Separate from the member case: a guard on `role = 'member'` would leak admins.
#[tokio::test]
async fn an_admin_cannot_request_to_join_their_own_group() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    let outcome = apply_create_join_request(&conn, Some(ADMIN), &body("req-1", GROUP, ADMIN))
        .await
        .expect("apply");

    assert!(
        matches!(outcome, JoinRequestOutcome::AlreadyMember),
        "an admin must not be able to request to join their own group, got {outcome:?}"
    );
    assert_eq!(request_rows(&db).await, 0, "no row may be written for an admin");
}

/// A join request comes from a non-member by definition: no blanket membership rule.
#[tokio::test]
async fn a_non_member_may_request_to_join() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    let outcome =
        apply_create_join_request(&conn, Some(STRANGER), &body("req-1", GROUP, STRANGER))
            .await
            .expect("apply");

    assert!(
        matches!(outcome, JoinRequestOutcome::Ok),
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

/// Only "already a member" is invalid, not "a row already exists".
#[tokio::test]
async fn a_rejected_requester_may_apply_again() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

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
        matches!(outcome, JoinRequestOutcome::Ok),
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

/// The client's combination of the shared `pollis_schema::authz` statements.
async fn client_preflight_allows(
    conn: &libsql::Connection,
    group_id: &str,
    requester: &str,
) -> bool {
    let mut exists = conn
        .query(
            pollis_schema::authz::GROUP_EXISTS_SQL,
            libsql::params![group_id],
        )
        .await
        .expect("group exists query");
    if exists.next().await.expect("row").is_none() {
        return false;
    }
    let mut role = conn
        .query(
            pollis_schema::authz::GROUP_ROLE_SQL,
            libsql::params![group_id, requester],
        )
        .await
        .expect("group role query");
    role.next().await.expect("row").is_none()
}

/// #942: client preflight and DS guard must agree on every row, against a literal expected column.
#[tokio::test]
async fn preflight_parity() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    let matrix: &[(&str, &str, bool, &str)] = &[
        (GROUP, STRANGER, true, "a non-member of a real group may ask"),
        (GROUP, MEMBER, false, "a current member has nothing to request"),
        (GROUP, ADMIN, false, "an admin is a member too"),
        ("grp-does-not-exist", STRANGER, false, "an unknown group id is refused"),
        ("", STRANGER, false, "an empty group id is not a group"),
        // Allowed: rejecting an unauthenticated caller is `resolve_actor`'s job, not this rule's.
        (GROUP, "", true, "an empty requester is still a non-member of a real group"),
    ];

    for (group, requester, expected, why) in matrix {
        let client = client_preflight_allows(&conn, group, requester).await;

        let ds = matches!(
            apply_create_join_request(&conn, Some(requester), &body("req-parity", group, requester))
                .await
                .expect("apply"),
            JoinRequestOutcome::Ok
        );
        // Leave no row behind, or the upsert couples matrix rows.
        conn.execute("DELETE FROM group_join_request", ())
            .await
            .expect("reset");

        assert_eq!(
            client, ds,
            "({group}, {requester}): the client preflight and the DS guard disagree \
             — client says {client}, DS says {ds}. The DS is authoritative, so the \
             client is wrong. ({why})"
        );
        assert_eq!(
            ds, *expected,
            "({group}, {requester}): the agreed answer is not the intended one — {why}"
        );
    }
}

/// Pins that an empty id is really absent from `groups`.
#[tokio::test]
async fn the_matrix_edge_cases_are_actually_edge_cases() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    let mut rows = conn
        .query(pollis_schema::authz::GROUP_EXISTS_SQL, libsql::params![""])
        .await
        .expect("query");
    assert!(rows.next().await.expect("row").is_none(), "'' must not be a group");

    let mut rows = conn
        .query(pollis_schema::authz::GROUP_ROLE_SQL, libsql::params![GROUP, ""])
        .await
        .expect("query");
    assert!(
        rows.next().await.expect("row").is_none(),
        "'' must not be a member of the fixture group"
    );
}
