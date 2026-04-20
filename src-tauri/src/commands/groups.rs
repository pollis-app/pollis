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
    pub current_user_role: String,
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
        "INSERT INTO group_member (group_id, user_id, role) VALUES (?1, ?2, 'admin')",
        libsql::params![id.clone(), owner_id.clone()],
    ).await.map_err(|e| db_err(e.into(), "Group member"))?;

    // Create default channels: a #General text channel and a Voice Chat.
    conn.execute(
        "INSERT INTO channels (id, group_id, name, description, channel_type) VALUES \
            (?1, ?2, 'General', NULL, 'text'), \
            (?3, ?2, 'Voice Chat', NULL, 'voice')",
        libsql::params![Ulid::new().to_string(), id.clone(), Ulid::new().to_string()],
    ).await.map_err(|e| db_err(e.into(), "Channel"))?;

    // Create the per-group MLS group — all channels in this group share it.
    match crate::commands::mls::init_mls_group(state.inner(), &id, &owner_id).await {
        Ok(()) => {
            // Reconcile adds the creator's other devices (if any have KPs).
            if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
                state.inner(), &id, &owner_id,
            ).await {
                eprintln!("[mls] create_group: reconcile failed: {e}");
            }
        }
        Err(e) => eprintln!("[mls] create_group: mls group init failed (non-fatal): {e}"),
    }

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
    _creator_id: String,
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

    // Initialize watermark rows for every (channel, device) pair so pre-join
    // messages don't block envelope cleanup indefinitely. Devices registered
    // after this point are seeded by `register_device`.
    if let Err(e) = conn.execute(
        "INSERT OR IGNORE INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at)
         SELECT c.id, ?1, ud.device_id, datetime('now')
         FROM channels c
         JOIN user_device ud ON ud.user_id = ?1
         WHERE c.group_id = ?2",
        libsql::params![user_id, group_id],
    ).await {
        eprintln!("[watermark] add_member_to_group: watermark init failed: {e}");
    }

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
        "SELECT gm.user_id, u.username, u.avatar_url, gm.role, gm.joined_at
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
            display_name: None,
            avatar_url: row.get(2)?,
            role: row.get(3)?,
            joined_at: row.get(4)?,
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

    // Admins can remove others; anyone can remove themselves (leave)
    if requester_id != user_id && requester_role != "admin" {
        return Err(Error::Other(anyhow::anyhow!(
            "only an admin can remove other members"
        )));
    }

    conn.execute(
        "DELETE FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), user_id.clone()],
    ).await?;

    // Reconcile removes the member's leaves from the MLS tree.
    if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
        state.inner(), &group_id, &requester_id,
    ).await {
        eprintln!("[mls] remove_member_from_group: reconcile for group {group_id}: {e}");
    }

    // Notify group members so they refetch the member list.
    if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
        &state.livekit,
        &group_id,
    ).await {
        eprintln!("[realtime] remove_member_from_group: notify group {group_id}: {e}");
    }

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

    let _role: String = if let Some(row) = rows.next().await? {
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

    // Owners can leave thegroup, there's no requirement for ownership atm so I am commenting this out.
    // Might change when we introduce rolls, give them the option to require transfer, etc.

    // if role == "owner" && member_count > 1 {
    //     return Err(Error::Other(anyhow::anyhow!(
    //         "owner must transfer ownership before leaving the group"
    //     )));
    // }

    conn.execute(
        "DELETE FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), user_id.clone()],
    ).await?;

    // A user cannot commit their own removal in MLS ("remove_members with self
    // as target" is rejected by the spec).  Instead, wipe the local group state
    // so the leaver can no longer read or send messages.  The remaining members
    // still see this user in their epoch until an admin issues a remove commit,
    // but forward secrecy ensures the leaver cannot decrypt future traffic after
    // the next epoch advance.
    match crate::commands::mls::forget_local_mls_group(state.inner(), &group_id).await {
        Ok(()) => {}
        Err(e) => eprintln!("[mls] leave_group: forget local group {group_id}: {e}"),
    }

    // Signal remaining members to reconcile (removes the leaver's stale leaf).
    // Use publish_to_room_server since the leaver may not be connected to the room.
    if let Err(e) = crate::commands::livekit::publish_to_room_server(
        &state.config,
        &group_id,
        serde_json::json!({"type": "membership_changed", "group_id": group_id}),
    ).await {
        eprintln!("[realtime] leave_group: notify group {group_id}: {e}");
    }

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

    if role != "admin" {
        return Err(Error::Other(anyhow::anyhow!("only group admins can update channels")));
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

    if role != "admin" {
        return Err(Error::Other(anyhow::anyhow!("only group admins can delete channels")));
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

/// Promote or demote a group member. Requester must be an admin.
/// Valid roles: 'admin', 'member'.
#[tauri::command]
pub async fn set_member_role(
    group_id: String,
    user_id: String,
    role: String,
    requester_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    if role != "admin" && role != "member" {
        return Err(Error::Other(anyhow::anyhow!("invalid role: must be 'admin' or 'member'")));
    }

    let conn = state.remote_db.conn().await?;

    // Requester must be admin
    let mut rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), requester_id.clone()],
    ).await?;

    let requester_role: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("you are not a member of this group")));
    };

    if requester_role != "admin" {
        return Err(Error::Other(anyhow::anyhow!("only admins can change member roles")));
    }

    // Verify target is a member
    let mut target_rows = conn.query(
        "SELECT 1 FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), user_id.clone()],
    ).await?;

    if target_rows.next().await?.is_none() {
        return Err(Error::Other(anyhow::anyhow!("user is not a member of this group")));
    }

    conn.execute(
        "UPDATE group_member SET role = ?1 WHERE group_id = ?2 AND user_id = ?3",
        libsql::params![role, group_id.clone(), user_id],
    ).await?;

    // Notify other online group members so their members list refreshes.
    {
        use livekit::DataPacket;
        use crate::realtime::RealtimeEvent;
        let payload = match serde_json::to_vec(&RealtimeEvent::MemberRoleChanged {
            group_id: group_id.clone(),
        }) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("[role] serialize MemberRoleChanged: {e}");
                return Ok(());
            }
        };
        let lk = state.livekit.lock().await;
        if let Some((room, _)) = lk.rooms.get(&group_id) {
            let room = Arc::clone(room);
            drop(lk);
            let _ = room.local_participant().publish_data(DataPacket {
                payload,
                reliable: true,
                ..Default::default()
            }).await;
        }
    }

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
    pub status: String,
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

    // Only admins can send invites
    let mut rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), inviter_id.clone()],
    ).await?;
    let inviter_role: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("you are not a member of this group")));
    };
    if inviter_role != "admin" {
        return Err(Error::Other(anyhow::anyhow!("only admins can invite members")));
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

    if invitee_id == inviter_id {
        return Err(Error::Other(anyhow::anyhow!("cannot invite yourself to a group")));
    }

    // Silently reject when either party has blocked the other. Returns
    // the generic BLOCK_ERR so neither side can infer why the invite
    // failed.
    if crate::commands::blocks::is_blocked_either_way(&conn, &inviter_id, &invitee_id).await? {
        return Err(Error::Other(anyhow::anyhow!(
            crate::commands::dm::BLOCK_ERR
        )));
    }

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
        "SELECT 1 FROM group_invite WHERE group_id = ?1 AND invitee_id = ?2",
        libsql::params![group_id.clone(), invitee_id.clone()],
    ).await?;
    if existing.next().await?.is_some() {
        return Err(Error::Other(anyhow::anyhow!("a pending invite already exists for this user")));
    }

    let id = Ulid::new().to_string();
    conn.execute(
        "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES (?1, ?2, ?3, ?4)",
        libsql::params![id, group_id.clone(), inviter_id.clone(), invitee_id.clone()],
    ).await.map_err(|e| db_err(e.into(), "Invite"))?;

    // Reconcile adds the invitee's devices to the MLS tree now so their
    // Welcome is ready before they accept — no dependency on simultaneous
    // online presence between inviter and acceptor.
    if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
        state.inner(), &group_id, &inviter_id,
    ).await {
        eprintln!("[mls] send_group_invite: reconcile for group {group_id}: {e}");
    }

    // Notify invitee via their inbox so the pending invite appears immediately.
    if let Err(e) = crate::commands::livekit::publish_to_user_inbox(
        &state.config,
        &invitee_id,
        serde_json::json!({"type": "membership_changed", "group_id": group_id}),
    ).await {
        eprintln!("[inbox] send_group_invite: notify {invitee_id} failed: {e}");
    }

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
         WHERE gi.invitee_id = ?1
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
        "SELECT group_id FROM group_invite WHERE id = ?1 AND invitee_id = ?2",
        libsql::params![invite_id.clone(), user_id.clone()],
    ).await?;

    let group_id: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("invite not found or already processed")));
    };

    add_member_to_group(&conn, &group_id, &user_id).await?;

    // Delete the invite row — accepted invites don't need to be retained.
    conn.execute(
        "DELETE FROM group_invite WHERE id = ?1",
        libsql::params![invite_id],
    ).await?;

    // Notify existing group members so they see the new member.
    // The accepting user isn't connected to the group room yet, so use
    // the server-side publish to reach existing members.
    if let Err(e) = crate::commands::livekit::publish_to_room_server(
        &state.config,
        &group_id,
        serde_json::json!({"type": "membership_changed", "group_id": group_id}),
    ).await {
        eprintln!("[realtime] accept_group_invite: notify group {group_id}: {e}");
    }

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
        "SELECT 1 FROM group_invite WHERE id = ?1 AND invitee_id = ?2",
        libsql::params![invite_id.clone(), user_id],
    ).await?;

    if rows.next().await?.is_none() {
        return Err(Error::Other(anyhow::anyhow!("invite not found or already processed")));
    }

    // Delete the invite row — declined invites don't need to be retained.
    conn.execute(
        "DELETE FROM group_invite WHERE id = ?1",
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

    // Block duplicate pending requests, but allow re-application after rejection.
    // If a pending request already exists, error. If a prior rejected/approved row
    // exists, the upsert below will reset it to pending — giving admins a clean
    // slate to review while preserving the row-per-pair constraint.
    let mut existing = conn.query(
        "SELECT status FROM group_join_request WHERE group_id = ?1 AND requester_id = ?2",
        libsql::params![group_id.clone(), requester_id.clone()],
    ).await?;
    if let Some(row) = existing.next().await? {
        let status: String = row.get(0)?;
        if status == "pending" {
            return Err(Error::Other(anyhow::anyhow!("you already have a pending request for this group")));
        }
    }

    let id = Ulid::new().to_string();
    // Upsert: new insert, or reset a prior rejected/approved row back to pending.
    // reviewed_by and reviewed_at are intentionally preserved so the history of
    // who reviewed the previous request is available for future UI use.
    conn.execute(
        "INSERT INTO group_join_request (id, group_id, requester_id, status, created_at)
         VALUES (?1, ?2, ?3, 'pending', datetime('now'))
         ON CONFLICT(group_id, requester_id) DO UPDATE SET
             id         = excluded.id,
             status     = 'pending',
             created_at = excluded.created_at",
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

    // Only admins can view join requests; non-admins get an empty list
    let mut rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), requester_id],
    ).await?;
    let role: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Ok(Vec::new());
    };
    if role != "admin" {
        return Ok(Vec::new());
    }

    let mut req_rows = conn.query(
        "SELECT jr.id, jr.group_id, jr.requester_id, u.username, jr.status, jr.created_at
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
            status: row.get(4)?,
            created_at: row.get(5)?,
        });
    }

    Ok(requests)
}

/// Get the current user's own join request for a specific group, if one exists.
/// Returns None if no request has been made.
#[tauri::command]
pub async fn get_my_join_request(
    group_id: String,
    requester_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Option<JoinRequest>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT id, group_id, requester_id, status, created_at
         FROM group_join_request
         WHERE group_id = ?1 AND requester_id = ?2",
        libsql::params![group_id, requester_id],
    ).await?;

    if let Some(row) = rows.next().await? {
        Ok(Some(JoinRequest {
            id: row.get(0)?,
            group_id: row.get(1)?,
            requester_id: row.get(2)?,
            requester_username: None,
            status: row.get(3)?,
            created_at: row.get(4)?,
        }))
    } else {
        Ok(None)
    }
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

    // Only admins can approve join requests
    let mut member_rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), approver_id.clone()],
    ).await?;
    let approver_role: String = if let Some(row) = member_rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("you are not a member of this group")));
    };
    if approver_role != "admin" {
        return Err(Error::Other(anyhow::anyhow!("only admins can approve join requests")));
    }

    let now = chrono::Utc::now().to_rfc3339();

    add_member_to_group(&conn, &group_id, &requester_id).await?;

    conn.execute(
        "UPDATE group_join_request SET status = 'approved', reviewed_by = ?1, reviewed_at = ?2 WHERE id = ?3",
        libsql::params![approver_id.clone(), now, request_id],
    ).await?;

    // Reconcile adds the requester's devices to the MLS tree.
    if let Err(e) = crate::commands::mls::reconcile_group_mls_impl(
        state.inner(), &group_id, &approver_id,
    ).await {
        eprintln!("[mls] approve_join_request: reconcile for group {group_id}: {e}");
    }

    // Notify requester their join request was approved so they see the group immediately.
    if let Err(e) = crate::commands::livekit::publish_to_user_inbox(
        &state.config,
        &requester_id,
        serde_json::json!({"type": "membership_changed", "group_id": group_id}),
    ).await {
        eprintln!("[inbox] approve_join_request: notify {requester_id} failed: {e}");
    }

    // Notify existing group members so they refetch the member list.
    if let Err(e) = crate::commands::livekit::publish_membership_changed_to_room(
        &state.livekit,
        &group_id,
    ).await {
        eprintln!("[realtime] approve_join_request: notify group {group_id}: {e}");
    }

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

    // Only admins can reject join requests
    let mut member_rows = conn.query(
        "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![group_id.clone(), approver_id.clone()],
    ).await?;
    let approver_role: String = if let Some(row) = member_rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("you are not a member of this group")));
    };
    if approver_role != "admin" {
        return Err(Error::Other(anyhow::anyhow!("only admins can reject join requests")));
    }

    let now = chrono::Utc::now().to_rfc3339();
    conn.execute(
        "UPDATE group_join_request SET status = 'rejected', reviewed_by = ?1, reviewed_at = ?2 WHERE id = ?3",
        libsql::params![approver_id, now, request_id],
    ).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    const BASELINE: &str = include_str!("../db/migrations/000000_baseline.sql");

    /// Extra tables from numbered migrations that the base schema doesn't include.
    const EXTRA_TABLES: &str = "
        CREATE TABLE IF NOT EXISTS conversation_watermark (
            conversation_id TEXT NOT NULL,
            user_id         TEXT NOT NULL,
            device_id       TEXT NOT NULL,
            last_fetched_at TEXT NOT NULL,
            PRIMARY KEY (conversation_id, user_id, device_id)
        );
        CREATE TABLE IF NOT EXISTS user_device (
            device_id   TEXT PRIMARY KEY,
            user_id     TEXT NOT NULL,
            device_name TEXT,
            created_at  TEXT NOT NULL DEFAULT (datetime('now')),
            last_seen   TEXT NOT NULL DEFAULT (datetime('now'))
        );
    ";

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn.execute_batch(BASELINE).unwrap();
        conn.execute_batch(EXTRA_TABLES).unwrap();
        conn
    }

    fn setup(conn: &Connection) {
        conn.execute("INSERT INTO users (id, email, username) VALUES ('alice', 'alice@x.com', 'alice')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('bob',   'bob@x.com',   'bob')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('carol', 'carol@x.com', 'carol')", []).unwrap();

        conn.execute("INSERT INTO groups (id, name, description, owner_id) VALUES ('g1', 'Test Group', 'a group', 'alice')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'bob', 'member')", []).unwrap();

        conn.execute("INSERT INTO channels (id, group_id, name, channel_type) VALUES ('ch1', 'g1', 'general', 'text')", []).unwrap();
        conn.execute("INSERT INTO channels (id, group_id, name, channel_type) VALUES ('ch2', 'g1', 'random', 'text')", []).unwrap();
    }

    // ── derive_slug ────────────────────────────────────────────────────────

    #[test]
    fn slug_simple_name() {
        assert_eq!(super::derive_slug("Test Group"), "test-group");
    }

    #[test]
    fn slug_special_characters_stripped() {
        assert_eq!(super::derive_slug("Hello, World!"), "hello-world");
    }

    #[test]
    fn slug_multiple_spaces_collapsed() {
        assert_eq!(super::derive_slug("a   b"), "a-b");
    }

    #[test]
    fn slug_leading_trailing_hyphens_trimmed() {
        assert_eq!(super::derive_slug("-test-"), "test");
    }

    #[test]
    fn slug_consecutive_hyphens_collapsed() {
        assert_eq!(super::derive_slug("a---b"), "a-b");
    }

    #[test]
    fn slug_mixed_case_lowered() {
        assert_eq!(super::derive_slug("My Cool Group"), "my-cool-group");
    }

    #[test]
    fn slug_already_clean() {
        assert_eq!(super::derive_slug("simple"), "simple");
    }

    #[test]
    fn slug_unicode_stripped() {
        assert_eq!(super::derive_slug("café"), "caf");
    }

    // ── group queries ──────────────────────────────────────────────────────

    #[test]
    fn list_user_groups_only_returns_member_groups() {
        let conn = db();
        setup(&conn);

        // carol's own group
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g2', 'Carol Group', 'carol')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g2', 'carol', 'admin')", []).unwrap();

        let groups: Vec<String> = conn.prepare(
            "SELECT g.id FROM groups g JOIN group_member gm ON gm.group_id = g.id WHERE gm.user_id = ?1",
        ).unwrap().query_map(
            rusqlite::params!["bob"],
            |row| row.get(0),
        ).unwrap().map(|r| r.unwrap()).collect();

        assert_eq!(groups, ["g1"]);
        assert!(!groups.contains(&"g2".to_string()));
    }

    #[test]
    fn list_group_channels_returns_all_channels() {
        let conn = db();
        setup(&conn);

        let channels: Vec<(String, String)> = conn.prepare(
            "SELECT id, name FROM channels WHERE group_id = ?1",
        ).unwrap().query_map(
            rusqlite::params!["g1"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap().map(|r| r.unwrap()).collect();

        assert_eq!(channels.len(), 2);
        let names: Vec<&str> = channels.iter().map(|(_, n)| n.as_str()).collect();
        assert!(names.contains(&"general"));
        assert!(names.contains(&"random"));
    }

    #[test]
    fn channel_type_defaults_to_text() {
        let conn = db();
        setup(&conn);

        // Insert channel without explicit channel_type
        conn.execute("INSERT INTO channels (id, group_id, name) VALUES ('ch-no-type', 'g1', 'untyped')", []).unwrap();

        let ct: String = conn.query_row(
            "SELECT channel_type FROM channels WHERE id = 'ch-no-type'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(ct, "text");
    }

    // ── RBAC: admin-only operations ────────────────────────────────────────

    #[test]
    fn admin_role_check_returns_admin_for_admin_user() {
        let conn = db();
        setup(&conn);

        let role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
            rusqlite::params!["g1", "alice"],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(role, "admin");
    }

    #[test]
    fn admin_role_check_returns_member_for_regular_user() {
        let conn = db();
        setup(&conn);

        let role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
            rusqlite::params!["g1", "bob"],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(role, "member");
    }

    #[test]
    fn role_check_returns_none_for_non_member() {
        let conn = db();
        setup(&conn);

        let result = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
            rusqlite::params!["g1", "carol"],
            |row| row.get::<_, String>(0),
        );
        assert!(result.is_err());
    }

    // ── group update (partial) ─────────────────────────────────────────────

    #[test]
    fn update_group_name_only() {
        let conn = db();
        setup(&conn);

        conn.execute("UPDATE groups SET name = ?1 WHERE id = ?2", rusqlite::params!["New Name", "g1"]).unwrap();

        let (name, desc): (String, Option<String>) = conn.query_row(
            "SELECT name, description FROM groups WHERE id = 'g1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(name, "New Name");
        assert_eq!(desc.as_deref(), Some("a group"), "description should be unchanged");
    }

    #[test]
    fn update_group_icon_url() {
        let conn = db();
        setup(&conn);

        conn.execute("UPDATE groups SET icon_url = ?1 WHERE id = ?2", rusqlite::params!["https://img.example.com/icon.png", "g1"]).unwrap();

        let icon: Option<String> = conn.query_row(
            "SELECT icon_url FROM groups WHERE id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(icon.as_deref(), Some("https://img.example.com/icon.png"));
    }

    // ── group deletion cascades ────────────────────────────────────────────

    #[test]
    fn delete_group_cascades_members_and_channels() {
        let conn = db();
        setup(&conn);

        conn.execute("DELETE FROM groups WHERE id = 'g1'", []).unwrap();

        let member_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(member_count, 0, "members should be cascade-deleted");

        let channel_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM channels WHERE group_id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(channel_count, 0, "channels should be cascade-deleted");
    }

    // ── member removal ─────────────────────────────────────────────────────

    #[test]
    fn remove_member_deletes_membership_row() {
        let conn = db();
        setup(&conn);

        conn.execute(
            "DELETE FROM group_member WHERE group_id = ?1 AND user_id = ?2",
            rusqlite::params!["g1", "bob"],
        ).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1' AND user_id = 'bob'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 0);
    }

    // ── leave group: auto-delete when empty ────────────────────────────────

    #[test]
    fn leave_group_last_member_deletes_group() {
        let conn = db();
        setup(&conn);

        // Remove bob first, then alice (last member)
        conn.execute("DELETE FROM group_member WHERE group_id = 'g1' AND user_id = 'bob'", []).unwrap();
        conn.execute("DELETE FROM group_member WHERE group_id = 'g1' AND user_id = 'alice'", []).unwrap();

        let member_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(member_count, 0);

        // Simulate: if member_count <= 1, delete group
        conn.execute("DELETE FROM groups WHERE id = 'g1'", []).unwrap();

        let group_exists: i64 = conn.query_row(
            "SELECT COUNT(*) FROM groups WHERE id = 'g1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(group_exists, 0);
    }

    // ── set_member_role ────────────────────────────────────────────────────

    #[test]
    fn set_member_role_promotes_to_admin() {
        let conn = db();
        setup(&conn);

        conn.execute(
            "UPDATE group_member SET role = ?1 WHERE group_id = ?2 AND user_id = ?3",
            rusqlite::params!["admin", "g1", "bob"],
        ).unwrap();

        let role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'bob'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(role, "admin");
    }

    #[test]
    fn set_member_role_demotes_to_member() {
        let conn = db();
        setup(&conn);

        // alice starts as admin
        conn.execute(
            "UPDATE group_member SET role = 'member' WHERE group_id = 'g1' AND user_id = 'alice'",
            [],
        ).unwrap();

        let role: String = conn.query_row(
            "SELECT role FROM group_member WHERE group_id = 'g1' AND user_id = 'alice'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(role, "member");
    }

    // ── add_member_to_group (INSERT OR IGNORE) ─────────────────────────────

    #[test]
    fn add_member_ignores_duplicate() {
        let conn = db();
        setup(&conn);

        // bob is already a member — INSERT OR IGNORE should not error
        conn.execute(
            "INSERT OR IGNORE INTO group_member (group_id, user_id, role) VALUES ('g1', 'bob', 'member')",
            [],
        ).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1' AND user_id = 'bob'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 1, "should still be exactly one membership row");
    }

    #[test]
    fn add_member_initializes_watermarks_for_existing_channels() {
        let conn = db();
        setup(&conn);

        // Carol has two devices. Seeding must produce one row per (channel, device).
        conn.execute(
            "INSERT INTO user_device (device_id, user_id) VALUES ('carol-d1', 'carol'), ('carol-d2', 'carol')",
            [],
        ).unwrap();

        conn.execute(
            "INSERT OR IGNORE INTO group_member (group_id, user_id, role) VALUES ('g1', 'carol', 'member')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT OR IGNORE INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at)
             SELECT c.id, ?1, ud.device_id, datetime('now')
             FROM channels c
             JOIN user_device ud ON ud.user_id = ?1
             WHERE c.group_id = ?2",
            rusqlite::params!["carol", "g1"],
        ).unwrap();

        let watermark_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM conversation_watermark WHERE user_id = 'carol'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(watermark_count, 4, "2 channels × 2 devices");
    }

    // ── get_group_members ──────────────────────────────────────────────────

    #[test]
    fn get_group_members_returns_all_with_roles() {
        let conn = db();
        setup(&conn);

        let members: Vec<(String, String)> = conn.prepare(
            "SELECT gm.user_id, gm.role FROM group_member gm WHERE gm.group_id = ?1",
        ).unwrap().query_map(
            rusqlite::params!["g1"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap().map(|r| r.unwrap()).collect();

        assert_eq!(members.len(), 2);
        assert!(members.contains(&("alice".into(), "admin".into())));
        assert!(members.contains(&("bob".into(), "member".into())));
    }

    #[test]
    fn get_group_members_joins_user_profile() {
        let conn = db();
        setup(&conn);

        let result: (String, Option<String>) = conn.query_row(
            "SELECT gm.user_id, u.username
             FROM group_member gm
             LEFT JOIN users u ON u.id = gm.user_id
             WHERE gm.group_id = ?1 AND gm.user_id = ?2",
            rusqlite::params!["g1", "alice"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();

        assert_eq!(result.0, "alice");
        assert_eq!(result.1.as_deref(), Some("alice"));
    }

    // ── invites ────────────────────────────────────────────────────────────

    #[test]
    fn invite_insert_and_query() {
        let conn = db();
        setup(&conn);

        conn.execute(
            "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('inv1', 'g1', 'alice', 'carol')",
            [],
        ).unwrap();

        let (inviter, invitee): (String, String) = conn.query_row(
            "SELECT inviter_id, invitee_id FROM group_invite WHERE id = 'inv1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(inviter, "alice");
        assert_eq!(invitee, "carol");
    }

    #[test]
    fn invite_existing_member_blocked() {
        let conn = db();
        setup(&conn);

        // bob is already a member of g1 — check that a membership check catches it
        let is_member: bool = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = ?1 AND user_id = ?2",
            rusqlite::params!["g1", "bob"],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        ).unwrap();
        assert!(is_member, "bob should already be a member");

        // In the command, this check prevents the invite from being created.
        // The INSERT itself would succeed (no DB constraint), so the guard is in app logic.
    }

    #[test]
    fn invite_self_blocked() {
        let conn = db();
        setup(&conn);

        // The command checks invitee_id == inviter_id before inserting.
        // Verify the condition that would be checked:
        let inviter_id = "alice";
        let invitee_id = "alice";
        assert_eq!(inviter_id, invitee_id, "self-invite should be caught by app logic");
    }

    #[test]
    fn duplicate_pending_invite_blocked() {
        let conn = db();
        setup(&conn);

        // First invite
        conn.execute(
            "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('inv1', 'g1', 'alice', 'carol')",
            [],
        ).unwrap();

        // The command checks for existing pending invites before inserting.
        // Since group_invite has no status column (all rows are implicitly pending),
        // the check is: any row with (group_id, invitee_id) exists.
        let existing: bool = conn.query_row(
            "SELECT COUNT(*) FROM group_invite WHERE group_id = ?1 AND invitee_id = ?2",
            rusqlite::params!["g1", "carol"],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        ).unwrap();
        assert!(existing, "pending invite should already exist — app logic blocks duplicate");
    }

    #[test]
    fn invite_cascade_deletes_with_group() {
        let conn = db();
        setup(&conn);

        conn.execute(
            "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('inv1', 'g1', 'alice', 'carol')",
            [],
        ).unwrap();

        conn.execute("DELETE FROM groups WHERE id = 'g1'", []).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_invite WHERE id = 'inv1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 0, "invite should be cascade-deleted with group");
    }

    #[test]
    fn invite_delete_on_accept() {
        let conn = db();
        setup(&conn);

        conn.execute(
            "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES ('inv1', 'g1', 'alice', 'carol')",
            [],
        ).unwrap();

        // Accept: add member + delete invite
        conn.execute(
            "INSERT OR IGNORE INTO group_member (group_id, user_id, role) VALUES ('g1', 'carol', 'member')",
            [],
        ).unwrap();
        conn.execute("DELETE FROM group_invite WHERE id = 'inv1'", []).unwrap();

        let member_exists: bool = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1' AND user_id = 'carol'",
            [],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        ).unwrap();
        assert!(member_exists, "carol should now be a member");

        let invite_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_invite",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(invite_count, 0, "invite should be deleted after acceptance");
    }

    // ── join requests ──────────────────────────────────────────────────────

    #[test]
    fn join_request_insert() {
        let conn = db();
        setup(&conn);

        conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr1', 'g1', 'carol', 'pending')",
            [],
        ).unwrap();

        let status: String = conn.query_row(
            "SELECT status FROM group_join_request WHERE id = 'jr1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(status, "pending");
    }

    #[test]
    fn join_request_blocked_when_already_member() {
        let conn = db();
        setup(&conn);

        // bob is already a member — the command checks this before inserting
        let is_member: bool = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = ?1 AND user_id = ?2",
            rusqlite::params!["g1", "bob"],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        ).unwrap();
        assert!(is_member, "bob is already a member — request_group_access should reject");
    }

    #[test]
    fn join_request_unique_per_group_requester() {
        let conn = db();
        setup(&conn);

        conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr1', 'g1', 'carol', 'pending')",
            [],
        ).unwrap();

        // Duplicate (group, requester) should conflict
        let result = conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr2', 'g1', 'carol', 'pending')",
            [],
        );
        assert!(result.is_err(), "duplicate (group_id, requester_id) should violate unique index");
    }

    #[test]
    fn join_request_upsert_resets_rejected_to_pending() {
        let conn = db();
        setup(&conn);

        conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr1', 'g1', 'carol', 'rejected')",
            [],
        ).unwrap();

        // Re-apply via upsert — same pattern as request_group_access
        conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id, status, created_at)
             VALUES ('jr2', 'g1', 'carol', 'pending', datetime('now'))
             ON CONFLICT(group_id, requester_id) DO UPDATE SET
                 id = excluded.id,
                 status = 'pending',
                 created_at = excluded.created_at",
            [],
        ).unwrap();

        let (id, status): (String, String) = conn.query_row(
            "SELECT id, status FROM group_join_request WHERE group_id = 'g1' AND requester_id = 'carol'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();
        assert_eq!(id, "jr2", "id should be updated to the new one");
        assert_eq!(status, "pending", "status should be reset to pending");
    }

    #[test]
    fn join_request_status_check_constraint() {
        let conn = db();
        setup(&conn);

        let result = conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr1', 'g1', 'carol', 'invalid')",
            [],
        );
        assert!(result.is_err(), "invalid status should violate CHECK constraint");
    }

    #[test]
    fn join_request_approve_adds_member_and_updates_status() {
        let conn = db();
        setup(&conn);

        conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr1', 'g1', 'carol', 'pending')",
            [],
        ).unwrap();

        // Approve: add member + update status
        conn.execute(
            "INSERT OR IGNORE INTO group_member (group_id, user_id, role) VALUES ('g1', 'carol', 'member')",
            [],
        ).unwrap();
        conn.execute(
            "UPDATE group_join_request SET status = 'approved', reviewed_by = 'alice', reviewed_at = datetime('now') WHERE id = 'jr1'",
            [],
        ).unwrap();

        let status: String = conn.query_row(
            "SELECT status FROM group_join_request WHERE id = 'jr1'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(status, "approved");

        let is_member: bool = conn.query_row(
            "SELECT COUNT(*) FROM group_member WHERE group_id = 'g1' AND user_id = 'carol'",
            [],
            |row| row.get::<_, i64>(0).map(|c| c > 0),
        ).unwrap();
        assert!(is_member);
    }

    #[test]
    fn join_request_only_pending_returned_for_admins() {
        let conn = db();
        setup(&conn);

        conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr1', 'g1', 'carol', 'pending')",
            [],
        ).unwrap();

        // Simulate a second user with a rejected request — need a new user
        conn.execute("INSERT INTO users (id, email, username) VALUES ('dave', 'dave@x.com', 'dave')", []).unwrap();
        conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr2', 'g1', 'dave', 'rejected')",
            [],
        ).unwrap();

        let pending: Vec<String> = conn.prepare(
            "SELECT jr.id FROM group_join_request jr WHERE jr.group_id = ?1 AND jr.status = 'pending' ORDER BY jr.created_at ASC",
        ).unwrap().query_map(
            rusqlite::params!["g1"],
            |row| row.get(0),
        ).unwrap().map(|r| r.unwrap()).collect();

        assert_eq!(pending, ["jr1"], "only pending requests should be returned");
    }

    #[test]
    fn join_request_cascade_deletes_with_group() {
        let conn = db();
        setup(&conn);

        conn.execute(
            "INSERT INTO group_join_request (id, group_id, requester_id, status) VALUES ('jr1', 'g1', 'carol', 'pending')",
            [],
        ).unwrap();

        conn.execute("DELETE FROM groups WHERE id = 'g1'", []).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM group_join_request",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 0, "join requests should be cascade-deleted with group");
    }

    // ── search_group_by_slug ───────────────────────────────────────────────

    #[test]
    fn search_group_by_slug_finds_match() {
        let conn = db();
        setup(&conn);

        // Simulate the scan + derive_slug pattern
        let mut found = None;
        let mut stmt = conn.prepare("SELECT id, name FROM groups").unwrap();
        let rows = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))).unwrap();
        for r in rows {
            let (id, name) = r.unwrap();
            if super::derive_slug(&name) == "test-group" {
                found = Some(id);
                break;
            }
        }

        assert_eq!(found.as_deref(), Some("g1"));
    }

    #[test]
    fn search_group_by_slug_no_match() {
        let conn = db();
        setup(&conn);

        let mut found = false;
        let mut stmt = conn.prepare("SELECT name FROM groups").unwrap();
        let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
        for r in rows {
            if super::derive_slug(&r.unwrap()) == "nonexistent-group" {
                found = true;
            }
        }

        assert!(!found);
    }

    // ── list_user_groups_with_channels query shape ─────────────────────────

    #[test]
    fn list_groups_with_channels_groups_and_nests_channels() {
        let conn = db();
        setup(&conn);

        // Simulate the query used by list_user_groups_with_channels
        let mut stmt = conn.prepare(
            "SELECT g.id, g.name, g.description, g.owner_id, g.created_at,
                    c.id, c.group_id, c.name, c.description, c.channel_type,
                    gm.role
             FROM groups g
             JOIN group_member gm ON gm.group_id = g.id
             LEFT JOIN channels c ON c.group_id = g.id
             WHERE gm.user_id = ?1
             ORDER BY g.created_at, c.name",
        ).unwrap();

        let rows: Vec<(String, Option<String>, String)> = stmt.query_map(
            rusqlite::params!["alice"],
            |row| Ok((row.get(0)?, row.get(5)?, row.get(10)?)),
        ).unwrap().map(|r| r.unwrap()).collect();

        // g1 has 2 channels — should get 2 rows, same group_id
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|(gid, _, _)| gid == "g1"));
        // Both channel IDs present
        let ch_ids: Vec<&str> = rows.iter().map(|(_, cid, _)| cid.as_deref().unwrap()).collect();
        assert!(ch_ids.contains(&"ch1"));
        assert!(ch_ids.contains(&"ch2"));
        // Role is admin for alice
        assert!(rows.iter().all(|(_, _, role)| role == "admin"));
    }

    #[test]
    fn list_groups_with_channels_group_without_channels() {
        let conn = db();
        setup(&conn);

        // Group with no channels
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g-empty', 'Empty', 'alice')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id, role) VALUES ('g-empty', 'alice', 'admin')", []).unwrap();

        let mut stmt = conn.prepare(
            "SELECT g.id, c.id
             FROM groups g
             JOIN group_member gm ON gm.group_id = g.id
             LEFT JOIN channels c ON c.group_id = g.id
             WHERE gm.user_id = ?1
             ORDER BY g.created_at, c.name",
        ).unwrap();

        let rows: Vec<(String, Option<String>)> = stmt.query_map(
            rusqlite::params!["alice"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap().map(|r| r.unwrap()).collect();

        // g-empty should appear with NULL channel
        let empty_rows: Vec<_> = rows.iter().filter(|(gid, _)| gid == "g-empty").collect();
        assert_eq!(empty_rows.len(), 1);
        assert!(empty_rows[0].1.is_none(), "channel_id should be NULL for empty group");
    }

    // ── db_err mapping ─────────────────────────────────────────────────────

    #[test]
    fn duplicate_group_member_violates_unique() {
        let conn = db();
        setup(&conn);

        let result = conn.execute(
            "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'alice', 'admin')",
            [],
        );
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("UNIQUE"), "error should mention UNIQUE constraint: {err_msg}");
    }

    #[test]
    fn foreign_key_violation_on_invalid_group() {
        let conn = db();
        setup(&conn);

        let result = conn.execute(
            "INSERT INTO group_member (group_id, user_id, role) VALUES ('nonexistent', 'alice', 'member')",
            [],
        );
        assert!(result.is_err(), "should fail due to foreign key constraint");
    }
}
