use serde::{Deserialize, Serialize};
use tauri::State;
use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

#[derive(Debug, Serialize, Deserialize)]
pub struct UserProfile {
    pub id: String,
    pub username: Option<String>,
    pub phone: Option<String>,
    pub avatar_url: Option<String>,
}

#[tauri::command]
pub async fn get_user_profile(
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Option<UserProfile>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT id, username, phone, avatar_url FROM users WHERE id = ?1",
        libsql::params![user_id],
    ).await?;

    if let Some(row) = rows.next().await? {
        Ok(Some(UserProfile {
            id: row.get(0)?,
            username: row.get(1)?,
            phone: row.get(2)?,
            avatar_url: row.get(3)?,
        }))
    } else {
        Ok(None)
    }
}

#[tauri::command]
pub async fn update_user_profile(
    user_id: String,
    username: Option<String>,
    phone: Option<String>,
    avatar_url: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    conn.execute(
        "UPDATE users SET username = COALESCE(?2, username), phone = COALESCE(?3, phone), avatar_url = COALESCE(?4, avatar_url) WHERE id = ?1",
        libsql::params![user_id, username, phone, avatar_url],
    ).await?;

    Ok(())
}

#[tauri::command]
pub async fn get_preferences(
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<String> {
    // Try local first (local-first: DB is open while user is signed in)
    {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            let prefs: Option<String> = db
                .conn()
                .query_row(
                    "SELECT preferences FROM preferences LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .ok();
            if let Some(p) = prefs {
                return Ok(p);
            }
        }
    }

    // Fall back to remote
    let conn = state.remote_db.conn().await?;
    let mut rows = conn.query(
        "SELECT preferences FROM user_preferences WHERE user_id = ?1",
        libsql::params![user_id],
    ).await?;
    if let Some(row) = rows.next().await? {
        let prefs: String = row.get(0)?;
        return Ok(prefs);
    }

    Ok("{}".to_string())
}

#[tauri::command]
pub async fn save_preferences(
    user_id: String,
    preferences_json: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    // Write to local first (best-effort, fast)
    {
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            let _ = db.conn().execute(
                "UPDATE preferences SET preferences = ?1, updated_at = datetime('now')",
                rusqlite::params![preferences_json.clone()],
            );
        }
    }

    // Write to remote
    let conn = state.remote_db.conn().await?;
    conn.execute(
        "INSERT INTO user_preferences (user_id, preferences, updated_at) VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(user_id) DO UPDATE SET preferences = ?2, updated_at = datetime('now')",
        libsql::params![user_id.clone(), preferences_json.clone()],
    ).await?;

    Ok(())
}

#[tauri::command]
pub async fn search_user_by_username(
    username: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Option<UserProfile>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT id, username, phone, avatar_url FROM users WHERE username = ?1 OR email = ?1",
        libsql::params![username],
    ).await?;

    if let Some(row) = rows.next().await? {
        Ok(Some(UserProfile {
            id: row.get(0)?,
            username: row.get(1)?,
            phone: row.get(2)?,
            avatar_url: row.get(3)?,
        }))
    } else {
        Ok(None)
    }
}
