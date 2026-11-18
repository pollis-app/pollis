//! Per-group aliases (#2003): the display name and avatar a member shows in one
//! group instead of their main profile.
//!
//! The DS holds them (`group_alias`) and gates every read on current
//! membership; clients overlay them on the main profile at render time, so old
//! messages pick up a new alias too. A missing row, or a `None` field, means
//! "use the main profile" for that field.
//!
//! Each group read also refreshes `group_alias_cache` in the local DB, which is
//! what a push notification for that group names the sender from
//! (`notify::fetch`), with no network round trip.

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::state::AppState;

/// One alias. `None` fields fall back to the user's main profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupAlias {
    pub group_id: String,
    pub user_id: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
}

impl From<pollis_api::profile::GroupAliasWire> for GroupAlias {
    fn from(w: pollis_api::profile::GroupAliasWire) -> Self {
        GroupAlias {
            group_id: w.group_id,
            user_id: w.user_id,
            display_name: w.display_name.filter(|n| !n.is_empty()),
            avatar_url: w.avatar_url.filter(|a| !a.is_empty()),
        }
    }
}

/// Every alias in `group_id`, including those of members who have left. Empty
/// when the requester is not a current member: every surface then shows main
/// profiles, which is what a non-member is entitled to.
pub async fn list_group_aliases(
    group_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<GroupAlias>> {
    let resp = crate::commands::ds_reads::group_aliases(state, &requester_id, &group_id).await?;
    if !resp.authorized {
        return Ok(Vec::new());
    }
    let aliases: Vec<GroupAlias> = resp.aliases.into_iter().map(GroupAlias::from).collect();
    let guard = state.local_db.lock().await;
    if let Some(db) = guard.as_ref() {
        if let Err(e) = cache_group_aliases(db.conn(), &group_id, &aliases) {
            crate::diag!("[aliases] group_alias_cache not refreshed: {e}");
        }
    }
    Ok(aliases)
}

/// Every alias this account owns, in any group (including ones it left).
pub async fn list_my_aliases(user_id: String, state: &Arc<AppState>) -> Result<Vec<GroupAlias>> {
    Ok(crate::commands::ds_reads::my_aliases(state, &user_id)
        .await?
        .aliases
        .into_iter()
        .map(GroupAlias::from)
        .collect())
}

/// Set the caller's alias in `group_id`, or in every group they are currently
/// in when `group_id` is `None` ("apply to all"). Per field: `None` leaves it,
/// `Some("")` clears it (back to the main profile), anything else sets it.
pub async fn set_group_alias(
    user_id: String,
    group_id: Option<String>,
    display_name: Option<String>,
    avatar_url: Option<String>,
    state: &Arc<AppState>,
) -> Result<()> {
    if display_name.is_none() && avatar_url.is_none() {
        return Err(Error::Other(anyhow::anyhow!("nothing to change")));
    }
    let body = pollis_api::profile::SetGroupAliasBody {
        user_id,
        group_id,
        display_name,
        avatar_url,
    };
    crate::commands::mls::ds_post_ok(state, &body).await
}

/// Delete `user_id`'s alias in `group_id`. The owner may always; an admin of the
/// group may for anyone (the DS re-derives both).
pub async fn reset_group_alias(
    group_id: String,
    user_id: String,
    requester_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let body = pollis_api::profile::ResetGroupAliasBody {
        requester_id,
        group_id,
        user_id,
    };
    crate::commands::mls::ds_post_ok(state, &body).await
}

/// Replace `group_id`'s cached alias names with `aliases`. Only names are kept:
/// that is all a notification shows.
pub(crate) fn cache_group_aliases(
    conn: &rusqlite::Connection,
    group_id: &str,
    aliases: &[GroupAlias],
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM group_alias_cache WHERE group_id = ?1",
        rusqlite::params![group_id],
    )?;
    for a in aliases {
        if let Some(name) = a.display_name.as_deref() {
            conn.execute(
                "INSERT OR REPLACE INTO group_alias_cache (group_id, user_id, display_name, updated_at)
                 VALUES (?1, ?2, ?3, datetime('now'))",
                rusqlite::params![group_id, a.user_id, name],
            )?;
        }
    }
    Ok(())
}

/// The cached alias name `user_id` shows in `group_id`, if any.
pub(crate) fn cached_alias_name(
    conn: &rusqlite::Connection,
    group_id: &str,
    user_id: &str,
) -> Option<String> {
    conn.query_row(
        "SELECT display_name FROM group_alias_cache WHERE group_id = ?1 AND user_id = ?2",
        rusqlite::params![group_id, user_id],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::local::apply_local_schema(&conn).unwrap();
        conn
    }

    fn alias(user: &str, name: Option<&str>, avatar: Option<&str>) -> GroupAlias {
        GroupAlias {
            group_id: "g1".into(),
            user_id: user.into(),
            display_name: name.map(str::to_string),
            avatar_url: avatar.map(str::to_string),
        }
    }

    #[test]
    fn an_empty_wire_field_reads_as_the_fallback() {
        let a = GroupAlias::from(pollis_api::profile::GroupAliasWire {
            group_id: "g".into(),
            user_id: "u".into(),
            display_name: Some(String::new()),
            avatar_url: Some(String::new()),
        });
        assert_eq!(a.display_name, None);
        assert_eq!(a.avatar_url, None);
    }

    #[test]
    fn the_cache_holds_names_per_group_and_a_refresh_replaces_it() {
        let conn = db();
        cache_group_aliases(
            &conn,
            "g1",
            &[alias("bob", Some("Bobby"), None), alias("carol", None, Some("avatars/carol/x.jpg"))],
        )
        .unwrap();
        assert_eq!(cached_alias_name(&conn, "g1", "bob").as_deref(), Some("Bobby"));
        assert_eq!(cached_alias_name(&conn, "g1", "carol"), None, "an avatar-only alias keeps the main name");
        assert_eq!(cached_alias_name(&conn, "g2", "bob"), None, "another group falls back");

        // Bob reset his alias: the next read no longer carries it.
        cache_group_aliases(&conn, "g1", &[]).unwrap();
        assert_eq!(cached_alias_name(&conn, "g1", "bob"), None);
    }
}
