//! Domain C (#419) — profile / preferences / blocks / DMs. Every CLIENT remote
//! write in this domain routes through the DS, copying the domain-A shape in
//! [`crate::messages`] verbatim:
//!
//!   - **Bodies** — one `#[derive(Deserialize)] *Body` per endpoint, plain JSON
//!     (no binary fields in this domain).
//!   - **Pure conn-level fns** — `apply_*` take a bare [`Connection`] (the MAIN
//!     DB), the authenticated user (`Option<&str>`; `None` only on the no-auth
//!     path), and a parsed `*Body`. They embed BOTH the authorization decision
//!     AND the write, returning [`WriteOutcome`], so the in-process harness and
//!     the production axum handler exercise the *exact* same authz.
//!   - **axum handlers** — `(State, Method, Uri, HeaderMap, Bytes) -> Response`,
//!     all identical: `gate` → parse → `apply_*` → map outcome.
//!
//! ## Where the writes land
//!
//! Every domain-C table — `users`, `user_preferences`, `user_block`,
//! `dm_channel`, `dm_channel_member`, `conversation_watermark` — lives in the
//! **MAIN DB** (`state.db`), NOT the commit-log DB. So all `apply_*` fns run on
//! the main connection.
//!
//! ## Authorization (the security core)
//!
//! `gate` proves *which user* signed the request; each `apply_*` then proves the
//! user may make that specific write:
//!   - profile update / preferences: the actor may only edit their OWN row
//!     (`user_id` bound to the authenticated user).
//!   - block / unblock: the blocker may only manage their OWN block list
//!     (`blocker_id` bound to the authenticated user).
//!   - DM create: the creator is the authenticated user, and no proposed pairing
//!     may be blocked in either direction (re-checked server-side).
//!   - DM accept: the authenticated user is the recipient flipping their OWN
//!     membership row to accepted.
//!
//! On the no-auth path (`authed == None`, only reachable when the DS runs with
//! `POLLIS_DS_REQUIRE_AUTH` off) the identity checks fall back to the body actor
//! and membership/block checks are skipped — mirroring `messages` / `writes`.

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

// ── Shared block helper ──────────────────────────────────────────────────────

/// True when `user_a` has blocked `user_b` OR vice versa. Mirrors pollis-core's
/// `blocks::is_blocked_either_way` (a symmetric `user_block` lookup) so the
/// server re-derives the block relationship rather than trusting the client.
async fn is_blocked_either_way(
    conn: &Connection,
    user_a: &str,
    user_b: &str,
) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM user_block \
             WHERE (blocker_id = ?1 AND blocked_id = ?2) \
                OR (blocker_id = ?2 AND blocked_id = ?1) \
             LIMIT 1",
            libsql::params![user_a.to_string(), user_b.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

// ── POST /v1/profile/update ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct UpdateProfileBody {
    /// The profile being edited; when signed it must equal the authenticated
    /// user (you can only edit your OWN profile).
    pub user_id: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub preferred_name: Option<String>,
    #[serde(default)]
    pub phone: Option<String>,
    #[serde(default)]
    pub avatar_url: Option<String>,
}

pub async fn update_profile(
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
    let parsed: UpdateProfileBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_update_profile(&conn, authed.as_deref(), &parsed).await?)
}

/// COALESCE-UPDATE the user's own profile row. Authz: the actor may only edit
/// their own `users` row (`user_id` bound to the authenticated user).
pub async fn apply_update_profile(
    conn: &Connection,
    authed: Option<&str>,
    body: &UpdateProfileBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, Some(body.user_id.as_str())) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    conn.execute(
        "UPDATE users SET \
            username = COALESCE(?2, username), \
            preferred_name = COALESCE(?3, preferred_name), \
            phone = COALESCE(?4, phone), \
            avatar_url = COALESCE(?5, avatar_url) \
         WHERE id = ?1",
        libsql::params![
            user,
            body.username.clone(),
            body.preferred_name.clone(),
            body.phone.clone(),
            body.avatar_url.clone(),
        ],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/profile/preferences ─────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SavePreferencesBody {
    /// The owner of the preferences; when signed it must equal the authenticated
    /// user (you can only edit your OWN preferences).
    pub user_id: String,
    pub preferences: String,
}

pub async fn save_preferences(
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
    let parsed: SavePreferencesBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_save_preferences(&conn, authed.as_deref(), &parsed).await?)
}

/// UPSERT the user's `user_preferences` row. Authz: the actor may only write
/// their own preferences (`user_id` bound to the authenticated user).
pub async fn apply_save_preferences(
    conn: &Connection,
    authed: Option<&str>,
    body: &SavePreferencesBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, Some(body.user_id.as_str())) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    conn.execute(
        "INSERT INTO user_preferences (user_id, preferences, updated_at) \
             VALUES (?1, ?2, datetime('now')) \
         ON CONFLICT(user_id) DO UPDATE SET \
             preferences = ?2, updated_at = datetime('now')",
        libsql::params![user, body.preferences.clone()],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/blocks/add ──────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct BlockBody {
    /// The user doing the blocking; when signed it must equal the authenticated
    /// user (you manage only your OWN block list).
    pub blocker_id: String,
    pub blocked_id: String,
}

pub async fn block_user(
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
    let parsed: BlockBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_block_user(&conn, authed.as_deref(), &parsed).await?)
}

/// Insert a block row and reset the blocker's `accepted_at` for every DM shared
/// with the blocked user (so an unblock later resurfaces the channel as a
/// request), in one transaction. Authz: the blocker is the authenticated user.
pub async fn apply_block_user(
    conn: &Connection,
    authed: Option<&str>,
    body: &BlockBody,
) -> anyhow::Result<WriteOutcome> {
    let blocker = match resolve_actor(authed, Some(body.blocker_id.as_str())) {
        Ok(b) => b,
        Err(o) => return Ok(o),
    };
    // You cannot block yourself — an invalid state; refuse it.
    if blocker == body.blocked_id {
        return Ok(WriteOutcome::Forbidden);
    }
    let tx = conn.transaction().await?;
    tx.execute(
        "INSERT OR IGNORE INTO user_block (blocker_id, blocked_id) VALUES (?1, ?2)",
        libsql::params![blocker.clone(), body.blocked_id.clone()],
    )
    .await?;
    tx.execute(
        "UPDATE dm_channel_member \
            SET accepted_at = NULL \
          WHERE user_id = ?1 \
            AND dm_channel_id IN ( \
                SELECT dm_channel_id FROM dm_channel_member WHERE user_id = ?2 \
            )",
        libsql::params![blocker, body.blocked_id.clone()],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/blocks/remove ───────────────────────────────────────────────────

pub async fn unblock_user(
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
    let parsed: BlockBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_unblock_user(&conn, authed.as_deref(), &parsed).await?)
}

/// Delete a block row. Authz: the blocker is the authenticated user — the DELETE
/// is scoped `blocker_id = :blocker`, so it can never touch another user's list.
pub async fn apply_unblock_user(
    conn: &Connection,
    authed: Option<&str>,
    body: &BlockBody,
) -> anyhow::Result<WriteOutcome> {
    let blocker = match resolve_actor(authed, Some(body.blocker_id.as_str())) {
        Ok(b) => b,
        Err(o) => return Ok(o),
    };
    conn.execute(
        "DELETE FROM user_block WHERE blocker_id = ?1 AND blocked_id = ?2",
        libsql::params![blocker, body.blocked_id.clone()],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/dm/create ───────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CreateDmBody {
    /// The new channel id (a ULID the client generated).
    pub id: String,
    /// The creator; when signed it must equal the authenticated user.
    pub creator_id: String,
    /// Every participant (may or may not include the creator — the creator is
    /// always inserted as auto-accepted regardless).
    pub member_ids: Vec<String>,
    pub created_at: String,
}

pub async fn create_dm(
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
    let parsed: CreateDmBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_create_dm(&conn, authed.as_deref(), &parsed).await?)
}

/// Create a DM channel: insert `dm_channel`, the creator's auto-accepted
/// membership, each other member as a pending request, and seed every member's
/// per-device watermark — all in one transaction. Authz: the creator is the
/// authenticated user, and no proposed pairing may be blocked in either
/// direction (re-checked server-side; a blocked write is `Forbidden`).
pub async fn apply_create_dm(
    conn: &Connection,
    authed: Option<&str>,
    body: &CreateDmBody,
) -> anyhow::Result<WriteOutcome> {
    let creator = match resolve_actor(authed, Some(body.creator_id.as_str())) {
        Ok(c) => c,
        Err(o) => return Ok(o),
    };

    // The other participants (everyone but the creator, de-duplicated).
    let mut others: Vec<String> = Vec::new();
    for m in &body.member_ids {
        if *m != creator && !others.contains(m) {
            others.push(m.clone());
        }
    }

    // Re-check blocks server-side: refuse if ANY pairing is blocked either way.
    // Skipped on the no-auth path (mirrors the membership skips in `messages`).
    if authed.is_some() {
        for other in &others {
            if is_blocked_either_way(conn, &creator, other).await? {
                return Ok(WriteOutcome::Forbidden);
            }
        }
    }

    let tx = conn.transaction().await?;
    tx.execute(
        "INSERT INTO dm_channel (id, created_by, created_at) VALUES (?1, ?2, ?3)",
        libsql::params![body.id.clone(), creator.clone(), body.created_at.clone()],
    )
    .await?;
    // Creator is auto-accepted (they initiated the conversation).
    tx.execute(
        "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at, accepted_at) \
         VALUES (?1, ?2, ?3, ?4, ?4)",
        libsql::params![
            body.id.clone(),
            creator.clone(),
            creator.clone(),
            body.created_at.clone()
        ],
    )
    .await?;
    // Every other member starts un-accepted — a pending request.
    for other in &others {
        tx.execute(
            "INSERT OR IGNORE INTO dm_channel_member \
                 (dm_channel_id, user_id, added_by, added_at, accepted_at) \
             VALUES (?1, ?2, ?3, ?4, NULL)",
            libsql::params![
                body.id.clone(),
                other.clone(),
                creator.clone(),
                body.created_at.clone()
            ],
        )
        .await?;
    }
    // Seed a watermark for every (member, device) pair so envelope cleanup isn't
    // blocked by devices that didn't exist at channel creation.
    for member in std::iter::once(&creator).chain(others.iter()) {
        tx.execute(
            "INSERT OR IGNORE INTO conversation_watermark \
                 (conversation_id, user_id, device_id, last_fetched_at) \
             SELECT ?1, ?2, ud.device_id, datetime('now') \
             FROM user_device ud WHERE ud.user_id = ?2",
            libsql::params![body.id.clone(), member.clone()],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/dm/accept ───────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AcceptDmBody {
    pub dm_channel_id: String,
    /// The accepting member; when signed it must equal the authenticated user
    /// (you accept your OWN pending request, never someone else's).
    pub user_id: String,
    pub accepted_at: String,
}

pub async fn accept_dm(
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
    let parsed: AcceptDmBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_accept_dm(&conn, authed.as_deref(), &parsed).await?)
}

/// Flip the actor's own `dm_channel_member.accepted_at` from NULL → now. Authz:
/// the accepter is the authenticated user — the UPDATE is scoped `user_id =
/// :user` so it can only ever accept the actor's OWN request, and the
/// `accepted_at IS NULL` guard keeps it idempotent.
pub async fn apply_accept_dm(
    conn: &Connection,
    authed: Option<&str>,
    body: &AcceptDmBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, Some(body.user_id.as_str())) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    conn.execute(
        "UPDATE dm_channel_member \
            SET accepted_at = ?3 \
          WHERE dm_channel_id = ?1 \
            AND user_id = ?2 \
            AND accepted_at IS NULL",
        libsql::params![body.dm_channel_id.clone(), user, body.accepted_at.clone()],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}
