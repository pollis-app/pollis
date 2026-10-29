//! Profile, preferences, read cursors, blocks, channel mutes and DMs. All
//! main-DB writes; each `apply_*` holds both the authz and the write, so the
//! harness and the handler share one path.
//!
//! On the no-auth path (`authed == None`) identity falls back to the body actor
//! and membership/block checks are skipped, as in `messages` / `writes`.

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

// Request bodies are declared once in `pollis-api`, shared with pollis-core.
pub use pollis_api::profile::*;

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

pub const USERNAME_MIN_LEN: usize = 3;
pub const USERNAME_MAX_LEN: usize = 32;

/// The username character class `[a-z0-9_.-]`.
///
/// Must never admit `@`: identifier lookups pick the `email` or `username`
/// column by whether the identifier contains one, so a username like
/// `alice@corp.com` would capture invites meant for that email. The `@` half is
/// also enforced by migration `000021`'s triggers. Mirrored client-side
/// (`frontend/src/utils/username.ts`, `mobile/lib/username.ts`).
pub fn is_username_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '.' | '-')
}

/// `^[a-z0-9_.-]{3,32}$`, spelled without a regex dependency.
pub fn is_valid_username(s: &str) -> bool {
    (USERNAME_MIN_LEN..=USERNAME_MAX_LEN).contains(&s.len()) && s.chars().all(is_username_char)
}

pub const USERNAME_INVALID: &str =
    "username must be 3-32 characters of a-z, 0-9, '_', '.' or '-'";

/// Upper bound on a `preferred_name` or group name (#1095), in characters.
pub const DISPLAY_NAME_MAX_LEN: usize = 100;

/// Whether `s` is acceptable as a human display name.
///
/// Not the username charset: a display name is rendered, never resolved, so any
/// script and punctuation passes. Refused: over [`DISPLAY_NAME_MAX_LEN`],
/// control characters, bidi overrides/isolates and LRM/RLM (display-order
/// spoofing), and `<`/`>` (#1095; the notification sink also escapes them). `&`
/// is allowed.
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

pub const DISPLAY_NAME_INVALID: &str =
    "name must be 1-100 characters, without control characters, bidi overrides, '<' or '>'";

/// [`WriteOutcome`] plus `Invalid`: an impossible value is a 400, not a 403. The
/// `&'static str` means no caller input is ever echoed back.
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

/// COALESCE-UPDATE the actor's own profile row.
///
/// A username equal to the stored one passes unvalidated, because clients
/// re-send it on every save and legacy default names (`alice_7XK2`) fail
/// [`is_valid_username`]. The carve-out never extends to `@`.
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

/// Ceiling on one account's cursor blob (well over a thousand conversations).
/// An uncapped opaque blob would be a free per-account object store.
const READ_CURSOR_BLOB_MAX_BYTES: usize = 256 * 1024;

/// AES-256-GCM nonce length.
const READ_CURSOR_NONCE_LEN: usize = 12;

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
    profile_outcome_response::<SaveReadCursorsBody>(
        apply_save_read_cursors(&conn, authed.as_deref(), &parsed).await?,
    )
}

/// UPSERT the account's encrypted read-cursor blob (#844). Only the envelope is
/// validated; the DS holds no key for it.
///
/// Last-writer-wins is deliberate: clients merge with a `max()` after
/// decryption and cursors only move forward, so a race loser is re-pushed.
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
    // The block un-authorizes the blocked user for the shared DM's realtime room
    // (`broker::dm_peer_blocked`); this ends the session they already hold.
    if matches!(outcome, WriteOutcome::Ok) {
        let blocker = authed
            .clone()
            .unwrap_or_else(|| parsed.0.blocker_id.clone());
        let rooms = crate::broker::shared_dm_rooms(&conn, &blocker, &parsed.0.blocked_id).await?;
        crate::broker::evict_user_from_rooms(&state, &rooms, &parsed.0.blocked_id).await;
    }
    outcome_response::<AddBlock>(outcome)
}

/// Insert a block row and reset the blocker's `accepted_at` on every shared DM,
/// so an unblock later resurfaces the channel as a request.
pub async fn apply_block_user(
    conn: &Connection,
    authed: Option<&str>,
    body: &BlockBody,
) -> anyhow::Result<WriteOutcome> {
    let blocker = match resolve_actor(authed, Some(body.blocker_id.as_str())) {
        Ok(b) => b,
        Err(o) => return Ok(o),
    };
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
        // Tell the account's other devices to re-read their mutes. DS-sent (a
        // client may not publish this type); best-effort, since devices re-read
        // on launch or foreground.
        let user = authed.clone().unwrap_or_else(|| parsed.user_id.clone());
        let inbox = format!("inbox-{user}");
        let event = serde_json::json!({ "type": "channel_mutes_changed" });
        if let Err(e) = crate::broker::room_send_data(&state, &inbox, &event).await {
            tracing::warn!("channel-mute inbox notify failed (non-fatal): {e}");
        }
    }
    outcome_response::<SetChannelMuteBody>(outcome)
}

/// Mute or unmute one group channel (#1431). Stored readable because the push
/// fan-out reads it. A mute row can only name a channel the actor is in; the
/// row's existence is the mute, so both directions are idempotent.
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
        // No membership check: a former member clearing a stale row is harmless.
        conn.execute(
            "DELETE FROM channel_mute WHERE user_id = ?1 AND channel_id = ?2",
            libsql::params![user, body.channel_id.clone()],
        )
        .await?;
    }
    Ok(WriteOutcome::Ok)
}

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

/// The channels `user_id` muted and is still a member of, sorted by id. The
/// membership join filters stale rows a removal left behind.
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

/// What `apply_create_dm` decided. `Blocked` is not an authorization failure,
/// and folding it into `Forbidden` would let a client tell "you may not" from
/// "this pairing is blocked".
#[derive(Debug)]
pub enum DmOutcome {
    Created { id: String, existing: bool },
    Blocked,
    Forbidden,
}

/// Create a DM channel, its memberships (creator accepted, others pending) and
/// every member's per-device watermarks in one transaction. A 2-person DM
/// dedupes to the existing pair's channel, so one DM per pair is a property of
/// the write, not of client timing (#987); 3+ members always create fresh.
pub async fn apply_create_dm(
    conn: &Connection,
    authed: Option<&str>,
    body: &CreateDmBody,
) -> anyhow::Result<DmOutcome> {
    let creator = match resolve_actor(authed, Some(body.creator_id.as_str())) {
        Ok(c) => c,
        Err(_) => return Ok(DmOutcome::Forbidden),
    };

    let mut others: Vec<String> = Vec::new();
    for m in &body.member_ids {
        if *m != creator && !others.contains(m) {
            others.push(m.clone());
        }
    }
    if others.is_empty() {
        return Ok(DmOutcome::Forbidden);
    }

    if authed.is_some() {
        for other in &others {
            if is_blocked_either_way(conn, &creator, other).await? {
                return Ok(DmOutcome::Blocked);
            }
        }
    }

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

    // `is_member` ORs across conversation kinds, so reusing another
    // conversation's id would grant membership of it.
    if conversation_id_taken(conn, &body.id).await? {
        return Ok(DmOutcome::Forbidden);
    }
    let tx = conn.transaction().await?;
    // Same transaction, so a refused claim rolls the DM back (#880).
    claim_conversation_id(&tx, &body.id, ConversationKind::Dm).await?;
    tx.execute(
        "INSERT INTO dm_channel (id, created_by, created_at) VALUES (?1, ?2, ?3)",
        libsql::params![body.id.clone(), creator.clone(), body.created_at.clone()],
    )
    .await?;
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
    // Seed every live (member, device) watermark so envelope GC is not blocked;
    // revoked devices are not part of the GC roster (#685).
    let cursor = crate::messages::seeded_watermark_cursor();
    for member in std::iter::once(&creator).chain(others.iter()) {
        // `reported_at` marks the device live as of now (#720); the two time
        // columns differ in format on purpose — see `seeded_watermark_cursor`.
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

/// Accept the actor's own pending DM membership; idempotent.
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

/// DM-only membership (unlike `writes::is_member`, which also matches groups).
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

/// Add a member to a DM the actor belongs to, seeding their per-device
/// watermarks in the same transaction.
pub async fn apply_add_dm_member(
    conn: &Connection,
    authed: Option<&str>,
    body: &AddDmMemberBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, Some(body.added_by.as_str())) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
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
    // As in `apply_create_dm`'s watermark seeding.
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
    // The log DB cannot join the main-DB transaction.
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

/// Remove a DM member: the actor may remove themselves, or any member as the
/// channel's creator. Emptying the DM tears it down exactly as
/// [`apply_leave_dm`] does, since both doors reach the same state. Returns
/// `(outcome, torn_down)`; the caller does the log-DB half.
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
            None => return Ok((WriteOutcome::Forbidden, false)),
        }
    }
    let tx = conn.transaction().await?;
    tx.execute(
        "DELETE FROM dm_channel_member WHERE dm_channel_id = ?1 AND user_id = ?2",
        libsql::params![body.dm_channel_id.clone(), body.user_id.clone()],
    )
    .await?;
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

/// Remove the actor's own DM membership and, when the channel is left empty,
/// tear it down in the same transaction. The `bool` is "the channel was
/// destroyed", so the caller knows to purge its commit-log state.
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

    /// #1095's payload.
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

    #[test]
    fn bidi_overrides_are_refused() {
        for bad in ["admin\u{202E}gpj.exe", "x\u{202A}y", "x\u{2066}y", "x\u{200F}y"] {
            assert!(!is_valid_display_name(bad), "{bad:?} must be refused");
        }
    }

    #[test]
    fn the_bound_is_on_characters_not_bytes() {
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
