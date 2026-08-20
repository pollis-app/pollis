use std::sync::Arc;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::GroupMember;
use super::authz;

/// Every member of a group, with the profile fields the member list renders.
///
/// #917: members-only. This took no caller at all, so any signed-in user who
/// named a group id got its full roster — usernames and avatars included — and
/// ids are enumerable because every client holds a whole-DB read-only Turso
/// token. A roster is precisely the social-graph metadata this product exists
/// to protect, so the answer for an outsider is the error, not an empty list:
/// #875 established that a silence a caller cannot distinguish from "there is
/// nothing here" is not an authorization outcome.
pub async fn get_group_members(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<GroupMember>> {
    // Membership and the roster in ONE request. Non-membership comes back as
    // `authorized: false`, which this turns into the shared refusal — the point
    // of #875's chokepoint, now enforced by the server rather than asked of it.
    let resp = crate::commands::ds_reads::group(state, &group_id, &requester_id, false).await?;
    if !resp.authorized {
        return Err(Error::Other(anyhow::anyhow!(authz::NOT_A_MEMBER)));
    }
    Ok(resp
        .members
        .into_iter()
        .map(|m| GroupMember {
            user_id: m.user_id,
            username: m.username,
            display_name: None,
            avatar_url: m.avatar_url,
            role: m.role,
            joined_at: m.joined_at,
        })
        .collect())
}

pub async fn remove_member_from_group(
    group_id: String,
    user_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // Admins can remove others; anyone can remove themselves (leave). Both
    // branches need membership first, so take the role once.
    let requester_role = authz::require_member(state, &group_id, &requester_id).await?;
    if requester_id != user_id && !requester_role.is_admin() {
        return Err(Error::Other(anyhow::anyhow!(
            "only group admins can remove other members"
        )));
    }

    // Route the member-row delete through the Delivery Service (which re-derives
    // the admin/self rule server-side).
    let body = pollis_api::groups::RemoveMemberBody {
        group_id: group_id.clone(),
        user_id,
        requester_id: Some(requester_id.clone()),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

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
    // Membership is the only bar: any member may leave, admins included. There
    // is no ownership to transfer — migration 000008 renamed the 'owner' role to
    // 'admin' and `groups.owner_id` is a display field, not an authz input.
    authz::require_member(state, &group_id, &user_id).await?;

    // Route the leaver's member-row delete (and, when the group empties, the group
    // delete) through the Delivery Service — one server-authorized write scoped to
    // the signer's own row.
    let body = pollis_api::groups::LeaveGroupBody {
        group_id: group_id.clone(),
        user_id: Some(user_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

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
        state,
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

    authz::require_admin(state, &group_id, &requester_id, "change member roles").await?;
    authz::require_target_member(state, &group_id, &requester_id, &user_id).await?;

    // Route the role update through the Delivery Service (admin re-derived
    // server-side, target-membership re-checked).
    let body = pollis_api::groups::SetMemberRoleBody {
        group_id: group_id.clone(),
        user_id,
        role,
        requester_id: Some(requester_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

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
