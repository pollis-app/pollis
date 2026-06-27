use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::JoinRequest;

/// Request access to a group. Creates a pending join request.
pub async fn request_group_access(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Verify group exists
    let mut rows = conn.query(
        "SELECT 1 FROM groups WHERE id = ?1",
        libsql::params![group_id.clone()],
    ).await?;
    if rows.next().await?.is_none() {
        return Err(Error::Other(anyhow::anyhow!("group not found")));
    }

    // Check not already a member
    let mut member_rows = conn.query(
        "SELECT 1 FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), requester_id.clone()],
    ).await?;
    if member_rows.next().await?.is_some() {
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
    let body = serde_json::json!({
        "id": id,
        "group_id": group_id,
        "requester_id": requester_id,
    });
    crate::commands::mls::ds_post_ok(state, "/v1/join-requests/create", &body).await?;

    // Notify the group's existing admins so the pending-request list (menu
    // badge + bottom bar) refreshes live instead of waiting for a manual
    // refetch. The requester isn't a member and isn't connected to the group's
    // room, so this rides the server-side publish path. Best-effort — a flaky
    // LiveKit blip must not fail the request.
    if let Err(e) = crate::commands::livekit::publish_join_requests_changed_to_room(
        &state.config,
        &group_id,
    ).await {
        eprintln!("[realtime] request_group_access: notify group {group_id}: {e}");
    }

    Ok(())
}

/// Get all pending join requests for a group. Requester must be a member.
pub async fn get_group_join_requests(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<JoinRequest>> {
    let conn = state.remote_db.conn().await?;

    // Only admins can view join requests; non-admins get an empty list
    let mut rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), requester_id],
    ).await?;
    let role: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Ok(Vec::new());
    };
    if role != "admin" {
        return Ok(Vec::new());
    }

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

    // Only admins can approve join requests
    let mut member_rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), approver_id.clone()],
    ).await?;
    let approver_role: String = if let Some(row) = member_rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("you are not a member of this group")));
    };
    if approver_role != "admin" {
        return Err(Error::Other(anyhow::anyhow!("only admins can approve join requests")));
    }

    let now = chrono::Utc::now().to_rfc3339();

    // DS seam: route the member-add + request-approve through the Delivery
    // Service (one transactional, admin-gated write).
    let body = serde_json::json!({
        "request_id": request_id,
        "approver_id": approver_id,
        "reviewed_at": now,
    });
    crate::commands::mls::ds_post_ok(state, "/v1/join-requests/approve", &body).await?;

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
        &state.config,
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

    // Only admins can reject join requests
    let mut member_rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), approver_id.clone()],
    ).await?;
    let approver_role: String = if let Some(row) = member_rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("you are not a member of this group")));
    };
    if approver_role != "admin" {
        return Err(Error::Other(anyhow::anyhow!("only admins can reject join requests")));
    }

    let now = chrono::Utc::now().to_rfc3339();
    // DS seam: route the status update through the Delivery Service (admin
    // re-derived server-side).
    let body = serde_json::json!({
        "request_id": request_id,
        "approver_id": approver_id,
        "reviewed_at": now,
    });
    crate::commands::mls::ds_post_ok(state, "/v1/join-requests/reject", &body).await?;

    Ok(())
}
