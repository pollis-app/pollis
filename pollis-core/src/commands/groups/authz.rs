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
//! trusts a client-supplied role. Reads go direct to Turso, where this *is* the
//! only check; that is exactly why it must be honest rather than silently empty.
//!
//! Every entry point takes the caller's existing `&Connection` rather than
//! `&AppState`: for a REMOTE database `Database::connect()` builds a whole new
//! `hyper::Client` with a cold pool, so a helper that opened its own would turn
//! one preflight into an extra TCP + TLS handshake on every guarded command.
//!
//! ## Roles
//!
//! `group_member.role` carries `'admin'` or `'member'` and nothing else.
//! `'owner'` was renamed to `'admin'` by migration 000008 (see
//! `db::remote::migration_008_owner_role_becomes_admin`) and `groups.owner_id`
//! is a display field, never an authorization input — so an unknown role string
//! is treated as a plain member, matching the DS's `== Some("admin")`.

use libsql::Connection;

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
pub async fn group_role(
    conn: &Connection,
    group_id: &str,
    user_id: &str,
) -> Result<Option<GroupRole>> {
    let mut rows = conn
        .query(
            "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
            libsql::params![group_id.to_string(), user_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(GroupRole::from_column(&row.get::<String>(0)?)),
        None => None,
    })
}

/// The caller's role in the group that owns `channel_id`, or `None` when the
/// conversation is not a group channel (a DM has no `channels` row) or the
/// caller is not a member. One round trip, mirroring the DS's
/// `messages::channel_group_role`.
pub async fn channel_group_role(
    conn: &Connection,
    channel_id: &str,
    user_id: &str,
) -> Result<Option<GroupRole>> {
    let mut rows = conn
        .query(
            "SELECT gm.role FROM channels c \
             JOIN group_member gm ON gm.group_id = c.group_id \
             WHERE c.id = ?1 AND gm.user_id = ?2",
            libsql::params![channel_id.to_string(), user_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(GroupRole::from_column(&row.get::<String>(0)?)),
        None => None,
    })
}

/// Require membership. Returns the caller's role so an admin-only *branch* can
/// be taken without a second query (e.g. "members may remove themselves, admins
/// may remove anyone").
pub async fn require_member(
    conn: &Connection,
    group_id: &str,
    user_id: &str,
) -> Result<GroupRole> {
    group_role(conn, group_id, user_id)
        .await?
        .ok_or_else(not_a_member)
}

/// Require admin. `action` completes "only group admins can …".
pub async fn require_admin(
    conn: &Connection,
    group_id: &str,
    user_id: &str,
    action: &str,
) -> Result<()> {
    if require_member(conn, group_id, user_id).await?.is_admin() {
        return Ok(());
    }
    Err(not_an_admin(action))
}

/// Require that `user_id` is a member of `group_id` — used to validate the
/// *target* of an action, not the caller.
pub async fn require_target_member(
    conn: &Connection,
    group_id: &str,
    user_id: &str,
) -> Result<GroupRole> {
    group_role(conn, group_id, user_id)
        .await?
        .ok_or_else(|| Error::Other(anyhow::anyhow!("user is not a member of this group")))
}
