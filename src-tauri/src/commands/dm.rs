use serde::{Deserialize, Serialize};
use tauri::State;
use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;

#[derive(Debug, Serialize, Deserialize)]
pub struct DmChannel {
    pub id: String,
    pub created_by: String,
    pub created_at: String,
    pub members: Vec<DmChannelMember>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DmChannelMember {
    pub user_id: String,
    pub username: Option<String>,
    pub added_by: String,
    pub added_at: String,
}

/// Fetch members of a DM channel from the remote DB.
async fn fetch_dm_members(
    conn: &libsql::Connection,
    dm_channel_id: &str,
) -> Result<Vec<DmChannelMember>> {
    let mut rows = conn.query(
        "SELECT dcm.user_id, u.username, dcm.added_by, dcm.added_at
         FROM dm_channel_member dcm
         LEFT JOIN users u ON u.id = dcm.user_id
         WHERE dcm.dm_channel_id = ?1",
        libsql::params![dm_channel_id],
    ).await?;

    let mut members = Vec::new();
    while let Some(row) = rows.next().await? {
        members.push(DmChannelMember {
            user_id: row.get(0)?,
            username: row.get(1)?,
            added_by: row.get(2)?,
            added_at: row.get(3)?,
        });
    }
    Ok(members)
}


#[tauri::command]
pub async fn create_dm_channel(
    creator_id: String,
    member_ids: Vec<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<DmChannel> {
    let conn = state.remote_db.conn().await?;
    let id = Ulid::new().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        "INSERT INTO dm_channel (id, created_by, created_at) VALUES (?1, ?2, ?3)",
        libsql::params![id.clone(), creator_id.clone(), now.clone()],
    ).await?;

    // Add creator as first member
    conn.execute(
        "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at)
         VALUES (?1, ?2, ?3, ?4)",
        libsql::params![id.clone(), creator_id.clone(), creator_id.clone(), now.clone()],
    ).await?;

    // Add all other members
    for member_id in &member_ids {
        if member_id == &creator_id {
            continue;
        }
        conn.execute(
            "INSERT OR IGNORE INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at)
             VALUES (?1, ?2, ?3, ?4)",
            libsql::params![id.clone(), member_id.clone(), creator_id.clone(), now.clone()],
        ).await?;
    }

    let members = fetch_dm_members(&conn, &id).await?;

    // Initialize watermark rows for all members so envelope cleanup can proceed.
    for member in &members {
        if let Err(e) = conn.execute(
            "INSERT OR IGNORE INTO conversation_watermark (conversation_id, user_id, last_fetched_at) VALUES (?1, ?2, datetime('now'))",
            libsql::params![id.clone(), member.user_id.clone()],
        ).await {
            eprintln!("[watermark] create_dm_channel: watermark init failed for {}: {e}", member.user_id);
        }
    }

    // Initialise the MLS group for this DM (creator becomes the sole member).
    match crate::commands::mls::init_mls_group(state.inner(), &id, &creator_id).await {
        Ok(()) => {}
        Err(e) => eprintln!("[mls] create_dm_channel: mls group init failed (non-fatal): {e}"),
    }

    // Add every non-creator member to the MLS group so they receive a Welcome
    // and can send/decrypt immediately once they call poll_mls_welcomes.
    for member in members.iter().filter(|m| m.user_id != creator_id) {
        match crate::commands::mls::add_member_mls_inner(
            state.inner(), &id, &member.user_id, &creator_id,
        ).await {
            Ok(()) => {}
            Err(e) => eprintln!("[mls] create_dm_channel: add_member for {}: {e}", member.user_id),
        }
    }

    Ok(DmChannel {
        id,
        created_by: creator_id,
        created_at: now,
        members,
    })
}

#[tauri::command]
pub async fn list_dm_channels(
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<DmChannel>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT dc.id, dc.created_by, dc.created_at
         FROM dm_channel dc
         JOIN dm_channel_member dcm ON dcm.dm_channel_id = dc.id
         WHERE dcm.user_id = ?1",
        libsql::params![user_id],
    ).await?;

    let mut channels = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let created_by: String = row.get(1)?;
        let created_at: String = row.get(2)?;

        let members = fetch_dm_members(&conn, &id).await?;

        channels.push(DmChannel {
            id,
            created_by,
            created_at,
            members,
        });
    }

    Ok(channels)
}

#[tauri::command]
pub async fn get_dm_channel(
    dm_channel_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<DmChannel> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT id, created_by, created_at FROM dm_channel WHERE id = ?1",
        libsql::params![dm_channel_id.clone()],
    ).await?;

    let (id, created_by, created_at) = if let Some(row) = rows.next().await? {
        (row.get::<String>(0)?, row.get::<String>(1)?, row.get::<String>(2)?)
    } else {
        return Err(Error::Other(anyhow::anyhow!("DM channel not found: {dm_channel_id}")));
    };

    let members = fetch_dm_members(&conn, &id).await?;

    Ok(DmChannel { id, created_by, created_at, members })
}

#[tauri::command]
pub async fn add_user_to_dm_channel(
    dm_channel_id: String,
    user_id: String,
    added_by: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        "INSERT OR IGNORE INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at)
         VALUES (?1, ?2, ?3, ?4)",
        libsql::params![dm_channel_id.clone(), user_id.clone(), added_by.clone(), now],
    ).await?;

    // Initialize watermark for the new member so pre-join messages don't
    // block envelope cleanup indefinitely.
    if let Err(e) = conn.execute(
        "INSERT OR IGNORE INTO conversation_watermark (conversation_id, user_id, last_fetched_at) VALUES (?1, ?2, datetime('now'))",
        libsql::params![dm_channel_id.clone(), user_id.clone()],
    ).await {
        eprintln!("[watermark] add_user_to_dm_channel: watermark init failed: {e}");
    }

    // Add new member to the MLS group so they can decrypt future messages.
    match crate::commands::mls::add_member_mls_inner(
        state.inner(), &dm_channel_id, &user_id, &added_by,
    ).await {
        Ok(()) => {}
        Err(e) => eprintln!("[mls] add_user_to_dm_channel: add_member: {e}"),
    }

    Ok(())
}

#[tauri::command]
pub async fn remove_user_from_dm_channel(
    dm_channel_id: String,
    user_id: String,
    requester_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Only the channel creator or the user themselves can remove
    let mut rows = conn.query(
        "SELECT created_by FROM dm_channel WHERE id = ?1",
        libsql::params![dm_channel_id.clone()],
    ).await?;

    let creator_id: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Err(Error::Other(anyhow::anyhow!("DM channel not found")));
    };

    if requester_id != creator_id && requester_id != user_id {
        return Err(Error::Other(anyhow::anyhow!(
            "only the channel creator or the user themselves can remove a member"
        )));
    }

    conn.execute(
        "DELETE FROM dm_channel_member WHERE dm_channel_id = ?1 AND user_id = ?2",
        libsql::params![dm_channel_id, user_id],
    ).await?;

    Ok(())
}

#[tauri::command]
pub async fn leave_dm_channel(
    dm_channel_id: String,
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    conn.execute(
        "DELETE FROM dm_channel_member WHERE dm_channel_id = ?1 AND user_id = ?2",
        libsql::params![dm_channel_id.clone(), user_id],
    ).await?;

    // If no members remain, clean up the channel and all associated data
    let mut rows = conn.query(
        "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = ?1",
        libsql::params![dm_channel_id.clone()],
    ).await?;

    let remaining: i64 = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        0
    };

    if remaining == 0 {
        conn.execute("DELETE FROM message_envelope WHERE conversation_id = ?1", libsql::params![dm_channel_id.clone()]).await?;
        conn.execute("DELETE FROM dm_channel WHERE id = ?1", libsql::params![dm_channel_id]).await?;
    }

    Ok(())
}
