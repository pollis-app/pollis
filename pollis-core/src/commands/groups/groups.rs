use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::{Channel, Group, GroupPreview, GroupWithChannels};
use super::authz;

/// The sidebar tree: every group the user belongs to, each carrying its
/// channels and the user's role in it.
///
/// Derived from `group_member` server-side, never from `user_groups` (a stale
/// backfill since #540).
pub async fn list_user_groups_with_channels(
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<GroupWithChannels>> {
    let groups: Vec<GroupWithChannels> = crate::commands::ds_reads::bootstrap(state, &user_id)
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
        .collect();

    // Cache channel names locally so on-device search can name and route hits
    // (#850); this is the moment they are knowable.
    cache_channels(state, &groups).await;

    Ok(groups)
}

/// Mirror the group tree into `conversation_cache`.
async fn cache_channels(state: &Arc<AppState>, groups: &[GroupWithChannels]) {
    let entries: Vec<crate::commands::messages::CachedConversation> = groups
        .iter()
        .flat_map(|g| {
            g.channels
                .iter()
                .map(|c| crate::commands::messages::CachedConversation {
                    id: c.id.clone(),
                    kind: "channel",
                    name: Some(c.name.clone()),
                    group_id: Some(g.id.clone()),
                    group_name: Some(g.name.clone()),
                })
        })
        .collect();
    crate::commands::messages::cache_conversations(state, &entries).await;
}

/// Every group the user belongs to, without their channels.
///
/// Same bootstrap read as [`list_user_groups_with_channels`], projected down.
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

/// Wire → domain for one channel row, in one place.
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
    // Opt-in to auto-creating a #General text channel (defaults to false).
    create_default_text_channel: Option<bool>,
    // Opt-in to auto-creating a Voice Chat voice channel.
    create_default_voice_channel: Option<bool>,
    state: &Arc<AppState>,
) -> Result<Group> {
    let id = Ulid::new().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    let text_channel_id =
        create_default_text_channel.unwrap_or(false).then(|| Ulid::new().to_string());
    let voice_channel_id =
        create_default_voice_channel.unwrap_or(false).then(|| Ulid::new().to_string());

    // One transactional, server-authorized write.
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

    match crate::commands::mls::init_mls_group(state, &id, &owner_id).await {
        Ok(()) => {
            if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
                state, &id, &owner_id,
            ).await {
                crate::diag!("[mls] create_group: reconcile failed: {e}");
            }
        }
        Err(e) => crate::diag!("[mls] create_group: mls group init failed (non-fatal): {e}"),
    }

    // Wake the creator's other devices through their inbox (they are not in
    // this group's room yet); a bare `membership_changed` makes them pull their
    // Welcomes, queued by the reconcile above. Best effort.
    if let Err(e) = crate::commands::livekit::publish_to_user_inbox(
        state,
        &owner_id,
        crate::commands::livekit_signalling::membership_changed_payload(&id),
    )
    .await
    {
        crate::diag!("[inbox] create_group: notify own devices of {id} failed: {e}");
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

    // The DS re-derives admin and returns the updated row (#987), so a later
    // concurrent update cannot be reported as this one's result.
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

    // Announce the change (#874), or other members only refresh on focus.
    if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
        state,
        &group_id,
    ).await {
        crate::diag!("[realtime] update_group: notify group {group_id}: {e}");
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

    // CASCADE deletes group_member and channels; admin re-checked by the DS.
    let body = pollis_api::groups::DeleteGroupBody {
        group_id,
        requester_id: Some(requester_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

/// Find a group whose name derives to the given slug, for the join flow.
///
/// Deliberately not membership-gated (#917): discovery of a group you are not
/// in is the feature, as with Discord vanity invites and Slack workspace URLs.
/// It returns only a [`GroupPreview`]; existence at a slug is the one thing it
/// discloses. The O(rows) scan runs on the DS under its tight `probe` rate tier.
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
