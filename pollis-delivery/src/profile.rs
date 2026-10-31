//! Domain C (#419) — profile / preferences / blocks / DMs, in the domain-A shape of [`crate::messages`].
//!
//! ## Authorization (the security core)
//!
//! `gate` proves *which user* signed the request; each `apply_*` then proves the
//! user may make that specific write:
//!   - profile update / preferences: the actor's OWN row only.
//!   - block / unblock: the actor's OWN block list only.
//!   - DM create: no proposed pairing may be blocked in either direction.
//!   - DM accept: the actor's OWN membership row.
//!
//! On the no-auth path identity falls back to the body actor; membership/block checks are skipped.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
};
use libsql::Connection;

use crate::error::{AppError, AuthRejection};
use crate::writes::{
    bad_request,
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
use crate::util::b64_decode;

// Request bodies live in `pollis-api`, shared with pollis-core; re-exported here.
pub use pollis_api::profile::*;

/// True when `user_a` has blocked `user_b` or vice versa, re-derived server-side.
pub(crate) async fn is_blocked_either_way(
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

pub async fn update_profile(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<UpdateProfileBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    profile_outcome_response::<UpdateProfileBody>(
        apply_update_profile(&conn, authed.as_deref(), &parsed).await?,
    )
}

/// Shortest username the DS accepts.
pub const USERNAME_MIN_LEN: usize = 3;
/// Longest username the DS accepts.
pub const USERNAME_MAX_LEN: usize = 32;

/// The one character class a username may be drawn from: `[a-z0-9_.-]`.
///
/// WITHOUT `@`: lookups dispatch on `@`, so such a username could squat an email (`000021` too).
///
/// Mirrored in `frontend/src/utils/username.ts` and `mobile/lib/username.ts`; the DS check counts.
pub fn is_username_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-')
}

/// `^[a-z0-9_.-]{3,32}$`, spelled without a regex dependency.
pub fn is_valid_username(s: &str) -> bool {
    (USERNAME_MIN_LEN..=USERNAME_MAX_LEN).contains(&s.len()) && s.chars().all(is_username_char)
}

/// The message a rejected username is refused with.
pub const USERNAME_INVALID: &str =
    "username must be 3-32 characters of a-z, 0-9, '_', '.' or '-'";

/// Upper bound on a human display name (preferred or group name, #1095).
pub const DISPLAY_NAME_MAX_LEN: usize = 100;

/// Whether `s` is acceptable as a human display name.
///
/// NOT the username charset (display names are rendered, never resolved). Refused:
///
/// * longer than [`DISPLAY_NAME_MAX_LEN`] (#1095);
/// * control characters, newlines included;
/// * bidi overrides/isolates and LRM/RLM (display-order spoofing);
/// * `<` and `>` (the sink escapes them too); `&` is allowed.
pub fn is_valid_display_name(s: &str) -> bool {
    !s.trim().is_empty()
        && s.chars().count() <= DISPLAY_NAME_MAX_LEN
        && !s.chars().any(|c| {
            c.is_control()
                || matches!(c,
                    '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}')
                || c == '<'
                || c == '>'
        })
}

/// The message a rejected display name is refused with.
pub const DISPLAY_NAME_INVALID: &str =
    "name must be 1-100 characters, without control characters, bidi overrides, '<' or '>'";

/// [`WriteOutcome`] plus `Invalid`: a malformed value is a 400, not a permission problem.
#[derive(Debug)]
pub enum ProfileOutcome {
    Ok,
    Forbidden,
    Invalid(&'static str),
}

pub(crate) fn profile_outcome_response<B>(outcome: ProfileOutcome) -> Result<Response, AppError>
where
    B: pollis_api::DsRequest<Response = pollis_api::StatusOk>,
{
    Ok(match outcome {
        ProfileOutcome::Ok => crate::writes::ok_response::<B>(pollis_api::StatusOk::Ok),
        ProfileOutcome::Forbidden => AuthRejection::Forbidden.into_response(),
        ProfileOutcome::Invalid(why) => bad_request(why),
    })
}

/// COALESCE-UPDATE the user's own profile row. Authz: the actor may only edit
/// their own `users` row (`user_id` bound to the authenticated user).
///
/// A `username` must pass [`is_valid_username`] unless it equals the stored one; `@` never passes.
pub async fn apply_update_profile(
    conn: &Connection,
    authed: Option<&str>,
    body: &UpdateProfileBody,
) -> anyhow::Result<ProfileOutcome> {
    let user = match resolve_actor(authed, Some(body.user_id.as_str())) {
        Ok(u) => u,
        Err(_) => return Ok(ProfileOutcome::Forbidden),
    };
    if let Some(username) = body.username.as_deref() {
        let current = current_username(conn, &user).await?;
        let unchanged = current.as_deref() == Some(username);
        let grandfathered = unchanged && !username.contains('@');
        if !grandfathered && !is_valid_username(username) {
            return Ok(ProfileOutcome::Invalid(USERNAME_INVALID));
        }
    }
    // #1095: a preferred_name reaches other people's notification banners.
    if let Some(preferred) = body.preferred_name.as_deref() {
        if !is_valid_display_name(preferred) {
            return Ok(ProfileOutcome::Invalid(DISPLAY_NAME_INVALID));
        }
    }
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
    Ok(ProfileOutcome::Ok)
}

async fn current_username(conn: &Connection, user_id: &str) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT username FROM users WHERE id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get(0)?),
        None => Ok(None),
    }
}

pub async fn save_preferences(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<SavePreferencesBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<SavePreferencesBody>(apply_save_preferences(&conn, authed.as_deref(), &parsed).await?)
}

/// Merge a save into the user's `user_preferences` row (#1650). Authz: the actor may only
/// write their own preferences (`user_id` bound to the authenticated user).
///
/// The blob is shared by desktop and mobile, and each knows only some of its keys, so a save
/// is a top-level merge, never a replacement: a key the save does not name keeps its stored
/// value, a key it names takes the new value whole (nested objects included), and a key it
/// sets to `null` is deleted. Merging here, in one statement, means no writer can drop a key
/// it does not know, old builds included. The stored blob is always a JSON object: a save
/// that is not one is refused.
pub async fn apply_save_preferences(
    conn: &Connection,
    authed: Option<&str>,
    body: &SavePreferencesBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, Some(body.user_id.as_str())) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    let Some(clear) = clear_named_keys(&body.preferences) else {
        return Ok(WriteOutcome::Invalid("preferences must be a JSON object"));
    };
    // `json_patch` is RFC 7396, which merges nested objects. Nulling every named key first
    // makes the second patch replace each one whole instead.
    conn.execute(
        "INSERT INTO user_preferences (user_id, preferences, updated_at) \
             VALUES (?1, json_patch('{}', ?3), datetime('now')) \
         ON CONFLICT(user_id) DO UPDATE SET \
             preferences = json_patch(json_patch( \
                 CASE WHEN json_valid(user_preferences.preferences) \
                       AND json_type(user_preferences.preferences) = 'object' \
                      THEN user_preferences.preferences ELSE '{}' END, \
                 ?2), ?3), \
             updated_at = datetime('now')",
        libsql::params![user, clear, body.preferences.clone()],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

/// The merge patch that removes every top-level key a save names; `None` when the save is
/// not a JSON object.
fn clear_named_keys(raw: &str) -> Option<String> {
    let serde_json::Value::Object(map) = serde_json::from_str::<serde_json::Value>(raw).ok()? else {
        return None;
    };
    let clear: serde_json::Map<String, serde_json::Value> =
        map.into_iter().map(|(k, _)| (k, serde_json::Value::Null)).collect();
    Some(serde_json::Value::Object(clear).to_string())
}

/// Ceiling on one account's stored cursor blob.
///
/// ~120 bytes per conversation; an opaque blob with no cap would be a free object store.
const READ_CURSOR_BLOB_MAX_BYTES: usize = 256 * 1024;

/// AES-256-GCM nonce length.
const READ_CURSOR_NONCE_LEN: usize = 12;

/// The result of a read-cursor push; `Invalid` means a malformed envelope.
pub type ReadCursorOutcome = ProfileOutcome;

pub async fn save_read_cursors(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<SaveReadCursorsBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let outcome = apply_save_read_cursors(&conn, authed.as_deref(), &parsed).await?;
    if matches!(outcome, ReadCursorOutcome::Ok) {
        // #1628: nudge the account's other running devices to pull the new reads. Content-free,
        // like `channel_mutes_changed`; the write it follows already tells the DS the timing.
        let user = authed.clone().unwrap_or_else(|| parsed.user_id.clone());
        let inbox = format!("inbox-{user}");
        let event = serde_json::json!({ "type": "read_cursors_changed" });
        if let Err(e) = crate::broker::room_send_data(&state, &inbox, &event).await {
            tracing::warn!("read-cursor inbox notify failed (non-fatal): {e}");
        }
    }
    profile_outcome_response::<SaveReadCursorsBody>(outcome)
}

/// UPSERT the account's encrypted read-cursor blob (#844).
///
/// Authz: the actor's OWN cursors only.
///
/// Only the ENVELOPE is validated; the DS holds no key for the contents (#844).
///
/// Last-writer-wins: clients merge by `max()` after decryption, and cursors only move forward.
pub async fn apply_save_read_cursors(
    conn: &Connection,
    authed: Option<&str>,
    body: &SaveReadCursorsBody,
) -> anyhow::Result<ReadCursorOutcome> {
    let user = match resolve_actor(authed, Some(body.user_id.as_str())) {
        Ok(u) => u,
        Err(_) => return Ok(ReadCursorOutcome::Forbidden),
    };
    let Ok(nonce) = b64_decode(&body.nonce) else {
        return Ok(ReadCursorOutcome::Invalid("nonce is not valid base64"));
    };
    if nonce.len() != READ_CURSOR_NONCE_LEN {
        return Ok(ReadCursorOutcome::Invalid("nonce must be 12 bytes"));
    }
    let Ok(blob) = b64_decode(&body.blob) else {
        return Ok(ReadCursorOutcome::Invalid("blob is not valid base64"));
    };
    if blob.is_empty() || blob.len() > READ_CURSOR_BLOB_MAX_BYTES {
        return Ok(ReadCursorOutcome::Invalid("blob size out of range"));
    }

    conn.execute(
        "INSERT INTO read_cursor_sync (user_id, nonce, blob, updated_at) \
             VALUES (?1, ?2, ?3, datetime('now')) \
         ON CONFLICT(user_id) DO UPDATE SET \
             nonce = ?2, blob = ?3, updated_at = datetime('now')",
        libsql::params![user, nonce, blob],
    )
    .await?;
    Ok(ReadCursorOutcome::Ok)
}

pub async fn block_user(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<AddBlock>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let outcome = apply_block_user(&conn, authed.as_deref(), &parsed.0).await?;
    // The block un-authorizes the shared DM's realtime room; this ends the session they hold.
    if matches!(outcome, WriteOutcome::Ok) {
        let blocker = authed
            .clone()
            .unwrap_or_else(|| parsed.0.blocker_id.clone());
        let rooms = crate::broker::shared_dm_rooms(&conn, &blocker, &parsed.0.blocked_id).await?;
        crate::broker::evict_user_from_rooms(&state, &rooms, &parsed.0.blocked_id).await;
    }
    outcome_response::<AddBlock>(outcome)
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

pub async fn unblock_user(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<RemoveBlock>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
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

pub async fn set_channel_mute(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<SetChannelMuteBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let outcome = apply_set_channel_mute(&conn, authed.as_deref(), &parsed).await?;
    if matches!(outcome, WriteOutcome::Ok) {
        // Nudge the account's other devices to re-read their mutes. Best-effort.
        let user = authed.clone().unwrap_or_else(|| parsed.user_id.clone());
        let inbox = format!("inbox-{user}");
        let event = serde_json::json!({ "type": "channel_mutes_changed" });
        if let Err(e) = crate::broker::room_send_data(&state, &inbox, &event).await {
            tracing::warn!("channel-mute inbox notify failed (non-fatal): {e}");
        }
    }
    outcome_response::<SetChannelMuteBody>(outcome)
}

/// Mute or unmute one group channel for the caller's account (#1431).
///
/// Authz: a group CHANNEL the actor is in; the row is the mute, so both ways are idempotent.
pub async fn apply_set_channel_mute(
    conn: &Connection,
    authed: Option<&str>,
    body: &SetChannelMuteBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, Some(body.user_id.as_str())) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    if body.muted {
        if !is_channel_member(conn, &body.channel_id, &user).await? {
            return Ok(WriteOutcome::Forbidden);
        }
        conn.execute(
            "INSERT OR IGNORE INTO channel_mute (user_id, channel_id) VALUES (?1, ?2)",
            libsql::params![user, body.channel_id.clone()],
        )
        .await?;
    } else {
        // Unmuting needs no membership check: the DELETE is scoped to the actor's own rows.
        conn.execute(
            "DELETE FROM channel_mute WHERE user_id = ?1 AND channel_id = ?2",
            libsql::params![user, body.channel_id.clone()],
        )
        .await?;
    }
    Ok(WriteOutcome::Ok)
}

/// True when `channel_id` names a group CHANNEL (not a DM, not a group) whose
/// group `user_id` belongs to.
async fn is_channel_member(
    conn: &Connection,
    channel_id: &str,
    user_id: &str,
) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM channels c \
             JOIN group_member gm ON gm.group_id = c.group_id \
             WHERE c.id = ?1 AND gm.user_id = ?2 LIMIT 1",
            libsql::params![channel_id.to_string(), user_id.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

pub async fn read_channel_mutes(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: ChannelMutesBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match crate::reads::authed_user(&state, &req, Some(parsed.user_id.as_str())).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    Ok(crate::writes::ok_response::<ChannelMutesBody>(
        list_channel_mutes(&conn, &who).await?,
    ))
}

/// The channels `user_id` muted and is still a member of, sorted by id.
///
/// The membership join drops rows a pre-#1431 removal left behind.
pub async fn list_channel_mutes(
    conn: &Connection,
    user_id: &str,
) -> anyhow::Result<ChannelMutesResponse> {
    let mut rows = conn
        .query(
            "SELECT cm.channel_id FROM channel_mute cm \
             JOIN channels c ON c.id = cm.channel_id \
             JOIN group_member gm ON gm.group_id = c.group_id AND gm.user_id = cm.user_id \
             WHERE cm.user_id = ?1 ORDER BY cm.channel_id",
            libsql::params![user_id.to_string()],
        )
        .await?;
    let mut channel_ids = Vec::new();
    while let Some(row) = rows.next().await? {
        channel_ids.push(row.get::<String>(0)?);
    }
    Ok(ChannelMutesResponse { channel_ids })
}

pub async fn create_dm(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<CreateDmBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
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
/// Authz: the creator is the caller; no pairing may be blocked. 2-person DMs dedupe here (#987).
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
    // Reported as `Blocked`, NOT `Forbidden`, so the generic refusal reveals nothing.
    if authed.is_some() {
        for other in &others {
            if is_blocked_either_way(conn, &creator, other).await? {
                return Ok(DmOutcome::Blocked);
            }
        }
    }

    // 2-person dedupe: an existing exact {creator, target} channel is returned.
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

    // Refuse an id used by another conversation kind; `is_member` ORs across kinds on one id.
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
    // Seed a watermark per live (member, device) pair (#685) so envelope GC isn't blocked.
    let cursor = crate::messages::seeded_watermark_cursor();
    for member in std::iter::once(&creator).chain(others.iter()) {
        // `reported_at` marks the device LIVE (#720); the cursor format differs on purpose (#908).
        tx.execute(
            "INSERT OR IGNORE INTO conversation_watermark \
                 (conversation_id, user_id, device_id, last_fetched_at, last_seq, reported_at) \
             SELECT ?1, ?2, ud.device_id, ?3, \
                    COALESCE((SELECT next_seq FROM conversation_seq WHERE conversation_id = ?1), 0), \
                    datetime('now') \
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

pub async fn accept_dm(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<AcceptDmBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<AcceptDmBody>(apply_accept_dm(&conn, authed.as_deref(), &parsed).await?)
}

/// Flip the actor's own `accepted_at` from NULL → now; scoped to `user_id`, idempotent.
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

/// True when `user_id` is a member of DM `dm_channel_id` (DM table only, unlike `is_member`).
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

pub async fn add_dm_member(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<AddDmMemberBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<AddDmMemberBody>(apply_add_dm_member(&conn, authed.as_deref(), &parsed).await?)
}

/// Add a DM member and seed their watermarks; the actor must be a current member.
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
    // Seed the new member's live devices (#685); `reported_at` LIVE (#720), cursor per #908.
    tx.execute(
        "INSERT OR IGNORE INTO conversation_watermark \
             (conversation_id, user_id, device_id, last_fetched_at, last_seq, reported_at) \
         SELECT ?1, ?2, ud.device_id, ?3, \
                COALESCE((SELECT next_seq FROM conversation_seq WHERE conversation_id = ?1), 0), \
                datetime('now') \
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

pub async fn remove_dm_member(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<RemoveDmMemberBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let (outcome, torn_down) = apply_remove_dm_member(&conn, authed.as_deref(), &parsed).await?;
    // Same follow-through `/v1/dm/leave` has: the control-plane tables live on
    // the log DB, which the main-DB transaction cannot reach.
    if matches!(outcome, WriteOutcome::Ok) && torn_down {
        let log_conn = state.log_db.conn().await?;
        crate::teardown::purge_conversation_log(
            &log_conn,
            std::slice::from_ref(&parsed.dm_channel_id),
        )
        .await?;
    }
    outcome_response::<RemoveDmMemberBody>(outcome)
}

/// Delete a `dm_channel_member` row: yourself, or anyone if you created the channel.
///
/// Emptying a DM here tears it down exactly as `/v1/dm/leave` does (L3).
///
/// Returns `(outcome, torn_down)` like [`apply_leave_dm`]; the caller does the
/// log-DB half.
pub async fn apply_remove_dm_member(
    conn: &Connection,
    authed: Option<&str>,
    body: &RemoveDmMemberBody,
) -> anyhow::Result<(WriteOutcome, bool)> {
    let actor = match resolve_actor(authed, Some(body.requester_id.as_str())) {
        Ok(a) => a,
        Err(o) => return Ok((o, false)),
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
                    return Ok((WriteOutcome::Forbidden, false));
                }
            }
            // Channel doesn't exist — nothing to remove; refuse rather than
            // silently no-op a write the actor isn't entitled to.
            None => return Ok((WriteOutcome::Forbidden, false)),
        }
    }
    let tx = conn.transaction().await?;
    tx.execute(
        "DELETE FROM dm_channel_member WHERE dm_channel_id = ?1 AND user_id = ?2",
        libsql::params![body.dm_channel_id.clone(), body.user_id.clone()],
    )
    .await?;
    // Existence, not a tally (see `groups::apply_leave_group`); same purge as `/v1/dm/leave`.
    let mut survivors = tx
        .query(
            "SELECT 1 FROM dm_channel_member WHERE dm_channel_id = ?1 LIMIT 1",
            libsql::params![body.dm_channel_id.clone()],
        )
        .await?;
    let torn_down = survivors.next().await?.is_none();
    drop(survivors);
    if torn_down {
        crate::teardown::purge_dm_channel(&tx, &body.dm_channel_id).await?;
    }
    tx.commit().await?;
    Ok((WriteOutcome::Ok, torn_down))
}

pub async fn leave_dm(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<LeaveDmBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
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

/// Leave a DM (self only) and tear it down when empty, in one transaction.
///
/// [`crate::teardown::purge_dm_channel`] is the full set (no cascades in production).
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
    // If no members remain, clean up the channel and all associated data.
    // Existence, not a tally — see `groups::apply_leave_group`.
    let mut survivors = tx
        .query(
            "SELECT 1 FROM dm_channel_member WHERE dm_channel_id = ?1 LIMIT 1",
            libsql::params![body.dm_channel_id.clone()],
        )
        .await?;
    let torn_down = survivors.next().await?.is_none();
    drop(survivors);
    if torn_down {
        crate::teardown::purge_dm_channel(&tx, &body.dm_channel_id).await?;
    }
    tx.commit().await?;
    Ok((WriteOutcome::Ok, torn_down))
}

#[cfg(test)]
mod display_name_tests {
    use super::{is_valid_display_name, DISPLAY_NAME_MAX_LEN};

    /// A display name is rendered, not resolved — a rule that rejected real
    /// names would be a bug, not hardening.
    #[test]
    fn real_names_are_accepted() {
        for ok in [
            "Alice",
            "Bob's laptop",
            "Café déjà vu",
            "研究チーム",
            "Ops & Infra",
            "team-1.0_v2",
            "  padded  ",
        ] {
            assert!(is_valid_display_name(ok), "{ok:?} must be accepted");
        }
    }

    /// #1095's payload: markup that a Linux notification daemon would render.
    #[test]
    fn markup_characters_are_refused() {
        for bad in [
            "<img src=\"http://attacker.test/p.png\">",
            "<a href=\"http://x\">x</a>",
            "team <b>",
            "a > b",
        ] {
            assert!(!is_valid_display_name(bad), "{bad:?} must be refused");
        }
    }

    #[test]
    fn control_characters_and_newlines_are_refused() {
        for bad in ["two\nlines", "tab\there", "nul\0byte", "bell\u{7}"] {
            assert!(!is_valid_display_name(bad), "{bad:?} must be refused");
        }
    }

    /// Display-order spoofing: the name a person reads is not the name stored.
    #[test]
    fn bidi_overrides_are_refused() {
        for bad in ["admin\u{202E}gpj.exe", "x\u{202A}y", "x\u{2066}y", "x\u{200F}y"] {
            assert!(!is_valid_display_name(bad), "{bad:?} must be refused");
        }
    }

    #[test]
    fn the_bound_is_on_characters_not_bytes() {
        // 100 non-ASCII characters is 100 characters, not 300 bytes.
        let wide = "é".repeat(DISPLAY_NAME_MAX_LEN);
        assert!(is_valid_display_name(&wide), "exactly the cap is allowed");
        let too_wide = "é".repeat(DISPLAY_NAME_MAX_LEN + 1);
        assert!(!is_valid_display_name(&too_wide), "one over the cap is refused");
    }

    #[test]
    fn an_empty_or_whitespace_name_is_refused() {
        for bad in ["", "   ", "\u{00a0}"] {
            assert!(!is_valid_display_name(bad), "{bad:?} must be refused");
        }
    }
}
