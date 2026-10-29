//! One group-membership preflight, used by every group-scoped command.
//!
//! A client-side preflight, not the enforcement point: the DS re-derives the
//! same role for every write and answers 403. It exists for the error message,
//! and costs a round trip the write would make anyway, so a command that needs
//! no specific message should just post the write and report its refusal.
//! "Not allowed" is always an error, never an empty result (#875).
//!
//! Roles are `'admin'` or `'member'`; an unknown string is a plain member,
//! matching the DS's `== Some("admin")`.

use std::sync::Arc;

use crate::state::AppState;

use crate::error::{Error, Result};

/// The caller's standing in a group. Absence is represented by `Option`, never
/// by a third variant — "not a member" is not a role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupRole {
    Admin,
    Member,
}

impl GroupRole {
    pub fn is_admin(self) -> bool {
        matches!(self, GroupRole::Admin)
    }

    /// `group_member.role` -> a role. An unknown string is a plain member,
    /// matching the DS, or a corrupt column would mean different things on
    /// each side.
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

/// The one wording for "the target of this action is not in this group".
pub const TARGET_NOT_A_MEMBER: &str = "user is not a member of this group";

fn not_a_member() -> Error {
    Error::Other(anyhow::anyhow!(NOT_A_MEMBER))
}

/// `only group admins can {action}` — `action` is a verb phrase, e.g.
/// `"update group settings"`.
fn not_an_admin(action: &str) -> Error {
    Error::Other(anyhow::anyhow!("only group admins can {action}"))
}

/// The caller's role in `group_id`, or `None` when they are not a member.
/// Prefer [`require_member`] / [`require_admin`] — reach for this only where
/// "not a member" is a legitimate non-error outcome.
///
/// `group_id` may also name a channel; the answer is then about its owning
/// group.
pub async fn group_role(
    state: &Arc<AppState>,
    group_id: &str,
    user_id: &str,
) -> Result<Option<GroupRole>> {
    let resp = crate::commands::ds_reads::group_role(state, group_id, user_id).await?;
    Ok(resp.role.as_deref().map(GroupRole::from_column))
}

/// Whether `group_id` names a real group.
///
/// Reported separately from membership: "no such group" and "already a member"
/// are different refusals.
pub async fn group_exists(state: &Arc<AppState>, group_id: &str) -> Result<bool> {
    Ok(crate::commands::ds_reads::group_role(state, group_id, "")
        .await?
        .exists)
}

/// The caller's role in the group that owns `channel_id`, or `None` for a DM or
/// a non-member.
pub async fn channel_group_role(
    state: &Arc<AppState>,
    channel_id: &str,
    user_id: &str,
) -> Result<Option<GroupRole>> {
    group_role(state, channel_id, user_id).await
}

// Pure decisions, so the wordings are testable without a DS; the async
// wrappers only fetch and delegate.

/// Membership required: `Some(role)` passes through, `None` is [`NOT_A_MEMBER`].
pub fn decide_member(role: Option<GroupRole>) -> Result<GroupRole> {
    role.ok_or_else(not_a_member)
}

/// Admin required. A non-member is told they are not a member, or a stranger
/// could learn which groups exist.
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

/// Require membership. Returns the role, so an admin-only branch needs no
/// second query.
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

/// Require that `user_id` (the target, not the caller) is a member. The DS
/// re-derives this inside the write it guards.
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
