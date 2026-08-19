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
//! # What #987 changed about this file
//!
//! It used to build an in-memory libsql database, seed `group_member`, and run
//! the whole preflight — fetch and verdict together — against it. The client
//! holds no database credential any more: the role arrives from
//! `POST /v1/directory/group`, and the preflight's client-side share is exactly
//! the two pure decisions plus the column mapping. So that is what this
//! exercises, and it no longer needs its own process to avoid libsql's
//! `sqlite3_config` collision with rusqlite/SQLCipher.
//!
//! The FETCH half did not lose coverage, it moved with the query:
//!
//!   * `pollis-delivery/tests/join_requests.rs::preflight_parity` runs
//!     [`pollis_schema::authz::GROUP_ROLE_SQL`] and `GROUP_EXISTS_SQL` — the
//!     statements both crates share — against the SHIPPED schema.
//!   * The DS re-derives the same role inside every write it guards, so the
//!     authorization these messages describe is enforced there regardless of
//!     what a client believes.

use pollis_core::commands::groups::authz::{
    decide_admin, decide_member, decide_target_member, GroupRole, NOT_A_MEMBER,
    TARGET_NOT_A_MEMBER,
};

fn msg(e: pollis_core::error::Error) -> String {
    e.to_string()
}

/// `group_member.role` carries `'admin'` or `'member'` and nothing else
/// (`'owner'` was renamed by migration 000008). An unknown string must read as a
/// plain member, because that is what the DS's `== Some("admin")` does — the two
/// sides disagreeing about a corrupt column is how a non-admin becomes an admin
/// on one of them.
#[test]
fn the_role_column_maps_the_same_way_the_ds_maps_it() {
    assert_eq!(GroupRole::from_column("admin"), GroupRole::Admin);
    assert_eq!(GroupRole::from_column("member"), GroupRole::Member);
    assert_eq!(
        GroupRole::from_column("wizard"),
        GroupRole::Member,
        "an unknown role must NOT be admin — the DS tests for exactly \"admin\", so \
         reading it as admin here would let a corrupt column grant privileges the \
         server would then refuse (or, worse, not)"
    );
    assert_eq!(GroupRole::from_column(""), GroupRole::Member);
    assert_eq!(
        GroupRole::from_column("Admin"),
        GroupRole::Member,
        "the comparison is exact; the column is written lowercase by the schema"
    );
}

/// The headline of #875: ONE sentence for "you are not in this group", not four
/// different ones depending on which command you called.
#[test]
fn a_non_member_gets_one_wording() {
    assert_eq!(msg(decide_member(None).unwrap_err()), NOT_A_MEMBER);
    assert_eq!(
        msg(decide_admin(None, "update group settings").unwrap_err()),
        NOT_A_MEMBER,
        "a non-member must be told they are not a member, not that they are not an \
         admin — the second answer confirms the group exists to someone with no \
         standing in it"
    );
}

/// A member who is not an admin is told WHICH action they lack the standing for.
/// The action is a verb phrase supplied by the call site, so the message is
/// specific without each site inventing its own noun for "admin".
#[test]
fn a_member_who_is_not_an_admin_is_told_which_action() {
    assert_eq!(
        msg(decide_admin(Some(GroupRole::Member), "delete the group").unwrap_err()),
        "only group admins can delete the group"
    );
    assert!(decide_admin(Some(GroupRole::Admin), "delete the group").is_ok());
}

/// Membership passes the role through, so an admin-only BRANCH can be taken
/// without a second round trip ("members may remove themselves, admins may
/// remove anyone").
#[test]
fn membership_returns_the_role_rather_than_a_bare_ok() {
    assert_eq!(decide_member(Some(GroupRole::Admin)).unwrap(), GroupRole::Admin);
    assert_eq!(
        decide_member(Some(GroupRole::Member)).unwrap(),
        GroupRole::Member
    );
    assert!(!GroupRole::Member.is_admin());
    assert!(GroupRole::Admin.is_admin());
}

/// The TARGET of an action is a different subject from the caller, and gets its
/// own sentence. Collapsing the two would report "you are not a member of this
/// group" to an admin who is very much a member and merely named someone who is
/// not.
#[test]
fn target_membership_has_its_own_wording() {
    assert_eq!(
        msg(decide_target_member(None).unwrap_err()),
        TARGET_NOT_A_MEMBER
    );
    assert_ne!(
        TARGET_NOT_A_MEMBER, NOT_A_MEMBER,
        "the caller and the target must not share a sentence"
    );
    assert!(decide_target_member(Some(GroupRole::Member)).is_ok());
}
