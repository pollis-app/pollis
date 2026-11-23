//! The local `user_cache`: the handle and display name this device last read for each user id
//! (#2084, #2085).
//!
//! It names message senders (pages, threads, search hits, pins, pushes) without a DS round trip
//! per row, so it must never be the last word on a name. A row is trusted for [`FRESH_SECS`]; past
//! that, or when it was marked stale, the next read re-asks the DS for it in the same one batch it
//! uses for misses, and keeps the old name only when that read fails (offline). Rows go stale
//! when the database opens (every launch, unlock and resume), and a profile change elsewhere
//! refreshes the changed user at once (`get_user_profile`, driven by the DS's
//! content-free `profile_changed` nudge).

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::Connection;

use crate::state::AppState;

/// How long a cached name is trusted before a read re-asks the DS.
pub const FRESH_SECS: i64 = 600;

/// What `updated_at` is set to when a row must be re-read on its next use.
const STALE_STAMP: &str = "1970-01-01 00:00:00";

/// How this device names one user: the handle, and the display name when they set one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CachedName {
    pub username: String,
    pub display_name: Option<String>,
}

impl CachedName {
    pub(crate) fn new(username: String, display_name: Option<String>) -> Self {
        // An empty or blank display name is no display name.
        let display_name = display_name.filter(|n| !n.trim().is_empty());
        Self { username, display_name }
    }
}

impl From<&pollis_api::directory::UserWire> for CachedName {
    fn from(u: &pollis_api::directory::UserWire) -> Self {
        Self::new(u.username.clone(), u.preferred_name.clone())
    }
}

/// Cached names for `ids`, and whether each one is still fresh.
pub(crate) fn cached(conn: &Connection, ids: &[String]) -> HashMap<String, (CachedName, bool)> {
    let mut out = HashMap::new();
    for chunk in crate::db::chunk::bind_chunks(ids, 0) {
        let sql = format!(
            "SELECT id, username, preferred_name, updated_at > datetime('now', '-{FRESH_SECS} seconds') \
             FROM user_cache WHERE id IN ({})",
            crate::db::chunk::placeholders(chunk.len(), 1)
        );
        let Ok(mut stmt) = conn.prepare(&sql) else {
            continue;
        };
        let Ok(rows) = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                CachedName::new(row.get(1)?, row.get(2)?),
                row.get::<_, bool>(3)?,
            ))
        }) else {
            continue;
        };
        for (id, name, fresh) in rows.flatten() {
            out.insert(id, (name, fresh));
        }
    }
    out
}

/// One cached name, fresh or not (synchronous call sites that cannot reach the DS).
pub(crate) fn cached_one(conn: &Connection, id: &str) -> Option<CachedName> {
    cached(conn, std::slice::from_ref(&id.to_string()))
        .remove(id)
        .map(|(name, _)| name)
}

/// Record whole names the DS just returned, fresh as of now.
pub(crate) fn store(conn: &Connection, names: &[(String, CachedName)]) {
    for (id, name) in names {
        if let Err(e) = conn.execute(
            "INSERT INTO user_cache (id, username, preferred_name, updated_at) \
             VALUES (?1, ?2, ?3, datetime('now')) \
             ON CONFLICT(id) DO UPDATE SET username = ?2, preferred_name = ?3, updated_at = datetime('now')",
            rusqlite::params![id, name.username, name.display_name],
        ) {
            crate::diag!("[user_cache] store failed (non-fatal): {e}");
        }
    }
}

/// This device's own profile edit: write only the fields it changed (`None` keeps a field;
/// an empty display name clears it). A row this device has never cached is left for the next
/// read, which fetches the whole name.
pub(crate) fn note_own_edit(conn: &Connection, id: &str, username: Option<&str>, display_name: Option<&str>) {
    if let Some(username) = username {
        let _ = conn.execute(
            "UPDATE user_cache SET username = ?2, updated_at = datetime('now') WHERE id = ?1",
            rusqlite::params![id, username],
        );
    }
    if let Some(display_name) = display_name {
        let value = Some(display_name).filter(|n| !n.trim().is_empty());
        let _ = conn.execute(
            "UPDATE user_cache SET preferred_name = ?2 WHERE id = ?1",
            rusqlite::params![id, value],
        );
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
fn to_fetch(ids: &[String], known: &HashMap<String, (CachedName, bool)>) -> Vec<String> {
    ids.iter()
        .filter(|id| !matches!(known.get(*id), Some((_, true))))
        .cloned()
        .collect()
}

/// Names for `ids`: the cache where fresh, ONE DS batch for the rest, the stale cached name
/// where that batch fails or leaves an id out. Ids nothing can name are absent.
pub(crate) async fn names(
    state: &Arc<AppState>,
    ids: &[String],
    deadline: Option<tokio::time::Instant>,
) -> HashMap<String, CachedName> {
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
    let mut out: HashMap<String, CachedName> =
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
            let fresh: Vec<(String, CachedName)> =
                users.iter().map(|u| (u.id.clone(), CachedName::from(u))).collect();
            if !fresh.is_empty() {
                let guard = state.local_db.lock().await;
                if let Some(db) = guard.as_ref() {
                    store(db.conn(), &fresh);
                }
            }
            out.extend(fresh);
        }
        Err(e) => {
            // Offline or transient: the stale names stand; a later read refreshes them.
            crate::diag!("[user_cache] refresh failed (non-fatal): {e}");
        }
    }
    out
}

/// Handles only, for the callers that name people by handle (exports, `from:` search, pushes).
pub(crate) async fn usernames(
    state: &Arc<AppState>,
    ids: &[String],
    deadline: Option<tokio::time::Instant>,
) -> HashMap<String, String> {
    names(state, ids, deadline)
        .await
        .into_iter()
        .map(|(id, name)| (id, name.username))
        .collect()
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
                preferred_name TEXT,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );",
        )
        .unwrap();
        conn
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn name(u: &str, d: Option<&str>) -> CachedName {
        CachedName::new(u.to_string(), d.map(str::to_string))
    }

    #[test]
    fn a_stored_name_is_fresh_and_not_refetched() {
        let conn = db();
        store(&conn, &[("u1".into(), name("alice", Some("Alice A")))]);
        let known = cached(&conn, &ids(&["u1"]));
        assert_eq!(known.get("u1"), Some(&(name("alice", Some("Alice A")), true)));
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
        assert_eq!(known.get("u1"), Some(&(name("old", None), false)));
        assert_eq!(to_fetch(&ids(&["u1", "u2"]), &known), ids(&["u1", "u2"]));
        assert_eq!(cached_one(&conn, "u1"), Some(name("old", None)));
    }

    #[test]
    fn opening_the_database_makes_every_row_stale() {
        let conn = db();
        store(&conn, &[("u1".into(), name("alice", None)), ("u2".into(), name("bo", None))]);
        mark_all_stale(&conn);
        let known = cached(&conn, &ids(&["u1", "u2"]));
        assert!(known.values().all(|(_, fresh)| !fresh));
        assert_eq!(to_fetch(&ids(&["u1", "u2"]), &known).len(), 2);
    }

    #[test]
    fn a_refreshed_name_replaces_the_old_one_display_name_included() {
        let conn = db();
        store(&conn, &[("u1".into(), name("old", Some("Old Name")))]);
        mark_all_stale(&conn);
        store(&conn, &[("u1".into(), name("new", None))]);
        assert_eq!(cached(&conn, &ids(&["u1"])).get("u1"), Some(&(name("new", None), true)));
    }

    #[test]
    fn a_blank_display_name_is_none() {
        assert_eq!(name("bo", Some("   ")).display_name, None);
        assert_eq!(name("bo", Some("")).display_name, None);
    }

    #[test]
    fn an_own_edit_changes_only_the_fields_it_names() {
        let conn = db();
        store(&conn, &[("me".into(), name("mia", Some("Mia")))]);
        note_own_edit(&conn, "me", None, Some("Mia M."));
        assert_eq!(cached_one(&conn, "me"), Some(name("mia", Some("Mia M."))));
        note_own_edit(&conn, "me", Some("mia2"), None);
        assert_eq!(cached_one(&conn, "me"), Some(name("mia2", Some("Mia M."))));
        note_own_edit(&conn, "me", None, Some(""));
        assert_eq!(cached_one(&conn, "me"), Some(name("mia2", None)));
    }
}
