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
    // ONE request, where there used to be six (#987).
    //
    // The client used to run five reads before it could even build this body:
    // its own admin preflight, resolve the identifier to a user id, check the
    // block pair, check existing membership, and check for a pending invite.
    // The DS re-derived every one of them inside the write anyway, so the reads
    // bought nothing but a specific error message — and each one was a window
    // in which the answer could change before the insert landed.
    //
    // The body now carries the identifier AS TYPED and the response names the
    // outcome, so every message below is the one the user used to get.
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
            // The generic block error, so neither side can infer why the invite
            // failed — unchanged, and now decided somewhere the client cannot
            // observe the direction at all.
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

    // `inviter_username` / `group_name` came back with the invite (#396, #987):
    // the alert names the person and the group, and the DS had both strings in
    // hand from authorizing the write. Either may be `None` if its row does not
    // resolve, which degrades the alert to a generic name exactly as the
    // best-effort lookup did.

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
    // DS seam: route the member-add + invite-delete through the Delivery Service
    // (one transactional write, authorized as the invitee server-side) — and
    // take the group it admitted us to from the RESPONSE (#987). Reading the
    // invite first was a round trip and a TOCTOU: the row could be revoked
    // between the read and the write, leaving this announcing membership of a
    // group it never joined.
    let body = pollis_api::groups::AcceptInviteBody {
        invite_id,
        user_id: Some(user_id),
    };
    let pollis_api::groups::AcceptedInvite::Ok { group_id } =
        crate::commands::mls::ds_post_json(state, &body).await?;

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
    // No preflight read since #987: the DS scopes the delete to the invitee and
    // answers 403 for an invite that is not theirs or no longer exists — the
    // same refusal the read produced, without the round trip or the window
    // between them.
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
    // Client-side admin check for a clean error message. The DS re-derives this
    // server-side and is the authority — this is UX, not enforcement.
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
    // Admin gate and the link list in ONE request: the DS returns `invite_links`
    // only to an admin, so the guard and the payload are the same decision.
    // `is_live` is still computed by SQLite with the same `datetime()`
    // normalisation the redeem path uses, so the badge cannot disagree with what
    // redemption will actually do.
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

    // Typed through `pollis-api` (#922): the group id is the ONE thing a
    // successful redemption tells the caller, and reading it out of an untyped
    // `Value` meant a server-side rename degraded into "redeem response missing
    // group_id" rather than failing to build.
    let pollis_api::groups::RedeemInviteLinkResponse::Ok {
        group_id,
        group_name,
    } =
        crate::commands::mls::decode_response::<pollis_api::groups::RedeemInviteLinkBody>(resp)
            .await?;

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

    // `group_name` came back with the redemption (#987). The DS had already read
    // the row to authorize the join, so asking again was a round trip for a
    // string it was holding — and one that could answer differently if the group
    // was renamed in between.

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

