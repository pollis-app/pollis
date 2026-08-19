//! One group-membership preflight, used by every group-scoped command.
//!
//! Before #875 this check was copied fourteen times across `groups/*.rs` and
//! `messages/edit_delete.rs`. The copies had drifted into four different
//! answers for the *same* condition — "you are not a member of this group",
//! "requester is not a group member", "user is not a member of this group", and
//! two sites that returned `Ok(Vec::new())`, so a caller could not tell "you may
//! not see this" from "there is nothing here". That last one is the reason this
//! module exists: an authorization outcome that is indistinguishable from an
//! empty result is not a message, it is a silence.
//!
//! ## What this layer is and is not
//!
//! It is a **client-side preflight**: it turns a doomed round-trip into a clear
//! local error and lets the UI render the right state. It is **not** the
//! enforcement point. Every write these commands guard also goes through the
//! Delivery Service, which re-derives the same role from `group_member` and
//! answers 403 (`pollis_delivery::groups::{group_role, is_admin}`) — the DS is
//! untrusted-by-the-client but authoritative-for-the-server, and it never
//! trusts a client-supplied role.
//!
//! ## What #987 changed
//!
//! The role now comes from `POST /v1/directory/group` (`role_only`), not from a
//! client-held database credential — which is the whole point of #987, and also
//! resolves the residual this module used to have to write down. The old note
//! said the guards here were an *application-level* protection only, because the
//! client's whole-DB read token let any user run `SELECT * FROM group_member`
//! themselves. **There is no such token any more.** The DS decides what a caller
//! may read, using the same predicate it uses to decide what they may write, so
//! the guard and the enforcement are the same rule evaluated in the same place.
//!
//! Two consequences worth stating, because both are easy to forget:
//!
//!   * These calls are now NETWORK calls. An unreachable DS makes a preflight
//!     fail where it used to succeed against a warm connection — which is the
//!     correct direction (refuse rather than proceed into a doomed write), but it
//!     means a preflight failure is now an ordinary event rather than a
//!     vanishing one.
//!   * The preflight is no longer a saving. It costs a round trip that the write
//!     would have made anyway, so it exists purely for the ERROR MESSAGE. That is
//!     still worth it — "only group admins can invite members" is a different
//!     product than an opaque 403 — but a new command that does not need a
//!     specific message should simply post the write and report its refusal.
//!
//! ## Roles
//!
//! `group_member.role` carries `'admin'` or `'member'` and nothing else.
//! `'owner'` was renamed to `'admin'` by migration 000008 and `groups.owner_id`
//! is a display field, never an authorization input — so an unknown role string
//! is treated as a plain member, matching the DS's `== Some("admin")`.

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

    fn from_column(raw: &str) -> Self {
        if raw == "admin" {
            GroupRole::Admin
        } else {
            GroupRole::Member
        }
    }
}

/// The one wording for "the caller is not in this group". Every site that used
/// to invent its own now returns exactly this.
pub const NOT_A_MEMBER: &str = "you are not a member of this group";

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
/// `group_id` may also name a CHANNEL, in which case the answer is about its
/// owning group. That is the DS's `resolve_conversation`, and it is what lets
/// the moderation preflight ask one question instead of two.
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
/// The other half of `request_group_access`'s preflight. Reported by the DS
/// separately from membership because the join flow needs both: "no such group"
/// and "you are already a member" are different refusals, and a single boolean
/// would make one of them wrong.
pub async fn group_exists(state: &Arc<AppState>, group_id: &str) -> Result<bool> {
    Ok(crate::commands::ds_reads::group_role(state, group_id, "")
        .await?
        .exists)
}

/// The caller's role in the group that owns `channel_id`, or `None` when the
/// conversation is not a group channel (a DM has no `channels` row) or the
/// caller is not a member. One round trip, mirroring the DS's
/// `messages::channel_group_role`.
pub async fn channel_group_role(
    state: &Arc<AppState>,
    channel_id: &str,
    user_id: &str,
) -> Result<Option<GroupRole>> {
    group_role(state, channel_id, user_id).await
}

/// Require membership. Returns the caller's role so an admin-only *branch* can
/// be taken without a second query (e.g. "members may remove themselves, admins
/// may remove anyone").
pub async fn require_member(
    state: &Arc<AppState>,
    group_id: &str,
    user_id: &str,
) -> Result<GroupRole> {
    group_role(state, group_id, user_id)
        .await?
        .ok_or_else(not_a_member)
}

/// Require admin. `action` completes "only group admins can …".
pub async fn require_admin(
    state: &Arc<AppState>,
    group_id: &str,
    user_id: &str,
    action: &str,
) -> Result<()> {
    if require_member(state, group_id, user_id).await?.is_admin() {
        return Ok(());
    }
    Err(not_an_admin(action))
}

/// Require that `user_id` is a member of `group_id` — used to validate the
/// *target* of an action, not the caller.
///
/// Since #987 the DS answers about the AUTHENTICATED caller, not an arbitrary
/// user — a client cannot ask "is Bob in this group" and get a straight answer,
/// which is a tightening, not a regression. The target-membership rule is
/// re-derived server-side inside the write it guards
/// (`pollis_delivery::groups::apply_set_member_role`), so the enforcement is
/// unchanged; what is gone is the client's ability to enumerate someone else's
/// standing before attempting it.
pub async fn require_target_member(
    state: &Arc<AppState>,
    group_id: &str,
    user_id: &str,
) -> Result<GroupRole> {
    let members = crate::commands::ds_reads::group(state, group_id, user_id, false)
        .await?
        .members;
    members
        .iter()
        .find(|m| m.user_id == user_id)
        .map(|m| GroupRole::from_column(&m.role))
        .ok_or_else(|| Error::Other(anyhow::anyhow!("user is not a member of this group")))
}
