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
//!   - join-request create: the actor is the requester, the group exists, and
//!     the actor is not already a member (#917).
//!
//! On the no-auth path (`authed == None`, only when `POLLIS_DS_REQUIRE_AUTH` is
//! off) the role/identity checks are skipped and the actor comes from the body,
//! mirroring `commit::submit`, `writes.rs`, and `messages.rs`.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, Uri},
    response::{IntoResponse as _, Response},
};
use libsql::Connection;

use crate::error::AppError;
use crate::writes::{
    bad_request, claim_conversation_id, conversation_id_taken, gate, ok_json, outcome_response,
    resolve_actor, ConversationKind, WriteOutcome,
};
use crate::AppState;

// The request bodies for this module's endpoints live in `pollis-api`, the
// crate pollis-core builds its requests from — one declaration, both ends, so
// a client field that does not exist here is a compile error rather than a
// silently-absent JSON key. Re-exported so `pollis_delivery::groups::*Body`
// keeps resolving for handlers, tests and the flows harness.
pub use pollis_api::groups::*;

// ── Shared authz helpers ─────────────────────────────────────────────────────

/// The actor's role in `group_id`, re-derived server-side from `group_member`.
/// `None` when the actor is not a member. Every admin-gated domain-B write
/// re-derives this rather than trusting any client-supplied role.
///
/// The statement itself is [`pollis_schema::authz::GROUP_ROLE_SQL`], shared with
/// the client's preflight so the two cannot drift (#942).
///
/// `pub(crate)` because it is the ONLY group-role read in the DS: `emoji.rs`
/// used to carry its own copy whose `row.get::<String>(0).ok()` swallowed a
/// decode error into "not an admin", so a corrupt `role` column silently denied
/// instead of erroring (#875). One reader, one behaviour.
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

/// True when `group_id` names a real group.
///
/// #917: needed because this crate's connections run with `foreign_keys=OFF`
/// (see `db.rs`), so a `REFERENCES groups(id)` column does NOT reject a write
/// naming a group that was never created. Any `apply_*` that inserts a row
/// keyed by a client-supplied group id has to say so itself.
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
/// device) watermarks.
///
/// This is the **only** implementation. It used to be documented as mirroring a
/// `pollis_core::add_member_to_group` byte-for-byte; that function no longer
/// exists (group membership is a DS-owned write — the client never INSERTs into
/// `group_member`), and the only `add_member_to_group` left in pollis-core is an
/// unrelated MLS test helper. Shape: idempotent `INSERT OR IGNORE` plus a
/// best-effort watermark seed, so accepting an invite and approving a join
/// request land identical rows.
///
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
    // logs and continues on failure. Revoked devices are excluded (#685) — they
    // never rejoin, so they are not part of the GC roster.
    // `reported_at = datetime('now')` marks each seeded device LIVE as of the join
    // (#720): it pins for the staleness window, then stops if it never reports.
    // The cursor is bound from `seeded_watermark_cursor` and the two columns are
    // deliberately in different formats — see that function (#908).
    let _ = conn
        .execute(
            "INSERT OR IGNORE INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at, reported_at)
             SELECT c.id, ?1, ud.device_id, ?3, datetime('now')
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

// ── POST /v1/groups/create ───────────────────────────────────────────────────

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
    let conn = state.db.conn().await?;
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
    // Every id this one request would register: the group, plus whichever
    // default channels it asks for. All three are taken straight from the
    // request body, so all three are attacker-chosen — #879 checked `body.id`
    // alone, which left the two default-channel ids as an unguarded route to the
    // same attack (#880).
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
        // Refuse an id already in use as any other kind of conversation. See
        // `conversation_id_taken` — `is_member` ORs across dm/group/channel on
        // one id, so reusing another conversation's id grants membership of it.
        if conversation_id_taken(conn, id).await? {
            return Ok(WriteOutcome::Forbidden);
        }
        // And refuse one request that names the same id twice (a group that is
        // also its own channel, or two default channels sharing an id). Nothing
        // exists yet, so no lookup of existing rows can see this one; the
        // registry's primary key would catch it below, but a 403 is the honest
        // answer to a malformed request.
        if claims[..i].iter().any(|(seen, _)| seen == id) {
            return Ok(WriteOutcome::Forbidden);
        }
    }

    let tx = conn.transaction().await?;
    // Claim the ids in the SAME transaction as the rows they name, so a claim
    // the registry refuses takes the whole creation with it.
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

// ── POST /v1/groups/update ───────────────────────────────────────────────────

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
    let conn = state.db.conn().await?;
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
    let conn = state.db.conn().await?;
    let (outcome, dead_conversations) =
        apply_delete_group(&conn, authed.as_deref(), &parsed).await?;
    if matches!(outcome, WriteOutcome::Ok) && !dead_conversations.is_empty() {
        let log_conn = state.log_db.conn().await?;
        crate::teardown::purge_conversation_log(&log_conn, &dead_conversations).await?;
    }
    outcome_response(outcome)
}

/// Delete a group and everything belonging to it, in one transaction. Authz:
/// admin.
///
/// This used to be `DELETE FROM groups` alone, trusting `ON DELETE CASCADE` for
/// members, channels and invites. Production Turso has `foreign_keys=OFF`, so
/// that cascade has never fired and the group's entire membership roster,
/// channel list, message envelopes and invites survived the delete. Teardown is
/// now explicit — see [`crate::teardown::purge_group`].
///
/// Returns the destroyed conversation ids for the caller's post-commit
/// commit-log purge.
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

// ── POST /v1/groups/leave ────────────────────────────────────────────────────

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
    let conn = state.db.conn().await?;
    let (outcome, dead_conversations) =
        apply_leave_group(&conn, authed.as_deref(), &parsed).await?;
    if matches!(outcome, WriteOutcome::Ok) && !dead_conversations.is_empty() {
        let log_conn = state.log_db.conn().await?;
        crate::teardown::purge_conversation_log(&log_conn, &dead_conversations).await?;
    }
    outcome_response(outcome)
}

/// Remove the actor's own membership; if the group is now empty, tear it down.
/// Authz: the actor is a current member (a signed request may only remove its
/// OWN row — `user_id` is bound to the signer).
///
/// The teardown is explicit for the same reason as [`apply_delete_group`]: the
/// cascade it used to rely on does not fire on Turso.
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
    // The leaver's own group-scoped rows go with them; other members' rows stay.
    tx.execute(
        "DELETE FROM user_groups WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![body.group_id.clone(), user.clone()],
    )
    .await?;
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
    let mut dead: Vec<String> = Vec::new();
    if remaining == 0 {
        dead = crate::teardown::purge_group(&tx, &body.group_id).await?;
        dead.push(body.group_id.clone());
    }
    tx.commit().await?;
    Ok((WriteOutcome::Ok, dead))
}

// ── POST /v1/channels/create ─────────────────────────────────────────────────

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
    let conn = state.db.conn().await?;
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
    // Refuse an id already in use as any other kind of conversation. See
    // `conversation_id_taken` — `is_member` ORs across dm/group/channel on one
    // id, so reusing another conversation's id grants membership of it.
    if conversation_id_taken(conn, &body.id).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    // One transaction, so the registry claim and the channel it names commit
    // together or not at all (#880).
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

// ── POST /v1/channels/update ─────────────────────────────────────────────────

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
    let conn = state.db.conn().await?;
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
    outcome_response(outcome)
}

/// Delete a channel and its whole message history in one transaction. Authz:
/// admin of the owning group (a destructive op).
///
/// Envelopes and watermarks were already explicit here; the reactions and
/// attachment references hanging off those envelopes were not, and outlived
/// them (nothing cascades on Turso, and neither table has a foreign key in the
/// first place). [`crate::teardown::purge_conversation_rows`] covers the set.
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

// ── POST /v1/members/remove ──────────────────────────────────────────────────

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
    let conn = state.db.conn().await?;
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
    let tx = conn.transaction().await?;
    tx.execute(
        "DELETE FROM group_member WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![body.group_id.clone(), body.user_id.clone()],
    )
    .await?;
    // The directory-index row for exactly this (user, group) pair goes with the
    // membership it mirrors. Scoped to both ids, so it cannot reach another
    // member's row or another group's.
    tx.execute(
        "DELETE FROM user_groups WHERE group_id = ?1 AND user_id = ?2",
        libsql::params![body.group_id.clone(), body.user_id.clone()],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/members/role ────────────────────────────────────────────────────

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
    let conn = state.db.conn().await?;
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
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/invites/create ──────────────────────────────────────────────────

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
    let conn = state.db.conn().await?;
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
    let conn = state.db.conn().await?;
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
    let conn = state.db.conn().await?;
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
    let conn = state.db.conn().await?;
    outcome_response(apply_create_join_request(&conn, authed.as_deref(), &parsed).await?)
}

/// UPSERT a pending join request (or reset a prior rejected/approved row back to
/// pending). Authz: the actor requests for THEMSELVES (`requester_id` bound to
/// the signer), for a group that EXISTS, and only while they are NOT already in
/// it.
///
/// #917: those last two used to be documented here as "checks [that] stay
/// client-side", which is another way of saying they were not checks. The
/// client half lives in `pollis_core::commands::groups::request_group_access`
/// and is a helpful preflight, but the DS is the only authoritative writer, so
/// anything it does not re-derive is unenforced. A client that simply skipped
/// the preflight could:
///
///   * insert `group_join_request` rows for arbitrary — including nonexistent —
///     group ids, since `foreign_keys` is OFF on this connection
///     (`db.rs`), so the schema's `REFERENCES groups(id)` does not catch it; and
///   * spray a row into every admin's pending-request list, group by group,
///     using ids harvested by slug enumeration.
///
/// Both are now refused here. This mirrors the rest of domain B, where the
/// admin role is re-derived server-side rather than trusted from the body.
///
/// Note the check is `is_member` **negated** — a join request is by definition
/// from a non-member, so "no membership check" was never the fix; the invariant
/// is that a member cannot ask to join what they are already in.
pub async fn apply_create_join_request(
    conn: &Connection,
    authed: Option<&str>,
    body: &CreateJoinRequestBody,
) -> anyhow::Result<WriteOutcome> {
    let requester = match resolve_actor(authed, body.requester_id.as_deref()) {
        Ok(r) => r,
        Err(o) => return Ok(o),
    };

    // The group must exist. Checked explicitly rather than left to the FK,
    // because this connection runs with `foreign_keys=OFF`.
    if !group_exists(conn, &body.group_id).await? {
        return Ok(WriteOutcome::Forbidden);
    }

    // A current member has nothing to request. Letting this through would make
    // an approvable request for someone already in the group representable —
    // and approving it would re-run `add_member_rows` for an existing member.
    if is_member(conn, &body.group_id, &requester).await? {
        return Ok(WriteOutcome::Forbidden);
    }

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
    let conn = state.db.conn().await?;
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
    let conn = state.db.conn().await?;
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

// ── #847 shareable invite links ──────────────────────────────────────────────
//
// ## Why redemption is not a second membership path
//
// The security crux of #847 is that a link must NOT become another way into a
// group. `apply_redeem_invite_link` therefore performs the member-add by calling
// `add_member_rows` — the exact function `apply_accept_invite` and
// `apply_approve_join_request` call, and the only function in this module that
// writes `group_member`. A redeemer lands as an ordinary `role = 'member'` row
// with seeded watermarks, indistinguishable from someone an admin invited by
// name.
//
// That matters more here than it first appears. `group_member` is the SOLE
// capability in this system: `desired_roster_user_ids`
// (`pollis-core/src/commands/mls/reconcile.rs`) derives the MLS roster from it,
// and `writes::is_member` derives commit-submission rights from it. Nothing
// downstream re-validates how the row got there. So this handler is the entire
// trust boundary, and the token check and the member-add must be — and are — one
// transaction. Redemption itself never touches MLS state, never stages a commit,
// and cannot put a device in the tree; it can only make the roster say "this
// user is a member", which is precisely the authority the admin delegated when
// they created the link.
//
// ## Why every failure looks identical
//
// Wrong token, malformed token, revoked link, expired link, exhausted link — all
// five return `RedeemOutcome::Rejected`, which maps to one 403 with one body.
// Telling an attacker "that token was right but expired" confirms a valid token
// and turns a search over 2^256 into a hunt for a fresher link. The predicates
// are therefore evaluated into booleans and combined ONCE, with no early return
// between the secret compare and the verdict.
//
// Rate limiting is the one distinguishable response (429), deliberately: it is a
// statement about the CALLER, not about the token, so it confirms nothing about
// whether any particular token is valid.

/// How far back the durable redemption rate limit counts failed attempts.
const REDEEM_FAILURE_WINDOW_SECS: i64 = 600;

/// Failed redemptions allowed per user within [`REDEEM_FAILURE_WINDOW_SECS`].
///
/// Sized for humans, not for search. A legitimate user redeems once; ten
/// failures in ten minutes already means they are pasting the wrong thing
/// repeatedly.
///
/// This is the durable half of the bound and the half that actually binds. The
/// per-IP middleware tier (`ratelimit::classify`) sheds floods but an attacker
/// rotates IPs; this one is keyed on the authenticated user and read from the
/// database, so it survives a DS restart and is shared across instances. Against
/// the 256-bit secret both are belt-and-braces — but they are what stops
/// redemption being a free oracle for a selector whose secret is being ground.
const REDEEM_FAILURE_MAX: i64 = 10;

// ── POST /v1/invite-links/create ─────────────────────────────────────────────

pub async fn create_invite_link(
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
    let parsed: CreateInviteLinkBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    outcome_response(apply_create_invite_link(&conn, authed.as_deref(), &parsed).await?)
}

/// INSERT an invite link. Authz: the creator is a re-derived admin — the same bar
/// as `apply_create_invite`, because a link IS an invite with the addressee left
/// open.
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
    // Refuse anything that is not a sha256 hex digest. The column would accept
    // any TEXT, and a short or low-entropy value is how a buggy — or hostile —
    // client could plant a hash it can cheaply preimage. Checked on BOTH the
    // auth-on and auth-off paths: this is a well-formedness invariant on the
    // stored secret, not an identity check.
    if body.secret_hash.len() != 64 || !body.secret_hash.chars().all(|c| c.is_ascii_hexdigit()) {
        return Ok(WriteOutcome::Forbidden);
    }
    // `max_uses = 0` is rejected by the table CHECK; refuse it here too so the
    // caller gets a 403 rather than a 500 from a constraint violation.
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

// ── POST /v1/invite-links/revoke ─────────────────────────────────────────────

pub async fn revoke_invite_link(
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
    let parsed: RevokeInviteLinkBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    outcome_response(apply_revoke_invite_link(&conn, authed.as_deref(), &parsed).await?)
}

/// Revoke a link. Authz: the actor is a re-derived admin OF THE LINK'S OWN GROUP
/// — resolved from the link row, never from the request, so an admin of group A
/// cannot revoke group B's link. Same ordering as
/// `apply_approve_join_request`: resolve the row first, then check the role.
///
/// Revocation is a one-way stamp, not a delete: the row stays so the redemption
/// audit trail keeps pointing at something, and `revoked_at IS NULL` remains the
/// single expression of "live".
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
    // `revoked_at IS NULL` keeps the first revocation's timestamp — re-revoking
    // must not rewrite when the link actually died.
    conn.execute(
        "UPDATE group_invite_link SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL",
        libsql::params![body.revoked_at.clone(), body.link_id.clone()],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/invite-links/redeem ─────────────────────────────────────────────

/// The result of a redemption attempt.
///
/// Three variants on purpose. Collapsing every way a token can fail into one
/// `Rejected` is a security property, not an oversight — an `Expired` or
/// `Revoked` variant would leak exactly what #847 says must not leak.
#[derive(Debug, PartialEq, Eq)]
pub enum RedeemOutcome {
    /// The actor is now a member of this group.
    Joined { group_id: String },
    /// The token did not yield a live link. Indistinguishable by construction.
    Rejected,
    /// The actor has failed too many times recently.
    RateLimited,
}

pub async fn redeem_invite_link(
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
    let parsed: RedeemInviteLinkBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    Ok(
        match apply_redeem_invite_link(&conn, authed.as_deref(), &parsed).await? {
            RedeemOutcome::Joined { group_id } => {
                ok_json(serde_json::json!({ "status": "ok", "group_id": group_id }))
            }
            RedeemOutcome::Rejected => redeem_rejected(),
            RedeemOutcome::RateLimited => redeem_rate_limited(),
        },
    )
}

/// The single rejection response: one status, one body, for every way a token
/// can fail to be live.
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

/// Redeem an invite link.
///
/// Authz: the actor is whoever `gate` authenticated. That is the admission floor
/// — a link is not a bypass around holding a Pollis identity, it only removes the
/// need for an admin to know your username in advance. An anonymous caller
/// cannot reach this at all. "Authenticated but not yet a member" is already a
/// first-class case on this service (`/v1/join-requests/create`,
/// `/v1/invites/accept`), so no new auth machinery is involved.
pub async fn apply_redeem_invite_link(
    conn: &Connection,
    authed: Option<&str>,
    body: &RedeemInviteLinkBody,
) -> anyhow::Result<RedeemOutcome> {
    let user = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(u) => u,
        Err(_) => return Ok(RedeemOutcome::Rejected),
    };

    // Durable rate limit: recent FAILED attempts by this actor, counted from the
    // audit table rather than memory, so a rolling restart does not hand an
    // attacker a fresh budget and every DS instance sees the same count.
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM group_invite_link_redemption \
             WHERE user_id = ?1 AND succeeded = 0 \
               AND datetime(attempted_at) > datetime(?2, ?3)",
            libsql::params![
                user.clone(),
                body.now.clone(),
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
        record_redemption(conn, &body.attempt_id, None, &user, &body.now, false).await?;
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
            record_redemption(conn, &body.attempt_id, None, &user, &body.now, false).await?;
            return Ok(RedeemOutcome::Rejected);
        }
    };
    drop(rows);

    // Every predicate is evaluated, THEN combined. No early return sits between
    // the secret compare and the verdict, so the four failure modes are not
    // separable by response and not separable by control flow either.
    let secret_ok = crate::invite_token::hash_eq(
        &crate::invite_token::hash_secret(&secret),
        &secret_hash,
    );
    let not_revoked = revoked_at.is_none();
    let not_expired = match expires_at.as_deref() {
        Some(exp) => !expiry_passed(conn, exp, &body.now).await?,
        None => true,
    };
    let uses_left = match max_uses {
        Some(max) => uses < max,
        None => true,
    };

    if !(secret_ok && not_revoked && not_expired && uses_left) {
        // Record the link id only when the SECRET matched, so an admin can see a
        // specific live link being probed. The attacker never reads this table,
        // so it leaks nothing back to them.
        let probed = if secret_ok { Some(link_id.as_str()) } else { None };
        record_redemption(conn, &body.attempt_id, probed, &user, &body.now, false).await?;
        return Ok(RedeemOutcome::Rejected);
    }

    // Already a member → idempotent success. Re-opening the same link must not
    // burn a use or fail; it just says "you're in".
    let mut rows = conn
        .query(
            "SELECT 1 FROM group_member WHERE group_id = ?1 AND user_id = ?2",
            libsql::params![group_id.clone(), user.clone()],
        )
        .await?;
    let already_member = rows.next().await?.is_some();
    drop(rows);
    if already_member {
        return Ok(RedeemOutcome::Joined { group_id });
    }

    let tx = conn.transaction().await?;
    // THE shared membership primitive — the same call `apply_accept_invite` and
    // `apply_approve_join_request` make. This is what keeps a link from being a
    // second admission path.
    add_member_rows(&tx, &group_id, &user).await?;
    // Re-check every bound INSIDE the transaction and let the row count decide.
    // Two people redeeming the last use concurrently both pass the checks above;
    // only one can pass this guarded UPDATE. The table's `uses <= max_uses`
    // CHECK is the backstop beneath even this.
    let updated = tx
        .execute(
            "UPDATE group_invite_link SET uses = uses + 1 \
             WHERE id = ?1 \
               AND revoked_at IS NULL \
               AND (expires_at IS NULL OR datetime(expires_at) > datetime(?2)) \
               AND (max_uses IS NULL OR uses < max_uses)",
            libsql::params![link_id.clone(), body.now.clone()],
        )
        .await?;
    if updated == 0 {
        tx.rollback().await?;
        record_redemption(conn, &body.attempt_id, Some(&link_id), &user, &body.now, false).await?;
        return Ok(RedeemOutcome::Rejected);
    }
    tx.execute(
        "INSERT INTO group_invite_link_redemption \
           (id, link_id, user_id, attempted_at, succeeded) VALUES (?1, ?2, ?3, ?4, 1)",
        libsql::params![
            body.attempt_id.clone(),
            link_id,
            user.clone(),
            body.now.clone(),
        ],
    )
    .await?;
    tx.commit().await?;

    Ok(RedeemOutcome::Joined { group_id })
}

/// Whether `expires_at` is at or before `now`, compared BY SQLITE via
/// `datetime()` so the two RFC3339 strings normalise the same way the guarded
/// UPDATE normalises them. Comparing in Rust with string ordering would disagree
/// with SQL for offsets like `+00:00` versus `Z`.
async fn expiry_passed(conn: &Connection, expires_at: &str, now: &str) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT datetime(?1) <= datetime(?2)",
            libsql::params![expires_at.to_string(), now.to_string()],
        )
        .await?;
    // An unparseable timestamp yields NULL, not 0/1. Treat anything that is not
    // a definite "not yet expired" as EXPIRED — fail closed.
    Ok(match rows.next().await? {
        Some(row) => row.get::<Option<i64>>(0)?.unwrap_or(1) == 1,
        None => true,
    })
}

/// Append one row to the redemption audit trail.
///
/// `OR IGNORE` so a replayed `attempt_id` cannot error the request — the point
/// is the count, not the individual row.
async fn record_redemption(
    conn: &Connection,
    attempt_id: &str,
    link_id: Option<&str>,
    user_id: &str,
    now: &str,
    succeeded: bool,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO group_invite_link_redemption \
           (id, link_id, user_id, attempted_at, succeeded) VALUES (?1, ?2, ?3, ?4, ?5)",
        libsql::params![
            attempt_id.to_string(),
            link_id.map(|s| s.to_string()),
            user_id.to_string(),
            now.to_string(),
            i64::from(succeeded),
        ],
    )
    .await?;
    Ok(())
}
