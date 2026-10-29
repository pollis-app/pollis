use std::sync::Arc;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::GroupMember;
use super::authz;

/// Every member of a group, with the profile fields the member list renders.
/// Members-only (#917): an outsider gets the error, not an empty list.
pub async fn get_group_members(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<GroupMember>> {
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
    // Admins can remove others; anyone can remove themselves (leave).
    let requester_role = authz::require_member(state, &group_id, &requester_id).await?;
    if requester_id != user_id && !requester_role.is_admin() {
        return Err(Error::Other(anyhow::anyhow!(
            "only group admins can remove other members"
        )));
    }

    let body = pollis_api::groups::RemoveMemberBody {
        group_id: group_id.clone(),
        user_id,
        requester_id: Some(requester_id.clone()),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Ingest-before-advance (#440): catch up before reconcile, which holds `mls_group_lock` (re-acquired by the catch-up).
    if let Err(e) = crate::commands::messages::catch_up_mls_group_interleaved(
        state, &group_id, &requester_id,
    ).await {
        crate::diag!("[mls] remove_member_from_group: catch_up_mls_group for {group_id}: {e}");
    }

    // Reconcile removes the member's leaves from the MLS tree.
    if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
        state, &group_id, &requester_id,
    ).await {
        crate::diag!("[mls] remove_member_from_group: reconcile for group {group_id}: {e}");
    }

    // Notify group members so they refetch the member list.
    if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
        state,
        &group_id,
    ).await {
        crate::diag!("[realtime] remove_member_from_group: notify group {group_id}: {e}");
    }

    Ok(())
}

pub async fn leave_group(
    group_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // Any member may leave, admins included; there is no ownership to transfer.
    authz::require_member(state, &group_id, &user_id).await?;

    let body = pollis_api::groups::LeaveGroupBody {
        group_id: group_id.clone(),
        user_id: Some(user_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // MLS forbids committing your own removal, so wipe local state; the next epoch locks the leaver out.
    match crate::commands::mls::forget_local_mls_group(state, &group_id).await {
        Ok(()) => {}
        Err(e) => crate::diag!("[mls] leave_group: forget local group {group_id}: {e}"),
    }

    // `kind: "leave"` makes remaining members commit the removal the leaver cannot (#1081).
    if let Err(e) = crate::commands::livekit::publish_to_room_server(
        state,
        &group_id,
        crate::commands::livekit_signalling::member_left_group_payload(&group_id),
    ).await {
        crate::diag!("[realtime] leave_group: notify group {group_id}: {e}");
    }

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

    let body = pollis_api::groups::SetMemberRoleBody {
        group_id: group_id.clone(),
        user_id,
        role,
        requester_id: Some(requester_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Notify other online group members so their members list refreshes.
    if let Err(e) = crate::commands::livekit::publish_member_role_changed_to_room(
        state,
        &group_id,
    )
    .await
    {
        crate::diag!("[role] publish MemberRoleChanged: {e}");
    }

    Ok(())
}
