use serde::{Deserialize, Serialize};
use tauri::State;
use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

/// Map a libsql error to a user-facing message based on known constraint patterns.
fn db_err(e: crate::error::Error, context: &str) -> crate::error::Error {
    let msg = e.to_string();
    if msg.contains("FOREIGN KEY") {
        Error::Other(anyhow::anyhow!(
            "{context}: your session may be out of sync — please sign out and sign back in."
        ))
    } else if msg.contains("UNIQUE") || msg.contains("SQLITE_CONSTRAINT_UNIQUE") {
        Error::Other(anyhow::anyhow!("{context} already exists."))
    } else {
        e
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub owner_id: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Channel {
    pub id: String,
    pub group_id: String,
    pub name: String,
    pub description: Option<String>,
    // 'text' or 'voice' — persisted in Turso.
    // Migration: ALTER TABLE channels ADD COLUMN channel_type TEXT NOT NULL DEFAULT 'text';
    pub channel_type: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GroupWithChannels {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub owner_id: String,
    pub created_at: String,
    pub channels: Vec<Channel>,
}

#[tauri::command]
pub async fn list_user_groups_with_channels(
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<GroupWithChannels>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT g.id, g.name, g.description, g.owner_id, g.created_at,
                c.id, c.group_id, c.name, c.description, c.channel_type
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
                channels,
            });
        }
    }

    Ok(groups)
}

#[tauri::command]
pub async fn list_user_groups(
    user_id: String,
    state: State<'_, Arc<AppState>>,
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

#[tauri::command]
pub async fn list_group_channels(
    group_id: String,
    state: State<'_, Arc<AppState>>,
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

#[tauri::command]
pub async fn create_group(
    name: String,
    description: Option<String>,
    owner_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Group> {
    let conn = state.remote_db.conn().await?;
    let id = Ulid::new().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        "INSERT INTO groups (id, name, description, owner_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        libsql::params![id.clone(), name.clone(), description.clone(), owner_id.clone(), now.clone()],
    ).await.map_err(|e| db_err(e.into(), "Group"))?;

    conn.execute(
        "INSERT INTO group_member (group_id, user_id, role) VALUES (?1, ?2, 'owner')",
        libsql::params![id.clone(), owner_id.clone()],
    ).await.map_err(|e| db_err(e.into(), "Group member"))?;

    Ok(Group { id, name, description, owner_id, created_at: now })
}

#[tauri::command]
pub async fn create_channel(
    group_id: String,
    name: String,
    description: Option<String>,
    // 'text' (default) or 'voice' — stored in the channel_type column.
    // Requires Turso migration: ALTER TABLE channels ADD COLUMN channel_type TEXT NOT NULL DEFAULT 'text';
    channel_type: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<Channel> {
    let conn = state.remote_db.conn().await?;
    let id = Ulid::new().to_string();
    let channel_type = channel_type.unwrap_or_else(|| "text".to_string());

    conn.execute(
        "INSERT INTO channels (id, group_id, name, description, channel_type) VALUES (?1, ?2, ?3, ?4, ?5)",
        libsql::params![id.clone(), group_id.clone(), name.clone(), description.clone(), channel_type.clone()],
    ).await.map_err(|e| db_err(e.into(), "Channel"))?;

    Ok(Channel { id, group_id, name, description, channel_type })
}

/// Internal helper: add a user directly to a group as a member.
/// Used by invite acceptance and join request approval.
async fn add_member_to_group(
    conn: &libsql::Connection,
    group_id: &str,
    user_id: &str,
) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO group_member (group_id, user_id, role) VALUES (?1, ?2, 'member')",
        libsql::params![group_id, user_id],
    ).await?;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GroupMember {
    pub user_id: String,
    pub username: Option<String>,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub role: String,
    pub joined_at: String,
}

#[tauri::command]
pub async fn update_group(
    group_id: String,
    requester_id: String,
    name: Option<String>,
    description: Option<String>,
    icon_url: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<Group> {
    let conn = state.remote_db.conn().await?;

    // Only owner can update
    let mut rows = conn.query(
        "SELECT owner_id FROM groups WHERE id = ?1",
        libsql::params![group_id.clone()],
    ).await?;

    let owner_id: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("group not found")));
    };

    if owner_id != requester_id {
        return Err(Error::Other(anyhow::anyhow!("only the group owner can update group settings")));
    }

    if let Some(ref n) = name {
        conn.execute(
            "UPDATE groups SET name = ?1 WHERE id = ?2",
            libsql::params![n.clone(), group_id.clone()],
        ).await?;
    }
    if let Some(ref d) = description {
        conn.execute(
            "UPDATE groups SET description = ?1 WHERE id = ?2",
            libsql::params![d.clone(), group_id.clone()],
        ).await?;
    }
    if let Some(ref u) = icon_url {
        conn.execute(
            "UPDATE groups SET icon_url = ?1 WHERE id = ?2",
            libsql::params![u.clone(), group_id.clone()],
        ).await?;
    }

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

#[tauri::command]
pub async fn delete_group(
    group_id: String,
    requester_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT owner_id FROM groups WHERE id = ?1",
        libsql::params![group_id.clone()],
    ).await?;

    let owner_id: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("group not found")));
    };

    if owner_id != requester_id {
        return Err(Error::Other(anyhow::anyhow!("only the group owner can delete the group")));
    }

    // CASCADE deletes group_member and channels entries
    conn.execute(
        "DELETE FROM groups WHERE id = ?1",
        libsql::params![group_id],
    ).await?;

    Ok(())
}

#[tauri::command]
pub async fn get_group_members(
    group_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<GroupMember>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT gm.user_id, u.username, u.display_name, u.avatar_url, gm.role, gm.joined_at
         FROM group_member gm
         LEFT JOIN users u ON u.id = gm.user_id
         WHERE gm.group_id = ?1",
        libsql::params![group_id],
    ).await?;

    let mut members = Vec::new();
    while let Some(row) = rows.next().await? {
        members.push(GroupMember {
            user_id: row.get(0)?,
            username: row.get(1)?,
            display_name: row.get(2)?,
            avatar_url: row.get(3)?,
            role: row.get(4)?,
            joined_at: row.get(5)?,
        });
    }

    Ok(members)
}

#[tauri::command]
pub async fn remove_member_from_group(
    group_id: String,
    user_id: String,
    requester_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Check requester's role
    let mut rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), requester_id.clone()],
    ).await?;

    let requester_role: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("requester is not a group member")));
    };

    // Owner or admin can remove others; anyone can remove themselves
    if requester_id != user_id && requester_role != "owner" && requester_role != "admin" {
        return Err(Error::Other(anyhow::anyhow!(
            "only an owner or admin can remove other members"
        )));
    }

    // Owner cannot be removed (must transfer first)
    if requester_id != user_id {
        let mut target_rows = conn.query(
            "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
            libsql::params![group_id.clone(), user_id.clone()],
        ).await?;

        if let Some(row) = target_rows.next().await? {
            let target_role: String = row.get(0)?;
            if target_role == "owner" {
                return Err(Error::Other(anyhow::anyhow!(
                    "cannot remove the group owner; transfer ownership first"
                )));
            }
        }
    }

    conn.execute(
        "DELETE FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id, user_id],
    ).await?;

    Ok(())
}

#[tauri::command]
pub async fn leave_group(
    group_id: String,
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Check if user is the owner
    let mut rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), user_id.clone()],
    ).await?;

    let role: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("user is not a member of this group")));
    };

    // Check how many members the group has
    let mut count_rows = conn.query(
        "SELECT COUNT(*) FROM group_member WHERE group_id = ?1",
        libsql::params![group_id.clone()],
    ).await?;
    let member_count: i64 = if let Some(row) = count_rows.next().await? {
        row.get(0)?
    } else {
        0
    };

    if role == "owner" && member_count > 1 {
        return Err(Error::Other(anyhow::anyhow!(
            "owner must transfer ownership before leaving the group"
        )));
    }

    conn.execute(
        "DELETE FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), user_id],
    ).await?;

    // If no members remain, delete the group (cascades to channels, invites, etc.)
    if member_count <= 1 {
        conn.execute(
            "DELETE FROM groups WHERE id = ?1",
            libsql::params![group_id],
        ).await?;
    }

    Ok(())
}

#[tauri::command]
pub async fn update_channel(
    channel_id: String,
    requester_id: String,
    name: Option<String>,
    description: Option<String>,
    state: State<'_, Arc<AppState>>,
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

    if role != "owner" && role != "admin" {
        return Err(Error::Other(anyhow::anyhow!("only group owner or admin can update channels")));
    }

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

#[tauri::command]
pub async fn delete_channel(
    channel_id: String,
    requester_id: String,
    state: State<'_, Arc<AppState>>,
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
        libsql::params![group_id, requester_id],
    ).await?;

    let role: String = if let Some(row) = role_rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("requester is not a group member")));
    };

    if role != "owner" && role != "admin" {
        return Err(Error::Other(anyhow::anyhow!("only group owner or admin can delete channels")));
    }

    conn.execute(
        "DELETE FROM channels WHERE id = ?1",
        libsql::params![channel_id],
    ).await?;

    Ok(())
}

/// Mirrors the frontend `deriveSlug` in urlRouting.ts.
fn derive_slug(name: &str) -> String {
    let lower = name.to_lowercase();
    let cleaned: String = lower
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || c.is_ascii_whitespace() || *c == '-')
        .collect();
    let with_hyphens = cleaned.split_ascii_whitespace().collect::<Vec<_>>().join("-");
    let mut result = String::new();
    let mut prev_hyphen = false;
    for c in with_hyphens.chars() {
        if c == '-' {
            if !prev_hyphen {
                result.push('-');
            }
            prev_hyphen = true;
        } else {
            result.push(c);
            prev_hyphen = false;
        }
    }
    result.trim_matches('-').to_string()
}

/// Find a group whose name derives to the given slug.
/// Returns an error if no match is found.
#[tauri::command]
pub async fn search_group_by_slug(
    slug: String,
    state: State<'_, Arc<AppState>>,
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

#[tauri::command]
pub async fn transfer_ownership(
    group_id: String,
    new_owner_id: String,
    requester_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Verify requester is current owner
    let mut rows = conn.query(
        "SELECT owner_id FROM groups WHERE id = ?1",
        libsql::params![group_id.clone()],
    ).await?;

    let current_owner: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("group not found")));
    };

    if current_owner != requester_id {
        return Err(Error::Other(anyhow::anyhow!("only the current owner can transfer ownership")));
    }

    // Verify new owner is a member
    let mut member_rows = conn.query(
        "SELECT 1 FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), new_owner_id.clone()],
    ).await?;

    if member_rows.next().await?.is_none() {
        return Err(Error::Other(anyhow::anyhow!("new owner must be a current member of the group")));
    }

    // Update the owner_id in groups table
    conn.execute(
        "UPDATE groups SET owner_id = ?1 WHERE id = ?2",
        libsql::params![new_owner_id.clone(), group_id.clone()],
    ).await?;

    // Update roles in group_member
    conn.execute(
        "UPDATE group_member SET role = 'member' WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), requester_id],
    ).await?;

    conn.execute(
        "UPDATE group_member SET role = 'owner' WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id, new_owner_id],
    ).await?;

    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PendingInvite {
    pub id: String,
    pub group_id: String,
    pub group_name: String,
    pub inviter_id: String,
    pub inviter_username: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct JoinRequest {
    pub id: String,
    pub group_id: String,
    pub requester_id: String,
    pub requester_username: Option<String>,
    pub created_at: String,
}

/// Invite a user (by username) to a group. Inviter must be a current member.
#[tauri::command]
pub async fn send_group_invite(
    group_id: String,
    inviter_id: String,
    invitee_identifier: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Verify inviter is a member
    let mut rows = conn.query(
        "SELECT 1 FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), inviter_id.clone()],
    ).await?;
    if rows.next().await?.is_none() {
        return Err(Error::Other(anyhow::anyhow!("you are not a member of this group")));
    }

    // Look up invitee by username or email
    let mut user_rows = conn.query(
        "SELECT id FROM users WHERE username = ?1 OR email = ?1",
        libsql::params![invitee_identifier.clone()],
    ).await?;
    let invitee_id: String = if let Some(row) = user_rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("user '{}' not found", invitee_identifier)));
    };

    // Check if invitee is already a member
    let mut member_rows = conn.query(
        "SELECT 1 FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), invitee_id.clone()],
    ).await?;
    if member_rows.next().await?.is_some() {
        return Err(Error::Other(anyhow::anyhow!("that user is already a member of this group")));
    }

    // Check for existing pending invite
    let mut existing = conn.query(
        "SELECT 1 FROM group_invite WHERE group_id = ?1 AND invitee_id = ?2 AND status = 'pending'",
        libsql::params![group_id.clone(), invitee_id.clone()],
    ).await?;
    if existing.next().await?.is_some() {
        return Err(Error::Other(anyhow::anyhow!("a pending invite already exists for this user")));
    }

    let id = Ulid::new().to_string();
    conn.execute(
        "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES (?1, ?2, ?3, ?4)",
        libsql::params![id, group_id, inviter_id, invitee_id],
    ).await.map_err(|e| db_err(e.into(), "Invite"))?;

    Ok(())
}

/// Get all pending invites for the given user.
#[tauri::command]
pub async fn get_pending_invites(
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<PendingInvite>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT gi.id, gi.group_id, g.name, gi.inviter_id, u.username, gi.created_at
         FROM group_invite gi
         JOIN groups g ON g.id = gi.group_id
         LEFT JOIN users u ON u.id = gi.inviter_id
         WHERE gi.invitee_id = ?1 AND gi.status = 'pending'
         ORDER BY gi.created_at DESC",
        libsql::params![user_id],
    ).await?;

    let mut invites = Vec::new();
    while let Some(row) = rows.next().await? {
        invites.push(PendingInvite {
            id: row.get(0)?,
            group_id: row.get(1)?,
            group_name: row.get(2)?,
            inviter_id: row.get(3)?,
            inviter_username: row.get(4)?,
            created_at: row.get(5)?,
        });
    }

    Ok(invites)
}

/// Accept a pending invite. Adds the user to the group.
#[tauri::command]
pub async fn accept_group_invite(
    invite_id: String,
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT group_id FROM group_invite WHERE id = ?1 AND invitee_id = ?2 AND status = 'pending'",
        libsql::params![invite_id.clone(), user_id.clone()],
    ).await?;

    let group_id: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("invite not found or already processed")));
    };

    add_member_to_group(&conn, &group_id, &user_id).await?;

    conn.execute(
        "UPDATE group_invite SET status = 'accepted' WHERE id = ?1",
        libsql::params![invite_id],
    ).await?;

    Ok(())
}

/// Decline a pending invite.
#[tauri::command]
pub async fn decline_group_invite(
    invite_id: String,
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT 1 FROM group_invite WHERE id = ?1 AND invitee_id = ?2 AND status = 'pending'",
        libsql::params![invite_id.clone(), user_id],
    ).await?;

    if rows.next().await?.is_none() {
        return Err(Error::Other(anyhow::anyhow!("invite not found or already processed")));
    }

    conn.execute(
        "UPDATE group_invite SET status = 'declined' WHERE id = ?1",
        libsql::params![invite_id],
    ).await?;

    Ok(())
}

/// Request access to a group. Creates a pending join request.
#[tauri::command]
pub async fn request_group_access(
    group_id: String,
    requester_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Verify group exists
    let mut rows = conn.query(
        "SELECT 1 FROM groups WHERE id = ?1",
        libsql::params![group_id.clone()],
    ).await?;
    if rows.next().await?.is_none() {
        return Err(Error::Other(anyhow::anyhow!("group not found")));
    }

    // Check not already a member
    let mut member_rows = conn.query(
        "SELECT 1 FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), requester_id.clone()],
    ).await?;
    if member_rows.next().await?.is_some() {
        return Err(Error::Other(anyhow::anyhow!("you are already a member of this group")));
    }

    // Check not already a pending request
    let mut existing = conn.query(
        "SELECT 1 FROM group_join_request WHERE group_id = ?1 AND requester_id = ?2 AND status = 'pending'",
        libsql::params![group_id.clone(), requester_id.clone()],
    ).await?;
    if existing.next().await?.is_some() {
        return Err(Error::Other(anyhow::anyhow!("you already have a pending request for this group")));
    }

    let id = Ulid::new().to_string();
    conn.execute(
        "INSERT INTO group_join_request (id, group_id, requester_id) VALUES (?1, ?2, ?3)",
        libsql::params![id, group_id, requester_id],
    ).await.map_err(|e| db_err(e.into(), "Join request"))?;

    Ok(())
}

/// Get all pending join requests for a group. Requester must be a member.
#[tauri::command]
pub async fn get_group_join_requests(
    group_id: String,
    requester_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<JoinRequest>> {
    let conn = state.remote_db.conn().await?;

    // Verify caller is a member
    let mut rows = conn.query(
        "SELECT 1 FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), requester_id],
    ).await?;
    if rows.next().await?.is_none() {
        return Err(Error::Other(anyhow::anyhow!("you are not a member of this group")));
    }

    let mut req_rows = conn.query(
        "SELECT jr.id, jr.group_id, jr.requester_id, u.username, jr.created_at
         FROM group_join_request jr
         LEFT JOIN users u ON u.id = jr.requester_id
         WHERE jr.group_id = ?1 AND jr.status = 'pending'
         ORDER BY jr.created_at ASC",
        libsql::params![group_id],
    ).await?;

    let mut requests = Vec::new();
    while let Some(row) = req_rows.next().await? {
        requests.push(JoinRequest {
            id: row.get(0)?,
            group_id: row.get(1)?,
            requester_id: row.get(2)?,
            requester_username: row.get(3)?,
            created_at: row.get(4)?,
        });
    }

    Ok(requests)
}

/// Approve a join request. Approver must be a group member. Adds the requester to the group.
#[tauri::command]
pub async fn approve_join_request(
    request_id: String,
    approver_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT group_id, requester_id FROM group_join_request WHERE id = ?1 AND status = 'pending'",
        libsql::params![request_id.clone()],
    ).await?;

    let (group_id, requester_id): (String, String) = if let Some(row) = rows.next().await? {
        (row.get(0)?, row.get(1)?)
    } else {
        return Err(Error::Other(anyhow::anyhow!("join request not found or already processed")));
    };

    // Verify approver is a member
    let mut member_rows = conn.query(
        "SELECT 1 FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), approver_id.clone()],
    ).await?;
    if member_rows.next().await?.is_none() {
        return Err(Error::Other(anyhow::anyhow!("you are not a member of this group")));
    }

    let now = chrono::Utc::now().to_rfc3339();

    add_member_to_group(&conn, &group_id, &requester_id).await?;

    conn.execute(
        "UPDATE group_join_request SET status = 'approved', reviewed_by = ?1, reviewed_at = ?2 WHERE id = ?3",
        libsql::params![approver_id, now, request_id],
    ).await?;

    Ok(())
}

/// Reject a join request. Approver must be a group member.
#[tauri::command]
pub async fn reject_join_request(
    request_id: String,
    approver_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT group_id FROM group_join_request WHERE id = ?1 AND status = 'pending'",
        libsql::params![request_id.clone()],
    ).await?;

    let group_id: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("join request not found or already processed")));
    };

    // Verify approver is a member
    let mut member_rows = conn.query(
        "SELECT 1 FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id, approver_id.clone()],
    ).await?;
    if member_rows.next().await?.is_none() {
        return Err(Error::Other(anyhow::anyhow!("you are not a member of this group")));
    }

    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE group_join_request SET status = 'rejected', reviewed_by = ?1, reviewed_at = ?2 WHERE id = ?3",
        libsql::params![approver_id, now, request_id],
    ).await?;

    Ok(())
}
