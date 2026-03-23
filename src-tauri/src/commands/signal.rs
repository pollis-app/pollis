use serde::{Deserialize, Serialize};
use tauri::State;
use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

#[derive(Debug, Serialize, Deserialize)]
pub struct PreKeyBundle {
    pub user_id: String,
    pub identity_key: String,
    pub signed_prekey_id: u32,
    pub signed_prekey: String,
    pub signed_prekey_sig: String,
    pub one_time_prekey_id: Option<u32>,
    pub one_time_prekey: Option<String>,
}

#[tauri::command]
pub async fn get_prekey_bundle(
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<PreKeyBundle> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT u.identity_key, s.key_id, s.public_key, s.signature
         FROM users u
         JOIN signed_prekey s ON s.user_id = u.id
         WHERE u.id = ?1",
        libsql::params![user_id.clone()],
    ).await?;

    let row = rows.next().await?.ok_or_else(|| crate::error::Error::Other(
        anyhow::anyhow!("user not found: {user_id}")
    ))?;

    let identity_key: String = row.get(0)?;
    let spk_id: i64 = row.get(1)?;
    let spk_pub: String = row.get(2)?;
    let spk_sig: String = row.get(3)?;

    // Try to claim a one-time pre-key
    let mut opk_rows = conn.query(
        "SELECT key_id, public_key FROM one_time_prekey
         WHERE user_id = ?1 AND used = 0
         LIMIT 1",
        libsql::params![user_id.clone()],
    ).await?;

    let (opk_id, opk_pub) = if let Some(opk_row) = opk_rows.next().await? {
        let id: i64 = opk_row.get(0)?;
        let pub_key: String = opk_row.get(1)?;

        conn.execute(
            "UPDATE one_time_prekey SET used = 1 WHERE user_id = ?1 AND key_id = ?2",
            libsql::params![user_id.clone(), id],
        ).await?;

        (Some(id as u32), Some(pub_key))
    } else {
        (None, None)
    };

    Ok(PreKeyBundle {
        user_id,
        identity_key,
        signed_prekey_id: spk_id as u32,
        signed_prekey: spk_pub,
        signed_prekey_sig: spk_sig,
        one_time_prekey_id: opk_id,
        one_time_prekey: opk_pub,
    })
}

#[tauri::command]
pub async fn rotate_signed_prekey(
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let identity = crate::signal::identity::IdentityKey::load().await?
        .ok_or(crate::error::Error::NotInitialized)?;

    // Get current max spk_id
    let db = state.local_db.lock().await;
    let max_id: u32 = db.conn().query_row(
        "SELECT COALESCE(MAX(id), 0) FROM signed_prekey",
        [],
        |row| row.get::<_, i64>(0),
    ).unwrap_or(0) as u32;

    let new_id = max_id + 1;
    let (spk_pub, spk_sig) = crate::signal::identity::generate_signed_prekey(new_id, &identity).await?;

    db.conn().execute(
        "INSERT INTO signed_prekey (id, public_key, signature) VALUES (?1, ?2, ?3)",
        rusqlite::params![new_id, spk_pub.clone(), spk_sig.clone()],
    )?;
    drop(db);

    let conn = state.remote_db.conn().await?;
    conn.execute(
        "INSERT OR REPLACE INTO signed_prekey (user_id, key_id, public_key, signature) VALUES (?1, ?2, ?3, ?4)",
        libsql::params![user_id, new_id as i64, hex::encode(&spk_pub), hex::encode(&spk_sig)],
    ).await?;

    Ok(())
}

#[tauri::command]
pub async fn replenish_one_time_prekeys(
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<u32> {
    let db = state.local_db.lock().await;
    let max_id: u32 = db.conn().query_row(
        "SELECT COALESCE(MAX(id), 0) FROM one_time_prekey",
        [],
        |row| row.get::<_, i64>(0),
    ).unwrap_or(0) as u32;

    let new_opks = crate::signal::identity::generate_one_time_prekeys(max_id + 1, 50).await?;
    let count = new_opks.len() as u32;

    for (id, pub_key) in &new_opks {
        db.conn().execute(
            "INSERT INTO one_time_prekey (id, public_key) VALUES (?1, ?2)",
            rusqlite::params![id, pub_key],
        )?;
    }
    drop(db);

    let conn = state.remote_db.conn().await?;
    let tx = conn.transaction().await?;
    for (id, pub_key) in &new_opks {
        tx.execute(
            "INSERT INTO one_time_prekey (user_id, key_id, public_key) VALUES (?1, ?2, ?3)",
            libsql::params![user_id.clone(), *id as i64, hex::encode(pub_key)],
        ).await?;
    }
    tx.commit().await?;

    Ok(count)
}
