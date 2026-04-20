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
    // Remote is authoritative so changes made on another device are visible
    // immediately on this one. The local row is a last-known-good cache used
    // only when the remote read fails (offline / flaky connection).
    match fetch_remote_preferences(&state, &user_id).await {
        Ok(Some(prefs)) => {
            upsert_local_preferences(state.inner(), &prefs).await;
            Ok(prefs)
        }
        Ok(None) => Ok("{}".to_string()),
        Err(e) => {
            eprintln!("[preferences] remote fetch failed ({e}); falling back to local cache");
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
            Ok("{}".to_string())
        }
    }
}

async fn fetch_remote_preferences(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<Option<String>> {
    let conn = state.remote_db.conn().await?;
    let mut rows = conn
        .query(
            "SELECT preferences FROM user_preferences WHERE user_id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    if let Some(row) = rows.next().await? {
        let prefs: String = row.get(0)?;
        return Ok(Some(prefs));
    }
    Ok(None)
}

#[tauri::command]
pub async fn save_preferences(
    user_id: String,
    preferences_json: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    upsert_local_preferences(state.inner(), &preferences_json).await;

    let conn = state.remote_db.conn().await?;
    conn.execute(
        "INSERT INTO user_preferences (user_id, preferences, updated_at) VALUES (?1, ?2, datetime('now'))
         ON CONFLICT(user_id) DO UPDATE SET preferences = ?2, updated_at = datetime('now')",
        libsql::params![user_id.clone(), preferences_json.clone()],
    ).await?;

    Ok(())
}

async fn upsert_local_preferences(state: &Arc<AppState>, preferences_json: &str) {
    let guard = state.local_db.lock().await;
    if let Some(db) = guard.as_ref() {
        let _ = db.conn().execute(
            "INSERT OR IGNORE INTO preferences (preferences) VALUES (?1)",
            rusqlite::params![preferences_json],
        );
        let _ = db.conn().execute(
            "UPDATE preferences SET preferences = ?1, updated_at = datetime('now')",
            rusqlite::params![preferences_json],
        );
    }
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

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    const BASELINE: &str = include_str!("../db/migrations/000000_baseline.sql");

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn.execute_batch(BASELINE).unwrap();
        conn
    }

    fn setup(conn: &Connection) {
        conn.execute(
            "INSERT INTO users (id, email, username, phone, avatar_url) VALUES ('alice', 'alice@x.com', 'alice', '555-0001', 'https://img.example.com/alice.png')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO users (id, email, username) VALUES ('bob', 'bob@x.com', 'bob')",
            [],
        ).unwrap();
    }

    // ── get_user_profile ───────────────────────────────────────────────────

    #[test]
    fn get_user_profile_returns_existing_user() {
        let conn = db();
        setup(&conn);

        let (id, username, phone, avatar): (String, Option<String>, Option<String>, Option<String>) =
            conn.query_row(
                "SELECT id, username, phone, avatar_url FROM users WHERE id = ?1",
                rusqlite::params!["alice"],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            ).unwrap();

        assert_eq!(id, "alice");
        assert_eq!(username.as_deref(), Some("alice"));
        assert_eq!(phone.as_deref(), Some("555-0001"));
        assert_eq!(avatar.as_deref(), Some("https://img.example.com/alice.png"));
    }

    #[test]
    fn get_user_profile_returns_none_for_missing_user() {
        let conn = db();
        setup(&conn);

        let result = conn.query_row(
            "SELECT id FROM users WHERE id = ?1",
            rusqlite::params!["nonexistent"],
            |row| row.get::<_, String>(0),
        );
        assert!(result.is_err());
    }

    #[test]
    fn get_user_profile_nullable_fields() {
        let conn = db();
        setup(&conn);

        let (phone, avatar): (Option<String>, Option<String>) = conn.query_row(
            "SELECT phone, avatar_url FROM users WHERE id = 'bob'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();

        assert!(phone.is_none());
        assert!(avatar.is_none());
    }

    // ── update_user_profile (COALESCE) ─────────────────────────────────────

    #[test]
    fn update_profile_only_username() {
        let conn = db();
        setup(&conn);

        conn.execute(
            "UPDATE users SET username = COALESCE(?2, username), phone = COALESCE(?3, phone), avatar_url = COALESCE(?4, avatar_url) WHERE id = ?1",
            rusqlite::params!["alice", Some("alice_new"), None::<String>, None::<String>],
        ).unwrap();

        let (username, phone, avatar): (Option<String>, Option<String>, Option<String>) =
            conn.query_row(
                "SELECT username, phone, avatar_url FROM users WHERE id = 'alice'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).unwrap();

        assert_eq!(username.as_deref(), Some("alice_new"), "username should be updated");
        assert_eq!(phone.as_deref(), Some("555-0001"), "phone should be preserved");
        assert_eq!(avatar.as_deref(), Some("https://img.example.com/alice.png"), "avatar should be preserved");
    }

    #[test]
    fn update_profile_only_phone() {
        let conn = db();
        setup(&conn);

        conn.execute(
            "UPDATE users SET username = COALESCE(?2, username), phone = COALESCE(?3, phone), avatar_url = COALESCE(?4, avatar_url) WHERE id = ?1",
            rusqlite::params!["alice", None::<String>, Some("555-9999"), None::<String>],
        ).unwrap();

        let (username, phone): (Option<String>, Option<String>) = conn.query_row(
            "SELECT username, phone FROM users WHERE id = 'alice'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ).unwrap();

        assert_eq!(username.as_deref(), Some("alice"), "username preserved");
        assert_eq!(phone.as_deref(), Some("555-9999"), "phone updated");
    }

    #[test]
    fn update_profile_all_fields() {
        let conn = db();
        setup(&conn);

        conn.execute(
            "UPDATE users SET username = COALESCE(?2, username), phone = COALESCE(?3, phone), avatar_url = COALESCE(?4, avatar_url) WHERE id = ?1",
            rusqlite::params!["alice", Some("new_alice"), Some("555-0000"), Some("https://new-avatar.png")],
        ).unwrap();

        let (username, phone, avatar): (Option<String>, Option<String>, Option<String>) =
            conn.query_row(
                "SELECT username, phone, avatar_url FROM users WHERE id = 'alice'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            ).unwrap();

        assert_eq!(username.as_deref(), Some("new_alice"));
        assert_eq!(phone.as_deref(), Some("555-0000"));
        assert_eq!(avatar.as_deref(), Some("https://new-avatar.png"));
    }

    #[test]
    fn update_profile_no_fields_is_noop() {
        let conn = db();
        setup(&conn);

        conn.execute(
            "UPDATE users SET username = COALESCE(?2, username), phone = COALESCE(?3, phone), avatar_url = COALESCE(?4, avatar_url) WHERE id = ?1",
            rusqlite::params!["alice", None::<String>, None::<String>, None::<String>],
        ).unwrap();

        let username: Option<String> = conn.query_row(
            "SELECT username FROM users WHERE id = 'alice'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(username.as_deref(), Some("alice"), "should be unchanged");
    }

    // ── search_user_by_username ────────────────────────────────────────────

    #[test]
    fn search_by_username() {
        let conn = db();
        setup(&conn);

        let id: String = conn.query_row(
            "SELECT id FROM users WHERE username = ?1 OR email = ?1",
            rusqlite::params!["bob"],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(id, "bob");
    }

    #[test]
    fn search_by_email() {
        let conn = db();
        setup(&conn);

        let id: String = conn.query_row(
            "SELECT id FROM users WHERE username = ?1 OR email = ?1",
            rusqlite::params!["alice@x.com"],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(id, "alice");
    }

    #[test]
    fn search_no_match() {
        let conn = db();
        setup(&conn);

        let result = conn.query_row(
            "SELECT id FROM users WHERE username = ?1 OR email = ?1",
            rusqlite::params!["nobody"],
            |row| row.get::<_, String>(0),
        );
        assert!(result.is_err());
    }

    // ── preferences upsert ─────────────────────────────────────────────────

    #[test]
    fn preferences_upsert_insert_then_update() {
        let conn = db();
        setup(&conn);

        // First save — INSERT
        conn.execute(
            "INSERT INTO user_preferences (user_id, preferences, updated_at) VALUES (?1, ?2, datetime('now'))
             ON CONFLICT(user_id) DO UPDATE SET preferences = ?2, updated_at = datetime('now')",
            rusqlite::params!["alice", r#"{"theme":"dark"}"#],
        ).unwrap();

        let prefs: String = conn.query_row(
            "SELECT preferences FROM user_preferences WHERE user_id = 'alice'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(prefs, r#"{"theme":"dark"}"#);

        // Second save — UPDATE via ON CONFLICT
        conn.execute(
            "INSERT INTO user_preferences (user_id, preferences, updated_at) VALUES (?1, ?2, datetime('now'))
             ON CONFLICT(user_id) DO UPDATE SET preferences = ?2, updated_at = datetime('now')",
            rusqlite::params!["alice", r#"{"theme":"light","font_size":14}"#],
        ).unwrap();

        let prefs: String = conn.query_row(
            "SELECT preferences FROM user_preferences WHERE user_id = 'alice'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(prefs, r#"{"theme":"light","font_size":14}"#);

        // Still only one row
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM user_preferences WHERE user_id = 'alice'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn preferences_cascade_deletes_with_user() {
        let conn = db();
        setup(&conn);

        conn.execute(
            "INSERT INTO user_preferences (user_id, preferences) VALUES ('alice', '{}')",
            [],
        ).unwrap();

        conn.execute("DELETE FROM users WHERE id = 'alice'", []).unwrap();

        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM user_preferences WHERE user_id = 'alice'",
            [],
            |row| row.get(0),
        ).unwrap();
        assert_eq!(count, 0, "preferences should be cascade-deleted with user");
    }

    // ── email uniqueness ───────────────────────────────────────────────────

    #[test]
    fn duplicate_email_rejected() {
        let conn = db();
        setup(&conn);

        let result = conn.execute(
            "INSERT INTO users (id, email, username) VALUES ('carol', 'alice@x.com', 'carol')",
            [],
        );
        assert!(result.is_err(), "duplicate email should violate UNIQUE constraint");
    }
}
