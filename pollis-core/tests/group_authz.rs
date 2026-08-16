//! #875 — the ONE group-membership preflight
//! ([`pollis_core::commands::groups::authz`]).
//!
//! Fourteen copies of this check existed across `commands/groups/*.rs` and
//! `commands/messages/edit_delete.rs`, and they had drifted into four different
//! answers for one condition: "you are not a member of this group", "requester
//! is not a group member", "user is not a member of this group", and two sites
//! that returned `Ok(Vec::new())` — indistinguishable from "there is nothing
//! here". These tests pin the unified contract.
//!
//! They live in an integration binary rather than pollis-core's `--lib` tests
//! for the same reason as `resign_stale_certs.rs` / `revoked_device_reconcile.rs`:
//! libsql's local backend calls `sqlite3_config` on first use, which fails once
//! rusqlite/SQLCipher — which `LocalDb` uses — has already initialised SQLite in
//! the same process. An integration test gets its own process and never loads
//! rusqlite, so the real preflight runs against a real DB.
//!
//! The schema is the SHIPPED one (`pollis_schema`), not a hand-rolled slice, so
//! a `group_member` a fixture invents cannot be laxer than the table the
//! preflight actually reads in production.

use pollis_core::commands::groups::authz::{
    channel_group_role, group_role, require_admin, require_member, require_target_member,
    GroupRole, NOT_A_MEMBER,
};

/// Returns the `Database` alongside the `Connection`: libsql's local backend
/// panics on drop if a connection outlives the database it came from, so the
/// handle has to stay bound in the test.
async fn db() -> (libsql::Database, libsql::Connection) {
    let db = libsql::Builder::new_local(":memory:")
        .build()
        .await
        .expect("in-memory libsql");
    let conn = db.connect().expect("connect");
    for sql in pollis_schema::main_scripts() {
        conn.execute_batch(sql).await.expect("schema");
    }
    conn.execute_batch(
        "INSERT INTO users (id, email, username) VALUES \
           ('alice', 'a@x.com', 'alice'), ('bob', 'b@x.com', 'bob'), \
           ('carol', 'c@x.com', 'carol'), ('dave', 'd@x.com', 'dave');\
         INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'alice');\
         INSERT INTO group_member (group_id, user_id, role) VALUES \
           ('g1', 'alice', 'admin'), ('g1', 'bob', 'member'), ('g1', 'dave', 'wizard');\
         INSERT INTO channels (id, group_id, name) VALUES ('ch1', 'g1', 'general');\
         INSERT INTO dm_channel (id, created_by) VALUES ('dm1', 'alice');",
    )
    .await
    .expect("seed");
    (db, conn)
}

fn msg(e: pollis_core::error::Error) -> String {
    e.to_string()
}

#[tokio::test]
async fn role_is_read_from_group_member() {
    let (_db, conn) = db().await;
    assert_eq!(
        group_role(&conn, "g1", "alice").await.unwrap(),
        Some(GroupRole::Admin)
    );
    assert_eq!(
        group_role(&conn, "g1", "bob").await.unwrap(),
        Some(GroupRole::Member)
    );
    assert_eq!(group_role(&conn, "g1", "carol").await.unwrap(), None);
}

/// `group_member.role` has no CHECK constraint, so an unrecognised string is
/// storable. It must read as a plain member — never as an admin — matching the
/// DS's `== Some("admin")`. A `role != "admin"` comparison and a
/// `role == "admin"` comparison agree on this case only if both are written
/// carefully; one helper removes the chance of getting it wrong twice.
#[tokio::test]
async fn unknown_role_string_is_not_admin() {
    let (_db, conn) = db().await;
    assert_eq!(
        group_role(&conn, "g1", "dave").await.unwrap(),
        Some(GroupRole::Member)
    );
    assert!(require_admin(&conn, "g1", "dave", "do the thing")
        .await
        .is_err());
}

/// The single wording. Before #875 this same condition produced three different
/// sentences depending on which command you called.
#[tokio::test]
async fn non_member_gets_one_wording() {
    let (_db, conn) = db().await;
    assert_eq!(
        msg(require_member(&conn, "g1", "carol").await.unwrap_err()),
        NOT_A_MEMBER
    );
    assert_eq!(
        msg(require_admin(&conn, "g1", "carol", "update group settings")
            .await
            .unwrap_err()),
        NOT_A_MEMBER,
        "a non-member must be told they are not a member, not that they are not an admin"
    );
}

#[tokio::test]
async fn member_who_is_not_admin_is_told_which_action() {
    let (_db, conn) = db().await;
    assert_eq!(
        msg(require_admin(&conn, "g1", "bob", "delete the group")
            .await
            .unwrap_err()),
        "only group admins can delete the group"
    );
    assert!(require_admin(&conn, "g1", "alice", "delete the group")
        .await
        .is_ok());
}

#[tokio::test]
async fn target_membership_has_its_own_wording() {
    let (_db, conn) = db().await;
    assert_eq!(
        msg(require_target_member(&conn, "g1", "carol")
            .await
            .unwrap_err()),
        "user is not a member of this group",
        "the TARGET of an action is a different subject from the caller"
    );
    assert!(require_target_member(&conn, "g1", "bob").await.is_ok());
}

/// A DM has no `channels` row and no admin concept, so it must be `None` rather
/// than an error — the caller decides what that means.
#[tokio::test]
async fn channel_role_resolves_through_the_owning_group() {
    let (_db, conn) = db().await;
    assert_eq!(
        channel_group_role(&conn, "ch1", "alice").await.unwrap(),
        Some(GroupRole::Admin)
    );
    assert_eq!(
        channel_group_role(&conn, "ch1", "carol").await.unwrap(),
        None
    );
    assert_eq!(
        channel_group_role(&conn, "dm1", "alice").await.unwrap(),
        None
    );
}
