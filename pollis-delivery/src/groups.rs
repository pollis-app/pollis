//! Groups, channels, membership, invites and join requests (#419).
//!
//! Same shape as [`crate::messages`]: each `apply_*(conn, authed, body)` holds
//! both the authorization decision and the write, and is shared by the axum
//! handler and the test harness. All writes land on the main DB.
//!
//! Admin-gated writes re-derive the actor's role from `group_member` and never
//! trust a client-supplied one. On the no-auth path (`authed == None`, only
//! when `POLLIS_DS_REQUIRE_AUTH` is off) role checks are skipped and the actor
//! comes from the body.

use axum::{
    extract::State,
    response::{IntoResponse as _, Response},
};
use libsql::Connection;

use crate::error::{AppError, AuthRejection};
use crate::writes::{
    claim_conversation_id,
    conversation_id_taken,
    gate_and_parse,
    outcome_response,
    resolve_actor,
    ConversationKind,
    RawRequest,
    WriteOutcome,
};
use crate::AppState;

// Request bodies live in `pollis-api` so client and server share one
// declaration.
pub use pollis_api::groups::*;

/// The actor's role in `group_id`, re-derived server-side; `None` when not a
/// member. The SQL is shared with the client preflight (#942), and this is the
/// DS's only group-role read (#875).
pub(crate) async fn group_role(
    conn: &Connection,
    group_id: &str,
    user_id: &str,
) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            pollis_schema::authz::GROUP_ROLE_SQL,
            libsql::params![group_id.to_string(), user_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(row.get::<String>(0)?),
        None => None,
    })
}

/// True when the actor is a current member of `group_id` (re-derived server-side).
pub(crate) async fn is_member(
    conn: &Connection,
    group_id: &str,
    user_id: &str,
) -> anyhow::Result<bool> {
    Ok(group_role(conn, group_id, user_id).await?.is_some())
}

/// True when the actor is a current admin of `group_id` (re-derived server-side).
pub(crate) async fn is_admin(conn: &Connection, group_id: &str, user_id: &str) -> anyhow::Result<bool> {
    Ok(group_role(conn, group_id, user_id).await?.as_deref() == Some("admin"))
}

/// True when `group_id` names a real group. Needed because connections run
/// with `foreign_keys=OFF`, so `REFERENCES groups(id)` rejects nothing (#917).
pub(crate) async fn group_exists(conn: &Connection, group_id: &str) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            pollis_schema::authz::GROUP_EXISTS_SQL,
            libsql::params![group_id.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// The group that owns a channel, or `None` if the channel doesn't exist.
async fn channel_group_id(conn: &Connection, channel_id: &str) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT group_id FROM channels WHERE id = ?1",
            libsql::params![channel_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(row.get::<String>(0)?),
        None => None,
    })
}

/// Add `user_id` to `group_id` as a plain member and seed their per-(channel,
/// device) watermarks. The one member-add shared by invite accept,
/// join-request approve and link redemption, so all three land identical rows. Takes a bare [`Connection`] so it composes inside a
/// transaction.
async fn add_member_rows(conn: &Connection, group_id: &str, user_id: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO group_member (group_id, user_id, role) VALUES (?1, ?2, 'member')",
        libsql::params![group_id.to_string(), user_id.to_string()],
    )
    .await?;
    // Seed watermarks so pre-join messages don't block envelope cleanup.
    // Best-effort. Revoked devices are excluded (#685); `reported_at` marks the
    // seeded device live as of the join (#720). See `seeded_watermark_cursor` for
    // why the two columns differ in format (#908).
    let _ = conn
        .execute(
            "INSERT OR IGNORE INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at, last_seq, reported_at)
             SELECT c.id, ?1, ud.device_id, ?3,
                    COALESCE((SELECT next_seq FROM conversation_seq WHERE conversation_id = c.id), 0),
                    datetime('now')
             FROM channels c
             JOIN user_device ud ON ud.user_id = ?1 AND ud.revoked_at IS NULL
             WHERE c.group_id = ?2",
            libsql::params![
                user_id.to_string(),
                group_id.to_string(),
                crate::messages::seeded_watermark_cursor()
            ],
        )
        .await;
    Ok(())
}

pub async fn create_group(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<CreateGroupBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<CreateGroupBody>(apply_create_group(&conn, authed.as_deref(), &parsed).await?)
}

/// Create the group, its creator's admin membership and any default channels
/// in one transaction. A signed request may only create a group it owns.
pub async fn apply_create_group(
    conn: &Connection,
    authed: Option<&str>,
    body: &CreateGroupBody,
) -> anyhow::Result<WriteOutcome> {
    let owner = match resolve_actor(authed, body.owner_id.as_deref()) {
        Ok(o) => o,
        Err(o) => return Ok(o),
    };
    // Every id in the body is attacker-chosen, the default-channel ids included
    // (#880).
    let mut claims: Vec<(&str, ConversationKind)> =
        vec![(body.id.as_str(), ConversationKind::Group)];
    for channel_id in [
        body.default_text_channel_id.as_deref(),
        body.default_voice_channel_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        claims.push((channel_id, ConversationKind::Channel));
    }
    for (i, (id, _)) in claims.iter().enumerate() {
        // An id already used by another conversation kind would grant membership of
        // it, since `is_member` ORs across dm/group/channel on one id.
        if conversation_id_taken(conn, id).await? {
            return Ok(WriteOutcome::Forbidden);
        }
        // A request naming the same id twice is malformed; answer 403 rather than
        // let the registry's primary key fail.
        if claims[..i].iter().any(|(seen, _)| seen == id) {
            return Ok(WriteOutcome::Forbidden);
        }
    }

    // #1095: a group name reaches every member's notification banner.
    if !crate::profile::is_valid_display_name(&body.name) {
        return Ok(WriteOutcome::Forbidden);
    }

    let tx = conn.transaction().await?;
    // Claim in the same transaction so a refused claim aborts the creation.
    for (id, kind) in &claims {
        claim_conversation_id(&tx, id, *kind).await?;
    }
    tx.execute(
        "INSERT INTO groups (id, name, description, owner_id, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        libsql::params![
            body.id.clone(),
            body.name.clone(),
            body.description.clone(),
            owner.clone(),
            body.created_at.clone(),
        ],
    )
    .await?;
    tx.execute(
        "INSERT INTO group_member (group_id, user_id, role) VALUES (?1, ?2, 'admin')",
        libsql::params![body.id.clone(), owner.clone()],
    )
    .await?;
    if let Some(text_id) = &body.default_text_channel_id {
        tx.execute(
            "INSERT INTO channels (id, group_id, name, description, channel_type) \
             VALUES (?1, ?2, 'General', NULL, 'text')",
            libsql::params![text_id.clone(), body.id.clone()],
        )
        .await?;
    }
    if let Some(voice_id) = &body.default_voice_channel_id {
        tx.execute(
            "INSERT INTO channels (id, group_id, name, description, channel_type) \
             VALUES (?1, ?2, 'Voice Chat', NULL, 'voice')",
            libsql::params![voice_id.clone(), body.id.clone()],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

pub async fn update_group(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<UpdateGroupBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    // Answer with the updated row (#987), so the client needs no read-back that
    // could observe a later concurrent update.
    match apply_update_group(&conn, authed.as_deref(), &parsed).await? {
        WriteOutcome::Forbidden => Ok(AuthRejection::Forbidden.into_response()),
        WriteOutcome::EpochBehind {
            head_generation,
            head_epoch,
        } => Ok(crate::writes::epoch_behind_response(head_generation, head_epoch)),
        WriteOutcome::Invalid(msg) => Ok(crate::writes::bad_request(msg)),
        WriteOutcome::Ok => match crate::directory::group_row(&conn, &parsed.group_id).await? {
            Some(g) => Ok(crate::writes::ok_response::<UpdateGroupBody>(
                UpdatedGroup::Ok {
                    id: g.id,
                    name: g.name,
                    description: g.description,
                    owner_id: g.owner_id,
                    created_at: g.created_at,
                },
            )),
            None => Err(AppError(anyhow::anyhow!(
                "group {} vanished between its update and its read-back",
                parsed.group_id
            ))),
        },
    }
}

/// Update a group's mutable settings. Authz: the actor is a re-derived admin.
pub async fn apply_update_group(
    conn: &Connection,
    authed: Option<&str>,
    body: &UpdateGroupBody,
) -> anyhow::Result<WriteOutcome> {
    let requester = match resolve_actor(authed, body.requester_id.as_deref()) {
        Ok(r) => r,
        Err(o) => return Ok(o),
    };
    if authed.is_some() && !is_admin(conn, &body.group_id, &requester).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    if let Some(n) = &body.name {
        // #1095: same bound on rename as on create, or the rule is a formality.
        if !crate::profile::is_valid_display_name(n) {
            return Ok(WriteOutcome::Forbidden);
        }
        conn.execute(
            "UPDATE groups SET name = ?1 WHERE id = ?2",
            libsql::params![n.clone(), body.group_id.clone()],
        )
        .await?;
    }
    if let Some(d) = &body.description {
        conn.execute(
            "UPDATE groups SET description = ?1 WHERE id = ?2",
            libsql::params![d.clone(), body.group_id.clone()],
        )
        .await?;
    }
    if let Some(u) = &body.icon_url {
        conn.execute(
            "UPDATE groups SET icon_url = ?1 WHERE id = ?2",
            libsql::params![u.clone(), body.group_id.clone()],
        )
        .await?;
    }
    Ok(WriteOutcome::Ok)
}

pub async fn delete_group(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<DeleteGroupBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let (outcome, dead_conversations) =
        apply_delete_group(&conn, authed.as_deref(), &parsed).await?;
    if matches!(outcome, WriteOutcome::Ok) && !dead_conversations.is_empty() {
        let log_conn = state.log_db.conn().await?;
        crate::teardown::purge_conversation_log(&log_conn, &dead_conversations).await?;
    }
    outcome_response::<DeleteGroupBody>(outcome)
}

/// Delete a group and everything belonging to it, in one transaction. Authz:
/// admin. Teardown is explicit because Turso has `foreign_keys=OFF`, so no
/// cascade fires; see [`crate::teardown::purge_group`].
///
/// Returns the destroyed conversation ids for the post-commit commit-log purge.
pub async fn apply_delete_group(
    conn: &Connection,
    authed: Option<&str>,
    body: &DeleteGroupBody,
) -> anyhow::Result<(WriteOutcome, Vec<String>)> {
    let requester = match resolve_actor(authed, body.requester_id.as_deref()) {
        Ok(r) => r,
        Err(o) => return Ok((o, Vec::new())),
    };
    if authed.is_some() && !is_admin(conn, &body.group_id, &requester).await? {
        return Ok((WriteOutcome::Forbidden, Vec::new()));
    }
    let tx = conn.transaction().await?;
    let mut dead = crate::teardown::purge_group(&tx, &body.group_id).await?;
    tx.commit().await?;
    dead.push(body.group_id.clone());
    Ok((WriteOutcome::Ok, dead))
}

pub async fn leave_group(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<LeaveGroupBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    // Read the rooms first: tearing down an empty group deletes the `channels`
    // rows that say which voice rooms to evict the leaver from.
    let rooms = crate::broker::group_rooms(&conn, &parsed.group_id).await?;
    let (outcome, dead_conversations) =
        apply_leave_group(&conn, authed.as_deref(), &parsed).await?;
    if matches!(outcome, WriteOutcome::Ok) && !dead_conversations.is_empty() {
        let log_conn = state.log_db.conn().await?;
        crate::teardown::purge_conversation_log(&log_conn, &dead_conversations).await?;
    }
    // Leaving deletes the membership row, not the LiveKit session it authorized.
    if matches!(outcome, WriteOutcome::Ok) {
        if let Some(user) = authed.clone().or_else(|| parsed.user_id.clone()) {
            crate::broker::evict_user_from_rooms(&state, &rooms, &user).await;
        }
    }
    outcome_response::<LeaveGroupBody>(outcome)
}

/// Remove the actor's own membership; if the group is now empty, tear it down.
/// A signed request may only remove its own row.
pub async fn apply_leave_group(
    conn: &Connection,
    authed: Option<&str>,
    body: &LeaveGroupBody,
) -> anyhow::Result<(WriteOutcome, Vec<String>)> {
    let user = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(u) => u,
        Err(o) => return Ok((o, Vec::new())),
    };
    if authed.is_some() && group_role(conn, &body.group_id, &user).await?.is_none() {
        return Ok((WriteOutcome::Forbidden, Vec::new()));
    }
    let tx = conn.transaction().await?;
    tx.execute(
        "DELETE FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![body.group_id.clone(), user.clone()],
    )
    .await?;
    // The leaver's channel mutes (#1431) go with the membership.
    crate::teardown::purge_member_channel_mutes(&tx, &body.group_id, &user).await?;
    let mut survivors = tx
        .query(
            "SELECT 1 FROM group_member WHERE group_id = ?1 LIMIT 1",
            libsql::params![body.group_id.clone()],
        )
        .await?;
    let group_is_empty = survivors.next().await?.is_none();
    drop(survivors);
    let mut dead: Vec<String> = Vec::new();
    if group_is_empty {
        dead = crate::teardown::purge_group(&tx, &body.group_id).await?;
        dead.push(body.group_id.clone());
    }
    tx.commit().await?;
    Ok((WriteOutcome::Ok, dead))
}

pub async fn create_channel(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<CreateChannelBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<CreateChannelBody>(apply_create_channel(&conn, authed.as_deref(), &parsed).await?)
}

/// INSERT a channel. Authz: the actor is a current member of the owning group.
pub async fn apply_create_channel(
    conn: &Connection,
    authed: Option<&str>,
    body: &CreateChannelBody,
) -> anyhow::Result<WriteOutcome> {
    let creator = match resolve_actor(authed, body.creator_id.as_deref()) {
        Ok(c) => c,
        Err(o) => return Ok(o),
    };
    if authed.is_some() && group_role(conn, &body.group_id, &creator).await?.is_none() {
        return Ok(WriteOutcome::Forbidden);
    }
    // An id already used by another conversation kind would grant membership of
    // it (see `conversation_id_taken`).
    if conversation_id_taken(conn, &body.id).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    // Claim and insert commit together or not at all (#880).
    let tx = conn.transaction().await?;
    claim_conversation_id(&tx, &body.id, ConversationKind::Channel).await?;
    tx.execute(
        "INSERT INTO channels (id, group_id, name, description, channel_type) VALUES (?1, ?2, ?3, ?4, ?5)",
        libsql::params![
            body.id.clone(),
            body.group_id.clone(),
            body.name.clone(),
            body.description.clone(),
            body.channel_type.clone(),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

pub async fn update_channel(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<UpdateChannelBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    match apply_update_channel(&conn, authed.as_deref(), &parsed).await? {
        WriteOutcome::Forbidden => Ok(AuthRejection::Forbidden.into_response()),
        WriteOutcome::EpochBehind {
            head_generation,
            head_epoch,
        } => Ok(crate::writes::epoch_behind_response(head_generation, head_epoch)),
        WriteOutcome::Invalid(msg) => Ok(crate::writes::bad_request(msg)),
        WriteOutcome::Ok => match channel_row(&conn, &parsed.channel_id).await? {
            Some(c) => Ok(crate::writes::ok_response::<UpdateChannelBody>(c)),
            None => Err(AppError(anyhow::anyhow!(
                "channel {} vanished between its update and its read-back",
                parsed.channel_id
            ))),
        },
    }
}

/// One channel row, in the shape `/v1/channels/update` answers with.
async fn channel_row(
    conn: &Connection,
    channel_id: &str,
) -> anyhow::Result<Option<UpdatedChannel>> {
    let mut rows = conn
        .query(
            "SELECT id, group_id, name, description, channel_type FROM channels WHERE id = ?1",
            libsql::params![channel_id.to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(UpdatedChannel::Ok {
            id: row.get(0)?,
            group_id: row.get(1)?,
            name: row.get(2)?,
            description: row.get(3)?,
            channel_type: row
                .get::<Option<String>>(4)?
                .unwrap_or_else(|| "text".to_string()),
        })),
        None => Ok(None),
    }
}

/// Update a channel's name/description. Authz: admin of the owning group.
pub async fn apply_update_channel(
    conn: &Connection,
    authed: Option<&str>,
    body: &UpdateChannelBody,
) -> anyhow::Result<WriteOutcome> {
    let requester = match resolve_actor(authed, body.requester_id.as_deref()) {
        Ok(r) => r,
        Err(o) => return Ok(o),
    };
    let group_id = match channel_group_id(conn, &body.channel_id).await? {
        Some(g) => g,
        None => return Ok(WriteOutcome::Forbidden),
    };
    if authed.is_some() && !is_admin(conn, &group_id, &requester).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    if let Some(n) = &body.name {
        conn.execute(
            "UPDATE channels SET name = ?1 WHERE id = ?2",
            libsql::params![n.clone(), body.channel_id.clone()],
        )
        .await?;
    }
    if let Some(d) = &body.description {
        conn.execute(
            "UPDATE channels SET description = ?1 WHERE id = ?2",
            libsql::params![d.clone(), body.channel_id.clone()],
        )
        .await?;
    }
    Ok(WriteOutcome::Ok)
}

pub async fn delete_channel(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<DeleteChannelBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let outcome = apply_delete_channel(&conn, authed.as_deref(), &parsed).await?;
    if matches!(outcome, WriteOutcome::Ok) {
        let log_conn = state.log_db.conn().await?;
        crate::teardown::purge_conversation_log(
            &log_conn,
            std::slice::from_ref(&parsed.channel_id),
        )
        .await?;
    }
    outcome_response::<DeleteChannelBody>(outcome)
}

/// Delete a channel and its whole message history in one transaction. Authz:
/// admin of the owning group. Nothing cascades on Turso, so
/// [`crate::teardown::purge_conversation_rows`] removes the dependent rows.
pub async fn apply_delete_channel(
    conn: &Connection,
    authed: Option<&str>,
    body: &DeleteChannelBody,
) -> anyhow::Result<WriteOutcome> {
    let requester = match resolve_actor(authed, body.requester_id.as_deref()) {
        Ok(r) => r,
        Err(o) => return Ok(o),
    };
    let group_id = match channel_group_id(conn, &body.channel_id).await? {
        Some(g) => g,
        None => return Ok(WriteOutcome::Forbidden),
    };
    if authed.is_some() && !is_admin(conn, &group_id, &requester).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    let tx = conn.transaction().await?;
    crate::teardown::purge_conversation_rows(&tx, &body.channel_id).await?;
    tx.execute(
        "DELETE FROM channels WHERE id = ?1",
        libsql::params![body.channel_id.clone()],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

pub async fn remove_member(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<RemoveMemberBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let outcome = apply_remove_member(&conn, authed.as_deref(), &parsed).await?;
    // A LiveKit token is checked only at join, so an ex-member's open connection
    // must be evicted explicitly (#18).
    if matches!(outcome, WriteOutcome::Ok) {
        let rooms = crate::broker::group_rooms(&conn, &parsed.group_id).await?;
        crate::broker::evict_user_from_rooms(&state, &rooms, &parsed.user_id).await;
    }
    outcome_response::<RemoveMemberBody>(outcome)
}

/// Remove a member. Authz: the actor removes themselves (leave) OR is a
/// re-derived admin. A non-admin can never remove anyone but themselves.
pub async fn apply_remove_member(
    conn: &Connection,
    authed: Option<&str>,
    body: &RemoveMemberBody,
) -> anyhow::Result<WriteOutcome> {
    let requester = match resolve_actor(authed, body.requester_id.as_deref()) {
        Ok(r) => r,
        Err(o) => return Ok(o),
    };
    if authed.is_some()
        && requester != body.user_id
        && !is_admin(conn, &body.group_id, &requester).await?
    {
        return Ok(WriteOutcome::Forbidden);
    }
    let tx = conn.transaction().await?;
    tx.execute(
        "DELETE FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![body.group_id.clone(), body.user_id.clone()],
    )
    .await?;
    // The removed member's channel mutes (#1431) go with the membership.
    crate::teardown::purge_member_channel_mutes(&tx, &body.group_id, &body.user_id).await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

pub async fn set_member_role(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<SetMemberRoleBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<SetMemberRoleBody>(apply_set_member_role(&conn, authed.as_deref(), &parsed).await?)
}

/// Promote/demote a member. Authz: the actor is a re-derived admin, the target
/// is a current member, and the new role is valid (`admin` / `member`).
pub async fn apply_set_member_role(
    conn: &Connection,
    authed: Option<&str>,
    body: &SetMemberRoleBody,
) -> anyhow::Result<WriteOutcome> {
    if body.role != "admin" && body.role != "member" {
        return Ok(WriteOutcome::Forbidden);
    }
    let requester = match resolve_actor(authed, body.requester_id.as_deref()) {
        Ok(r) => r,
        Err(o) => return Ok(o),
    };
    if authed.is_some() && !is_admin(conn, &body.group_id, &requester).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    if group_role(conn, &body.group_id, &body.user_id)
        .await?
        .is_none()
    {
        return Ok(WriteOutcome::Forbidden);
    }
    conn.execute(
        "UPDATE group_member SET role = ?1 WHERE group_id = ?2 AND user_id = ?3",
        libsql::params![body.role.clone(), body.group_id.clone(), body.user_id.clone()],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

pub async fn create_invite(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<CreateInviteBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let answer = match apply_create_invite(&conn, authed.as_deref(), &parsed).await? {
        InviteOutcome::Created {
            invitee_id,
            inviter_username,
            group_name,
        } => InviteCreated::Ok {
            invitee_id,
            inviter_username,
            group_name,
        },
        InviteOutcome::NoSuchUser => InviteCreated::NoSuchUser,
        InviteOutcome::SelfInvite => InviteCreated::SelfInvite,
        InviteOutcome::Blocked => InviteCreated::Blocked,
        InviteOutcome::AlreadyMember => InviteCreated::AlreadyMember,
        InviteOutcome::AlreadyInvited => InviteCreated::AlreadyInvited,
        InviteOutcome::Forbidden => return Ok(AuthRejection::Forbidden.into_response()),
    };
    Ok(crate::writes::ok_response::<CreateInviteBody>(answer))
}

/// Resolve an invitee by username or email, check every precondition, and
/// insert the pending invite. Authz: the inviter is a re-derived admin. The
/// checks live here rather than in client reads so none can go stale before
/// the insert (#987).
pub async fn apply_create_invite(
    conn: &Connection,
    authed: Option<&str>,
    body: &CreateInviteBody,
) -> anyhow::Result<InviteOutcome> {
    let inviter = match resolve_actor(authed, body.inviter_id.as_deref()) {
        Ok(i) => i,
        Err(_) => return Ok(InviteOutcome::Forbidden),
    };
    if authed.is_some() && !is_admin(conn, &body.group_id, &inviter).await? {
        return Ok(InviteOutcome::Forbidden);
    }

    // Exact match on email if it contains `@`, else on username; never `username
    // OR email` (see `directory::user_by_identifier`).
    let Some(invitee) = crate::directory::user_by_identifier(conn, &body.invitee_identifier).await?
    else {
        return Ok(InviteOutcome::NoSuchUser);
    };
    let invitee_id = invitee.id;

    if invitee_id == inviter {
        return Ok(InviteOutcome::SelfInvite);
    }
    // A block answers exactly as a missing user does (L3), so the inviter cannot
    // learn that this account blocked them. `InviteCreated::Blocked` stays on the
    // wire for shipped clients but is never produced.
    if crate::profile::is_blocked_either_way(conn, &inviter, &invitee_id).await? {
        return Ok(InviteOutcome::NoSuchUser);
    }
    if is_member(conn, &body.group_id, &invitee_id).await? {
        return Ok(InviteOutcome::AlreadyMember);
    }

    // The duplicate check is the INSERT's own row count, so a concurrent invite
    // cannot slip between check and write. A guard rather than `ON CONFLICT`
    // because adding a UNIQUE index on `(group_id, invitee_id)` would be a
    // tightening migration.
    let inserted = conn
        .execute(
            "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) \
             SELECT ?1, ?2, ?3, ?4 \
             WHERE NOT EXISTS ( \
               SELECT 1 FROM group_invite WHERE group_id = ?2 AND invitee_id = ?4 \
             )",
            libsql::params![
                body.id.clone(),
                body.group_id.clone(),
                inviter.clone(),
                invitee_id.clone(),
            ],
        )
        .await?;
    if inserted == 0 {
        return Ok(InviteOutcome::AlreadyInvited);
    }

    let (inviter_username, group_name) = invite_alert_names(conn, &body.group_id, &inviter).await?;
    Ok(InviteOutcome::Created {
        invitee_id,
        inviter_username,
        group_name,
    })
}

/// What `apply_create_invite` decided; the refusals are distinct so the client
/// can tell the user which one applies.
#[derive(Debug)]
pub enum InviteOutcome {
    Created {
        invitee_id: String,
        inviter_username: Option<String>,
        group_name: Option<String>,
    },
    NoSuchUser,
    SelfInvite,
    Blocked,
    AlreadyMember,
    AlreadyInvited,
    Forbidden,
}

/// The inviter's username and the group's name for the invitee's alert (#396).
async fn invite_alert_names(
    conn: &Connection,
    group_id: &str,
    inviter_id: &str,
) -> anyhow::Result<(Option<String>, Option<String>)> {
    let mut rows = conn
        .query(
            "SELECT u.username, g.name FROM groups g LEFT JOIN users u ON u.id = ?2 WHERE g.id = ?1",
            libsql::params![group_id.to_string(), inviter_id.to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok((row.get(0)?, row.get(1)?)),
        None => Ok((None, None)),
    }
}

pub async fn accept_invite(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<AcceptInviteBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    // Name the admitted group (#987). Read before `apply_accept_invite` consumes
    // the row.
    let group_id = accepted_invite_group(&conn, authed.as_deref(), &parsed).await?;
    match apply_accept_invite(&conn, authed.as_deref(), &parsed).await? {
        WriteOutcome::Forbidden => Ok(AuthRejection::Forbidden.into_response()),
        WriteOutcome::EpochBehind {
            head_generation,
            head_epoch,
        } => Ok(crate::writes::epoch_behind_response(head_generation, head_epoch)),
        WriteOutcome::Invalid(msg) => Ok(crate::writes::bad_request(msg)),
        WriteOutcome::Ok => match group_id {
            Some(group_id) => Ok(crate::writes::ok_response::<AcceptInviteBody>(
                AcceptedInvite::Ok { group_id },
            )),
            // `apply_accept_invite` answers Forbidden when the row is gone, so reaching
            // here means it vanished between the two reads.
            None => Err(AppError(anyhow::anyhow!(
                "invite {} resolved to no group",
                parsed.invite_id
            ))),
        },
    }
}

/// The group an invite addressed to the actor names, read BEFORE the accept
/// consumes the row.
async fn accepted_invite_group(
    conn: &Connection,
    authed: Option<&str>,
    body: &AcceptInviteBody,
) -> anyhow::Result<Option<String>> {
    let Ok(user) = resolve_actor(authed, body.user_id.as_deref()) else {
        return Ok(None);
    };
    let mut rows = conn
        .query(
            "SELECT group_id FROM group_invite WHERE id = ?1 AND invitee_id = ?2",
            libsql::params![body.invite_id.clone(), user],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row.get::<String>(0)?)),
        None => Ok(None),
    }
}

/// Accept an invite: add the actor as a member and delete the invite, in one
/// transaction. The invite must be addressed to the signer.
pub async fn apply_accept_invite(
    conn: &Connection,
    authed: Option<&str>,
    body: &AcceptInviteBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    let mut rows = conn
        .query(
            "SELECT group_id FROM group_invite WHERE id = ?1 AND invitee_id = ?2",
            libsql::params![body.invite_id.clone(), user.clone()],
        )
        .await?;
    let group_id: String = match rows.next().await? {
        Some(row) => row.get(0)?,
        None => return Ok(WriteOutcome::Forbidden),
    };
    drop(rows);

    let tx = conn.transaction().await?;
    add_member_rows(&tx, &group_id, &user).await?;
    tx.execute(
        "DELETE FROM group_invite WHERE id = ?1",
        libsql::params![body.invite_id.clone()],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

pub async fn decline_invite(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<DeclineInviteBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<DeclineInviteBody>(apply_decline_invite(&conn, authed.as_deref(), &parsed).await?)
}

/// Decline an invite. The DELETE is scoped `invitee_id = :actor`, so a signer can
/// only ever decline their own invite.
pub async fn apply_decline_invite(
    conn: &Connection,
    authed: Option<&str>,
    body: &DeclineInviteBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    conn.execute(
        "DELETE FROM group_invite WHERE id = ?1 AND invitee_id = ?2",
        libsql::params![body.invite_id.clone(), user],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

pub async fn revoke_invite(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<RevokeInviteBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let target = revoked_invite_target(&conn, &parsed.invite_id).await?;
    let outcome = apply_revoke_invite(&conn, authed.as_deref(), &parsed).await?;
    if let (WriteOutcome::Ok, Some((invitee, group_id))) = (&outcome, target) {
        // The DS sends this because once the row is gone the revoker shares no roster
        // with the invitee, so the broker would refuse the revoker's own publish.
        // Best-effort: the invitee re-reads on its next fetch, and accepting a revoked
        // invite is refused.
        let inbox = format!("inbox-{invitee}");
        let event = serde_json::json!({ "type": "membership_changed", "group_id": group_id });
        if let Err(e) = crate::broker::room_send_data(&state, &inbox, &event).await {
            tracing::warn!("revoked-invite inbox notify failed (non-fatal): {e}");
        }
    }
    outcome_response::<RevokeInviteBody>(outcome)
}

/// `(invitee_id, group_id)` of an invite, if it still exists.
async fn revoked_invite_target(
    conn: &Connection,
    invite_id: &str,
) -> anyhow::Result<Option<(String, String)>> {
    let mut rows = conn
        .query(
            "SELECT invitee_id, group_id FROM group_invite WHERE id = ?1",
            libsql::params![invite_id.to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some((row.get(0)?, row.get(1)?))),
        None => Ok(None),
    }
}

/// Withdraw a pending invite (#1506). Authz: the actor is a member of the
/// invite's group and either sent the invite or is an admin; group and inviter
/// come from the row, never the request.
///
/// Deleting the row removes the invitee from the desired MLS roster
/// ([`crate::directory::desired_roster`]), so the revoker's next reconcile
/// removes their leaves. An already-gone invite answers Forbidden.
pub async fn apply_revoke_invite(
    conn: &Connection,
    authed: Option<&str>,
    body: &RevokeInviteBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.actor_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    let mut rows = conn
        .query(
            "SELECT group_id, inviter_id FROM group_invite WHERE id = ?1",
            libsql::params![body.invite_id.clone()],
        )
        .await?;
    let (group_id, inviter_id): (String, String) = match rows.next().await? {
        Some(row) => (row.get(0)?, row.get(1)?),
        None => return Ok(WriteOutcome::Forbidden),
    };
    drop(rows);

    if authed.is_some() {
        let allowed = match group_role(conn, &group_id, &actor).await? {
            Some(role) => role == "admin" || inviter_id == actor,
            None => false,
        };
        if !allowed {
            return Ok(WriteOutcome::Forbidden);
        }
    }
    conn.execute(
        "DELETE FROM group_invite WHERE id = ?1 AND group_id = ?2",
        libsql::params![body.invite_id.clone(), group_id],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

pub async fn create_join_request(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<CreateJoinRequestBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let answer = match apply_create_join_request(&conn, authed.as_deref(), &parsed).await? {
        JoinRequestOutcome::Ok => JoinRequestCreated::Ok,
        JoinRequestOutcome::NoSuchGroup => JoinRequestCreated::NoSuchGroup,
        JoinRequestOutcome::AlreadyMember => JoinRequestCreated::AlreadyMember,
        JoinRequestOutcome::AlreadyPending => JoinRequestCreated::AlreadyPending,
        JoinRequestOutcome::Forbidden => return Ok(AuthRejection::Forbidden.into_response()),
    };
    Ok(crate::writes::ok_response::<CreateJoinRequestBody>(answer))
}

/// Upsert a pending join request (resetting a prior rejected/approved row).
/// Authz: the actor requests for themselves, for a group that exists, while
/// not already a member. The DS must re-derive the last two itself; the client
/// preflight is not a check (#917).
pub async fn apply_create_join_request(
    conn: &Connection,
    authed: Option<&str>,
    body: &CreateJoinRequestBody,
) -> anyhow::Result<JoinRequestOutcome> {
    let requester = match resolve_actor(authed, body.requester_id.as_deref()) {
        Ok(r) => r,
        Err(_) => return Ok(JoinRequestOutcome::Forbidden),
    };

    // Checked explicitly because `foreign_keys=OFF`.
    if !group_exists(conn, &body.group_id).await? {
        return Ok(JoinRequestOutcome::NoSuchGroup);
    }

    // Approving a request from an existing member would re-run `add_member_rows`.
    if is_member(conn, &body.group_id, &requester).await? {
        return Ok(JoinRequestOutcome::AlreadyMember);
    }

    // The `WHERE status <> 'pending'` guard leaves a live request untouched, and a
    // zero row count is the "already pending" answer (#987).
    let touched = conn
        .execute(
            "INSERT INTO group_join_request (id, group_id, requester_id, status, created_at)
             VALUES (?1, ?2, ?3, 'pending', datetime('now'))
             ON CONFLICT(group_id, requester_id) DO UPDATE SET
                 id         = excluded.id,
                 status     = 'pending',
                 created_at = excluded.created_at
             WHERE group_join_request.status <> 'pending'",
            libsql::params![body.id.clone(), body.group_id.clone(), requester],
        )
        .await?;
    if touched == 0 {
        return Ok(JoinRequestOutcome::AlreadyPending);
    }
    Ok(JoinRequestOutcome::Ok)
}

/// What `apply_create_join_request` decided. `Forbidden` is only for an
/// unresolvable actor.
#[derive(Debug)]
pub enum JoinRequestOutcome {
    Ok,
    NoSuchGroup,
    AlreadyMember,
    AlreadyPending,
    Forbidden,
}

pub async fn approve_join_request(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<ApproveJoinRequestBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    review_response::<ApproveJoinRequestBody>(
        apply_approve_join_request(&conn, authed.as_deref(), &parsed).await?,
    )
}

/// Approve a pending join request: add the requester as a member and flip the row
/// to `approved`, in one transaction. Authz: the approver is a re-derived admin.
pub async fn apply_approve_join_request(
    conn: &Connection,
    authed: Option<&str>,
    body: &ApproveJoinRequestBody,
) -> anyhow::Result<ReviewOutcome> {
    let approver = match resolve_actor(authed, body.approver_id.as_deref()) {
        Ok(a) => a,
        Err(_) => return Ok(ReviewOutcome::Forbidden),
    };
    let mut rows = conn
        .query(
            "SELECT group_id, requester_id FROM group_join_request WHERE id = ?1 AND status = 'pending'",
            libsql::params![body.request_id.clone()],
        )
        .await?;
    let (group_id, requester_id): (String, String) = match rows.next().await? {
        Some(row) => (row.get(0)?, row.get(1)?),
        None => return Ok(ReviewOutcome::Forbidden),
    };
    drop(rows);

    if authed.is_some() && !is_admin(conn, &group_id, &approver).await? {
        return Ok(ReviewOutcome::Forbidden);
    }

    let tx = conn.transaction().await?;
    add_member_rows(&tx, &group_id, &requester_id).await?;
    tx.execute(
        "UPDATE group_join_request SET status = 'approved', reviewed_by = ?1, reviewed_at = ?2 WHERE id = ?3",
        libsql::params![approver, body.reviewed_at.clone(), body.request_id.clone()],
    )
    .await?;
    tx.commit().await?;
    // Report the resolved group so the caller needs no separate read (#987).
    Ok(ReviewOutcome::Reviewed {
        group_id,
        requester_id: Some(requester_id),
    })
}

/// What a join-request review resolved. `Forbidden` covers both "no such
/// pending request" and "not an admin": telling them apart would make a
/// request id a probe for other people's groups.
#[derive(Debug)]
pub enum ReviewOutcome {
    Reviewed {
        group_id: String,
        requester_id: Option<String>,
    },
    Forbidden,
}

pub async fn reject_join_request(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<RejectJoinRequestBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    review_response::<RejectJoinRequestBody>(
        apply_reject_join_request(&conn, authed.as_deref(), &parsed).await?,
    )
}

/// Map a [`ReviewOutcome`] to the HTTP response, generic over the request type
/// so neither review endpoint can answer the other's body.
fn review_response<B>(outcome: ReviewOutcome) -> Result<Response, AppError>
where
    B: pollis_api::DsRequest<Response = ReviewedJoinRequest>,
{
    Ok(match outcome {
        ReviewOutcome::Reviewed {
            group_id,
            requester_id,
        } => crate::writes::ok_response::<B>(ReviewedJoinRequest::Ok {
            group_id,
            requester_id,
        }),
        ReviewOutcome::Forbidden => AuthRejection::Forbidden.into_response(),
    })
}

/// Reject a pending join request. Authz: the approver is a re-derived admin.
pub async fn apply_reject_join_request(
    conn: &Connection,
    authed: Option<&str>,
    body: &RejectJoinRequestBody,
) -> anyhow::Result<ReviewOutcome> {
    let approver = match resolve_actor(authed, body.approver_id.as_deref()) {
        Ok(a) => a,
        Err(_) => return Ok(ReviewOutcome::Forbidden),
    };
    let mut rows = conn
        .query(
            "SELECT group_id FROM group_join_request WHERE id = ?1 AND status = 'pending'",
            libsql::params![body.request_id.clone()],
        )
        .await?;
    let group_id: String = match rows.next().await? {
        Some(row) => row.get(0)?,
        None => return Ok(ReviewOutcome::Forbidden),
    };
    drop(rows);

    if authed.is_some() && !is_admin(conn, &group_id, &approver).await? {
        return Ok(ReviewOutcome::Forbidden);
    }
    conn.execute(
        "UPDATE group_join_request SET status = 'rejected', reviewed_by = ?1, reviewed_at = ?2 WHERE id = ?3",
        libsql::params![approver, body.reviewed_at.clone(), body.request_id.clone()],
    )
    .await?;
    Ok(ReviewOutcome::Reviewed {
        group_id,
        requester_id: None,
    })
}

// Shareable invite links (#847).
//
// Redemption is not a second membership path: it adds the member through
// `add_member_rows`, like invite accept and join-request approve. Since
// `group_member` is the sole capability nothing downstream re-validates, the
// token check and the member-add must be one transaction.
//
// Every token failure (wrong, malformed, revoked, expired, exhausted) returns
// the same `Rejected`, so a valid-but-expired token is not confirmed. Rate
// limiting (429) is distinguishable because it describes the caller, not the
// token.

/// How far back the durable redemption rate limit counts failed attempts.
const REDEEM_FAILURE_WINDOW_SECS: i64 = 600;

/// Failed redemptions allowed per user within [`REDEEM_FAILURE_WINDOW_SECS`].
///
/// Keyed on the authenticated user and counted from the database, so it
/// survives restarts, is shared across instances, and binds even when an
/// attacker rotates IPs past the per-IP tier.
const REDEEM_FAILURE_MAX: i64 = 10;

pub async fn create_invite_link(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<CreateInviteLinkBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<CreateInviteLinkBody>(apply_create_invite_link(&conn, authed.as_deref(), &parsed).await?)
}

/// Insert an invite link. Authz: re-derived admin, the same bar as
/// `apply_create_invite`.
pub async fn apply_create_invite_link(
    conn: &Connection,
    authed: Option<&str>,
    body: &CreateInviteLinkBody,
) -> anyhow::Result<WriteOutcome> {
    let creator = match resolve_actor(authed, body.created_by.as_deref()) {
        Ok(c) => c,
        Err(o) => return Ok(o),
    };
    if authed.is_some() && !is_admin(conn, &body.group_id, &creator).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    // Only a sha256 hex digest, on both auth paths: a short or low-entropy value
    // would let a client plant a hash it can cheaply preimage.
    if body.secret_hash.len() != 64 || !body.secret_hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(WriteOutcome::Forbidden);
    }
    // The table CHECK rejects `max_uses = 0`; refuse it here for a 403, not a 500.
    if body.max_uses.is_some_and(|m| m <= 0) {
        return Ok(WriteOutcome::Forbidden);
    }
    conn.execute(
        "INSERT INTO group_invite_link \
           (id, group_id, created_by, selector, secret_hash, expires_at, max_uses) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        libsql::params![
            body.id.clone(),
            body.group_id.clone(),
            creator,
            body.selector.clone(),
            body.secret_hash.clone(),
            body.expires_at.clone(),
            body.max_uses,
        ],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

pub async fn revoke_invite_link(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<RevokeInviteLinkBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<RevokeInviteLinkBody>(apply_revoke_invite_link(&conn, authed.as_deref(), &parsed).await?)
}

/// Revoke a link. Authz: admin of the link's own group, resolved from the link
/// row, never the request. A one-way stamp, not a delete, so the redemption
/// audit trail keeps its target.
pub async fn apply_revoke_invite_link(
    conn: &Connection,
    authed: Option<&str>,
    body: &RevokeInviteLinkBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.actor_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    let mut rows = conn
        .query(
            "SELECT group_id FROM group_invite_link WHERE id = ?1",
            libsql::params![body.link_id.clone()],
        )
        .await?;
    let group_id: String = match rows.next().await? {
        Some(row) => row.get(0)?,
        None => return Ok(WriteOutcome::Forbidden),
    };
    drop(rows);

    if authed.is_some() && !is_admin(conn, &group_id, &actor).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    // Keep the first revocation's timestamp.
    conn.execute(
        "UPDATE group_invite_link SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL",
        libsql::params![body.revoked_at.clone(), body.link_id.clone()],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

/// The result of a redemption attempt. Every token failure is one `Rejected`
/// by design (#847).
#[derive(Debug, PartialEq, Eq)]
pub enum RedeemOutcome {
    /// The actor is now a member of this group.
    Joined {
        group_id: String,
        group_name: Option<String>,
    },
    /// The token did not yield a live link. Indistinguishable by construction.
    Rejected,
    /// The actor has failed too many times recently.
    RateLimited,
}

pub async fn redeem_invite_link(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<RedeemInviteLinkBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    Ok(
        match apply_redeem_invite_link(&conn, authed.as_deref(), &parsed).await? {
            RedeemOutcome::Joined {
                group_id,
                group_name,
            } => crate::writes::ok_response::<RedeemInviteLinkBody>(
                RedeemInviteLinkResponse::Ok {
                    group_id,
                    group_name,
                },
            ),
            RedeemOutcome::Rejected => redeem_rejected(),
            RedeemOutcome::RateLimited => redeem_rate_limited(),
        },
    )
}

/// The single rejection response for every way a token can fail.
fn redeem_rejected() -> Response {
    (
        axum::http::StatusCode::FORBIDDEN,
        axum::Json(serde_json::json!({ "error": "invite_invalid" })),
    )
        .into_response()
}

fn redeem_rate_limited() -> Response {
    (
        axum::http::StatusCode::TOO_MANY_REQUESTS,
        axum::Json(serde_json::json!({ "error": "too_many_attempts" })),
    )
        .into_response()
}

/// Redeem an invite link. Authz: the authenticated caller; a link removes the
/// need for an admin to know your username, not the need for an identity.
///
/// Every time comparison uses the server clock (`datetime('now')`), never
/// `body.now` (H3): a caller-chosen clock redeems expired links and empties
/// the rate-limit window. `body.now` stays on the wire for shipped clients.
pub async fn apply_redeem_invite_link(
    conn: &Connection,
    authed: Option<&str>,
    body: &RedeemInviteLinkBody,
) -> anyhow::Result<RedeemOutcome> {
    let user = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(u) => u,
        Err(_) => return Ok(RedeemOutcome::Rejected),
    };

    // Durable rate limit counted from the audit table, so a restart grants no
    // fresh budget.
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM group_invite_link_redemption \
             WHERE user_id = ?1 AND succeeded = 0 \
               AND datetime(attempted_at) > datetime('now', ?2)",
            libsql::params![
                user.clone(),
                format!("-{REDEEM_FAILURE_WINDOW_SECS} seconds"),
            ],
        )
        .await?;
    let recent_failures: i64 = match rows.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    };
    drop(rows);
    if recent_failures >= REDEEM_FAILURE_MAX {
        return Ok(RedeemOutcome::RateLimited);
    }

    // A malformed token is recorded and rejected exactly like a wrong one.
    let Some((selector, secret)) = crate::invite_token::parse(&body.token) else {
        record_redemption(conn, &body.attempt_id, None, &user, false).await?;
        return Ok(RedeemOutcome::Rejected);
    };

    let mut rows = conn
        .query(
            "SELECT id, group_id, secret_hash, expires_at, max_uses, uses, revoked_at \
             FROM group_invite_link WHERE selector = ?1",
            libsql::params![selector],
        )
        .await?;
    let found = rows.next().await?;
    #[allow(clippy::type_complexity)]
    let (link_id, group_id, secret_hash, expires_at, max_uses, uses, revoked_at): (
        String,
        String,
        String,
        Option<String>,
        Option<i64>,
        i64,
        Option<String>,
    ) = match found {
        Some(row) => (
            row.get(0)?,
            row.get(1)?,
            row.get(2)?,
            row.get(3)?,
            row.get(4)?,
            row.get(5)?,
            row.get(6)?,
        ),
        None => {
            drop(rows);
            record_redemption(conn, &body.attempt_id, None, &user, false).await?;
            return Ok(RedeemOutcome::Rejected);
        }
    };
    drop(rows);

    // Evaluate every predicate, then combine: no early return between the secret
    // compare and the verdict.
    let secret_ok = crate::invite_token::hash_eq(
        &crate::invite_token::hash_secret(&secret),
        &secret_hash,
    );
    let not_revoked = revoked_at.is_none();
    let not_expired = match expires_at.as_deref() {
        Some(exp) => !expiry_passed(conn, exp).await?,
        None => true,
    };
    let uses_left = match max_uses {
        Some(max) => uses < max,
        None => true,
    };

    if !(secret_ok && not_revoked && not_expired && uses_left) {
        // Record the link id only when the secret matched, so an admin can see a live
        // link being probed.
        let probed = if secret_ok { Some(link_id.as_str()) } else { None };
        record_redemption(conn, &body.attempt_id, probed, &user, false).await?;
        return Ok(RedeemOutcome::Rejected);
    }

    // Already a member: idempotent success that burns no use.
    if is_member(conn, &group_id, &user).await? {
        let group_name = group_name(conn, &group_id).await?;
        return Ok(RedeemOutcome::Joined {
            group_id,
            group_name,
        });
    }

    let tx = conn.transaction().await?;
    add_member_rows(&tx, &group_id, &user).await?;
    // Re-check inside the transaction and let the row count decide: of two
    // concurrent redeemers of the last use only one passes. The table's
    // `uses <= max_uses` CHECK is the backstop.
    let updated = tx
        .execute(
            "UPDATE group_invite_link SET uses = uses + 1 \
             WHERE id = ?1 \
               AND revoked_at IS NULL \
               AND (expires_at IS NULL OR datetime(expires_at) > datetime('now')) \
               AND (max_uses IS NULL OR uses < max_uses)",
            libsql::params![link_id.clone()],
        )
        .await?;
    if updated == 0 {
        tx.rollback().await?;
        record_redemption(conn, &body.attempt_id, Some(&link_id), &user, false).await?;
        return Ok(RedeemOutcome::Rejected);
    }
    tx.execute(
        "INSERT INTO group_invite_link_redemption \
           (id, link_id, user_id, attempted_at, succeeded) \
         VALUES (?1, ?2, ?3, datetime('now'), 1)",
        libsql::params![body.attempt_id.clone(), link_id, user.clone()],
    )
    .await?;
    tx.commit().await?;

    let group_name = group_name(conn, &group_id).await?;
    Ok(RedeemOutcome::Joined {
        group_id,
        group_name,
    })
}

/// A group's display name; `None` if the row is gone, never a failure.
async fn group_name(conn: &Connection, group_id: &str) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT name FROM groups WHERE id = ?1",
            libsql::params![group_id.to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row.get::<String>(0)?)),
        None => Ok(None),
    }
}

/// Whether `expires_at` is at or before the server clock (H3). Compared by
/// SQLite's `datetime()` so it normalises like the guarded UPDATE does; Rust
/// string ordering would disagree for `+00:00` versus `Z`.
async fn expiry_passed(conn: &Connection, expires_at: &str) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT datetime(?1) <= datetime('now')",
            libsql::params![expires_at.to_string()],
        )
        .await?;
    // An unparseable timestamp yields NULL: fail closed as expired.
    Ok(match rows.next().await? {
        Some(row) => row.get::<Option<i64>>(0)?.unwrap_or(1) == 1,
        None => true,
    })
}

/// Append a redemption audit row, stamped with the server clock because this
/// table is also the rate-limit counter (H3). `OR IGNORE` so a replayed
/// `attempt_id` cannot error the request.
async fn record_redemption(
    conn: &Connection,
    attempt_id: &str,
    link_id: Option<&str>,
    user_id: &str,
    succeeded: bool,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO group_invite_link_redemption \
           (id, link_id, user_id, attempted_at, succeeded) \
         VALUES (?1, ?2, ?3, datetime('now'), ?4)",
        libsql::params![
            attempt_id.to_string(),
            link_id.map(|s| s.to_string()),
            user_id.to_string(),
            i64::from(succeeded),
        ],
    )
    .await?;
    Ok(())
}
