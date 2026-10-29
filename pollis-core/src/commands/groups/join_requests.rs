use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::JoinRequest;
use super::authz;

/// Request access to a group. Creates a pending join request.
pub async fn request_group_access(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // No preflight (#987): the write names its outcome; a prior row resets to pending, keeping `reviewed_*`.
    let id = Ulid::new().to_string();
    let body = pollis_api::groups::CreateJoinRequestBody {
        id,
        group_id: group_id.clone(),
        requester_id: Some(requester_id),
    };
    match crate::commands::mls::ds_post_json(state, &body).await? {
        pollis_api::groups::JoinRequestCreated::Ok => {}
        pollis_api::groups::JoinRequestCreated::NoSuchGroup => {
            return Err(Error::Other(anyhow::anyhow!("group not found")));
        }
        pollis_api::groups::JoinRequestCreated::AlreadyMember => {
            return Err(Error::Other(anyhow::anyhow!(
                "you are already a member of this group"
            )));
        }
        pollis_api::groups::JoinRequestCreated::AlreadyPending => {
            return Err(Error::Other(anyhow::anyhow!(
                "you already have a pending request for this group"
            )));
        }
    }

    // Notify admins via server-side publish (the requester isn't in the room); best-effort.
    if let Err(e) = crate::commands::livekit::publish_join_requests_changed_to_room(
        state,
        &group_id,
    ).await {
        crate::diag!("[realtime] request_group_access: notify group {group_id}: {e}");
    }

    Ok(())
}

/// Get all pending join requests for a group. Admins only.
pub async fn get_group_join_requests(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<JoinRequest>> {
    // The DS returns `join_requests` only to an admin, so guard and payload are one decision.
    let resp = crate::commands::ds_reads::group(state, &group_id, &requester_id, false).await?;
    if !resp.authorized {
        return Err(Error::Other(anyhow::anyhow!(authz::NOT_A_MEMBER)));
    }
    if resp.role.as_deref() != Some("admin") {
        return Err(Error::Other(anyhow::anyhow!(
            "only group admins can view join requests"
        )));
    }
    Ok(resp
        .join_requests
        .into_iter()
        .map(join_request_from_wire)
        .collect())
}

/// Wire → domain for one join request.
fn join_request_from_wire(r: pollis_api::directory::JoinRequestWire) -> JoinRequest {
    JoinRequest {
        id: r.id,
        group_id: r.group_id,
        requester_id: r.requester_id,
        requester_username: r.requester_username,
        status: r.status,
        created_at: r.created_at,
    }
}

/// Get the current user's own join request for a specific group, if one exists.
/// Returns None if no request has been made.
pub async fn get_my_join_request(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<Option<JoinRequest>> {
    // From the admin-only pending list, so a non-member sees `None` (deliberate, #987).
    let resp = crate::commands::ds_reads::group(state, &group_id, &requester_id, false).await?;
    Ok(resp
        .join_requests
        .into_iter()
        .find(|r| r.requester_id == requester_id)
        .map(join_request_from_wire))
}

/// Approve a join request. Approver must be a group member. Adds the requester to the group.
pub async fn approve_join_request(
    request_id: String,
    approver_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();

    // Take the group and requester from the response; reading the row first raced (#987).
    let body = pollis_api::groups::ApproveJoinRequestBody {
        request_id,
        approver_id: Some(approver_id.clone()),
        reviewed_at: now,
    };
    let pollis_api::groups::ReviewedJoinRequest::Ok {
        group_id,
        requester_id,
    } = crate::commands::mls::ds_post_json(state, &body).await?;
    let requester_id = requester_id.unwrap_or_default();

    // Reconcile adds the requester's devices to the MLS tree.
    if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
        state, &group_id, &approver_id,
    ).await {
        crate::diag!("[mls] approve_join_request: reconcile for group {group_id}: {e}");
    }

    // Notify requester their join request was approved so they see the group immediately.
    // `kind: "approval"` keeps it silent; they initiated the request.
    if let Err(e) = crate::commands::livekit::publish_to_user_inbox(
        state,
        &requester_id,
        serde_json::json!({"type": "membership_changed", "group_id": group_id, "kind": "approval"}),
    ).await {
        crate::diag!("[inbox] approve_join_request: notify {requester_id} failed: {e}");
    }

    // Notify existing group members so they refetch the member list.
    if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
        state,
        &group_id,
    ).await {
        crate::diag!("[realtime] approve_join_request: notify group {group_id}: {e}");
    }

    Ok(())
}

/// Reject a join request. Approver must be a group member.
pub async fn reject_join_request(
    request_id: String,
    approver_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    let body = pollis_api::groups::RejectJoinRequestBody {
        request_id,
        approver_id: Some(approver_id),
        reviewed_at: now,
    };
    let pollis_api::groups::ReviewedJoinRequest::Ok { .. } =
        crate::commands::mls::ds_post_json(state, &body).await?;

    Ok(())
}
