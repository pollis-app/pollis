//! Domain B — groups / channels / membership / invites / join-requests.
//!
//! The second write-domain in Goal B (#419). It copies the convention domain A
//! ([`crate::messages`]) established verbatim:
//!
//!   - **Bodies** — one `#[derive(Deserialize)] *Body` per endpoint, plain JSON.
//!   - **Pure conn-level fns** — `apply_*(conn, authed, body)` embed BOTH the
//!     authorization decision AND the write, returning [`WriteOutcome`]. The real
//!     axum handler and the in-process test harness both call the *same*
//!     `apply_*`, so server-side authz is exercised with zero duplication.
//!   - **axum handlers** — `(State, Method, Uri, HeaderMap, Bytes) -> Response`:
//!     `gate` → parse → `apply_*` → map outcome to 200 / 403 / 400 / 500.
//!
//! ## Where the writes land
//!
//! Every domain-B table (`groups`, `channels`, `group_member`, `group_invite`,
//! `group_join_request`, plus the `conversation_watermark` / `message_envelope`
//! rows a channel-delete cleans up) lives in the **MAIN DB** (`state.db`). So all
//! `apply_*` fns run on the main connection.
//!
//! ## Authorization (the security core)
//!
//! `gate` proves *which user* signed; each `apply_*` then proves they're allowed:
//!   - create group: the actor is the creator (`owner_id` bound to the signer).
//!   - create channel: the actor is a current member of the group.
//!   - update/delete channel, update/delete group, role change, member remove,
//!     invite create, join-request approve/reject: the actor's role is
//!     **re-derived server-side** from `group_member` and must be `admin` (member
//!     remove additionally allows self-removal).
//!   - invite accept/decline: the actor is the invitee (writes are scoped
//!     `invitee_id = :actor`).
//!   - leave group: the actor is a current member (removes only their own row).
//!   - join-request create: the actor is the requester.
//!
//! On the no-auth path (`authed == None`, only when `POLLIS_DS_REQUIRE_AUTH` is
//! off) the role/identity checks are skipped and the actor comes from the body,
//! mirroring `commit::submit`, `writes.rs`, and `messages.rs`.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, Uri},
    response::Response,
};
use libsql::Connection;
use serde::Deserialize;

use crate::error::AppError;
use crate::writes::{bad_request, gate, outcome_response, resolve_actor, WriteOutcome};
use crate::AppState;

// ── Shared authz helpers ─────────────────────────────────────────────────────

/// The actor's role in `group_id`, re-derived server-side from `group_member`.
/// `None` when the actor is not a member. Every admin-gated domain-B write
/// re-derives this rather than trusting any client-supplied role.
async fn group_role(
    conn: &Connection,
    group_id: &str,
    user_id: &str,
) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
            libsql::params![group_id.to_string(), user_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(row.get::<String>(0)?),
        None => None,
    })
}

/// True when the actor is a current admin of `group_id` (re-derived server-side).
async fn is_admin(conn: &Connection, group_id: &str, user_id: &str) -> anyhow::Result<bool> {
    Ok(group_role(conn, group_id, user_id).await?.as_deref() == Some("admin"))
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
/// device) watermarks. Mirrors `pollis_core`'s `add_member_to_group` byte-for-
/// byte (idempotent `INSERT OR IGNORE` + best-effort watermark seed) so accepting
/// an invite / approving a join request through the DS lands the exact same rows.
/// Takes a bare [`Connection`] so it composes inside a caller's transaction
/// (`&Transaction` derefs to `&Connection`).
async fn add_member_rows(conn: &Connection, group_id: &str, user_id: &str) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO group_member (group_id, user_id, role) VALUES (?1, ?2, 'member')",
        libsql::params![group_id.to_string(), user_id.to_string()],
    )
    .await?;
    // Seed watermark rows for every (channel, device) pair so pre-join messages
    // don't block envelope cleanup. Best-effort — mirrors the core helper, which
    // logs and continues on failure.
    let _ = conn
        .execute(
            "INSERT OR IGNORE INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at)
             SELECT c.id, ?1, ud.device_id, datetime('now')
             FROM channels c
             JOIN user_device ud ON ud.user_id = ?1
             WHERE c.group_id = ?2",
            libsql::params![user_id.to_string(), group_id.to_string()],
        )
        .await;
    // Directory index (#261): project this membership into `user_groups`. Runs
    // for both add sites (accept-invite, approve-join-request) since both go
    // through here. The group_member row was just written, so the projection
    // reads the correct role.
    crate::directory::sync_group_member(conn, group_id, user_id).await?;
    Ok(())
}

// ── POST /v1/groups/create ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateGroupBody {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// The creator; bound to the authenticated user when signed.
    #[serde(default)]
    pub owner_id: Option<String>,
    /// When present, also create a default `#General` text channel with this id.
    #[serde(default)]
    pub default_text_channel_id: Option<String>,
    /// When present, also create a default `Voice Chat` voice channel with this id.
    #[serde(default)]
    pub default_voice_channel_id: Option<String>,
    pub created_at: String,
}

pub async fn create_group(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: CreateGroupBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_create_group(&conn, authed.as_deref(), &parsed).await?)
}

/// INSERT the group, its creator's admin `group_member`, and any default
/// channels — all in one transaction. Authz: a signed request may only create a
/// group it owns (`owner_id` bound to the signer).
pub async fn apply_create_group(
    conn: &Connection,
    authed: Option<&str>,
    body: &CreateGroupBody,
) -> anyhow::Result<WriteOutcome> {
    let owner = match resolve_actor(authed, body.owner_id.as_deref()) {
        Ok(o) => o,
        Err(o) => return Ok(o),
    };
    let tx = conn.transaction().await?;
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
    // Directory index (#261): project the creator's admin membership into
    // `user_groups`. The group_member row above is in this tx, so the projection
    // sees it.
    crate::directory::sync_group_member(&tx, &body.id, &owner).await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/groups/update ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct UpdateGroupBody {
    pub group_id: String,
    #[serde(default)]
    pub requester_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub icon_url: Option<String>,
}

pub async fn update_group(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: UpdateGroupBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_update_group(&conn, authed.as_deref(), &parsed).await?)
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
        conn.execute(
            "UPDATE groups SET name = ?1 WHERE id = ?2",
            libsql::params![n.clone(), body.group_id.clone()],
        )
        .await?;
        // Directory index (#261): re-project the new name onto every member's row.
        crate::directory::rename_group(conn, &body.group_id).await?;
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

// ── POST /v1/groups/delete ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DeleteGroupBody {
    pub group_id: String,
    #[serde(default)]
    pub requester_id: Option<String>,
}

pub async fn delete_group(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: DeleteGroupBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_delete_group(&conn, authed.as_deref(), &parsed).await?)
}

/// Delete a group (CASCADE removes members/channels/invites). Authz: admin.
pub async fn apply_delete_group(
    conn: &Connection,
    authed: Option<&str>,
    body: &DeleteGroupBody,
) -> anyhow::Result<WriteOutcome> {
    let requester = match resolve_actor(authed, body.requester_id.as_deref()) {
        Ok(r) => r,
        Err(o) => return Ok(o),
    };
    if authed.is_some() && !is_admin(conn, &body.group_id, &requester).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    conn.execute(
        "DELETE FROM groups WHERE id = ?1",
        libsql::params![body.group_id.clone()],
    )
    .await?;
    // Directory index (#261): drop every member's row for this group. Explicit —
    // FK cascade is off on the DS connection.
    crate::directory::remove_group(conn, &body.group_id).await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/groups/leave ────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct LeaveGroupBody {
    pub group_id: String,
    /// The leaver; bound to the authenticated user when signed.
    #[serde(default)]
    pub user_id: Option<String>,
}

pub async fn leave_group(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: LeaveGroupBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_leave_group(&conn, authed.as_deref(), &parsed).await?)
}

/// Remove the actor's own membership; if the group is now empty, delete it.
/// Authz: the actor is a current member (a signed request may only remove its
/// OWN row — `user_id` is bound to the signer).
pub async fn apply_leave_group(
    conn: &Connection,
    authed: Option<&str>,
    body: &LeaveGroupBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    if authed.is_some() && group_role(conn, &body.group_id, &user).await?.is_none() {
        return Ok(WriteOutcome::Forbidden);
    }
    let tx = conn.transaction().await?;
    tx.execute(
        "DELETE FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![body.group_id.clone(), user.clone()],
    )
    .await?;
    // Directory index (#261): drop the leaver's row.
    crate::directory::remove_group_member(&tx, &body.group_id, &user).await?;
    let mut count_rows = tx
        .query(
            "SELECT COUNT(*) FROM group_member WHERE group_id = ?1",
            libsql::params![body.group_id.clone()],
        )
        .await?;
    let remaining: i64 = match count_rows.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    };
    drop(count_rows);
    if remaining == 0 {
        tx.execute(
            "DELETE FROM groups WHERE id = ?1",
            libsql::params![body.group_id.clone()],
        )
        .await?;
        // Group is gone — clear any remaining index rows for it.
        crate::directory::remove_group(&tx, &body.group_id).await?;
    }
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/channels/create ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateChannelBody {
    pub id: String,
    pub group_id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    pub channel_type: String,
    /// The creator; bound to the authenticated user when signed.
    #[serde(default)]
    pub creator_id: Option<String>,
}

pub async fn create_channel(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: CreateChannelBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_create_channel(&conn, authed.as_deref(), &parsed).await?)
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
    conn.execute(
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
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/channels/update ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct UpdateChannelBody {
    pub channel_id: String,
    #[serde(default)]
    pub requester_id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

pub async fn update_channel(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: UpdateChannelBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_update_channel(&conn, authed.as_deref(), &parsed).await?)
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

// ── POST /v1/channels/delete ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DeleteChannelBody {
    pub channel_id: String,
    #[serde(default)]
    pub requester_id: Option<String>,
}

pub async fn delete_channel(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: DeleteChannelBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_delete_channel(&conn, authed.as_deref(), &parsed).await?)
}

/// Delete a channel and its envelopes/watermarks in one transaction. Authz:
/// admin of the owning group (a destructive op).
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
    tx.execute(
        "DELETE FROM message_envelope WHERE conversation_id = ?1",
        libsql::params![body.channel_id.clone()],
    )
    .await?;
    tx.execute(
        "DELETE FROM conversation_watermark WHERE conversation_id = ?1",
        libsql::params![body.channel_id.clone()],
    )
    .await?;
    tx.execute(
        "DELETE FROM channels WHERE id = ?1",
        libsql::params![body.channel_id.clone()],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/members/remove ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct RemoveMemberBody {
    pub group_id: String,
    /// The member being removed.
    pub user_id: String,
    /// The actor; bound to the authenticated user when signed.
    #[serde(default)]
    pub requester_id: Option<String>,
}

pub async fn remove_member(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: RemoveMemberBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_remove_member(&conn, authed.as_deref(), &parsed).await?)
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
    conn.execute(
        "DELETE FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![body.group_id.clone(), body.user_id.clone()],
    )
    .await?;
    // Directory index (#261): drop the removed member's row.
    crate::directory::remove_group_member(conn, &body.group_id, &body.user_id).await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/members/role ────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SetMemberRoleBody {
    pub group_id: String,
    /// The member whose role is being changed.
    pub user_id: String,
    pub role: String,
    /// The actor; bound to the authenticated user when signed.
    #[serde(default)]
    pub requester_id: Option<String>,
}

pub async fn set_member_role(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: SetMemberRoleBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_set_member_role(&conn, authed.as_deref(), &parsed).await?)
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
    // Target must be a current member.
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
    // Directory index (#261): re-project the new role onto the member's row.
    crate::directory::sync_group_member(conn, &body.group_id, &body.user_id).await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/invites/create ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateInviteBody {
    pub id: String,
    pub group_id: String,
    /// The inviter; bound to the authenticated user when signed.
    #[serde(default)]
    pub inviter_id: Option<String>,
    pub invitee_id: String,
}

pub async fn create_invite(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: CreateInviteBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_create_invite(&conn, authed.as_deref(), &parsed).await?)
}

/// INSERT a pending invite. Authz: the inviter is a re-derived admin. (Invitee
/// resolution, block checks, and dup detection stay client-side reads.)
pub async fn apply_create_invite(
    conn: &Connection,
    authed: Option<&str>,
    body: &CreateInviteBody,
) -> anyhow::Result<WriteOutcome> {
    let inviter = match resolve_actor(authed, body.inviter_id.as_deref()) {
        Ok(i) => i,
        Err(o) => return Ok(o),
    };
    if authed.is_some() && !is_admin(conn, &body.group_id, &inviter).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    conn.execute(
        "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) VALUES (?1, ?2, ?3, ?4)",
        libsql::params![
            body.id.clone(),
            body.group_id.clone(),
            inviter,
            body.invitee_id.clone(),
        ],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/invites/accept ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AcceptInviteBody {
    pub invite_id: String,
    /// The invitee; bound to the authenticated user when signed.
    #[serde(default)]
    pub user_id: Option<String>,
}

pub async fn accept_invite(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: AcceptInviteBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_accept_invite(&conn, authed.as_deref(), &parsed).await?)
}

/// Accept an invite: add the actor as a member and delete the invite, in one
/// transaction. Authz: the invite is addressed to the actor (`invitee_id` is
/// re-derived server-side and must equal the signer) — a signer cannot accept
/// someone else's invite.
pub async fn apply_accept_invite(
    conn: &Connection,
    authed: Option<&str>,
    body: &AcceptInviteBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    // Resolve the group from an invite addressed to THIS actor; missing → the
    // invite doesn't exist or isn't theirs.
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

// ── POST /v1/invites/decline ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DeclineInviteBody {
    pub invite_id: String,
    /// The invitee; bound to the authenticated user when signed.
    #[serde(default)]
    pub user_id: Option<String>,
}

pub async fn decline_invite(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: DeclineInviteBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_decline_invite(&conn, authed.as_deref(), &parsed).await?)
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

// ── POST /v1/join-requests/create ────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateJoinRequestBody {
    pub id: String,
    pub group_id: String,
    /// The requester; bound to the authenticated user when signed.
    #[serde(default)]
    pub requester_id: Option<String>,
}

pub async fn create_join_request(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: CreateJoinRequestBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_create_join_request(&conn, authed.as_deref(), &parsed).await?)
}

/// UPSERT a pending join request (or reset a prior rejected/approved row back to
/// pending). Authz: the actor requests for THEMSELVES (`requester_id` bound to
/// the signer). Group-existence / not-already-member checks stay client-side.
pub async fn apply_create_join_request(
    conn: &Connection,
    authed: Option<&str>,
    body: &CreateJoinRequestBody,
) -> anyhow::Result<WriteOutcome> {
    let requester = match resolve_actor(authed, body.requester_id.as_deref()) {
        Ok(r) => r,
        Err(o) => return Ok(o),
    };
    conn.execute(
        "INSERT INTO group_join_request (id, group_id, requester_id, status, created_at)
         VALUES (?1, ?2, ?3, 'pending', datetime('now'))
         ON CONFLICT(group_id, requester_id) DO UPDATE SET
             id         = excluded.id,
             status     = 'pending',
             created_at = excluded.created_at",
        libsql::params![body.id.clone(), body.group_id.clone(), requester],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/join-requests/approve ───────────────────────────────────────────

#[derive(Deserialize)]
pub struct ApproveJoinRequestBody {
    pub request_id: String,
    /// The approver; bound to the authenticated user when signed.
    #[serde(default)]
    pub approver_id: Option<String>,
    pub reviewed_at: String,
}

pub async fn approve_join_request(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: ApproveJoinRequestBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_approve_join_request(&conn, authed.as_deref(), &parsed).await?)
}

/// Approve a pending join request: add the requester as a member and flip the row
/// to `approved`, in one transaction. Authz: the approver is a re-derived admin.
pub async fn apply_approve_join_request(
    conn: &Connection,
    authed: Option<&str>,
    body: &ApproveJoinRequestBody,
) -> anyhow::Result<WriteOutcome> {
    let approver = match resolve_actor(authed, body.approver_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    let mut rows = conn
        .query(
            "SELECT group_id, requester_id FROM group_join_request WHERE id = ?1 AND status = 'pending'",
            libsql::params![body.request_id.clone()],
        )
        .await?;
    let (group_id, requester_id): (String, String) = match rows.next().await? {
        Some(row) => (row.get(0)?, row.get(1)?),
        None => return Ok(WriteOutcome::Forbidden),
    };
    drop(rows);

    if authed.is_some() && !is_admin(conn, &group_id, &approver).await? {
        return Ok(WriteOutcome::Forbidden);
    }

    let tx = conn.transaction().await?;
    add_member_rows(&tx, &group_id, &requester_id).await?;
    tx.execute(
        "UPDATE group_join_request SET status = 'approved', reviewed_by = ?1, reviewed_at = ?2 WHERE id = ?3",
        libsql::params![approver, body.reviewed_at.clone(), body.request_id.clone()],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/join-requests/reject ────────────────────────────────────────────

#[derive(Deserialize)]
pub struct RejectJoinRequestBody {
    pub request_id: String,
    /// The approver; bound to the authenticated user when signed.
    #[serde(default)]
    pub approver_id: Option<String>,
    pub reviewed_at: String,
}

pub async fn reject_join_request(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: RejectJoinRequestBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_reject_join_request(&conn, authed.as_deref(), &parsed).await?)
}

/// Reject a pending join request. Authz: the approver is a re-derived admin.
pub async fn apply_reject_join_request(
    conn: &Connection,
    authed: Option<&str>,
    body: &RejectJoinRequestBody,
) -> anyhow::Result<WriteOutcome> {
    let approver = match resolve_actor(authed, body.approver_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    let mut rows = conn
        .query(
            "SELECT group_id FROM group_join_request WHERE id = ?1 AND status = 'pending'",
            libsql::params![body.request_id.clone()],
        )
        .await?;
    let group_id: String = match rows.next().await? {
        Some(row) => row.get(0)?,
        None => return Ok(WriteOutcome::Forbidden),
    };
    drop(rows);

    if authed.is_some() && !is_admin(conn, &group_id, &approver).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    conn.execute(
        "UPDATE group_join_request SET status = 'rejected', reviewed_by = ?1, reviewed_at = ?2 WHERE id = ?3",
        libsql::params![approver, body.reviewed_at.clone(), body.request_id.clone()],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}
