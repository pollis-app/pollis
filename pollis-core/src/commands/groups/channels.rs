use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::{Channel, ChannelCategory};
use super::authz;

/// Every channel in a group.
/// Members-only (#917): channel names are content-adjacent metadata.
pub async fn list_group_channels(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<Channel>> {
    let resp = crate::commands::ds_reads::group(state, &group_id, &requester_id, false).await?;
    if !resp.authorized {
        return Err(Error::Other(anyhow::anyhow!(authz::NOT_A_MEMBER)));
    }
    Ok(resp
        .channels
        .into_iter()
        .map(super::groups::channel_from_wire)
        .collect())
}

/// Create a channel at the bottom of its block (#2031): of `category_id` when
/// given (admins only), else of the uncategorised block.
#[allow(clippy::too_many_arguments)]
pub async fn create_channel(
    group_id: String,
    name: String,
    description: Option<String>,
    // 'text' (default) or 'voice' — stored in the channel_type column.
    channel_type: Option<String>,
    _creator_id: String,
    category_id: Option<String>,
    state: &Arc<AppState>,
) -> Result<Channel> {
    let id = Ulid::new().to_string();
    let channel_type = channel_type.unwrap_or_else(|| "text".to_string());

    let body = pollis_api::groups::CreateChannelBody {
        id: id.clone(),
        group_id: group_id.clone(),
        name: name.clone(),
        description: description.clone(),
        channel_type: channel_type.clone(),
        creator_id: Some(_creator_id),
        category_id: category_id.clone(),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Tell other members a channel appeared (#874).
    notify_group(state, &group_id, "create_channel").await;

    Ok(Channel { id, group_id, name, description, channel_type, category_id })
}

/// Tell the group's other members (and this account's other devices) that its
/// channel list changed, through the group-change event (#874). Best effort.
async fn notify_group(state: &Arc<AppState>, group_id: &str, what: &str) {
    if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(state, group_id).await {
        crate::diag!("[realtime] {what}: notify group {group_id}: {e}");
    }
}

/// Create a category at the bottom of the group's categories (#2031).
/// Admin-only (the DS re-derives the role); the name follows the group-name rule.
pub async fn create_channel_category(
    group_id: String,
    name: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<ChannelCategory> {
    let id = Ulid::new().to_string();
    let name = name.trim().to_string();
    let body = pollis_api::groups::CreateChannelCategoryBody {
        id: id.clone(),
        group_id: group_id.clone(),
        name: name.clone(),
        requester_id: Some(requester_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;
    notify_group(state, &group_id, "create_channel_category").await;
    Ok(ChannelCategory { id, group_id, name })
}

/// Rename a category (#2031). Admin-only.
pub async fn rename_channel_category(
    group_id: String,
    category_id: String,
    name: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let body = pollis_api::groups::RenameChannelCategoryBody {
        category_id,
        group_id: group_id.clone(),
        name: name.trim().to_string(),
        requester_id: Some(requester_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;
    notify_group(state, &group_id, "rename_channel_category").await;
    Ok(())
}

/// Delete a category (#2031). Its channels move to the end of the
/// uncategorised block; none is deleted. Admin-only.
pub async fn delete_channel_category(
    group_id: String,
    category_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let body = pollis_api::groups::DeleteChannelCategoryBody {
        category_id,
        group_id: group_id.clone(),
        requester_id: Some(requester_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;
    notify_group(state, &group_id, "delete_channel_category").await;
    Ok(())
}

/// Put a category at `position` (0 = top) among the group's categories
/// (#2031). Admin-only.
pub async fn move_channel_category(
    group_id: String,
    category_id: String,
    position: u32,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let body = pollis_api::groups::MoveChannelCategoryBody {
        category_id,
        group_id: group_id.clone(),
        position,
        requester_id: Some(requester_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;
    notify_group(state, &group_id, "move_channel_category").await;
    Ok(())
}

/// Move a channel to `position` (0 = top) in `category_id`'s block, or the
/// uncategorised block when `None` (#2031). Admin-only; the category must be
/// one of the channel's own group.
pub async fn move_channel(
    group_id: String,
    channel_id: String,
    category_id: Option<String>,
    position: u32,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let body = pollis_api::groups::MoveChannelBody {
        channel_id,
        group_id: group_id.clone(),
        category_id,
        position,
        requester_id: Some(requester_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;
    notify_group(state, &group_id, "move_channel").await;
    Ok(())
}

pub async fn update_channel(
    channel_id: String,
    requester_id: String,
    name: Option<String>,
    description: Option<String>,
    state: &Arc<AppState>,
) -> Result<Channel> {
    // The DS resolves the channel to its group, so this is one request.
    let role = crate::commands::ds_reads::group_role(state, &channel_id, &requester_id).await?;
    if !role.exists {
        return Err(Error::Other(anyhow::anyhow!("channel not found")));
    }
    let group_id = role.group_id.clone();
    if role.role.as_deref() != Some("admin") {
        return Err(Error::Other(anyhow::anyhow!(
            "only group admins can update channels"
        )));
    }

    // Take the updated row from the DS response (#987).
    let body = pollis_api::groups::UpdateChannelBody {
        channel_id: channel_id.clone(),
        requester_id: Some(requester_id),
        name,
        description,
    };
    let pollis_api::groups::UpdatedChannel::Ok {
        id,
        group_id: updated_group_id,
        name,
        description,
        channel_type,
    } = crate::commands::mls::ds_post_json(state, &body).await?;
    let group_id = if group_id.is_empty() {
        updated_group_id.clone()
    } else {
        group_id
    };

    // Announce the rename (#874).
    if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
        state,
        &group_id,
    ).await {
        crate::diag!("[realtime] update_channel: notify group {group_id}: {e}");
    }

    Ok(Channel {
        id,
        group_id: updated_group_id,
        name,
        description,
        channel_type,
        // The update response does not carry it; callers re-read the list.
        category_id: None,
    })
}

pub async fn delete_channel(
    channel_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let role = crate::commands::ds_reads::group_role(state, &channel_id, &requester_id).await?;
    if !role.exists {
        return Err(Error::Other(anyhow::anyhow!("channel not found")));
    }
    if role.role.as_deref() != Some("admin") {
        return Err(Error::Other(anyhow::anyhow!(
            "only group admins can delete channels"
        )));
    }
    let group_id = role.group_id.clone();

    let body = pollis_api::groups::DeleteChannelBody {
        channel_id,
        requester_id: Some(requester_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
        state,
        &group_id,
    ).await {
        crate::diag!("[realtime] delete_channel: notify group {group_id}: {e}");
    }

    Ok(())
}

/// Mute or unmute a group channel for this account (#1431).
/// Stored on the DS, whose push fan-out skips muted members unless `@username`-mentioned.
pub async fn set_channel_muted(
    user_id: String,
    channel_id: String,
    muted: bool,
    state: &Arc<AppState>,
) -> Result<()> {
    let body = pollis_api::profile::SetChannelMuteBody {
        user_id,
        channel_id,
        muted,
    };
    crate::commands::mls::ds_post_ok(state, &body).await
}

/// The ids of every channel this account muted and is still a member of.
pub async fn list_muted_channels(user_id: String, state: &Arc<AppState>) -> Result<Vec<String>> {
    Ok(crate::commands::ds_reads::channel_mutes(state, &user_id)
        .await?
        .channel_ids)
}
