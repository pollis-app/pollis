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
use pollis_delivery::groups::{apply_create_join_request, CreateJoinRequestBody, JoinRequestOutcome};

mod common;

const GROUP: &str = "grp-1";
const ADMIN: &str = "admin-1";
const MEMBER: &str = "member-1";
const STRANGER: &str = "stranger-1";

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    // No `PRAGMA foreign_keys=OFF` here: since #919 `Db` stamps it on EVERY
    // connection it builds, so this fixture cannot get a stricter connection than
    // production has however deeply the code under test nests its checkouts. It
    // used to be set by hand right here, on this one connection, which was only
    // ever half a fix.
    let conn = db.conn().await.unwrap();
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

/// The core exposure: with `foreign_keys=OFF`, nothing in the schema stopped a
/// client from inserting join-request rows keyed by group ids that do not
/// exist. Combined with slug enumeration, that is a write primitive pointed at
/// arbitrary ids.
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

/// A member asking to join what they are already in is an invalid state: the
/// row is approvable, and approving it re-runs the member-add for someone who
/// is already a member.
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

/// The same rule, via the role that most obviously already has access. Kept
/// separate from the plain-member case because "admin" is a different row in
/// `group_member` and a guard written against `role = 'member'` would pass the
/// test above while leaking here.
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

/// The allowed case, and the reason none of the above may be implemented as a
/// blanket membership requirement: a join request comes from a non-member by
/// definition. If this test ever fails, the fix has broken the feature.
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

/// The upsert path still resets a prior decision back to pending. Guards
/// against an over-eager fix that treats "a row already exists" as the invalid
/// state — it is not; only "the requester is already a member" is.
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

// ── #942: the client preflight and the DS guard must agree ──────────────────

/// The client's copy of the rule, expressed the way
/// `pollis_core::commands::groups::join_requests::request_group_access` expresses
/// it: the group must exist, and the requester must not already be in it.
///
/// This is not a re-implementation. Both SELECTs are
/// `pollis_schema::authz`'s constants — the exact statements the DS runs and the
/// exact statements `pollis_core::commands::groups::authz` runs — so the only
/// thing restated here is how the client COMBINES them. That combination is what
/// this test is for; the statements themselves cannot drift because there is one
/// copy of each.
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

/// **The parity assertion (#942).**
///
/// `request_group_access` duplicates the DS's existence and membership checks
/// client-side. That is deliberate — it turns a doomed round trip into a fast,
/// specific error — but it makes one rule with two evaluators, and the DS is the
/// only one that enforces anything. A divergence is not cosmetic:
///
///   * client STRICTER than the DS → an action the server would have allowed is
///     refused locally, and the user is told "you are already a member" of a
///     group they can in fact ask to join. Nothing in the DS's tests would
///     notice.
///   * client LOOSER than the DS → the preflight passes, the request goes out,
///     and the user gets the opaque 403 the preflight exists to prevent.
///
/// So both sides run over the same matrix and must reach the same verdict on
/// every row. The expected column is spelled out literally as well, so a change
/// applied symmetrically to both evaluators still fails here rather than quietly
/// redefining the rule.
#[tokio::test]
async fn preflight_parity() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    // (group, requester, may request, why)
    let matrix: &[(&str, &str, bool, &str)] = &[
        (GROUP, STRANGER, true, "a non-member of a real group may ask"),
        (GROUP, MEMBER, false, "a current member has nothing to request"),
        (GROUP, ADMIN, false, "an admin is a member too"),
        ("grp-does-not-exist", STRANGER, false, "an unknown group id is refused"),
        ("", STRANGER, false, "an empty group id is not a group"),
        // Allowed, and correctly so: this predicate answers "is this actor in
        // the group", and an actor who is in no group is not. Rejecting an
        // unauthenticated caller is `resolve_actor`'s job upstream, not this
        // rule's — a row worth keeping precisely because it is the one a reader
        // expects to be `false`.
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
        // Leave no row behind — the next matrix row must see a clean slate, and
        // the upsert would otherwise make later rows depend on earlier ones.
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

/// The empty-group-id row above is only meaningful if an empty id really is
/// absent from `groups`; otherwise the case proves nothing. Pinned separately so
/// a fixture change cannot hollow it out.
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
