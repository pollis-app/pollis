use std::sync::Arc;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::GroupMember;

pub async fn get_group_members(
    group_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<GroupMember>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT gm.user_id, u.username, u.avatar_url, gm.role, gm.joined_at
         FROM group_member gm
         LEFT JOIN users u ON u.id = gm.user_id
         WHERE gm.group_id = ?1",
        libsql::params![group_id],
    ).await?;

    let mut members = Vec::new();
    while let Some(row) = rows.next().await? {
        members.push(GroupMember {
            user_id: row.get(0)?,
            username: row.get(1)?,
            display_name: None,
            avatar_url: row.get(2)?,
            role: row.get(3)?,
            joined_at: row.get(4)?,
        });
    }

    Ok(members)
}

pub async fn remove_member_from_group(
    group_id: String,
    user_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Check requester's role
    let mut rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), requester_id.clone()],
    ).await?;

    let requester_role: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("requester is not a group member")));
    };

    // Admins can remove others; anyone can remove themselves (leave)
    if requester_id != user_id && requester_role != "admin" {
        return Err(Error::Other(anyhow::anyhow!(
            "only an admin can remove other members"
        )));
    }

    // Route the member-row delete through the Delivery Service (which re-derives
    // the admin/self rule server-side).
    let body = serde_json::json!({
        "group_id": group_id,
        "user_id": user_id,
        "requester_id": requester_id,
    });
    crate::commands::mls::ds_post_ok(state, "/v1/members/remove", &body).await?;

    // Ingest-before-advance (issue #440, committer strand): catch this device up
    // to head with the INTERLEAVED ingesting catch-up — decrypting every bound
    // conversation's messages at each epoch — BEFORE reconcile stages + merges
    // our remove commit and advances our epoch. Reconcile advances the shared MLS
    // group; with `max_past_epochs = 0`, any current-epoch inbound message we
    // haven't fetched yet would have its keys discarded the instant we advance
    // past it. Hoisted ABOVE reconcile deliberately: `reconcile_group_mls_impl`
    // holds the per-conversation MLS lock (`mls_group_lock`) for its whole body,
    // and the interleaved catch-up re-acquires that SAME lock internally — running
    // it here, before reconcile takes the lock, ingests the current epoch without
    // deadlocking.
    if let Err(e) = crate::commands::messages::catch_up_mls_group_interleaved(
        state, &group_id, &requester_id,
    ).await {
        eprintln!("[mls] remove_member_from_group: catch_up_mls_group for {group_id}: {e}");
    }

    // Reconcile removes the member's leaves from the MLS tree.
    if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
        state, &group_id, &requester_id,
    ).await {
        eprintln!("[mls] remove_member_from_group: reconcile for group {group_id}: {e}");
    }

    // Notify group members so they refetch the member list.
    if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
        &state.livekit,
        &group_id,
    ).await {
        eprintln!("[realtime] remove_member_from_group: notify group {group_id}: {e}");
    }

    Ok(())
}

pub async fn leave_group(
    group_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Check if user is the owner
    let mut rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), user_id.clone()],
    ).await?;

    let _role: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("user is not a member of this group")));
    };

    // Owners can leave thegroup, there's no requirement for ownership atm so I am commenting this out.
    // Might change when we introduce rolls, give them the option to require transfer, etc.

    // if role == "owner" && member_count > 1 {
    //     return Err(Error::Other(anyhow::anyhow!(
    //         "owner must transfer ownership before leaving the group"
    //     )));
    // }

    // Route the leaver's member-row delete (and, when the group empties, the group
    // delete) through the Delivery Service — one server-authorized write scoped to
    // the signer's own row.
    let body = serde_json::json!({
        "group_id": group_id,
        "user_id": user_id,
    });
    crate::commands::mls::ds_post_ok(state, "/v1/groups/leave", &body).await?;

    // A user cannot commit their own removal in MLS ("remove_members with self
    // as target" is rejected by the spec).  Instead, wipe the local group state
    // so the leaver can no longer read or send messages.  The remaining members
    // still see this user in their epoch until an admin issues a remove commit,
    // but forward secrecy ensures the leaver cannot decrypt future traffic after
    // the next epoch advance.
    match crate::commands::mls::forget_local_mls_group(state, &group_id).await {
        Ok(()) => {}
        Err(e) => eprintln!("[mls] leave_group: forget local group {group_id}: {e}"),
    }

    // Signal remaining members to reconcile (removes the leaver's stale leaf).
    // Use publish_to_room_server since the leaver may not be connected to the room.
    if let Err(e) = crate::commands::livekit::publish_to_room_server(
        &state.config,
        &group_id,
        serde_json::json!({"type": "membership_changed", "group_id": group_id}),
    ).await {
        eprintln!("[realtime] leave_group: notify group {group_id}: {e}");
    }

    // If no members remain, the group is deleted (cascades to channels, invites,
    // etc.); the DS handles that server-side inside the leave write above.

    Ok(())
}

/// Promote or demote a group member. Requester must be an admin.
/// Valid roles: 'admin', 'member'.
pub async fn set_member_role(
    group_id: String,
    user_id: String,
    role: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    if role != "admin" && role != "member" {
        return Err(Error::Other(anyhow::anyhow!("invalid role: must be 'admin' or 'member'")));
    }

    let conn = state.remote_db.conn().await?;

    // Requester must be admin
    let mut rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), requester_id.clone()],
    ).await?;

    let requester_role: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("you are not a member of this group")));
    };

    if requester_role != "admin" {
        return Err(Error::Other(anyhow::anyhow!("only admins can change member roles")));
    }

    // Verify target is a member
    let mut target_rows = conn.query(
        "SELECT 1 FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), user_id.clone()],
    ).await?;

    if target_rows.next().await?.is_none() {
        return Err(Error::Other(anyhow::anyhow!("user is not a member of this group")));
    }

    // Route the role update through the Delivery Service (admin re-derived
    // server-side, target-membership re-checked).
    let body = serde_json::json!({
        "group_id": group_id,
        "user_id": user_id,
        "role": role,
        "requester_id": requester_id,
    });
    crate::commands::mls::ds_post_ok(state, "/v1/members/role", &body).await?;

    // Notify other online group members so their members list refreshes.
    // Best-effort realtime push; routed through the livekit boundary so this
    // call site stays platform-agnostic (no-op on mobile, see issue #185).
    if let Err(e) = crate::commands::livekit::publish_member_role_changed_to_room(
        &state.livekit,
        &group_id,
    )
    .await
    {
        eprintln!("[role] publish MemberRoleChanged: {e}");
    }

    Ok(())
}
