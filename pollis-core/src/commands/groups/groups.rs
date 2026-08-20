use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::{Channel, Group, GroupPreview, GroupWithChannels};
use super::authz;

/// The sidebar tree: every group the user belongs to, each carrying its
/// channels and the user's role in it.
///
/// Derived from `group_member` server-side, NOT from `user_groups`: that table
/// was the #532 directory index, reverted by #540, and now holds a one-time
/// backfill minus deletions. Reading it would serve a confidently wrong sidebar.
pub async fn list_user_groups_with_channels(
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<GroupWithChannels>> {
    Ok(crate::commands::ds_reads::bootstrap(state, &user_id)
        .await?
        .groups
        .into_iter()
        .map(|g| GroupWithChannels {
            id: g.group.id,
            name: g.group.name,
            description: g.group.description,
            owner_id: g.group.owner_id,
            created_at: g.group.created_at,
            channels: g.channels.into_iter().map(channel_from_wire).collect(),
            current_user_role: g.role,
        })
        .collect())
}

/// Every group the user belongs to, without their channels.
///
/// Same source as [`list_user_groups_with_channels`] — one bootstrap read,
/// projected down. Kept as a separate command because mobile and the flows
/// suite drive it; it is not a second query.
pub async fn list_user_groups(user_id: String, state: &Arc<AppState>) -> Result<Vec<Group>> {
    Ok(crate::commands::ds_reads::bootstrap(state, &user_id)
        .await?
        .groups
        .into_iter()
        .map(|g| Group {
            id: g.group.id,
            name: g.group.name,
            description: g.group.description,
            owner_id: g.group.owner_id,
            created_at: g.group.created_at,
        })
        .collect())
}

/// Wire → domain for one channel row. One conversion, so a field added to the
/// wire type cannot be picked up by one call site and missed by another.
pub(super) fn channel_from_wire(c: pollis_api::directory::ChannelWire) -> Channel {
    Channel {
        id: c.id,
        group_id: c.group_id,
        name: c.name,
        description: c.description,
        channel_type: c.channel_type,
    }
}

pub async fn create_group(
    name: String,
    description: Option<String>,
    owner_id: String,
    // Opt-in to auto-creating a #General text channel. Defaults to false
    // — the user is expected to set these toggles in the create-group
    // form. Tauri elides the param entirely when omitted by the caller.
    create_default_text_channel: Option<bool>,
    // Opt-in to auto-creating a Voice Chat voice channel.
    create_default_voice_channel: Option<bool>,
    state: &Arc<AppState>,
) -> Result<Group> {
    let id = Ulid::new().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    // Default channel ids are generated up front so the same value goes into the
    // DB write (direct or DS) regardless of which path runs.
    let text_channel_id =
        create_default_text_channel.unwrap_or(false).then(|| Ulid::new().to_string());
    let voice_channel_id =
        create_default_voice_channel.unwrap_or(false).then(|| Ulid::new().to_string());

    // Route the group + admin-member + default-channel inserts through the
    // Delivery Service (one transactional, server-authorized write).
    let body = pollis_api::groups::CreateGroupBody {
        id: id.clone(),
        name: name.clone(),
        description: description.clone(),
        owner_id: Some(owner_id.clone()),
        default_text_channel_id: text_channel_id,
        default_voice_channel_id: voice_channel_id,
        created_at: now.clone(),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Create the per-group MLS group — all channels in this group share it.
    match crate::commands::mls::init_mls_group(state, &id, &owner_id).await {
        Ok(()) => {
            // Reconcile adds the creator's other devices (if any have KPs).
            if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
                state, &id, &owner_id,
            ).await {
                eprintln!("[mls] create_group: reconcile failed: {e}");
            }
        }
        Err(e) => eprintln!("[mls] create_group: mls group init failed (non-fatal): {e}"),
    }

    Ok(Group { id, name, description, owner_id, created_at: now })
}

pub async fn update_group(
    group_id: String,
    requester_id: String,
    name: Option<String>,
    description: Option<String>,
    icon_url: Option<String>,
    state: &Arc<AppState>,
) -> Result<Group> {
    authz::require_admin(state, &group_id, &requester_id, "update group settings").await?;

    // Route the column updates through the Delivery Service (which re-derives the
    // admin role server-side) and take the UPDATED ROW from its response (#987).
    // Re-reading it afterwards was a second round trip, and worse: the read could
    // observe a LATER concurrent update and report it as the result of this one.
    let body = pollis_api::groups::UpdateGroupBody {
        group_id: group_id.clone(),
        requester_id: Some(requester_id),
        name,
        description,
        icon_url,
    };
    let pollis_api::groups::UpdatedGroup::Ok {
        id,
        name,
        description,
        owner_id,
        created_at,
    } = crate::commands::mls::ds_post_json(state, &body).await?;

    // Announce the rename/icon change to the group (#874). Without this the
    // only thing that refreshed another member's sidebar was a window-focus
    // refetch — a poll wearing an event's clothes.
    if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
        &state.livekit,
        &group_id,
    ).await {
        eprintln!("[realtime] update_group: notify group {group_id}: {e}");
    }

    Ok(Group {
        id,
        name,
        description,
        owner_id,
        created_at,
    })
}

pub async fn delete_group(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    authz::require_admin(state, &group_id, &requester_id, "delete the group").await?;

    // CASCADE deletes group_member and channels entries. Route the delete through
    // the Delivery Service (admin re-checked server-side).
    let body = pollis_api::groups::DeleteGroupBody {
        group_id,
        requester_id: Some(requester_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

/// Find a group whose name derives to the given slug.
/// Returns an error if no match is found.
///
/// #917: deliberately NOT membership-gated, and deliberately narrowed instead.
///
/// This is the discovery step of the join flow — `SearchGroupPage` calls it,
/// then offers `request_group_access` on what it finds. A `require_member`
/// guard here would mean you could only find groups you were already in, which
/// is not a stricter version of the feature, it is the removal of the feature.
/// Discord's vanity-invite lookup and Slack's workspace-URL lookup make the
/// same trade: the existence of a group at a guessed name is public, its
/// contents are not.
///
/// So the fix is the projection. This used to hand back the whole [`Group`] row
/// — including `owner_id`, a named user — to anyone who guessed a slug. It now
/// returns [`GroupPreview`], which carries exactly what the join prompt
/// renders. See that type for the field-by-field reasoning.
///
/// What this endpoint still discloses, unavoidably, is whether a group with a
/// given slug exists; that is inherent to slug lookup and is the same bet the
/// products above take.
///
/// The scan is O(rows) per query — `derive_slug` is Rust, not SQL, so it cannot
/// be an indexed lookup. Since #987 it runs on the DS
/// (`POST /v1/directory/group-by-slug`), which is what finally makes the rate
/// limit meaningful: the old note here said a limit applied at this function was
/// one the caller could simply decline to apply, because the query ran on the
/// client under its own read token. There is no such token now, and the DS puts
/// slug lookup on its own tight `probe` tier — the same treatment invite-link
/// redemption gets, and for the same reason: guessable input.
pub async fn search_group_by_slug(
    slug: String,
    state: &Arc<AppState>,
) -> Result<GroupPreview> {
    let body = pollis_api::directory::GroupBySlugBody { slug: slug.clone() };
    let found = crate::commands::mls::ds_post_json(state, &body).await?.group;
    match found {
        Some(g) => Ok(GroupPreview {
            id: g.id,
            name: g.name,
            description: g.description,
        }),
        None => Err(Error::Other(anyhow::anyhow!(
            "No group found with slug '{}'",
            slug
        ))),
    }
}
