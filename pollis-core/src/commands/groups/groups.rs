use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::{Channel, Group, GroupPreview, GroupWithChannels};
use super::authz;

/// The sidebar tree: every group the user belongs to, each carrying its
/// channels and the user's role in it.
/// Derived from `group_member`, not the stale `user_groups` table (#540).
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

    // Cache channel names for on-device search (#850).
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
    // Opt-in to auto-creating a #General text channel.
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
                crate::diag!("[mls] create_group: reconcile failed: {e}");
            }
        }
        Err(e) => crate::diag!("[mls] create_group: mls group init failed (non-fatal): {e}"),
    }

    // Wake the creator's other devices via their inbox; they are not in the group room.
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

    // Use the updated row from the response; a re-read could see a later update (#987).
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

    // Announce the change so other members' sidebars refresh (#874).
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

    // CASCADE deletes group_member and channels entries.
    let body = pollis_api::groups::DeleteGroupBody {
        group_id,
        requester_id: Some(requester_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

/// Find a group whose name derives to the given slug.
/// Deliberately not membership-gated (join-flow discovery); returns only [`GroupPreview`] (#917).
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
