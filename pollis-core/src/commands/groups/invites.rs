use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::PendingInvite;

/// Invite a user (by username) to a group. Inviter must be a current member.
pub async fn send_group_invite(
    group_id: String,
    inviter_id: String,
    invitee_identifier: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Only admins can send invites
    let mut rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), inviter_id.clone()],
    ).await?;
    let inviter_role: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("you are not a member of this group")));
    };
    if inviter_role != "admin" {
        return Err(Error::Other(anyhow::anyhow!("only admins can invite members")));
    }

    // Look up invitee by username or email
    let mut user_rows = conn.query(
        "SELECT id FROM users WHERE username = ?1 OR email = ?1",
        libsql::params![invitee_identifier.clone()],
    ).await?;
    let invitee_id: String = if let Some(row) = user_rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("user '{}' not found", invitee_identifier)));
    };

    if invitee_id == inviter_id {
        return Err(Error::Other(anyhow::anyhow!("cannot invite yourself to a group")));
    }

    // Silently reject when either party has blocked the other. Returns
    // the generic BLOCK_ERR so neither side can infer why the invite
    // failed.
    if crate::commands::blocks::is_blocked_either_way(&conn, &inviter_id, &invitee_id).await? {
        return Err(Error::Other(anyhow::anyhow!(
            crate::commands::dm::BLOCK_ERR
        )));
    }

    // Check if invitee is already a member
    let mut member_rows = conn.query(
        "SELECT 1 FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), invitee_id.clone()],
    ).await?;
    if member_rows.next().await?.is_some() {
        return Err(Error::Other(anyhow::anyhow!("that user is already a member of this group")));
    }

    // Check for existing pending invite
    let mut existing = conn.query(
        "SELECT 1 FROM group_invite WHERE group_id = ?1 AND invitee_id = ?2",
        libsql::params![group_id.clone(), invitee_id.clone()],
    ).await?;
    if existing.next().await?.is_some() {
        return Err(Error::Other(anyhow::anyhow!("a pending invite already exists for this user")));
    }

    let id = Ulid::new().to_string();
    // DS seam: route the invite insert through the Delivery Service (inviter's
    // admin role re-derived server-side).
    let body = serde_json::json!({
        "id": id,
        "group_id": group_id,
        "inviter_id": inviter_id,
        "invitee_id": invitee_id,
    });
    crate::commands::mls::ds_post_ok(state, "/v1/invites/create", &body).await?;

    // Reconcile adds the invitee's devices to the MLS tree now so their
    // Welcome is ready before they accept — no dependency on simultaneous
    // online presence between inviter and acceptor.
    if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
        state, &group_id, &inviter_id,
    ).await {
        eprintln!("[mls] send_group_invite: reconcile for group {group_id}: {e}");
    }

    // Notify invitee via their inbox so the pending invite appears immediately.
    // `kind: "invite"` lets the frontend raise a ping/OS notification — a
    // generic membership_changed (kind: null) would only invalidate queries.
    if let Err(e) = crate::commands::livekit::publish_to_user_inbox(
        &state.config,
        &invitee_id,
        serde_json::json!({"type": "membership_changed", "group_id": group_id, "kind": "invite"}),
    ).await {
        eprintln!("[inbox] send_group_invite: notify {invitee_id} failed: {e}");
    }

    Ok(())
}

/// Get all pending invites for the given user.
pub async fn get_pending_invites(
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<PendingInvite>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT gi.id, gi.group_id, g.name, gi.inviter_id, u.username, gi.created_at
         FROM group_invite gi
         JOIN groups g ON g.id = gi.group_id
         LEFT JOIN users u ON u.id = gi.inviter_id
         WHERE gi.invitee_id = ?1
         ORDER BY gi.created_at DESC",
        libsql::params![user_id],
    ).await?;

    let mut invites = Vec::new();
    while let Some(row) = rows.next().await? {
        invites.push(PendingInvite {
            id: row.get(0)?,
            group_id: row.get(1)?,
            group_name: row.get(2)?,
            inviter_id: row.get(3)?,
            inviter_username: row.get(4)?,
            created_at: row.get(5)?,
        });
    }

    Ok(invites)
}

/// Accept a pending invite. Adds the user to the group.
pub async fn accept_group_invite(
    invite_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT group_id FROM group_invite WHERE id = ?1 AND invitee_id = ?2",
        libsql::params![invite_id.clone(), user_id.clone()],
    ).await?;

    let group_id: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("invite not found or already processed")));
    };

    // DS seam: route the member-add + invite-delete through the Delivery Service
    // (one transactional write, authorized as the invitee server-side).
    let body = serde_json::json!({
        "invite_id": invite_id,
        "user_id": user_id,
    });
    crate::commands::mls::ds_post_ok(state, "/v1/invites/accept", &body).await?;

    // Notify existing group members so they see the new member.
    // The accepting user isn't connected to the group room yet, so use
    // the server-side publish to reach existing members.
    if let Err(e) = crate::commands::livekit::publish_to_room_server(
        &state.config,
        &group_id,
        serde_json::json!({"type": "membership_changed", "group_id": group_id}),
    ).await {
        eprintln!("[realtime] accept_group_invite: notify group {group_id}: {e}");
    }

    Ok(())
}

/// Decline a pending invite.
pub async fn decline_group_invite(
    invite_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT 1 FROM group_invite WHERE id = ?1 AND invitee_id = ?2",
        libsql::params![invite_id.clone(), user_id.clone()],
    ).await?;

    if rows.next().await?.is_none() {
        return Err(Error::Other(anyhow::anyhow!("invite not found or already processed")));
    }

    // Delete the invite row — declined invites don't need to be retained. DS
    // seam: route the delete (scoped to the invitee server-side) through the
    // Delivery Service.
    let body = serde_json::json!({
        "invite_id": invite_id,
        "user_id": user_id,
    });
    crate::commands::mls::ds_post_ok(state, "/v1/invites/decline", &body).await?;

    Ok(())
}
