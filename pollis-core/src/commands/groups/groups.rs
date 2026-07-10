use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::derive_slug;
use super::types::{Channel, Group, GroupWithChannels};

pub async fn list_user_groups_with_channels(
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<GroupWithChannels>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT g.id, g.name, g.description, g.owner_id, g.created_at,
                c.id, c.group_id, c.name, c.description, c.channel_type,
                gm.role
         FROM groups g
         JOIN group_member gm ON gm.group_id = g.id
         LEFT JOIN channels c ON c.group_id = g.id
         WHERE gm.user_id = ?1
         ORDER BY g.created_at, c.name",
        libsql::params![user_id],
    ).await?;

    let mut groups: Vec<GroupWithChannels> = Vec::new();
    while let Some(row) = rows.next().await? {
        let group_id: String = row.get(0)?;
        let channel_id: Option<String> = row.get(5)?;

        if let Some(existing) = groups.iter_mut().find(|g| g.id == group_id) {
            if let Some(cid) = channel_id {
                existing.channels.push(Channel {
                    id: cid,
                    group_id: row.get(6)?,
                    name: row.get(7)?,
                    description: row.get(8)?,
                    channel_type: row.get::<Option<String>>(9)?.unwrap_or_else(|| "text".to_string()),
                });
            }
        } else {
            let mut channels = Vec::new();
            if let Some(cid) = channel_id {
                channels.push(Channel {
                    id: cid,
                    group_id: row.get(6)?,
                    name: row.get(7)?,
                    description: row.get(8)?,
                    channel_type: row.get::<Option<String>>(9)?.unwrap_or_else(|| "text".to_string()),
                });
            }
            groups.push(GroupWithChannels {
                id: group_id,
                name: row.get(1)?,
                description: row.get(2)?,
                owner_id: row.get(3)?,
                created_at: row.get(4)?,
                current_user_role: row.get::<Option<String>>(10)?.unwrap_or_else(|| "member".to_string()),
                channels,
            });
        }
    }

    Ok(groups)
}

pub async fn list_user_groups(
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<Group>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT g.id, g.name, g.description, g.owner_id, g.created_at
         FROM groups g
         JOIN group_member gm ON gm.group_id = g.id
         WHERE gm.user_id = ?1",
        libsql::params![user_id],
    ).await?;

    let mut groups = Vec::new();
    while let Some(row) = rows.next().await? {
        groups.push(Group {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            owner_id: row.get(3)?,
            created_at: row.get(4)?,
        });
    }

    Ok(groups)
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
    let body = serde_json::json!({
        "id": id,
        "name": name,
        "description": description,
        "owner_id": owner_id,
        "default_text_channel_id": text_channel_id,
        "default_voice_channel_id": voice_channel_id,
        "created_at": now,
    });
    crate::commands::mls::ds_post_ok(state, "/v1/groups/create", &body).await?;

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
    let conn = state.remote_db.conn().await?;

    // Only admins can update group settings
    let mut rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), requester_id.clone()],
    ).await?;

    let role: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("you are not a member of this group")));
    };

    if role != "admin" {
        return Err(Error::Other(anyhow::anyhow!("only group admins can update group settings")));
    }

    // Route the column updates through the Delivery Service (which re-derives the
    // admin role server-side).
    let body = serde_json::json!({
        "group_id": group_id,
        "requester_id": requester_id,
        "name": name,
        "description": description,
        "icon_url": icon_url,
    });
    crate::commands::mls::ds_post_ok(state, "/v1/groups/update", &body).await?;

    let mut rows = conn.query(
        "SELECT id, name, description, owner_id, created_at FROM groups WHERE id = ?1",
        libsql::params![group_id],
    ).await?;

    if let Some(row) = rows.next().await? {
        Ok(Group {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            owner_id: row.get(3)?,
            created_at: row.get(4)?,
        })
    } else {
        Err(Error::Other(anyhow::anyhow!("group not found after update")))
    }
}

pub async fn delete_group(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Only admins can delete the group
    let mut rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), requester_id.clone()],
    ).await?;

    let role: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("you are not a member of this group")));
    };

    if role != "admin" {
        return Err(Error::Other(anyhow::anyhow!("only group admins can delete the group")));
    }

    // CASCADE deletes group_member and channels entries. Route the delete through
    // the Delivery Service (admin re-checked server-side).
    let body = serde_json::json!({
        "group_id": group_id,
        "requester_id": requester_id,
    });
    crate::commands::mls::ds_post_ok(state, "/v1/groups/delete", &body).await?;

    Ok(())
}

/// Find a group whose name derives to the given slug.
/// Returns an error if no match is found.
pub async fn search_group_by_slug(
    slug: String,
    state: &Arc<AppState>,
) -> Result<Group> {
    let conn = state.remote_db.conn().await?;
    let target = slug.trim().to_lowercase();

    let mut rows = conn.query(
        "SELECT id, name, description, owner_id, created_at FROM groups",
        libsql::params![],
    ).await?;

    while let Some(row) = rows.next().await? {
        let name: String = row.get(1)?;
        if derive_slug(&name) == target {
            return Ok(Group {
                id: row.get(0)?,
                name,
                description: row.get(2)?,
                owner_id: row.get(3)?,
                created_at: row.get(4)?,
            });
        }
    }

    Err(Error::Other(anyhow::anyhow!("No group found with slug '{}'", slug)))
}
