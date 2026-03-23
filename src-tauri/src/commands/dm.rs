use serde::{Deserialize, Serialize};
use tauri::State;
use std::sync::Arc;
use ulid::Ulid;

use crate::error::{Error, Result};
use crate::state::AppState;
use crate::signal::group::SenderKeyState;
use crate::signal::session;
use crate::signal::crypto;

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

/// Distribute sender keys from all current members to a newly added user.
async fn distribute_all_keys_to_new_member(
    state: &AppState,
    dm_channel_id: &str,
    new_user_id: &str,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Get new user's identity key and SPK
    let mut rows = conn.query(
        "SELECT u.identity_key,
                (SELECT spk.public_key FROM signed_prekey spk WHERE spk.user_id = u.id ORDER BY spk.key_id DESC LIMIT 1),
                (SELECT spk.key_id FROM signed_prekey spk WHERE spk.user_id = u.id ORDER BY spk.key_id DESC LIMIT 1)
         FROM users u WHERE u.id = ?1",
        libsql::params![new_user_id],
    ).await?;

    let (new_ik_hex, new_spk_hex, new_spk_id): (String, String, i64) =
        if let Some(row) = rows.next().await? {
            let ik: String = row.get(0)?;
            let spk: Option<String> = row.get(1)?;
            let spk_id: Option<i64> = row.get(2)?;
            match (spk, spk_id) {
                (Some(s), Some(id)) => (ik, s, id),
                _ => return Ok(()),
            }
        } else {
            return Ok(());
        };

    let new_ik_bytes = match hex::decode(&new_ik_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return Ok(()),
    };

    let new_spk_bytes = match hex::decode(&new_spk_hex) {
        Ok(b) if b.len() == 32 => {
            let mut arr = [0u8; 32];
            arr.copy_from_slice(&b);
            arr
        }
        _ => return Ok(()),
    };

    // Get all current members (excluding the new member) and distribute their sender keys
    let mut member_rows = conn.query(
        "SELECT user_id FROM dm_channel_member WHERE dm_channel_id = ?1 AND user_id != ?2",
        libsql::params![dm_channel_id, new_user_id],
    ).await?;

    let mut member_ids = Vec::new();
    while let Some(row) = member_rows.next().await? {
        let uid: String = row.get(0)?;
        member_ids.push(uid);
    }

    let local = state.local_db.lock().await;
    for member_id in &member_ids {
        let member_state = session::load_sender_key(local.conn(), dm_channel_id, member_id);
        let member_state = match member_state {
            Ok(Some(s)) => s,
            _ => continue,
        };

        let (encrypted_state, ephemeral_key) = match crypto::encrypt_sender_key_for_recipient(
            &member_state,
            &new_ik_bytes,
            &new_spk_bytes,
        ) {
            Ok(r) => r,
            Err(_) => continue,
        };

        let dist_id = Ulid::new().to_string();
        let _ = conn.execute(
            "INSERT OR REPLACE INTO sender_key_dist
             (id, channel_id, sender_id, recipient_id, encrypted_state, ephemeral_key, spk_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            libsql::params![
                dist_id,
                dm_channel_id,
                member_id.clone(),
                new_user_id,
                encrypted_state,
                ephemeral_key,
                new_spk_id,
            ],
        ).await;
    }

    Ok(())
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

    // Create and distribute a fresh sender key for the creator to all members
    let creator_state = SenderKeyState::new();
    {
        let db = state.local_db.lock().await;
        session::save_sender_key(db.conn(), &id, &creator_id, &creator_state)?;
    }

    // Distribute creator's key to all members
    let remote_conn = state.remote_db.conn().await?;
    let all_members: Vec<String> = {
        let mut ids = member_ids.clone();
        if !ids.contains(&creator_id) {
            ids.push(creator_id.clone());
        }
        ids
    };

    for member_id in &all_members {
        if member_id == &creator_id {
            continue;
        }

        let mut key_rows = remote_conn.query(
            "SELECT u.identity_key,
                    (SELECT spk.public_key FROM signed_prekey spk WHERE spk.user_id = u.id ORDER BY spk.key_id DESC LIMIT 1),
                    (SELECT spk.key_id FROM signed_prekey spk WHERE spk.user_id = u.id ORDER BY spk.key_id DESC LIMIT 1)
             FROM users u WHERE u.id = ?1",
            libsql::params![member_id.clone()],
        ).await?;

        if let Some(row) = key_rows.next().await? {
            let ik_hex: String = row.get(0)?;
            let spk_hex: Option<String> = row.get(1)?;
            let spk_id: Option<i64> = row.get(2)?;

            if let (Some(spk_hex), Some(spk_id)) = (spk_hex, spk_id) {
                let ik_ok = hex::decode(&ik_hex).ok().filter(|b| b.len() == 32);
                let spk_ok = hex::decode(&spk_hex).ok().filter(|b| b.len() == 32);

                if let (Some(ik_b), Some(spk_b)) = (ik_ok, spk_ok) {
                    let mut ik_arr = [0u8; 32];
                    let mut spk_arr = [0u8; 32];
                    ik_arr.copy_from_slice(&ik_b);
                    spk_arr.copy_from_slice(&spk_b);

                    if let Ok((encrypted_state, ephemeral_key)) = crypto::encrypt_sender_key_for_recipient(
                        &creator_state,
                        &ik_arr,
                        &spk_arr,
                    ) {
                        let dist_id = Ulid::new().to_string();
                        let _ = remote_conn.execute(
                            "INSERT OR REPLACE INTO sender_key_dist
                             (id, channel_id, sender_id, recipient_id, encrypted_state, ephemeral_key, spk_id)
                             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            libsql::params![
                                dist_id,
                                id.clone(),
                                creator_id.clone(),
                                member_id.clone(),
                                encrypted_state,
                                ephemeral_key,
                                spk_id,
                            ],
                        ).await;
                    }
                }
            }
        }
    }

    let members = fetch_dm_members(&conn, &id).await?;

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

    // Distribute all current members' sender keys to the new user
    distribute_all_keys_to_new_member(&state, &dm_channel_id, &user_id).await?;

    // Create a new sender key for the new user and distribute to all existing members
    let new_user_state = SenderKeyState::new();
    {
        let db = state.local_db.lock().await;
        session::save_sender_key(db.conn(), &dm_channel_id, &user_id, &new_user_state)?;
    }

    // Distribute the new user's key to all other members
    let mut member_rows = conn.query(
        "SELECT u.id, u.identity_key,
                (SELECT spk.public_key FROM signed_prekey spk WHERE spk.user_id = u.id ORDER BY spk.key_id DESC LIMIT 1),
                (SELECT spk.key_id FROM signed_prekey spk WHERE spk.user_id = u.id ORDER BY spk.key_id DESC LIMIT 1)
         FROM dm_channel_member dcm
         JOIN users u ON u.id = dcm.user_id
         WHERE dcm.dm_channel_id = ?1 AND dcm.user_id != ?2
           AND u.identity_key IS NOT NULL",
        libsql::params![dm_channel_id.clone(), user_id.clone()],
    ).await?;

    while let Some(row) = member_rows.next().await? {
        let member_id: String = row.get(0)?;
        let ik_hex: String = row.get(1)?;
        let spk_hex: Option<String> = row.get(2)?;
        let spk_id: Option<i64> = row.get(3)?;

        let spk_hex = match spk_hex {
            Some(s) => s,
            None => continue,
        };
        let spk_id = match spk_id {
            Some(id) => id,
            None => continue,
        };

        let ik_bytes = match hex::decode(&ik_hex).ok().filter(|b| b.len() == 32) {
            Some(b) => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&b);
                arr
            }
            None => continue,
        };

        let spk_bytes = match hex::decode(&spk_hex).ok().filter(|b| b.len() == 32) {
            Some(b) => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&b);
                arr
            }
            None => continue,
        };

        if let Ok((encrypted_state, ephemeral_key)) = crypto::encrypt_sender_key_for_recipient(
            &new_user_state,
            &ik_bytes,
            &spk_bytes,
        ) {
            let dist_id = Ulid::new().to_string();
            let _ = conn.execute(
                "INSERT OR REPLACE INTO sender_key_dist
                 (id, channel_id, sender_id, recipient_id, encrypted_state, ephemeral_key, spk_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                libsql::params![
                    dist_id,
                    dm_channel_id.clone(),
                    user_id.clone(),
                    member_id,
                    encrypted_state,
                    ephemeral_key,
                    spk_id,
                ],
            ).await;
        }
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
        libsql::params![dm_channel_id, user_id],
    ).await?;

    Ok(())
}
