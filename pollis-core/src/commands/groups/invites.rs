use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::{CreatedInviteLink, InviteLinkSummary, PendingInvite, RedeemedInvite};
use super::authz;

/// Invite a user (by username) to a group. Inviter must be a current member.
pub async fn send_group_invite(
    group_id: String,
    inviter_id: String,
    invitee_identifier: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    authz::require_admin(&conn, &group_id, &inviter_id, "invite members").await?;

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
    let body = pollis_api::groups::CreateInviteBody {
        id,
        group_id: group_id.clone(),
        inviter_id: Some(inviter_id.clone()),
        invitee_id: invitee_id.clone(),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Ingest-before-advance (issue #440, committer strand): catch this device up
    // to head with the INTERLEAVED ingesting catch-up — decrypting every bound
    // conversation's messages at each epoch — BEFORE reconcile stages + merges
    // our add commit and advances our epoch. Reconcile advances the shared MLS
    // group; with `max_past_epochs = 0`, any current-epoch inbound message we
    // haven't fetched yet would have its keys discarded the instant we advance
    // past it. Hoisted ABOVE reconcile deliberately: `reconcile_group_mls_impl`
    // holds the per-conversation MLS lock (`mls_group_lock`) for its whole body,
    // and the interleaved catch-up re-acquires that SAME lock internally — running
    // it here, before reconcile takes the lock, ingests the current epoch without
    // deadlocking.
    if let Err(e) = crate::commands::messages::catch_up_mls_group_interleaved(
        state, &group_id, &inviter_id,
    ).await {
        eprintln!("[mls] send_group_invite: catch_up_mls_group for {group_id}: {e}");
    }

    // Reconcile adds the invitee's devices to the MLS tree now so their
    // Welcome is ready before they accept — no dependency on simultaneous
    // online presence between inviter and acceptor.
    if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
        state, &group_id, &inviter_id,
    ).await {
        eprintln!("[mls] send_group_invite: reconcile for group {group_id}: {e}");
    }

    // Inviter's username + group name for the invitee's status-bar alert, so it
    // can name who invited them and to where (issue #396). Public directory
    // metadata — the same fields `get_pending_invites` already returns. The DS
    // write above has already landed the invite, so this lookup is best-effort:
    // a failure degrades the alert to a generic name, it never fails the invite.
    let (inviter_username, group_name): (Option<String>, Option<String>) =
        match fetch_invite_alert_names(&conn, &group_id, &inviter_id).await {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("[inbox] send_group_invite: alert-name lookup failed (non-fatal): {e}");
                (None, None)
            }
        };

    // Notify invitee via their inbox so the pending invite appears immediately.
    // `kind: "invite"` lets the frontend raise a ping/OS notification — a
    // generic membership_changed (kind: null) would only invalidate queries.
    if let Err(e) = crate::commands::livekit::publish_to_user_inbox(
        state,
        &invitee_id,
        crate::commands::livekit_signalling::group_invite_inbox_payload(
            &group_id,
            inviter_username.as_deref(),
            group_name.as_deref(),
        ),
    ).await {
        eprintln!("[inbox] send_group_invite: notify {invitee_id} failed: {e}");
    }

    Ok(())
}

/// Look up the inviter's username and the group's name for a group-invite alert.
/// Public directory metadata only. Returns `(None, None)` for either field that
/// can't be resolved (missing user/group row).
async fn fetch_invite_alert_names(
    conn: &libsql::Connection,
    group_id: &str,
    inviter_id: &str,
) -> Result<(Option<String>, Option<String>)> {
    let mut rows = conn.query(
        "SELECT u.username, g.name FROM groups g LEFT JOIN users u ON u.id = ?2 WHERE g.id = ?1",
        libsql::params![group_id.to_string(), inviter_id.to_string()],
    ).await?;
    match rows.next().await? {
        Some(row) => Ok((row.get(0)?, row.get(1)?)),
        None => Ok((None, None)),
    }
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
    let body = pollis_api::groups::AcceptInviteBody {
        invite_id,
        user_id: Some(user_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Notify existing group members so they see the new member.
    // The accepting user isn't connected to the group room yet, so use
    // the server-side publish to reach existing members.
    if let Err(e) = crate::commands::livekit::publish_to_room_server(
        state,
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
    let body = pollis_api::groups::DeclineInviteBody {
        invite_id,
        user_id: Some(user_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

// ── #847 shareable invite links ──────────────────────────────────────────────

/// Create a shareable invite link for a group.
///
/// The token is minted HERE, on the client, and only `sha256(secret)` is posted
/// to the Delivery Service — so the server never sees a working token at
/// creation time. The returned [`CreatedInviteLink`] is the one and only chance
/// to read it; nothing persists it.
///
/// `expires_in_hours` and `max_uses` are both `None` for an unbounded link.
/// Those two plus revocation are the entire control surface, deliberately: they
/// are the three Slack and Discord actually ship, and every additional knob is a
/// new way to misconfigure a security boundary.
pub async fn create_group_invite_link(
    group_id: String,
    creator_id: String,
    expires_in_hours: Option<i64>,
    max_uses: Option<i64>,
    state: &Arc<AppState>,
) -> Result<CreatedInviteLink> {
    let conn = state.remote_db.conn().await?;

    // Client-side admin check for a clean error message. The DS re-derives this
    // server-side and is the authority — this is UX, not enforcement.
    authz::require_admin(&conn, &group_id, &creator_id, "create invite links").await?;

    if max_uses.is_some_and(|m| m <= 0) {
        return Err(Error::Other(anyhow::anyhow!(
            "an invite link must allow at least one use"
        )));
    }
    if expires_in_hours.is_some_and(|h| h <= 0) {
        return Err(Error::Other(anyhow::anyhow!(
            "an expiry must be in the future"
        )));
    }

    let minted = super::invite_token::mint();
    let id = Ulid::new().to_string();
    let expires_at = expires_in_hours
        .map(|h| (chrono::Utc::now() + chrono::Duration::hours(h)).to_rfc3339());

    // DS seam: only the SELECTOR and the HASH cross the wire. The secret half
    // never leaves this function.
    let body = pollis_api::groups::CreateInviteLinkBody {
        id: id.clone(),
        group_id,
        created_by: Some(creator_id),
        selector: minted.selector,
        secret_hash: minted.secret_hash,
        expires_at: expires_at.clone(),
        max_uses,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(CreatedInviteLink {
        id,
        url: super::invite_token::invite_url(&minted.token),
        token: minted.token,
        expires_at,
        max_uses,
    })
}

/// List a group's invite links. Admins only.
///
/// #875: this used to answer `Ok(vec![])` for a non-member and for a non-admin
/// member alike, which is the same value as "this group has no invite links" —
/// so the caller could not tell a permission boundary from an empty page. Both
/// now report the real condition, exactly as every sibling command does.
///
/// Returns no tokens — see [`InviteLinkSummary`].
pub async fn list_group_invite_links(
    group_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<InviteLinkSummary>> {
    let conn = state.remote_db.conn().await?;

    authz::require_admin(&conn, &group_id, &user_id, "view invite links").await?;

    // `is_live` is computed by SQLite with the same `datetime()` normalisation
    // the redeem path uses, so the badge cannot disagree with what redemption
    // will actually do.
    let mut link_rows = conn
        .query(
            "SELECT l.id, l.created_at, u.username, l.expires_at, l.max_uses, l.uses, \
                    l.revoked_at, \
                    CASE WHEN l.revoked_at IS NULL \
                          AND (l.expires_at IS NULL OR datetime(l.expires_at) > datetime('now')) \
                          AND (l.max_uses IS NULL OR l.uses < l.max_uses) \
                         THEN 1 ELSE 0 END AS is_live \
             FROM group_invite_link l \
             LEFT JOIN users u ON u.id = l.created_by \
             WHERE l.group_id = ?1 \
             ORDER BY l.created_at DESC",
            libsql::params![group_id],
        )
        .await?;

    let mut links = Vec::new();
    while let Some(row) = link_rows.next().await? {
        links.push(InviteLinkSummary {
            id: row.get(0)?,
            created_at: row.get(1)?,
            creator_username: row.get(2)?,
            expires_at: row.get(3)?,
            max_uses: row.get(4)?,
            uses: row.get(5)?,
            revoked_at: row.get(6)?,
            is_live: row.get::<i64>(7)? == 1,
        });
    }

    Ok(links)
}

/// Revoke an invite link. Admins of that link's group only (re-derived by the DS).
pub async fn revoke_group_invite_link(
    link_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let body = pollis_api::groups::RevokeInviteLinkBody {
        link_id,
        actor_id: Some(user_id),
        revoked_at: chrono::Utc::now().to_rfc3339(),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;
    Ok(())
}

/// The single user-facing error for a failed redemption.
///
/// One string for every failure mode. The DS already collapses "wrong",
/// "expired", "revoked" and "exhausted" into one opaque response; rendering them
/// differently on the client would hand back the distinction the server just
/// spent effort hiding. Mirrors how `BLOCK_ERR` is shared across the three gates
/// in `send_group_invite`.
pub const INVITE_LINK_ERR: &str = "this invite link is not valid";

/// Redeem an invite link and join the group.
///
/// Two things happen after the DS admits us, and both matter:
///
/// 1. **Self-heal into the MLS tree.** Unlike `send_group_invite` — where the
///    inviter pre-grafts the invitee's devices before they ever accept — a link
///    redeemer has nobody to graft them: reconcile is always run by the actor
///    making the change, and no existing member is involved in a redemption. So
///    the redeemer joins the tree itself via an MLS **external commit** against
///    the server-stored `GroupInfo`, exactly as a newly enrolled device does in
///    `finalize_enrollment`. Without this the redeemer would be a `group_member`
///    who cannot decrypt a single message — a broken feature, not a partial one.
///
///    This is not a bypass. The external commit is posted to `mls_commit_log`
///    like any other, existing members merge it on their next pass, and the
///    inbound cross-signing cert check still applies — a device whose cert does
///    not chain to the user's `account_id_pub` is rejected by the other members.
///
/// 2. **Tell the existing members**, so their member list refreshes.
pub async fn redeem_group_invite_link(
    token: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<RedeemedInvite> {
    // Accept a bare code, a https URL, or the in-app scheme — people paste
    // whatever they were sent. A token that doesn't even parse still goes to the
    // DS: bailing early here would make "malformed" locally distinguishable from
    // "wrong", which is precisely the leak the server-side design avoids, and it
    // would skip the rate-limit accounting for a spraying client.
    let presented = super::invite_token::extract(&token)
        .map(|(selector, secret)| format!("{selector}.{secret}"))
        .unwrap_or_else(|| token.trim().to_string());

    let body = pollis_api::groups::RedeemInviteLinkBody {
        token: presented,
        user_id: Some(user_id.clone()),
        attempt_id: Ulid::new().to_string(),
        now: chrono::Utc::now().to_rfc3339(),
    };

    let resp = crate::commands::mls::ds_post(state, &body).await?;
    let status = resp.status();
    if !status.is_success() {
        // 429 is the one distinguishable failure, and only because it describes
        // the caller rather than the token.
        if status.as_u16() == 429 {
            return Err(Error::Other(anyhow::anyhow!(
                "too many attempts — wait a few minutes and try again"
            )));
        }
        return Err(Error::Other(anyhow::anyhow!(INVITE_LINK_ERR)));
    }

    let payload: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| Error::Other(anyhow::anyhow!("invalid redeem response: {e}")))?;
    let group_id = payload
        .get("group_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| Error::Other(anyhow::anyhow!("redeem response missing group_id")))?
        .to_string();

    // Self-heal into the MLS tree — see the note above. Skipped when this device
    // already holds the group (an idempotent re-redemption, or a second device
    // that was grafted normally).
    let already_joined = {
        let guard = state.local_db.lock().await;
        guard.as_ref().is_some_and(|db| {
            crate::commands::mls::has_local_group(db.conn(), &group_id)
        })
    };
    if !already_joined {
        if let Err(e) = crate::commands::mls::external_join_group(state, &group_id, &user_id).await {
            // Non-fatal: membership is already real and durable server-side. A
            // failed external join self-heals on the next pass (device
            // enrollment, message catch-up) rather than rolling back a join the
            // DS has committed.
            eprintln!("[mls] redeem_group_invite_link: external_join {group_id}: {e}");
        }
    }

    // Name the group for the caller's confirmation UI. Public directory
    // metadata; best-effort, and never fails a completed join.
    let group_name = match state.remote_db.conn().await {
        Ok(conn) => fetch_group_name(&conn, &group_id).await.unwrap_or(None),
        Err(e) => {
            eprintln!("[invites] redeem_group_invite_link: group-name lookup failed: {e}");
            None
        }
    };

    // Notify existing members so their roster refreshes. The redeemer may not be
    // connected to the group's room yet, so this rides the server-side publish,
    // matching `accept_group_invite`.
    if let Err(e) = crate::commands::livekit::publish_to_room_server(
        state,
        &group_id,
        serde_json::json!({"type": "membership_changed", "group_id": group_id}),
    )
    .await
    {
        eprintln!("[realtime] redeem_group_invite_link: notify group {group_id}: {e}");
    }

    Ok(RedeemedInvite {
        group_id,
        group_name,
    })
}

/// A group's display name, or `None` if the row is gone.
async fn fetch_group_name(
    conn: &libsql::Connection,
    group_id: &str,
) -> Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT name FROM groups WHERE id = ?1",
            libsql::params![group_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get(0)?,
        None => None,
    })
}
