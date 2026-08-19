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
    // NO PREFLIGHT since #987 — and that is the point.
    //
    // This used to run three reads before posting: does the group exist, am I
    // already a member, and do I already have a PENDING request. All three were
    // re-derived by the write anyway (the DS is the only copy that decides
    // anything), so what they bought was a specific error message rather than an
    // opaque 403. They also carried a TOCTOU: a request created between the
    // third check and the write was silently reset to pending.
    //
    // The write now NAMES its outcome, so every message survives, the reads are
    // gone, and the duplicate case is decided by a guarded upsert whose row
    // count cannot disagree with itself. `preflight_parity` in
    // `pollis-delivery/tests/join_requests.rs` covers the decision matrix on the
    // side that now owns all of it.
    //
    // Upsert semantics are unchanged: a new insert, or a prior
    // rejected/approved row reset to pending. `reviewed_by` / `reviewed_at` are
    // preserved so the history of who reviewed the previous request survives.
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

    // Notify the group's existing admins so the pending-request list (menu
    // badge + bottom bar) refreshes live instead of waiting for a manual
    // refetch. The requester isn't a member and isn't connected to the group's
    // room, so this rides the server-side publish path. Best-effort — a flaky
    // LiveKit blip must not fail the request.
    if let Err(e) = crate::commands::livekit::publish_join_requests_changed_to_room(
        state,
        &group_id,
    ).await {
        eprintln!("[realtime] request_group_access: notify group {group_id}: {e}");
    }

    Ok(())
}

/// Get all pending join requests for a group. Admins only.
///
/// #875: this used to answer `Ok(vec![])` for a non-member and for a non-admin
/// member alike — indistinguishable from "no one has asked to join". Both now
/// report the real condition. The UI already gates the affordance on
/// `current_user_role === 'admin'` (`Group.tsx`), so nothing that used to
/// succeed starts failing; what changes is that a caller who should not be
/// asking now learns so.
pub async fn get_group_join_requests(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<JoinRequest>> {
    // Admin gate and the pending list in ONE request: the DS returns
    // `join_requests` only to an admin, so the guard and the payload are the
    // same decision instead of two that have to agree.
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
    // Answered from the group's pending list, which the DS serves only to an
    // admin — so an admin sees their own request and a non-member sees `None`.
    //
    // That is a deliberate narrowing (#987). A non-member's own pending request
    // is the one row they have a claim to, but there is no endpoint shape that
    // serves it without also answering "has user X asked to join group Y" for
    // arbitrary X and Y. The join flow shows the requester their own state from
    // the outcome of `request_group_access` instead of asking the server to
    // remember on their behalf.
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

    // DS seam: route the member-add + request-approve through the Delivery
    // Service (one transactional, admin-gated write) — and take the group and
    // requester it RESOLVED from the response (#987). Reading the request row
    // first was a round trip AND a race: the row could change between the read
    // and the write, leaving the follow-up reconcile pointed at a different
    // group than the one actually joined.
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
        eprintln!("[mls] approve_join_request: reconcile for group {group_id}: {e}");
    }

    // Notify requester their join request was approved so they see the group immediately.
    // `kind: "approval"` keeps this silent on the requester's device — they
    // initiated the request, so an unsolicited ping isn't warranted.
    if let Err(e) = crate::commands::livekit::publish_to_user_inbox(
        state,
        &requester_id,
        serde_json::json!({"type": "membership_changed", "group_id": group_id, "kind": "approval"}),
    ).await {
        eprintln!("[inbox] approve_join_request: notify {requester_id} failed: {e}");
    }

    // Notify existing group members so they refetch the member list.
    if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
        &state.livekit,
        &group_id,
    ).await {
        eprintln!("[realtime] approve_join_request: notify group {group_id}: {e}");
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
    // DS seam: route the status update through the Delivery Service (admin
    // re-derived server-side). No preflight read since #987 — the write resolves
    // the request under the same admin gate the preflight used, and a request
    // that is not pending, or not in a group this caller administers, comes back
    // as the same 403 the preflight produced.
    let body = pollis_api::groups::RejectJoinRequestBody {
        request_id,
        approver_id: Some(approver_id),
        reviewed_at: now,
    };
    let pollis_api::groups::ReviewedJoinRequest::Ok { .. } =
        crate::commands::mls::ds_post_json(state, &body).await?;

    Ok(())
}
