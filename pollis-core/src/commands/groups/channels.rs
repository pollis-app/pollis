use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::Channel;
use super::authz;

/// Every channel in a group.
///
/// #917: members-only. Channel names are content-adjacent metadata — `#general`
/// says nothing, `#q3-layoffs` says a great deal — and this took no caller, so
/// any group's channel list was readable by anyone who named the group id. The
/// sibling read `list_user_groups_with_channels` has always joined through
/// `group_member`; this is the same rule, applied to the same data, reached by
/// a different door.
pub async fn list_group_channels(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<Channel>> {
    // Membership and the channel list in ONE request: the DS answers
    // `authorized: false` with an empty list, which is the same refusal
    // `require_member` produced — expressed once, server-side, by the same
    // predicate that gates the writes.
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
    // Requires Turso migration: ALTER TABLE channels ADD COLUMN channel_type TEXT NOT NULL DEFAULT 'text';
    channel_type: Option<String>,
    _creator_id: String,
    state: &Arc<AppState>,
) -> Result<Channel> {
    let id = Ulid::new().to_string();
    let channel_type = channel_type.unwrap_or_else(|| "text".to_string());

    // DS seam: route the channel insert through the Delivery Service (which
    // re-derives group membership server-side).
    let body = pollis_api::groups::CreateChannelBody {
        id: id.clone(),
        group_id: group_id.clone(),
        name: name.clone(),
        description: description.clone(),
        channel_type: channel_type.clone(),
        creator_id: Some(_creator_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Tell the group's other members a channel appeared (#874). `delete_channel`
    // already did this; creation did not, so a new channel was invisible to
    // everyone else until they happened to refocus the window — which is what
    // the sidebar's blanket `refetchOnWindowFocus` was silently paying for.
    if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
        &state.livekit,
        &group_id,
    ).await {
        eprintln!("[realtime] create_channel: notify group {group_id}: {e}");
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
    // The channel id resolves to its owning group server-side, so the preflight
    // is one request rather than two (resolve, then role).
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

    // DS seam: route the column updates through the Delivery Service (admin
    // re-derived server-side) and take the UPDATED ROW from its response (#987).
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

    // Same reasoning as `create_channel` (#874): a rename other members can't
    // see until they refocus is a stale sidebar, not a saved round trip.
    if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
        &state.livekit,
        &group_id,
    ).await {
        eprintln!("[realtime] update_channel: notify group {group_id}: {e}");
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

    // DS seam: route the envelope/watermark/channel deletes through the Delivery
    // Service (one transactional, admin-gated write).
    let body = pollis_api::groups::DeleteChannelBody {
        channel_id,
        requester_id: Some(requester_id),
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
        &state.livekit,
        &group_id,
    ).await {
        eprintln!("[realtime] delete_channel: notify group {group_id}: {e}");
    }

    Ok(())
}
