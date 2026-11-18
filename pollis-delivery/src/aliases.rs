//! Per-group aliases (#2003): the display name and avatar a member shows in one
//! group instead of their main profile.
//!
//! ## Authorization
//!
//!   - set: the actor's OWN row, only in a group they are currently a member of;
//!     "apply to all" resolves the actor's current groups server-side.
//!   - reset: the owner at any time (even after leaving), or an admin of that
//!     group for anyone's row.
//!   - read one group: a CURRENT member of the group. Rows of members who have
//!     left are included, so their old messages keep rendering the alias.
//!   - read own list: the owner only.
//!
//! ## Avatar objects
//!
//! An alias avatar is one of the owner's own content-addressed
//! `avatars/<user_id>/<sha256>.<ext>` objects. Whenever a write or a teardown
//! drops a reference to one, the key goes into `avatar_release`
//! ([`release_avatar`]); [`reclaim_released_avatars`] deletes the object once
//! nothing references it any more.

use axum::{extract::State, response::Response};
use libsql::Connection;

use crate::error::AppError;
use crate::profile::{is_valid_display_name, profile_outcome_response, ProfileOutcome, DISPLAY_NAME_INVALID};
use crate::writes::{gate_and_parse, outcome_response, resolve_actor, RawRequest, WriteOutcome};
use crate::AppState;

pub use pollis_api::profile::{
    GroupAliasWire, GroupAliasesBody, GroupAliasesResponse, MyAliasesBody, MyAliasesResponse,
    ResetGroupAliasBody, SetGroupAliasBody,
};

/// The message a rejected alias avatar key is refused with.
pub const ALIAS_AVATAR_INVALID: &str = "avatar must be one of your own uploaded profile pictures";

/// The message a write naming no field is refused with.
pub const ALIAS_EMPTY_WRITE: &str = "set a display name, an avatar, or both";

/// The realtime nudge members get when an alias in their group changes.
fn changed_event(group_id: Option<&str>) -> serde_json::Value {
    match group_id {
        Some(g) => serde_json::json!({ "type": "group_aliases_changed", "group_id": g }),
        None => serde_json::json!({ "type": "group_aliases_changed" }),
    }
}

/// Tell each group's rooms (the group and its channels) and the owner's inbox
/// that aliases changed, in the background. Best-effort: clients also re-read
/// on their next visit.
fn spawn_changed_wakes(state: &AppState, groups: Vec<String>, owner: String) {
    if state.broker.livekit_ready().is_none() {
        return;
    }
    let state = state.clone();
    tokio::spawn(async move {
        if let Ok(conn) = state.db.conn().await {
            for group_id in &groups {
                let rooms = crate::broker::group_rooms(&conn, group_id)
                    .await
                    .unwrap_or_else(|_| vec![group_id.clone()]);
                let event = changed_event(Some(group_id));
                for room in rooms {
                    if let Err(e) = crate::broker::room_send_data(&state, &room, &event).await {
                        tracing::warn!("group-alias wake-up failed (non-fatal): {e}");
                    }
                }
            }
        }
        let inbox = format!("inbox-{owner}");
        if let Err(e) = crate::broker::room_send_data(&state, &inbox, &changed_event(None)).await {
            tracing::warn!("group-alias inbox notify failed (non-fatal): {e}");
        }
    });
}

/// True when `key` is a content-addressed avatar object owned by `user_id` —
/// the only kind of key an alias may name, and the only kind the DS deletes.
pub fn is_own_avatar_key(key: &str, user_id: &str) -> bool {
    crate::broker::parse_r2_key(key).is_some_and(|k| {
        k.family == crate::broker::R2Family::Avatar && k.content_addressed && k.owner == Some(user_id)
    })
}

/// Queue `key` for [`reclaim_released_avatars`] if it names a content-addressed
/// avatar object. Anything else (empty, legacy mutable key) is ignored.
pub async fn release_avatar(conn: &Connection, key: Option<&str>) -> anyhow::Result<()> {
    let Some(key) = key.filter(|k| {
        crate::broker::parse_r2_key(k)
            .is_some_and(|p| p.family == crate::broker::R2Family::Avatar && p.content_addressed)
    }) else {
        return Ok(());
    };
    conn.execute(
        "INSERT INTO avatar_release (r2_key) VALUES (?1) \
         ON CONFLICT(r2_key) DO UPDATE SET released_at = datetime('now')",
        libsql::params![key.to_string()],
    )
    .await?;
    Ok(())
}

/// Release every alias avatar a `WHERE` clause over `group_alias` selects, then
/// delete those rows. Used by the owner/admin reset and by teardown.
pub async fn purge_aliases_where(
    conn: &Connection,
    where_sql: &str,
    params: &[String],
) -> anyhow::Result<()> {
    let bind = || libsql::params_from_iter(params.iter().cloned());
    let keys = crate::util::query_strings(
        conn,
        &format!("SELECT avatar_url FROM group_alias WHERE avatar_url IS NOT NULL AND {where_sql}"),
        bind(),
    )
    .await?;
    for key in &keys {
        release_avatar(conn, Some(key)).await?;
    }
    conn.execute(&format!("DELETE FROM group_alias WHERE {where_sql}"), bind())
        .await?;
    Ok(())
}

pub async fn set_group_alias(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<SetGroupAliasBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let (outcome, groups) = apply_set_group_alias(&conn, authed.as_deref(), &parsed).await?;
    if matches!(outcome, ProfileOutcome::Ok) {
        let owner = authed.clone().unwrap_or_else(|| parsed.user_id.clone());
        spawn_changed_wakes(&state, groups, owner);
    }
    profile_outcome_response::<SetGroupAliasBody>(outcome)
}

/// One field of a write: leave it, clear it, or set it.
#[derive(Clone, Copy)]
enum FieldWrite<'a> {
    Keep,
    Clear,
    Set(&'a str),
}

impl<'a> FieldWrite<'a> {
    fn from(v: Option<&'a str>) -> Self {
        match v {
            None => FieldWrite::Keep,
            Some("") => FieldWrite::Clear,
            Some(s) => FieldWrite::Set(s),
        }
    }

    fn apply(self, current: Option<String>) -> Option<String> {
        match self {
            FieldWrite::Keep => current,
            FieldWrite::Clear => None,
            FieldWrite::Set(s) => Some(s.to_string()),
        }
    }
}

/// Write the actor's alias in one group, or in every group they are currently in.
///
/// Returns the groups written, for the caller's wake-ups.
pub async fn apply_set_group_alias(
    conn: &Connection,
    authed: Option<&str>,
    body: &SetGroupAliasBody,
) -> anyhow::Result<(ProfileOutcome, Vec<String>)> {
    let user = match resolve_actor(authed, Some(body.user_id.as_str())) {
        Ok(u) => u,
        Err(_) => return Ok((ProfileOutcome::Forbidden, Vec::new())),
    };
    let name = FieldWrite::from(body.display_name.as_deref());
    let avatar = FieldWrite::from(body.avatar_url.as_deref());
    if matches!((name, avatar), (FieldWrite::Keep, FieldWrite::Keep)) {
        return Ok((ProfileOutcome::Invalid(ALIAS_EMPTY_WRITE), Vec::new()));
    }
    if let FieldWrite::Set(n) = name {
        if !is_valid_display_name(n) {
            return Ok((ProfileOutcome::Invalid(DISPLAY_NAME_INVALID), Vec::new()));
        }
    }
    if let FieldWrite::Set(k) = avatar {
        if !is_own_avatar_key(k, &user) {
            return Ok((ProfileOutcome::Invalid(ALIAS_AVATAR_INVALID), Vec::new()));
        }
    }

    let tx = crate::db::write_tx(conn).await?;
    let groups = match body.group_id.as_deref() {
        Some(group_id) => {
            if !crate::groups::is_member(&tx, group_id, &user).await? {
                return Ok((ProfileOutcome::Forbidden, Vec::new()));
            }
            vec![group_id.to_string()]
        }
        None => {
            crate::util::query_strings(
                &tx,
                "SELECT group_id FROM group_member WHERE user_id = ?1 ORDER BY group_id",
                libsql::params![user.clone()],
            )
            .await?
        }
    };
    for group_id in &groups {
        let current = {
            let mut rows = tx
                .query(
                    "SELECT display_name, avatar_url FROM group_alias \
                     WHERE group_id = ?1 AND user_id = ?2",
                    libsql::params![group_id.clone(), user.clone()],
                )
                .await?;
            match rows.next().await? {
                Some(row) => (row.get::<Option<String>>(0)?, row.get::<Option<String>>(1)?),
                None => (None, None),
            }
        };
        let old_avatar = current.1.clone();
        let next_name = name.apply(current.0);
        let next_avatar = avatar.apply(current.1);
        if old_avatar.is_some() && old_avatar != next_avatar {
            release_avatar(&tx, old_avatar.as_deref()).await?;
        }
        if next_name.is_none() && next_avatar.is_none() {
            tx.execute(
                "DELETE FROM group_alias WHERE group_id = ?1 AND user_id = ?2",
                libsql::params![group_id.clone(), user.clone()],
            )
            .await?;
        } else {
            tx.execute(
                "INSERT INTO group_alias (group_id, user_id, display_name, avatar_url, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, datetime('now')) \
                 ON CONFLICT(group_id, user_id) DO UPDATE SET \
                    display_name = excluded.display_name, \
                    avatar_url = excluded.avatar_url, \
                    updated_at = excluded.updated_at",
                libsql::params![group_id.clone(), user.clone(), next_name, next_avatar],
            )
            .await?;
        }
    }
    tx.commit().await?;
    Ok((ProfileOutcome::Ok, groups))
}

pub async fn reset_group_alias(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<ResetGroupAliasBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let outcome = apply_reset_group_alias(&conn, authed.as_deref(), &parsed).await?;
    if matches!(outcome, WriteOutcome::Ok) {
        spawn_changed_wakes(&state, vec![parsed.group_id.clone()], parsed.user_id.clone());
    }
    outcome_response::<ResetGroupAliasBody>(outcome)
}

/// Delete `user_id`'s alias in `group_id`. Authz: the owner, or an admin of the
/// group. Idempotent: resetting a missing row is a success.
pub async fn apply_reset_group_alias(
    conn: &Connection,
    authed: Option<&str>,
    body: &ResetGroupAliasBody,
) -> anyhow::Result<WriteOutcome> {
    let requester = match resolve_actor(authed, Some(body.requester_id.as_str())) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    if requester != body.user_id && !crate::groups::is_admin(conn, &body.group_id, &requester).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    let tx = crate::db::write_tx(conn).await?;
    purge_aliases_where(
        &tx,
        "group_id = ?1 AND user_id = ?2",
        &[body.group_id.clone(), body.user_id.clone()],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

pub async fn read_group_aliases(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (parsed, who) =
        match crate::reads::parse_user_read(&state, &req, |b: &GroupAliasesBody| &b.user_id).await? {
            Ok(v) => v,
            Err(resp) => return Ok(resp),
        };
    let conn = state.db.conn().await?;
    Ok(crate::writes::ok_response::<GroupAliasesBody>(
        list_group_aliases(&conn, &parsed.group_id, &who).await?,
    ))
}

/// The aliases `WHERE <column> = ?1` selects, ordered by `order`.
async fn query_aliases(
    conn: &Connection,
    column: &str,
    order: &str,
    value: &str,
) -> anyhow::Result<Vec<GroupAliasWire>> {
    let mut rows = conn
        .query(
            &format!(
                "SELECT group_id, user_id, display_name, avatar_url FROM group_alias \
                 WHERE {column} = ?1 ORDER BY {order}"
            ),
            libsql::params![value.to_string()],
        )
        .await?;
    let mut aliases = Vec::new();
    while let Some(row) = rows.next().await? {
        aliases.push(GroupAliasWire {
            group_id: row.get(0)?,
            user_id: row.get(1)?,
            display_name: row.get(2)?,
            avatar_url: row.get(3)?,
        });
    }
    Ok(aliases)
}

/// Every alias in `group_id` (members past and present), sorted by user, when
/// `reader` is a current member; `authorized: false` and nothing otherwise.
pub async fn list_group_aliases(
    conn: &Connection,
    group_id: &str,
    reader: &str,
) -> anyhow::Result<GroupAliasesResponse> {
    if !crate::groups::is_member(conn, group_id, reader).await? {
        return Ok(GroupAliasesResponse::default());
    }
    Ok(GroupAliasesResponse {
        authorized: true,
        aliases: query_aliases(conn, "group_id", "user_id", group_id).await?,
    })
}

pub async fn read_my_aliases(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let who = match crate::reads::parse_user_read(&state, &req, |b: &MyAliasesBody| &b.user_id).await? {
        Ok((_, who)) => who,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    Ok(crate::writes::ok_response::<MyAliasesBody>(list_my_aliases(&conn, &who).await?))
}

/// Every alias `user_id` owns, in any group (including ones they left), sorted by group.
pub async fn list_my_aliases(conn: &Connection, user_id: &str) -> anyhow::Result<MyAliasesResponse> {
    Ok(MyAliasesResponse {
        aliases: query_aliases(conn, "user_id", "group_id", user_id).await?,
    })
}

/// How long a released avatar waits before its object may be deleted. Covers
/// the gap between a client uploading a picture and saving it, so re-choosing a
/// picture that was just dropped never loses its bytes.
pub const AVATAR_RELEASE_GRACE_SECS: i64 = 600;

/// The most released avatars one pass takes on.
const AVATAR_RECLAIM_PASS_LIMIT: i64 = 100;

/// What one [`reclaim_released_avatars`] pass did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AvatarReclaimReport {
    /// Objects deleted from the bucket.
    pub deleted: u64,
    /// Released keys that turned out to be referenced again; kept.
    pub kept: u64,
    /// Deletes that failed; their rows stay for the next pass.
    pub failed: u64,
}

/// Delete every released avatar object that has waited out `grace_secs` and that
/// neither a profile (`users.avatar_url`) nor an alias (`group_alias.avatar_url`)
/// references. A referenced key just loses its release row. A failed delete
/// keeps the row, so the next pass retries.
pub async fn reclaim_released_avatars(
    conn: &Connection,
    store: &impl crate::broker::ObjectStore,
    grace_secs: i64,
) -> anyhow::Result<AvatarReclaimReport> {
    let pending = crate::util::query_strings(
        conn,
        "SELECT r2_key FROM avatar_release \
         WHERE released_at <= datetime('now', ?1) ORDER BY RANDOM() LIMIT ?2",
        libsql::params![format!("-{grace_secs} seconds"), AVATAR_RECLAIM_PASS_LIMIT],
    )
    .await?;
    let mut report = AvatarReclaimReport::default();
    for key in pending {
        let referenced = {
            let mut rows = conn
                .query(
                    "SELECT 1 FROM users WHERE avatar_url = ?1 \
                     UNION ALL SELECT 1 FROM group_alias WHERE avatar_url = ?1 LIMIT 1",
                    libsql::params![key.clone()],
                )
                .await?;
            rows.next().await?.is_some()
        };
        let is_avatar = crate::broker::parse_r2_key(&key)
            .is_some_and(|k| k.family == crate::broker::R2Family::Avatar && k.content_addressed);
        if referenced {
            report.kept += 1;
        } else if is_avatar {
            if let Err(e) = store.delete_object(&key).await {
                tracing::warn!("avatar reclaim: object delete failed, retried next pass: {e}");
                report.failed += 1;
                continue;
            }
            report.deleted += 1;
        }
        conn.execute(
            "DELETE FROM avatar_release WHERE r2_key = ?1",
            libsql::params![key],
        )
        .await?;
    }
    Ok(report)
}

/// Run [`reclaim_released_avatars`] in the background, one pass at a time
/// (`AppState::avatar_reclaim`). Kicked on every envelope-GC tick, which is also
/// the retry.
pub fn spawn_avatar_reclaim(state: &AppState) {
    if state.broker.r2_ready().is_none() {
        return;
    }
    let db = std::sync::Arc::clone(&state.db);
    let broker = state.broker.clone();
    state.avatar_reclaim.kick(move || {
        let db = std::sync::Arc::clone(&db);
        let broker = broker.clone();
        async move {
            let result = async {
                let conn = db.conn().await?;
                reclaim_released_avatars(&conn, &broker, AVATAR_RELEASE_GRACE_SECS).await
            }
            .await;
            match result {
                Ok(r) if r.deleted > 0 || r.failed > 0 => tracing::info!(
                    "avatar reclaim: {} deleted, {} still referenced, {} retried next pass",
                    r.deleted,
                    r.kept,
                    r.failed
                ),
                Ok(_) => {}
                Err(e) => tracing::warn!("avatar reclaim failed: {e:#}"),
            }
        }
    });
}
