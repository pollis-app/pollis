use serde::{Deserialize, Serialize};

use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

#[derive(Debug, Serialize, Deserialize)]
pub struct UserProfile {
    pub id: String,
    pub username: Option<String>,
    pub preferred_name: Option<String>,
    pub avatar_url: Option<String>,
}

pub async fn get_user_profile(
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Option<UserProfile>> {
    Ok(
        crate::commands::ds_reads::users(state, vec![user_id], None)
            .await?
            .into_iter()
            .next()
            .map(profile_from_wire),
    )
}

/// Wire to domain. No `phone` (#987): nothing renders it and it is the most
/// re-identifying field on the row, so it never crosses the boundary.
fn profile_from_wire(u: pollis_api::directory::UserWire) -> UserProfile {
    UserProfile {
        id: u.id,
        username: Some(u.username),
        preferred_name: u.preferred_name,
        avatar_url: u.avatar_url,
    }
}

pub async fn update_user_profile(
    user_id: String,
    username: Option<String>,
    preferred_name: Option<String>,
    phone: Option<String>,
    avatar_url: Option<String>,
    state: &Arc<AppState>,
) -> Result<()> {
    let body = pollis_api::profile::UpdateProfileBody {
        user_id: user_id.clone(),
        username: username.clone(),
        preferred_name,
        phone,
        avatar_url,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Our username is cached in the accounts index (read by `get_session`) and in
    // `user_cache` (sender names), neither of which consults the DS, so both must
    // follow the rename.
    if let Some(username) = username {
        if let Err(e) = crate::accounts::rename_account(&user_id, &username) {
            crate::diag!("[profile] accounts index not updated after rename: {e}");
        }
        let guard = state.local_db.lock().await;
        if let Some(db) = guard.as_ref() {
            if let Err(e) = db.conn().execute(
                "INSERT INTO user_cache (id, username, updated_at) VALUES (?1, ?2, datetime('now'))
                 ON CONFLICT(id) DO UPDATE SET username = ?2, updated_at = datetime('now')",
                rusqlite::params![user_id, username],
            ) {
                crate::diag!("[profile] user_cache not updated after rename: {e}");
            }
        }
    }

    Ok(())
}

pub async fn get_preferences(
    user_id: String,
    state: &Arc<AppState>,
) -> Result<String> {
    // Remote is authoritative so other devices' changes show at once; the local row
    // is a last-known-good cache for when the remote read fails.
    match fetch_remote_preferences(state, &user_id).await {
        Ok(Some(prefs)) => {
            upsert_local_preferences(state, &prefs).await;
            Ok(prefs)
        }
        Ok(None) => Ok("{}".to_string()),
        Err(e) => {
            crate::diag!("[preferences] remote fetch failed ({e}); falling back to local cache");
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
    Ok(crate::commands::ds_reads::bootstrap(state, user_id)
        .await?
        .preferences)
}

pub async fn save_preferences(
    user_id: String,
    preferences_json: String,
    state: &Arc<AppState>,
) -> Result<()> {
    upsert_local_preferences(state, &preferences_json).await;

    let body = pollis_api::profile::SavePreferencesBody {
        user_id,
        preferences: preferences_json,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

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

/// Resolve one person from a username or an email.
///
/// The DS matches an identifier containing `@` against `users.email` only and
/// anything else against `users.username` only, and refuses usernames containing
/// `@` (`000021`), so at most one row answers.
pub async fn search_user_by_username(
    username: String,
    state: &Arc<AppState>,
) -> Result<Option<UserProfile>> {
    Ok(
        crate::commands::ds_reads::users(state, Vec::new(), Some(username))
            .await?
            .into_iter()
            .next()
            .map(profile_from_wire),
    )
}

// Schema-constraint tests (CHECK, UNIQUE, PRIMARY KEY, ON DELETE CASCADE) against
// the shipped remote schema.
#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        // The shipped schema: baseline plus every numbered migration, in apply order.
        for sql in pollis_schema::main_scripts() {
            conn.execute_batch(sql).unwrap();
        }
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

    // The DS lookup (`directory::user_by_identifier`) relies on this schema contract:
    // an identifier goes to exactly one column, chosen by `@`.

    fn lookup(conn: &Connection, identifier: &str) -> Option<String> {
        let column = if identifier.contains('@') { "email" } else { "username" };
        conn.query_row(
            &format!("SELECT id FROM users WHERE {column} = ?1"),
            rusqlite::params![identifier],
            |row| row.get(0),
        )
        .ok()
    }

    /// `000021` makes an email-shaped username unrepresentable, the premise of the
    /// `@` dispatch.
    #[test]
    fn username_with_at_sign_is_unrepresentable() {
        let conn = db();
        setup(&conn);

        let inserted = conn.execute(
            "INSERT INTO users (id, email, username) VALUES ('mallory', 'mallory@x.com', 'alice@x.com')",
            [],
        );
        assert!(inserted.is_err(), "the schema must refuse an email-shaped username on INSERT");

        let updated = conn.execute(
            "UPDATE users SET username = 'alice@x.com' WHERE id = 'bob'",
            [],
        );
        assert!(updated.is_err(), "the schema must refuse an email-shaped username on UPDATE");

        assert_eq!(lookup(&conn, "alice@x.com").as_deref(), Some("alice"));
    }

    #[test]
    fn preferences_upsert_insert_then_update() {
        let conn = db();
        setup(&conn);

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
