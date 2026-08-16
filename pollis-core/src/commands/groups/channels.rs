use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::types::Channel;
use super::authz;

pub async fn list_group_channels(
    group_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<Channel>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT id, group_id, name, description, channel_type FROM channels WHERE group_id = ?1",
        libsql::params![group_id],
    ).await?;

    let mut channels = Vec::new();
    while let Some(row) = rows.next().await? {
        channels.push(Channel {
            id: row.get(0)?,
            group_id: row.get(1)?,
            name: row.get(2)?,
            description: row.get(3)?,
            channel_type: row.get::<Option<String>>(4)?.unwrap_or_else(|| "text".to_string()),
        });
    }

    Ok(channels)
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

    Ok(Channel { id, group_id, name, description, channel_type })
}

pub async fn update_channel(
    channel_id: String,
    requester_id: String,
    name: Option<String>,
    description: Option<String>,
    state: &Arc<AppState>,
) -> Result<Channel> {
    let conn = state.remote_db.conn().await?;

    // Get channel's group_id then check requester role
    let mut rows = conn.query(
        "SELECT group_id FROM channels WHERE id = ?1",
        libsql::params![channel_id.clone()],
    ).await?;

    let group_id: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("channel not found")));
    };

    authz::require_admin(&conn, &group_id, &requester_id, "update channels").await?;

    // DS seam: route the column updates through the Delivery Service (admin
    // re-derived server-side).
    let body = pollis_api::groups::UpdateChannelBody {
        channel_id: channel_id.clone(),
        requester_id: Some(requester_id),
        name,
        description,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    let mut rows = conn.query(
        "SELECT id, group_id, name, description, channel_type FROM channels WHERE id = ?1",
        libsql::params![channel_id],
    ).await?;

    if let Some(row) = rows.next().await? {
        Ok(Channel {
            id: row.get(0)?,
            group_id: row.get(1)?,
            name: row.get(2)?,
            description: row.get(3)?,
            channel_type: row.get::<Option<String>>(4)?.unwrap_or_else(|| "text".to_string()),
        })
    } else {
        Err(Error::Other(anyhow::anyhow!("channel not found after update")))
    }
}

pub async fn delete_channel(
    channel_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT group_id FROM channels WHERE id = ?1",
        libsql::params![channel_id.clone()],
    ).await?;

    let group_id: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("channel not found")));
    };

    authz::require_admin(&conn, &group_id, &requester_id, "delete channels").await?;

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
