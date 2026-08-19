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
    response::{IntoResponse, Response},
};
use libsql::Connection;

use crate::error::{AppError, AuthRejection};
use crate::writes::{
    bad_request, claim_conversation_id, conversation_id_taken, gate, outcome_response,
    resolve_actor, ConversationKind, WriteOutcome,
};
use crate::AppState;

// The request bodies for this module's endpoints live in `pollis-api`, the
// crate pollis-core builds its requests from — one declaration, both ends, so
// a client field that does not exist here is a compile error rather than a
// silently-absent JSON key. Re-exported so `pollis_delivery::profile::*Body`
// keeps resolving for handlers, tests and the flows harness.
pub use pollis_api::profile::*;

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
    let conn = state.db.conn().await?;
    outcome_response::<UpdateProfileBody>(apply_update_profile(&conn, authed.as_deref(), &parsed).await?)
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
    let conn = state.db.conn().await?;
    outcome_response::<SavePreferencesBody>(apply_save_preferences(&conn, authed.as_deref(), &parsed).await?)
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
    let parsed: AddBlock = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    outcome_response::<AddBlock>(apply_block_user(&conn, authed.as_deref(), &parsed.0).await?)
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
    let parsed: RemoveBlock = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    outcome_response::<RemoveBlock>(apply_unblock_user(&conn, authed.as_deref(), &parsed.0).await?)
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
    let conn = state.db.conn().await?;
    match apply_create_dm(&conn, authed.as_deref(), &parsed).await? {
        DmOutcome::Forbidden => Ok(AuthRejection::Forbidden.into_response()),
        DmOutcome::Blocked => Ok(crate::writes::ok_response::<CreateDmBody>(
            CreatedDm::Blocked,
        )),
        DmOutcome::Created { id, existing } => {
            match crate::directory::dm_row(&conn, &id, existing).await? {
                Some(dm) => Ok(crate::writes::ok_response::<CreateDmBody>(dm)),
                None => Err(AppError(anyhow::anyhow!(
                    "DM {id} vanished between its creation and its read-back"
                ))),
            }
        }
    }
}

/// What `apply_create_dm` decided.
///
/// Distinct from [`WriteOutcome`] because this write has THREE outcomes, not
/// two: a block is not an authorization failure (the caller is perfectly
/// entitled to try), and reporting it as one would let the client tell "you may
/// not do this" from "this particular pairing is blocked" — which is exactly the
/// inference the generic refusal exists to prevent.
#[derive(Debug)]
pub enum DmOutcome {
    Created { id: String, existing: bool },
    Blocked,
    Forbidden,
}

/// Create a DM channel: insert `dm_channel`, the creator's auto-accepted
/// membership, each other member as a pending request, and seed every member's
/// per-device watermark — all in one transaction.
///
/// Authz: the creator is the authenticated user, and no proposed pairing may be
/// blocked in either direction (re-checked server-side).
///
/// # Dedupe moved here in #987
///
/// The 2-person dedupe used to run on the CLIENT: scan `dm_channel_member` for
/// an existing pair, then re-read that channel. Two reads and a race — two
/// devices creating the same DM at once each saw no existing pair and each
/// created one, leaving the pair with two conversations and their messages split
/// across both. Deciding it here makes "one DM per pair" a property of the write
/// rather than of the client's timing. Multi-party DMs (3+ members) skip dedupe
/// and always create fresh, unchanged.
pub async fn apply_create_dm(
    conn: &Connection,
    authed: Option<&str>,
    body: &CreateDmBody,
) -> anyhow::Result<DmOutcome> {
    let creator = match resolve_actor(authed, Some(body.creator_id.as_str())) {
        Ok(c) => c,
        Err(_) => return Ok(DmOutcome::Forbidden),
    };

    // The other participants (everyone but the creator, de-duplicated).
    let mut others: Vec<String> = Vec::new();
    for m in &body.member_ids {
        if *m != creator && !others.contains(m) {
            others.push(m.clone());
        }
    }
    if others.is_empty() {
        return Ok(DmOutcome::Forbidden);
    }

    // Re-check blocks server-side: refuse if ANY pairing is blocked either way.
    // Skipped on the no-auth path (mirrors the membership skips in `messages`).
    //
    // Reported as `Blocked`, NOT as `Forbidden`: a block is not an authorization
    // failure — the caller is perfectly entitled to try — and collapsing the two
    // would let a client tell "you may not do this" from "this pairing is
    // blocked", which is the inference the generic refusal exists to prevent.
    if authed.is_some() {
        for other in &others {
            if is_blocked_either_way(conn, &creator, other).await? {
                return Ok(DmOutcome::Blocked);
            }
        }
    }

    // 2-person dedupe: an existing channel whose membership is exactly
    // {creator, target} (order-independent, regardless of accepted state) is
    // returned instead of a duplicate being created.
    if others.len() == 1 {
        let mut rows = conn
            .query(
                "SELECT dm_channel_id \
                 FROM dm_channel_member \
                 GROUP BY dm_channel_id \
                 HAVING COUNT(*) = 2 \
                    AND SUM(CASE WHEN user_id = ?1 THEN 1 ELSE 0 END) = 1 \
                    AND SUM(CASE WHEN user_id = ?2 THEN 1 ELSE 0 END) = 1 \
                 LIMIT 1",
                libsql::params![creator.clone(), others[0].clone()],
            )
            .await?;
        if let Some(row) = rows.next().await? {
            return Ok(DmOutcome::Created {
                id: row.get::<String>(0)?,
                existing: true,
            });
        }
    }

    // Refuse an id already in use as any other kind of conversation. See
    // `conversation_id_taken` — `is_member` ORs across dm/group/channel on one
    // id, so reusing another conversation's id grants membership of it.
    if conversation_id_taken(conn, &body.id).await? {
        return Ok(DmOutcome::Forbidden);
    }
    let tx = conn.transaction().await?;
    // Claim the id in the SAME transaction as the DM it names, so a claim the
    // registry refuses rolls the DM back with it (#880).
    claim_conversation_id(&tx, &body.id, ConversationKind::Dm).await?;
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
    // blocked by devices that didn't exist at channel creation. Revoked devices
    // are excluded (#685) — they never rejoin and so are not part of the roster
    // the envelope GC gate measures against.
    let cursor = crate::messages::seeded_watermark_cursor();
    for member in std::iter::once(&creator).chain(others.iter()) {
        // `reported_at = datetime('now')` marks each seeded device LIVE as of DM
        // creation (#720): it pins for the staleness window, then stops if silent.
        // The cursor is bound from `seeded_watermark_cursor` and the two columns
        // are deliberately in different formats — see that function (#908).
        tx.execute(
            "INSERT OR IGNORE INTO conversation_watermark \
                 (conversation_id, user_id, device_id, last_fetched_at, reported_at) \
             SELECT ?1, ?2, ud.device_id, ?3, datetime('now') \
             FROM user_device ud WHERE ud.user_id = ?2 AND ud.revoked_at IS NULL",
            libsql::params![body.id.clone(), member.clone(), cursor.clone()],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(DmOutcome::Created {
        id: body.id.clone(),
        existing: false,
    })
}

// ── POST /v1/dm/accept ───────────────────────────────────────────────────────

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
    let conn = state.db.conn().await?;
    outcome_response::<AcceptDmBody>(apply_accept_dm(&conn, authed.as_deref(), &parsed).await?)
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

// ── Shared DM membership helper ──────────────────────────────────────────────

/// True when `user_id` is a current member of DM channel `dm_channel_id`. A
/// dedicated `dm_channel_member` lookup (NOT the broader `writes::is_member`,
/// which also matches groups/channels) — DM churn only ever concerns the DM
/// membership table.
async fn is_dm_member(
    conn: &Connection,
    dm_channel_id: &str,
    user_id: &str,
) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM dm_channel_member \
             WHERE dm_channel_id = ?1 AND user_id = ?2 LIMIT 1",
            libsql::params![dm_channel_id.to_string(), user_id.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

// ── POST /v1/dm/add ──────────────────────────────────────────────────────────

pub async fn add_dm_member(
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
    let parsed: AddDmMemberBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    outcome_response::<AddDmMemberBody>(apply_add_dm_member(&conn, authed.as_deref(), &parsed).await?)
}

/// Insert a `dm_channel_member` row for `user_id` and seed that member's
/// per-device watermarks, in one transaction. Authz: the actor (`added_by`) is
/// the authenticated user AND a current member of the DM — a non-member cannot
/// pull someone into a conversation it isn't part of. Membership is re-derived
/// server-side; skipped only on the no-auth path (mirrors `apply_create_dm`'s
/// block re-check).
pub async fn apply_add_dm_member(
    conn: &Connection,
    authed: Option<&str>,
    body: &AddDmMemberBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, Some(body.added_by.as_str())) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    // The actor must already belong to the DM to add anyone to it.
    if authed.is_some() && !is_dm_member(conn, &body.dm_channel_id, &actor).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    let tx = conn.transaction().await?;
    tx.execute(
        "INSERT OR IGNORE INTO dm_channel_member (dm_channel_id, user_id, added_by, added_at) \
         VALUES (?1, ?2, ?3, ?4)",
        libsql::params![
            body.dm_channel_id.clone(),
            body.user_id.clone(),
            actor.clone(),
            body.added_at.clone()
        ],
    )
    .await?;
    // Seed a watermark for every device of the new member so pre-join messages
    // don't block envelope cleanup indefinitely. Revoked devices are excluded
    // (#685) — they never rejoin, so they are not part of the GC roster.
    // `reported_at = datetime('now')` marks each seeded device LIVE as of the add
    // (#720): it pins for the staleness window, then stops if it never reports.
    // The cursor is bound from `seeded_watermark_cursor` and the two columns are
    // deliberately in different formats — see that function (#908).
    tx.execute(
        "INSERT OR IGNORE INTO conversation_watermark \
             (conversation_id, user_id, device_id, last_fetched_at, reported_at) \
         SELECT ?1, ?2, ud.device_id, ?3, datetime('now') \
         FROM user_device ud WHERE ud.user_id = ?2 AND ud.revoked_at IS NULL",
        libsql::params![
            body.dm_channel_id.clone(),
            body.user_id.clone(),
            crate::messages::seeded_watermark_cursor()
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/dm/remove ───────────────────────────────────────────────────────

pub async fn remove_dm_member(
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
    let parsed: RemoveDmMemberBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    outcome_response::<RemoveDmMemberBody>(apply_remove_dm_member(&conn, authed.as_deref(), &parsed).await?)
}

/// Delete a `dm_channel_member` row. Authz (replicates the client's current
/// rule, server-side): the actor (`requester_id`, bound to the authenticated
/// user) may remove only themselves, or — as the channel's creator — any member.
/// The creator check is re-derived from `dm_channel.created_by`; skipped only on
/// the no-auth path.
pub async fn apply_remove_dm_member(
    conn: &Connection,
    authed: Option<&str>,
    body: &RemoveDmMemberBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, Some(body.requester_id.as_str())) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    if authed.is_some() && actor != body.user_id {
        // Not a self-removal — the actor must be the channel creator.
        let mut rows = conn
            .query(
                "SELECT created_by FROM dm_channel WHERE id = ?1",
                libsql::params![body.dm_channel_id.clone()],
            )
            .await?;
        match rows.next().await? {
            Some(row) => {
                let creator: String = row.get(0)?;
                if actor != creator {
                    return Ok(WriteOutcome::Forbidden);
                }
            }
            // Channel doesn't exist — nothing to remove; refuse rather than
            // silently no-op a write the actor isn't entitled to.
            None => return Ok(WriteOutcome::Forbidden),
        }
    }
    let tx = conn.transaction().await?;
    tx.execute(
        "DELETE FROM dm_channel_member WHERE dm_channel_id = ?1 AND user_id = ?2",
        libsql::params![body.dm_channel_id.clone(), body.user_id.clone()],
    )
    .await?;
    // The directory-index row mirrors exactly this (user, channel) membership.
    // Scoped to both ids, so it cannot reach another member's row.
    tx.execute(
        "DELETE FROM user_dms WHERE dm_channel_id = ?1 AND user_id = ?2",
        libsql::params![body.dm_channel_id.clone(), body.user_id.clone()],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/dm/leave ────────────────────────────────────────────────────────

pub async fn leave_dm(
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
    let parsed: LeaveDmBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    let (outcome, torn_down) = apply_leave_dm(&conn, authed.as_deref(), &parsed).await?;
    if matches!(outcome, WriteOutcome::Ok) && torn_down {
        let log_conn = state.log_db.conn().await?;
        crate::teardown::purge_conversation_log(
            &log_conn,
            std::slice::from_ref(&parsed.dm_channel_id),
        )
        .await?;
    }
    outcome_response::<LeaveDmBody>(outcome)
}

/// Remove the actor's own `dm_channel_member` row and, when the channel is left
/// empty, tear it down — all in one transaction so a DM never lingers
/// half-deleted. Authz: self only — the actor (`user_id`, bound to the
/// authenticated user) may leave only their own membership; `resolve_actor`
/// already refuses any other `user_id`.
///
/// The teardown used to drop the envelopes and the `dm_channel` row and trust
/// `ON DELETE CASCADE` for the rest; with `foreign_keys=OFF` in production that
/// left the per-device cursors, reactions, attachment references and the
/// directory-index rows behind. [`crate::teardown::purge_dm_channel`] is the
/// full set.
///
/// The `bool` is "the channel was destroyed", so the caller knows whether to
/// purge its commit-log-DB state.
pub async fn apply_leave_dm(
    conn: &Connection,
    authed: Option<&str>,
    body: &LeaveDmBody,
) -> anyhow::Result<(WriteOutcome, bool)> {
    let user = match resolve_actor(authed, Some(body.user_id.as_str())) {
        Ok(u) => u,
        Err(o) => return Ok((o, false)),
    };
    let tx = conn.transaction().await?;
    tx.execute(
        "DELETE FROM dm_channel_member WHERE dm_channel_id = ?1 AND user_id = ?2",
        libsql::params![body.dm_channel_id.clone(), user.clone()],
    )
    .await?;
    // The leaver's own directory-index row goes with their membership; the other
    // members' rows are untouched.
    tx.execute(
        "DELETE FROM user_dms WHERE dm_channel_id = ?1 AND user_id = ?2",
        libsql::params![body.dm_channel_id.clone(), user],
    )
    .await?;
    // If no members remain, clean up the channel and all associated data.
    let mut rows = tx
        .query(
            "SELECT COUNT(*) FROM dm_channel_member WHERE dm_channel_id = ?1",
            libsql::params![body.dm_channel_id.clone()],
        )
        .await?;
    let remaining: i64 = match rows.next().await? {
        Some(row) => row.get(0)?,
        None => 0,
    };
    drop(rows);
    let torn_down = remaining == 0;
    if torn_down {
        crate::teardown::purge_dm_channel(&tx, &body.dm_channel_id).await?;
    }
    tx.commit().await?;
    Ok((WriteOutcome::Ok, torn_down))
}
