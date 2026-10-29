use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::Channel;
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

pub async fn create_channel(
    group_id: String,
    name: String,
    description: Option<String>,
    // 'text' (default) or 'voice' — stored in the channel_type column.
    channel_type: Option<String>,
    _creator_id: String,
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
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Tell other members a channel appeared (#874).
    if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
        state,
        &group_id,
    ).await {
        crate::diag!("[realtime] create_channel: notify group {group_id}: {e}");
    }

    Ok(Channel { id, group_id, name, description, channel_type })
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
