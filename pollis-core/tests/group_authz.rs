//! #875 — the ONE group-membership preflight
//! ([`pollis_core::commands::groups::authz`]).
//!
//! The fetch half is tested by `pollis-delivery/tests/join_requests.rs::preflight_parity`.

use pollis_core::commands::groups::authz::{
    decide_admin, decide_member, decide_target_member, GroupRole, NOT_A_MEMBER,
    TARGET_NOT_A_MEMBER,
};

fn msg(e: pollis_core::error::Error) -> String {
    e.to_string()
}

/// An unknown role must read as member, matching the DS's `== Some("admin")`.
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

#[test]
fn a_member_who_is_not_an_admin_is_told_which_action() {
    assert_eq!(
        msg(decide_admin(Some(GroupRole::Member), "delete the group").unwrap_err()),
        "only group admins can delete the group"
    );
    assert!(decide_admin(Some(GroupRole::Admin), "delete the group").is_ok());
}

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
