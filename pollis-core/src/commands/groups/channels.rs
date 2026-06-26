use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::db_err;
use super::types::Channel;

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
    // re-derives group membership server-side) when configured; else direct.
    match state.config.pollis_delivery_url.as_deref() {
        Some(_) => {
            let body = serde_json::json!({
                "id": id,
                "group_id": group_id,
                "name": name,
                "description": description,
                "channel_type": channel_type,
                "creator_id": _creator_id,
            });
            crate::commands::mls::ds_post_ok(state, "/v1/channels/create", &body).await?;
        }
        None => {
            let conn = state.remote_db.conn().await?;
            conn.execute(
                "INSERT INTO channels (id, group_id, name, description, channel_type) VALUES (?1, ?2, ?3, ?4, ?5)",
                libsql::params![id.clone(), group_id.clone(), name.clone(), description.clone(), channel_type.clone()],
            ).await.map_err(|e| db_err(e.into(), "Channel"))?;
        }
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

    let mut role_rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), requester_id.clone()],
    ).await?;

    let role: String = if let Some(row) = role_rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("requester is not a group member")));
    };

    if role != "admin" {
        return Err(Error::Other(anyhow::anyhow!("only group admins can update channels")));
    }

    // DS seam: route the column updates through the Delivery Service (admin
    // re-derived server-side) when configured; else direct.
    match state.config.pollis_delivery_url.as_deref() {
        Some(_) => {
            let body = serde_json::json!({
                "channel_id": channel_id,
                "requester_id": requester_id,
                "name": name,
                "description": description,
            });
            crate::commands::mls::ds_post_ok(state, "/v1/channels/update", &body).await?;
        }
        None => {
            if let Some(ref n) = name {
                conn.execute(
                    "UPDATE channels SET name = ?1 WHERE id = ?2",
                    libsql::params![n.clone(), channel_id.clone()],
                ).await?;
            }
            if let Some(ref d) = description {
                conn.execute(
                    "UPDATE channels SET description = ?1 WHERE id = ?2",
                    libsql::params![d.clone(), channel_id.clone()],
                ).await?;
            }
        }
    }

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

    let mut role_rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), requester_id.clone()],
    ).await?;

    let role: String = if let Some(row) = role_rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("requester is not a group member")));
    };

    if role != "admin" {
        return Err(Error::Other(anyhow::anyhow!("only group admins can delete channels")));
    }

    // DS seam: route the envelope/watermark/channel deletes through the Delivery
    // Service (one transactional, admin-gated write) when configured; else direct.
    match state.config.pollis_delivery_url.as_deref() {
        Some(_) => {
            let body = serde_json::json!({
                "channel_id": channel_id,
                "requester_id": requester_id,
            });
            crate::commands::mls::ds_post_ok(state, "/v1/channels/delete", &body).await?;
        }
        None => {
            conn.execute(
                "DELETE FROM message_envelope WHERE conversation_id = ?1",
                libsql::params![channel_id.clone()],
            ).await?;

            conn.execute(
                "DELETE FROM conversation_watermark WHERE conversation_id = ?1",
                libsql::params![channel_id.clone()],
            ).await?;

            conn.execute(
                "DELETE FROM channels WHERE id = ?1",
                libsql::params![channel_id],
            ).await?;
        }
    }

    if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
        &state.livekit,
        &group_id,
    ).await {
        eprintln!("[realtime] delete_channel: notify group {group_id}: {e}");
    }

    Ok(())
}
