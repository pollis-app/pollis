//! One group-membership preflight, used by every group-scoped command.
//! A client-side preflight for error wording only; the DS enforces the same rule.

use std::sync::Arc;

use crate::state::AppState;

use crate::error::{Error, Result};

/// The caller's standing in a group; non-membership is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupRole {
    Admin,
    Member,
}

impl GroupRole {
    pub fn is_admin(self) -> bool {
        matches!(self, GroupRole::Admin)
    }

    /// An unknown string is a plain member, matching the DS's `== Some("admin")`.
    pub fn from_column(raw: &str) -> Self {
        if raw == "admin" {
            GroupRole::Admin
        } else {
            GroupRole::Member
        }
    }
}

/// The one wording for "the caller is not in this group".
pub const NOT_A_MEMBER: &str = "you are not a member of this group";

/// Wording for "the target of this action is not in this group".
pub const TARGET_NOT_A_MEMBER: &str = "user is not a member of this group";

fn not_a_member() -> Error {
    Error::Other(anyhow::anyhow!(NOT_A_MEMBER))
}

fn not_an_admin(action: &str) -> Error {
    Error::Other(anyhow::anyhow!("only group admins can {action}"))
}

/// The caller's role in `group_id`, or `None` when they are not a member.
/// `group_id` may also name a channel; the answer is then about its group.
pub async fn group_role(
    state: &Arc<AppState>,
    group_id: &str,
    user_id: &str,
) -> Result<Option<GroupRole>> {
    let resp = crate::commands::ds_reads::group_role(state, group_id, user_id).await?;
    Ok(resp.role.as_deref().map(GroupRole::from_column))
}

/// Whether `group_id` names a real group.
pub async fn group_exists(state: &Arc<AppState>, group_id: &str) -> Result<bool> {
    Ok(crate::commands::ds_reads::group_role(state, group_id, "")
        .await?
        .exists)
}

/// The caller's role in the group owning `channel_id`; `None` for a DM or non-member.
pub async fn channel_group_role(
    state: &Arc<AppState>,
    channel_id: &str,
    user_id: &str,
) -> Result<Option<GroupRole>> {
    group_role(state, channel_id, user_id).await
}

/// Membership required: `Some(role)` passes through, `None` is [`NOT_A_MEMBER`].
pub fn decide_member(role: Option<GroupRole>) -> Result<GroupRole> {
    role.ok_or_else(not_a_member)
}

/// Admin required. A NON-MEMBER is told they are not a member, not that they are
/// not an admin — the two are different facts and conflating them tells a
/// stranger which groups exist.
pub fn decide_admin(role: Option<GroupRole>, action: &str) -> Result<()> {
    if decide_member(role)?.is_admin() {
        return Ok(());
    }
    Err(not_an_admin(action))
}

/// The target of an action must be a member.
pub fn decide_target_member(role: Option<GroupRole>) -> Result<GroupRole> {
    role.ok_or_else(|| Error::Other(anyhow::anyhow!(TARGET_NOT_A_MEMBER)))
}

/// Require membership; returns the role so admin-only branches need no second query.
pub async fn require_member(
    state: &Arc<AppState>,
    group_id: &str,
    user_id: &str,
) -> Result<GroupRole> {
    decide_member(group_role(state, group_id, user_id).await?)
}

/// Require admin. `action` completes "only group admins can …".
pub async fn require_admin(
    state: &Arc<AppState>,
    group_id: &str,
    user_id: &str,
    action: &str,
) -> Result<()> {
    decide_admin(group_role(state, group_id, user_id).await?, action)
}

/// Require that `user_id` is a member of `group_id` — used to validate the
/// *target* of an action, not the caller.
pub async fn require_target_member(
    state: &Arc<AppState>,
    group_id: &str,
    requester_id: &str,
    target_user_id: &str,
) -> Result<GroupRole> {
    let members = crate::commands::ds_reads::group(state, group_id, requester_id, false)
        .await?
        .members;
    decide_target_member(
        members
            .iter()
            .find(|m| m.user_id == target_user_id)
            .map(|m| GroupRole::from_column(&m.role)),
    )
}
