use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::{CreatedInviteLink, GroupInvite, InviteLinkSummary, PendingInvite, RedeemedInvite};
use super::authz;

/// Invite a user (by username) to a group. Inviter must be a current member.
pub async fn send_group_invite(
    group_id: String,
    inviter_id: String,
    invitee_identifier: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // One request (#987): the DS resolves the identifier and checks blocks,
    // membership and pending invites inside the write, and names the outcome.
    let id = Ulid::new().to_string();
    let body = pollis_api::groups::CreateInviteBody {
        id,
        group_id: group_id.clone(),
        inviter_id: Some(inviter_id.clone()),
        invitee_identifier: invitee_identifier.clone(),
    };
    let (invitee_id, inviter_username, group_name) =
        match crate::commands::mls::ds_post_json(state, &body).await? {
            pollis_api::groups::InviteCreated::Ok {
                invitee_id,
                inviter_username,
                group_name,
            } => (invitee_id, inviter_username, group_name),
            pollis_api::groups::InviteCreated::NoSuchUser => {
                return Err(Error::Other(anyhow::anyhow!(
                    "user '{}' not found",
                    invitee_identifier
                )));
            }
            pollis_api::groups::InviteCreated::SelfInvite => {
                return Err(Error::Other(anyhow::anyhow!(
                    "cannot invite yourself to a group"
                )));
            }
            // The generic block error, so neither side can infer why.
            pollis_api::groups::InviteCreated::Blocked => {
                return Err(Error::Other(anyhow::anyhow!(
                    crate::commands::dm::BLOCK_ERR
                )));
            }
            pollis_api::groups::InviteCreated::AlreadyMember => {
                return Err(Error::Other(anyhow::anyhow!(
                    "that user is already a member of this group"
                )));
            }
            pollis_api::groups::InviteCreated::AlreadyInvited => {
                return Err(Error::Other(anyhow::anyhow!(
                    "a pending invite already exists for this user"
                )));
            }
        };

    // Ingest-before-advance (#440): the interleaved catch-up must run before
    // reconcile commits the add, or an unfetched current-epoch message is
    // stranded. It runs here, not inside reconcile, because reconcile holds the
    // per-conversation MLS lock the catch-up takes.
    if let Err(e) = crate::commands::messages::catch_up_mls_group_interleaved(
        state, &group_id, &inviter_id,
    ).await {
        crate::diag!("[mls] send_group_invite: catch_up_mls_group for {group_id}: {e}");
    }

    // Add the invitee's devices to the MLS tree now, so their Welcome is ready
    // before they accept, with no need for both sides online at once.
    if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
        state, &group_id, &inviter_id,
    ).await {
        crate::diag!("[mls] send_group_invite: reconcile for group {group_id}: {e}");
    }

    // `kind: "invite"` lets the frontend raise a notification; a bare
    // membership_changed would only invalidate queries.
    if let Err(e) = crate::commands::livekit::publish_to_user_inbox(
        state,
        &invitee_id,
        crate::commands::livekit_signalling::group_invite_inbox_payload(
            &group_id,
            inviter_username.as_deref(),
            group_name.as_deref(),
        ),
    ).await {
        crate::diag!("[inbox] send_group_invite: notify {invitee_id} failed: {e}");
    }

    Ok(())
}

/// Get all pending invites for the given user.
pub async fn get_pending_invites(
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<PendingInvite>> {
    Ok(crate::commands::ds_reads::bootstrap(state, &user_id)
        .await?
        .pending_invites
        .into_iter()
        .map(|i| PendingInvite {
            id: i.id,
            group_id: i.group_id,
            group_name: i.group_name,
            inviter_id: i.inviter_id,
            inviter_username: i.inviter_username,
            created_at: i.created_at,
        })
        .collect())
}

/// Accept a pending invite. Adds the user to the group.
pub async fn accept_group_invite(
    invite_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // One transactional write, authorized as the invitee; the group comes from
    // the response (#987), so a revoked invite cannot be announced as joined.
    let body = pollis_api::groups::AcceptInviteBody {
        invite_id,
        user_id: Some(user_id),
    };
    let pollis_api::groups::AcceptedInvite::Ok { group_id } =
        crate::commands::mls::ds_post_json(state, &body).await?;

    // The accepting user is not in the group room yet: use the server-side
    // publish.
    if let Err(e) = crate::commands::livekit::publish_to_room_server(
        state,
        &group_id,
        serde_json::json!({"type": "membership_changed", "group_id": group_id}),
    ).await {
        crate::diag!("[realtime] accept_group_invite: notify group {group_id}: {e}");
    }

    Ok(())
}

/// Decline a pending invite.
pub async fn decline_group_invite(
    invite_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // The DS scopes the delete to the invitee and answers 403 otherwise.
    let body = pollis_api::groups::DeclineInviteBody {
        invite_id,
        user_id: Some(user_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

/// A group's pending invites, for the member page's "Invited" section (#1506).
/// Members only.
pub async fn list_group_invites(
    group_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<GroupInvite>> {
    let resp = crate::commands::ds_reads::group_with_invites(state, &group_id, &user_id).await?;
    if !resp.authorized {
        return Err(Error::Other(anyhow::anyhow!(authz::NOT_A_MEMBER)));
    }
    Ok(resp
        .invites
        .into_iter()
        .map(|i| GroupInvite {
            id: i.id,
            invitee_id: i.invitee_id,
            invitee_username: i.invitee_username,
            inviter_id: i.inviter_id,
            inviter_username: i.inviter_username,
            created_at: i.created_at,
        })
        .collect())
}

/// Withdraw a pending invite (#1506). Allowed to the member who sent it and the
/// group's admins (re-derived by the DS).
///
/// The invitee's devices were grafted into the MLS tree at invite time, so this
/// device then commits them out; otherwise they would keep receiving every
/// message sealed for the group.
pub async fn revoke_group_invite(
    invite_id: String,
    group_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let body = pollis_api::groups::RevokeInviteBody {
        invite_id,
        actor_id: Some(user_id.clone()),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Ingest-before-advance (#440): see `remove_member_from_group`.
    if let Err(e) = crate::commands::messages::catch_up_mls_group_interleaved(
        state, &group_id, &user_id,
    ).await {
        crate::diag!("[mls] revoke_group_invite: catch_up_mls_group for {group_id}: {e}");
    }

    if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
        state, &group_id, &user_id,
    ).await {
        crate::diag!("[mls] revoke_group_invite: reconcile for group {group_id}: {e}");
    }

    // A bare wake-up naming no one (§5.3). The invitee's nudge comes from the
    // DS, the only party still allowed to reach their inbox.
    if let Err(e) = crate::commands::livekit::publish_to_room_server(
        state,
        &group_id,
        crate::commands::livekit_signalling::membership_changed_payload(&group_id),
    ).await {
        crate::diag!("[realtime] revoke_group_invite: notify group {group_id}: {e}");
    }

    Ok(())
}

/// Create a shareable invite link for a group.
///
/// The token is minted here; only `sha256(secret)` goes to the DS, and the
/// returned [`CreatedInviteLink`] is the only chance to read it. Expiry, use
/// count and revocation are the whole control surface, as in Slack and
/// Discord.
pub async fn create_group_invite_link(
    group_id: String,
    creator_id: String,
    expires_in_hours: Option<i64>,
    max_uses: Option<i64>,
    state: &Arc<AppState>,
) -> Result<CreatedInviteLink> {
    // UX only: the DS re-derives admin and is the authority.
    authz::require_admin(state, &group_id, &creator_id, "create invite links").await?;

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

    // Only the selector and the hash cross the wire.
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
/// Non-members and non-admins get the real refusal, never an empty list
/// (#875). Returns no tokens; see [`InviteLinkSummary`].
pub async fn list_group_invite_links(
    group_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<InviteLinkSummary>> {
    // The DS returns `invite_links` only to an admin. `is_live` is computed by
    // SQLite with the redeem path's `datetime()` normalisation, so the badge
    // cannot disagree with redemption.
    let resp = crate::commands::ds_reads::group(state, &group_id, &user_id, false).await?;
    if !resp.authorized {
        return Err(Error::Other(anyhow::anyhow!(authz::NOT_A_MEMBER)));
    }
    if resp.role.as_deref() != Some("admin") {
        return Err(Error::Other(anyhow::anyhow!(
            "only group admins can view invite links"
        )));
    }

    Ok(resp
        .invite_links
        .into_iter()
        .map(|l| InviteLinkSummary {
            id: l.id,
            created_at: l.created_at,
            creator_username: l.creator_username,
            expires_at: l.expires_at,
            max_uses: l.max_uses,
            uses: l.uses,
            revoked_at: l.revoked_at,
            is_live: l.is_live,
        })
        .collect())
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
/// The DS collapses wrong, expired, revoked and exhausted into one response;
/// distinct client messages would hand that distinction back.
pub const INVITE_LINK_ERR: &str = "this invite link is not valid";

/// Redeem an invite link and join the group.
///
/// A link redeemer has nobody to graft their devices into the MLS tree, so it
/// joins via an external commit against the stored `GroupInfo`, as a newly
/// enrolled device does. Not a bypass: the commit goes through the log, and the
/// cross-signing cert check still applies. Then the existing members are told.
pub async fn redeem_group_invite_link(
    token: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<RedeemedInvite> {
    // Accept a bare code, a URL, or the in-app scheme. An unparseable token
    // still goes to the DS: failing locally would distinguish "malformed" from
    // "wrong" and skip the rate-limit accounting.
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
        // 429 is the one distinguishable failure: it describes the caller.
        if status.as_u16() == 429 {
            return Err(Error::Other(anyhow::anyhow!(
                "too many attempts — wait a few minutes and try again"
            )));
        }
        return Err(Error::Other(anyhow::anyhow!(INVITE_LINK_ERR)));
    }

    let pollis_api::groups::RedeemInviteLinkResponse::Ok {
        group_id,
        group_name,
    } =
        crate::commands::mls::decode_response::<pollis_api::groups::RedeemInviteLinkBody>(resp)
            .await?;

    // Skipped when this device already holds the group (a re-redemption, or a
    // device grafted normally).
    let already_joined = {
        let guard = state.local_db.lock().await;
        guard.as_ref().is_some_and(|db| {
            crate::commands::mls::has_local_group(db.conn(), &group_id)
        })
    };
    if !already_joined {
        if let Err(e) = crate::commands::mls::external_join_group(state, &group_id, &user_id).await {
            // Non-fatal: membership is durable server-side, and a failed
            // external join self-heals on the next pass.
            crate::diag!("[mls] redeem_group_invite_link: external_join {group_id}: {e}");
        }
    }

    // The redeemer may not be in the group room yet: server-side publish.
    if let Err(e) = crate::commands::livekit::publish_to_room_server(
        state,
        &group_id,
        serde_json::json!({"type": "membership_changed", "group_id": group_id}),
    )
    .await
    {
        crate::diag!("[realtime] redeem_group_invite_link: notify group {group_id}: {e}");
    }

    Ok(RedeemedInvite {
        group_id,
        group_name,
    })
}

