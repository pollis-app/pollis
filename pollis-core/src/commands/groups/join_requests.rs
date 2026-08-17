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
    let conn = state.remote_db.conn().await?;

    // PREFLIGHT, not enforcement (#875's pattern, tightened by #942).
    //
    // The Delivery Service re-derives both of these conditions in
    // `apply_create_join_request` and is the only copy that decides anything —
    // a client that skipped this entirely would still be refused. What the
    // preflight buys is a specific local error instead of an opaque 403, which
    // is why it stays.
    //
    // What it must NOT be is a second, independently-worded version of the same
    // rule. Both checks below run `pollis_schema::authz`'s statements, which is
    // the same text the DS executes; `preflight_parity` in
    // `pollis-delivery/tests/join_requests.rs` asserts the two sides reach the
    // same verdict over the whole decision matrix. If they ever disagree the
    // client is wrong by definition.
    if !authz::group_exists(&conn, &group_id).await? {
        return Err(Error::Other(anyhow::anyhow!("group not found")));
    }

    if authz::group_role(&conn, &group_id, &requester_id).await?.is_some() {
        return Err(Error::Other(anyhow::anyhow!("you are already a member of this group")));
    }

    // Block duplicate pending requests, but allow re-application after rejection.
    // If a pending request already exists, error. If a prior rejected/approved row
    // exists, the upsert below will reset it to pending — giving admins a clean
    // slate to review while preserving the row-per-pair constraint.
    let mut existing = conn.query(
        "SELECT status FROM group_join_request WHERE group_id = ?1 AND requester_id = ?2",
        libsql::params![group_id.clone(), requester_id.clone()],
    ).await?;
    if let Some(row) = existing.next().await? {
        let status: String = row.get(0)?;
        if status == "pending" {
            return Err(Error::Other(anyhow::anyhow!("you already have a pending request for this group")));
        }
    }

    let id = Ulid::new().to_string();
    // Upsert: new insert, or reset a prior rejected/approved row back to pending.
    // reviewed_by and reviewed_at are intentionally preserved so the history of
    // who reviewed the previous request is available for future UI use. DS seam:
    // route the upsert (authorized as the requester server-side) through the
    // Delivery Service.
    let body = pollis_api::groups::CreateJoinRequestBody {
        id,
        group_id: group_id.clone(),
        requester_id: Some(requester_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

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
    let conn = state.remote_db.conn().await?;

    authz::require_admin(&conn, &group_id, &requester_id, "view join requests").await?;

    let mut req_rows = conn.query(
        "SELECT jr.id, jr.group_id, jr.requester_id, u.username, jr.status, jr.created_at
         FROM group_join_request jr
         LEFT JOIN users u ON u.id = jr.requester_id
         WHERE jr.group_id = ?1 AND jr.status = 'pending'
         ORDER BY jr.created_at ASC",
        libsql::params![group_id],
    ).await?;

    let mut requests = Vec::new();
    while let Some(row) = req_rows.next().await? {
        requests.push(JoinRequest {
            id: row.get(0)?,
            group_id: row.get(1)?,
            requester_id: row.get(2)?,
            requester_username: row.get(3)?,
            status: row.get(4)?,
            created_at: row.get(5)?,
        });
    }

    Ok(requests)
}

/// Get the current user's own join request for a specific group, if one exists.
/// Returns None if no request has been made.
pub async fn get_my_join_request(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<Option<JoinRequest>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT id, group_id, requester_id, status, created_at
         FROM group_join_request
         WHERE group_id = ?1 AND requester_id = ?2",
        libsql::params![group_id, requester_id],
    ).await?;

    if let Some(row) = rows.next().await? {
        Ok(Some(JoinRequest {
            id: row.get(0)?,
            group_id: row.get(1)?,
            requester_id: row.get(2)?,
            requester_username: None,
            status: row.get(3)?,
            created_at: row.get(4)?,
        }))
    } else {
        Ok(None)
    }
}

/// Approve a join request. Approver must be a group member. Adds the requester to the group.
pub async fn approve_join_request(
    request_id: String,
    approver_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT group_id, requester_id FROM group_join_request WHERE id = ?1 AND status = 'pending'",
        libsql::params![request_id.clone()],
    ).await?;

    let (group_id, requester_id): (String, String) = if let Some(row) = rows.next().await? {
        (row.get(0)?, row.get(1)?)
    } else {
        return Err(Error::Other(anyhow::anyhow!("join request not found or already processed")));
    };

    authz::require_admin(&conn, &group_id, &approver_id, "approve join requests").await?;

    let now = chrono::Utc::now().to_rfc3339();

    // DS seam: route the member-add + request-approve through the Delivery
    // Service (one transactional, admin-gated write).
    let body = pollis_api::groups::ApproveJoinRequestBody {
        request_id,
        approver_id: Some(approver_id.clone()),
        reviewed_at: now,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

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
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT group_id FROM group_join_request WHERE id = ?1 AND status = 'pending'",
        libsql::params![request_id.clone()],
    ).await?;

    let group_id: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("join request not found or already processed")));
    };

    authz::require_admin(&conn, &group_id, &approver_id, "reject join requests").await?;

    let now = chrono::Utc::now().to_rfc3339();
    // DS seam: route the status update through the Delivery Service (admin
    // re-derived server-side).
    let body = pollis_api::groups::RejectJoinRequestBody {
        request_id,
        approver_id: Some(approver_id),
        reviewed_at: now,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}
