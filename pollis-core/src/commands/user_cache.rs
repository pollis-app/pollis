//! The local `user_cache`: the handle this device last read for each user id (#2084).
//!
//! It names message senders (pages, search hits, pins, pushes) without a DS round trip per row,
//! so it must never be the last word on a name. A row is trusted for [`FRESH_SECS`]; past that,
//! or when it was marked stale, the next read re-asks the DS for it in the same one batch it
//! uses for misses, and keeps the old name only when that read fails (offline). Rows go stale
//! when the database opens (every launch, unlock and resume), and a profile change elsewhere
//! refreshes the changed user at once (`get_user_profile`, driven by the DS's
//! content-free `profile_changed` nudge).

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::Connection;

use crate::state::AppState;

/// How long a cached handle is trusted before a read re-asks the DS.
pub const FRESH_SECS: i64 = 600;

/// What `updated_at` is set to when a row must be re-read on its next use.
const STALE_STAMP: &str = "1970-01-01 00:00:00";

/// Cached handles for `ids`, and whether each one is still fresh.
pub(crate) fn cached(conn: &Connection, ids: &[String]) -> HashMap<String, (String, bool)> {
    let mut out = HashMap::new();
    for chunk in crate::db::chunk::bind_chunks(ids, 0) {
        let sql = format!(
            "SELECT id, username, updated_at > datetime('now', '-{FRESH_SECS} seconds') \
             FROM user_cache WHERE id IN ({})",
            crate::db::chunk::placeholders(chunk.len(), 1)
        );
        let Ok(mut stmt) = conn.prepare(&sql) else {
            continue;
        };
        let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, bool>(2)?))
        }) else {
            continue;
        };
        for (id, name, fresh) in rows.flatten() {
            out.insert(id, (name, fresh));
        }
    }
    out
}

/// One cached handle, fresh or not (synchronous call sites that cannot reach the DS).
pub(crate) fn cached_one(conn: &Connection, id: &str) -> Option<String> {
    cached(conn, std::slice::from_ref(&id.to_string()))
        .remove(id)
        .map(|(name, _)| name)
}

/// Record handles the DS just returned, fresh as of now.
pub(crate) fn store(conn: &Connection, names: &[(String, String)]) {
    for (id, name) in names {
        if let Err(e) = conn.execute(
            "INSERT INTO user_cache (id, username, updated_at) VALUES (?1, ?2, datetime('now')) \
             ON CONFLICT(id) DO UPDATE SET username = ?2, updated_at = datetime('now')",
            rusqlite::params![id, name],
        ) {
            crate::diag!("[user_cache] store failed (non-fatal): {e}");
        }
    }
}

/// Make every row re-read on its next use; called whenever the database opens.
pub(crate) fn mark_all_stale(conn: &Connection) {
    if let Err(e) = conn.execute(
        "UPDATE user_cache SET updated_at = ?1",
        rusqlite::params![STALE_STAMP],
    ) {
        crate::diag!("[user_cache] mark stale failed (non-fatal): {e}");
    }
}

/// The ids a read must ask the DS about: missing or stale.
fn to_fetch(ids: &[String], known: &HashMap<String, (String, bool)>) -> Vec<String> {
    ids.iter()
        .filter(|id| !matches!(known.get(*id), Some((_, true))))
        .cloned()
        .collect()
}

/// Handles for `ids`: the cache where fresh, ONE DS batch for the rest, the stale cached name
/// where that batch fails or leaves an id out. Ids nothing can name are absent.
pub(crate) async fn usernames(
    state: &Arc<AppState>,
    ids: &[String],
    deadline: Option<tokio::time::Instant>,
) -> HashMap<String, String> {
    let mut ids: Vec<String> = ids.to_vec();
    ids.sort();
    ids.dedup();
    let known = {
        let guard = state.local_db.lock().await;
        match guard.as_ref() {
            Some(db) => cached(db.conn(), &ids),
            None => HashMap::new(),
        }
    };
    let wanted = to_fetch(&ids, &known);
    let mut out: HashMap<String, String> =
        known.into_iter().map(|(id, (name, _))| (id, name)).collect();
    if wanted.is_empty() {
        return out;
    }
    let lookup = crate::commands::ds_reads::users(state, wanted, None);
    let fetched = match deadline {
        Some(at) => match tokio::time::timeout_at(at, lookup).await {
            Ok(r) => r,
            Err(_) => return out,
        },
        None => lookup.await,
    };
    match fetched {
        Ok(users) => {
            let names: Vec<(String, String)> = users.into_iter().map(|u| (u.id, u.username)).collect();
            if !names.is_empty() {
                let guard = state.local_db.lock().await;
                if let Some(db) = guard.as_ref() {
                    store(db.conn(), &names);
                }
            }
            out.extend(names);
        }
        Err(e) => {
            // Offline or transient: the stale names stand; a later read refreshes them.
            crate::diag!("[user_cache] refresh failed (non-fatal): {e}");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE user_cache (
                id TEXT PRIMARY KEY,
                username TEXT NOT NULL,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .unwrap();
        conn
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_stored_name_is_fresh_and_not_refetched() {
        let conn = db();
        store(&conn, &[("u1".into(), "alice".into())]);
        let known = cached(&conn, &ids(&["u1"]));
        assert_eq!(known.get("u1"), Some(&("alice".to_string(), true)));
        assert!(to_fetch(&ids(&["u1"]), &known).is_empty());
    }

    #[test]
    fn an_old_row_is_refetched_but_still_answers() {
        let conn = db();
        conn.execute(
            "INSERT INTO user_cache (id, username, updated_at) VALUES ('u1', 'old', datetime('now', '-1 day'))",
            [],
        )
        .unwrap();
        let known = cached(&conn, &ids(&["u1", "u2"]));
        assert_eq!(known.get("u1"), Some(&("old".to_string(), false)));
        assert_eq!(to_fetch(&ids(&["u1", "u2"]), &known), ids(&["u1", "u2"]));
        assert_eq!(cached_one(&conn, "u1").as_deref(), Some("old"));
    }

    #[test]
    fn opening_the_database_makes_every_row_stale() {
        let conn = db();
        store(&conn, &[("u1".into(), "alice".into()), ("u2".into(), "bo".into())]);
        mark_all_stale(&conn);
        let known = cached(&conn, &ids(&["u1", "u2"]));
        assert!(known.values().all(|(_, fresh)| !fresh));
        assert_eq!(to_fetch(&ids(&["u1", "u2"]), &known).len(), 2);
    }

    #[test]
    fn a_refreshed_name_replaces_the_old_one() {
        let conn = db();
        store(&conn, &[("u1".into(), "old".into())]);
        mark_all_stale(&conn);
        store(&conn, &[("u1".into(), "new".into())]);
        assert_eq!(cached(&conn, &ids(&["u1"])).get("u1"), Some(&("new".to_string(), true)));
    }
}
