//! Domain A — message envelopes, edits/deletes, reactions, watermarks, and
//! attachment dedup rows. The **reference vertical slice** for Goal B (#419):
//! every later write-domain (B/C/D) copies the shape established here.
//!
//! ## The per-domain convention (copy this)
//!
//!   - **Bodies** — one `#[derive(Deserialize)] *Body` per endpoint. Binary
//!     fields are base64 (STANDARD); everything else is plain JSON. (Domain A
//!     has no binary fields — `ciphertext` is already the `"mls:<hex>"` text the
//!     client stores, and content hashes / R2 keys are text.)
//!   - **Pure conn-level fns** — `apply_*` functions take a bare
//!     [`Connection`], the authenticated user (`Option<&str>`; `None` only on
//!     the no-auth path), and a parsed `*Body`. They embed BOTH the
//!     authorization decision AND the write, returning [`WriteOutcome`]. Putting
//!     authz inside the pure fn (rather than the axum handler, as `writes.rs`
//!     does) is deliberate: it makes the in-process test harness exercise the
//!     *exact* same authz the production handler runs, with zero duplication —
//!     both call sites are reduced to `gate → parse → apply → map outcome`.
//!   - **axum handlers** — `(State, Method, Uri, HeaderMap, Bytes) -> Response`,
//!     all identical in shape. They `gate` (shared auth, see
//!     [`crate::writes::gate`]), parse, call the matching `apply_*`, and map the
//!     outcome to 200 / 403 / 400 / 500.
//!
//! ## Where the writes land
//!
//! Every domain-A table lives in the **MAIN DB** (`state.db`), NOT the commit-log
//! DB — these are message-delivery rows, not MLS control-plane rows. So all
//! `apply_*` fns run on the main connection.
//!
//! ## Authorization (the security core)
//!
//! `gate` proves *which user* signed the request; each `apply_*` then proves the
//! user is *allowed* to make that specific write:
//!   - send / edit / delete: the user is a current member of the conversation
//!     (reusing [`crate::writes::is_member`]); edit/delete of a specific message
//!     additionally requires the user be the message's sender, or — for an
//!     admin-delete — a group admin of the channel's owning group.
//!   - reactions: the user is a member, and may only write/remove their OWN
//!     reaction (`user_id` is bound to the authenticated user).
//!   - watermark: the row is per `(conversation, user, device)`; the user may
//!     only advance their own.
//!   - envelope GC: the user is a member. (See the TODO on [`apply_envelope_gc`]
//!     — moving the *trigger* server-side does not yet make GC many-member
//!     correct; that redesign is out of scope for this slice.)
//!
//! On the no-auth path (`authed == None`, only reachable when the DS runs with
//! `POLLIS_DS_REQUIRE_AUTH` off) the membership/identity checks are skipped and
//! the actor comes from the body — mirroring `commit::submit` and `writes.rs`.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, Uri},
    response::{IntoResponse, Response},
};
use libsql::Connection;
use serde::Deserialize;
use ulid::Ulid;

use crate::error::AppError;
use crate::writes::{
    bad_request, gate, is_member, ok_json, outcome_response, resolve_actor, WriteOutcome,
};
use crate::AppState;

// ── Envelope GC SQL (mirrors pollis-core's ingest.rs, byte-for-byte) ─────────
//
// Envelope cleanup: TTL gate OR watermark gate (OR'd — either alone deletes).
// The watermark gate is keyed on (user, device): a multi-device user whose other
// device hasn't synced keeps envelopes alive until every device catches up or the
// 30-day TTL expires. Copied verbatim from `pollis_core::commands::messages::ingest`
// so moving the *trigger* behind the DS does not change deletion behavior.

const CLEANUP_CHANNEL_ENVELOPES: &str = "\
DELETE FROM message_envelope
 WHERE conversation_id = ?1
   AND (
     sent_at < datetime('now', '-30 days')
     OR sent_at < (
       SELECT CASE
                WHEN COUNT(ud.device_id) = COUNT(cw.last_fetched_at)
                THEN MIN(cw.last_fetched_at)
                ELSE NULL
              END
       FROM group_member gm
       JOIN channels c ON c.id = ?1 AND c.group_id = gm.group_id
       JOIN user_device ud ON ud.user_id = gm.user_id
       LEFT JOIN conversation_watermark cw
              ON cw.conversation_id = ?1
             AND cw.user_id = ud.user_id
             AND cw.device_id = ud.device_id
     )
   )";

const CLEANUP_DM_ENVELOPES: &str = "\
DELETE FROM message_envelope
 WHERE conversation_id = ?1
   AND (
     sent_at < datetime('now', '-30 days')
     OR sent_at < (
       SELECT CASE
                WHEN COUNT(ud.device_id) = COUNT(cw.last_fetched_at)
                THEN MIN(cw.last_fetched_at)
                ELSE NULL
              END
       FROM dm_channel_member dcm
       JOIN user_device ud ON ud.user_id = dcm.user_id
       LEFT JOIN conversation_watermark cw
              ON cw.conversation_id = ?1
             AND cw.user_id = ud.user_id
             AND cw.device_id = ud.device_id
       WHERE dcm.dm_channel_id = ?1
     )
   )";

// ── Shared authz helpers ─────────────────────────────────────────────────────

/// Resolve the acting user for a domain-A write.
///
///   - auth ON (`authed = Some`) → the authenticated user. A body-supplied actor
///     (if any) must equal it, else the request is forging another identity
///     (`Forbidden`).
///   - auth OFF (`authed = None`) → the body's actor (the no-auth path has no
///     signed identity). Missing/empty → `Forbidden` (no actor at all).
/// The original sender of a `type='message'` envelope, if it's still present
/// (it may have aged out via watermark/TTL GC).
async fn original_sender(conn: &Connection, message_id: &str) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT sender_id FROM message_envelope WHERE id = ?1 AND type = 'message'",
            libsql::params![message_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(row.get::<String>(0)?),
        None => None,
    })
}

/// The conversation a message belongs to, resolved from any envelope carrying
/// its id (used to membership-gate reactions, which only know `message_id`).
async fn conversation_for_message(
    conn: &Connection,
    message_id: &str,
) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT conversation_id FROM message_envelope WHERE id = ?1 LIMIT 1",
            libsql::params![message_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(row.get::<String>(0)?),
        None => None,
    })
}

/// The caller's role in the group that owns `conversation_id` (a text channel).
/// `None` when the conversation is not a group channel (e.g. a DM, which has no
/// `channels` row and no admin concept) or the caller is not a member.
async fn channel_group_role(
    conn: &Connection,
    conversation_id: &str,
    user_id: &str,
) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT gm.role FROM channels c \
             JOIN group_member gm ON gm.group_id = c.group_id \
             WHERE c.id = ?1 AND gm.user_id = ?2",
            libsql::params![conversation_id.to_string(), user_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(row.get::<String>(0)?),
        None => None,
    })
}

fn now_rfc3339() -> String {
    // RFC3339 with no extra deps — mirrors pollis-core's chrono output closely
    // enough for the textual `sent_at`/`created_at` columns (lexical ordering is
    // all the GC/watermark logic relies on).
    chrono_like_now()
}

/// Minimal RFC3339-ish timestamp. pollis-delivery has no `chrono` dep, so format
/// the unix epoch as an ISO-8601 UTC string. Only used for tombstone/reaction
/// rows whose timestamps are compared lexically, never parsed.
fn chrono_like_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0) as i64;
    // Days since epoch → civil date (Howard Hinnant's algorithm).
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{hh:02}:{mm:02}:{ss:02}+00:00")
}

// ── POST /v1/messages/send ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SendMessageBody {
    pub id: String,
    pub conversation_id: String,
    /// Bound to the authenticated user when signed; the no-auth fallback only.
    #[serde(default)]
    pub sender_id: Option<String>,
    /// The `"mls:<hex>"` ciphertext string the client persists — plain text, not
    /// binary, so no base64.
    pub ciphertext: String,
    #[serde(default)]
    pub reply_to_id: Option<String>,
    pub sent_at: String,
}

pub async fn send_message(
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
    let parsed: SendMessageBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_send_message(&conn, authed.as_deref(), &parsed).await?)
}

/// INSERT a `type='message'` envelope (the send). Authz: the sender is a current
/// member of the conversation, and a signed request may only send as itself.
pub async fn apply_send_message(
    conn: &Connection,
    authed: Option<&str>,
    body: &SendMessageBody,
) -> anyhow::Result<WriteOutcome> {
    let sender = match resolve_actor(authed, body.sender_id.as_deref()) {
        Ok(s) => s,
        Err(o) => return Ok(o),
    };
    if authed.is_some() && !is_member(conn, &body.conversation_id, &sender).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    conn.execute(
        "INSERT INTO message_envelope \
             (id, conversation_id, sender_id, ciphertext, reply_to_id, sent_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        libsql::params![
            body.id.clone(),
            body.conversation_id.clone(),
            sender,
            body.ciphertext.clone(),
            body.reply_to_id.clone(),
            body.sent_at.clone(),
        ],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/messages/edit ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct EditMessageBody {
    pub envelope_id: String,
    pub conversation_id: String,
    pub target_message_id: String,
    #[serde(default)]
    pub sender_id: Option<String>,
    pub ciphertext: String,
    pub sent_at: String,
}

pub async fn edit_message(
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
    let parsed: EditMessageBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_edit_message(&conn, authed.as_deref(), &parsed).await?)
}

/// Replace the single pending edit envelope (DELETE prior + INSERT new) in one
/// transaction. Authz: the editor is a member AND — when the original message
/// envelope is still present — its sender (edits are sender-only).
pub async fn apply_edit_message(
    conn: &Connection,
    authed: Option<&str>,
    body: &EditMessageBody,
) -> anyhow::Result<WriteOutcome> {
    let sender = match resolve_actor(authed, body.sender_id.as_deref()) {
        Ok(s) => s,
        Err(o) => return Ok(o),
    };
    if authed.is_some() {
        if !is_member(conn, &body.conversation_id, &sender).await? {
            return Ok(WriteOutcome::Forbidden);
        }
        if let Some(orig) = original_sender(conn, &body.target_message_id).await? {
            if orig != sender {
                return Ok(WriteOutcome::Forbidden);
            }
        }
    }
    let tx = conn.transaction().await?;
    tx.execute(
        "DELETE FROM message_envelope \
         WHERE conversation_id = ?1 AND target_message_id = ?2 AND type = 'edit'",
        libsql::params![body.conversation_id.clone(), body.target_message_id.clone()],
    )
    .await?;
    tx.execute(
        "INSERT INTO message_envelope \
             (id, conversation_id, sender_id, ciphertext, sent_at, type, target_message_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'edit', ?6)",
        libsql::params![
            body.envelope_id.clone(),
            body.conversation_id.clone(),
            sender,
            body.ciphertext.clone(),
            body.sent_at.clone(),
            body.target_message_id.clone(),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/messages/delete ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct DeleteMessageBody {
    pub message_id: String,
    pub conversation_id: String,
    /// The original sender the client resolved (from the remote envelope or its
    /// local cache). Only consulted to pick self-vs-admin WHEN the envelope has
    /// already aged out; never trusted for the admin authz decision (the server
    /// re-derives the role independently).
    #[serde(default)]
    pub msg_sender_id: Option<String>,
    /// No-auth fallback for the acting user.
    #[serde(default)]
    pub actor_id: Option<String>,
}

pub async fn delete_message(
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
    let parsed: DeleteMessageBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_delete_message(&conn, authed.as_deref(), &parsed).await?)
}

/// Delete a message. Self-delete (actor is the sender) removes the envelope +
/// any pending edit. Admin-delete (actor is a group admin of the channel) also
/// writes a `type='delete'` tombstone so every member soft-deletes on next
/// ingest. The self-path SQL is scoped `sender_id = :actor` and the admin path
/// is gated on a re-derived admin role, so trusting the client's branch hint is
/// safe — a wrong hint deletes nothing or is refused.
pub async fn apply_delete_message(
    conn: &Connection,
    authed: Option<&str>,
    body: &DeleteMessageBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.actor_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };

    // Prefer the authoritative remote envelope; fall back to the client's hint
    // only when it's been GC'd.
    let effective_sender = original_sender(conn, &body.message_id)
        .await?
        .or_else(|| body.msg_sender_id.clone());
    let is_self_delete = effective_sender.as_deref() == Some(actor.as_str());

    if is_self_delete {
        let tx = conn.transaction().await?;
        tx.execute(
            "DELETE FROM message_envelope WHERE id = ?1 AND sender_id = ?2",
            libsql::params![body.message_id.clone(), actor.clone()],
        )
        .await?;
        tx.execute(
            "DELETE FROM message_envelope WHERE target_message_id = ?1 AND type = 'edit'",
            libsql::params![body.message_id.clone()],
        )
        .await?;
        tx.commit().await?;
        return Ok(WriteOutcome::Ok);
    }

    // Admin-delete: the actor must be an admin of the group owning this channel.
    // Skipped on the no-auth path (mirrors submit / writes.rs).
    if authed.is_some() {
        match channel_group_role(conn, &body.conversation_id, &actor).await? {
            Some(role) if role == "admin" => {}
            _ => return Ok(WriteOutcome::Forbidden),
        }
    }

    let tombstone_id = Ulid::new().to_string();
    let now = now_rfc3339();
    let tx = conn.transaction().await?;
    tx.execute(
        "DELETE FROM message_envelope WHERE id = ?1",
        libsql::params![body.message_id.clone()],
    )
    .await?;
    tx.execute(
        "DELETE FROM message_envelope WHERE target_message_id = ?1 AND type = 'edit'",
        libsql::params![body.message_id.clone()],
    )
    .await?;
    tx.execute(
        "INSERT INTO message_envelope \
             (id, conversation_id, sender_id, ciphertext, sent_at, type, target_message_id) \
         VALUES (?1, ?2, ?3, '', ?4, 'delete', ?5)",
        libsql::params![
            tombstone_id,
            body.conversation_id.clone(),
            actor,
            now,
            body.message_id.clone(),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/reactions/add  &  /v1/reactions/remove ──────────────────────────

#[derive(Deserialize)]
pub struct ReactionBody {
    pub message_id: String,
    pub emoji: String,
    /// No-auth fallback for the reacting user.
    #[serde(default)]
    pub user_id: Option<String>,
}

pub async fn add_reaction(
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
    let parsed: ReactionBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_add_reaction(&conn, authed.as_deref(), &parsed).await?)
}

pub async fn remove_reaction(
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
    let parsed: ReactionBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_remove_reaction(&conn, authed.as_deref(), &parsed).await?)
}

/// A reaction is membership-gated through the reacted-to message's envelope.
/// When the envelope has aged out we cannot resolve the conversation, so we
/// allow the write (it is already scoped to the actor's own `user_id`) —
/// reacting to a still-locally-visible but GC'd message must keep working.
pub async fn apply_add_reaction(
    conn: &Connection,
    authed: Option<&str>,
    body: &ReactionBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    if authed.is_some() {
        if let Some(conv) = conversation_for_message(conn, &body.message_id).await? {
            if !is_member(conn, &conv, &user).await? {
                return Ok(WriteOutcome::Forbidden);
            }
        }
    }
    let id = Ulid::new().to_string();
    let now = now_rfc3339();
    conn.execute(
        "INSERT OR IGNORE INTO message_reaction (id, message_id, user_id, emoji, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        libsql::params![id, body.message_id.clone(), user, body.emoji.clone(), now],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

pub async fn apply_remove_reaction(
    conn: &Connection,
    authed: Option<&str>,
    body: &ReactionBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    // A user may only ever remove their OWN reaction — the DELETE is scoped to
    // `user_id = :user`, so even without a membership lookup it cannot touch
    // anyone else's row. We still membership-gate when determinable.
    if authed.is_some() {
        if let Some(conv) = conversation_for_message(conn, &body.message_id).await? {
            if !is_member(conn, &conv, &user).await? {
                return Ok(WriteOutcome::Forbidden);
            }
        }
    }
    conn.execute(
        "DELETE FROM message_reaction WHERE message_id = ?1 AND user_id = ?2 AND emoji = ?3",
        libsql::params![body.message_id.clone(), user, body.emoji.clone()],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/watermarks/advance ──────────────────────────────────────────────

#[derive(Deserialize)]
pub struct WatermarkBody {
    pub conversation_id: String,
    /// No-auth fallback; when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
    pub device_id: String,
    pub last_fetched_at: String,
}

pub async fn advance_watermark(
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
    let parsed: WatermarkBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_advance_watermark(&conn, authed.as_deref(), &parsed).await?)
}

/// Monotone UPSERT of a `(conversation, user, device)` watermark. Authz: the row
/// belongs to the actor (`user_id` bound to the authenticated user).
pub async fn apply_advance_watermark(
    conn: &Connection,
    authed: Option<&str>,
    body: &WatermarkBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    conn.execute(
        "INSERT INTO conversation_watermark \
             (conversation_id, user_id, device_id, last_fetched_at) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(conversation_id, user_id, device_id) DO UPDATE SET \
             last_fetched_at = MAX(last_fetched_at, excluded.last_fetched_at)",
        libsql::params![
            body.conversation_id.clone(),
            user,
            body.device_id.clone(),
            body.last_fetched_at.clone(),
        ],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/envelopes/gc ────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct EnvelopeGcBody {
    pub conversation_id: String,
    /// `true` → DM cleanup query; `false` → group-channel cleanup query.
    pub is_dm: bool,
    /// No-auth fallback for the acting user.
    #[serde(default)]
    pub actor_id: Option<String>,
}

pub async fn envelope_gc(
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
    let parsed: EnvelopeGcBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_envelope_gc(&conn, authed.as_deref(), &parsed).await?)
}

/// Run the TTL-or-watermark envelope GC for a conversation. Authz: the actor is
/// a current member.
///
/// TODO(#419): GC should be gated on the MIN watermark across ALL members rather
/// than triggered by whichever member happens to ingest. The SQL already
/// AND-gates deletion on `COUNT(devices) == COUNT(watermarks)` plus `MIN(cw)` and
/// the 30-day TTL, so moving the *trigger* server-side here is behavior-
/// preserving — but it does NOT by itself make the trigger many-member correct.
/// That redesign is deliberately out of scope for this slice; do not change the
/// deletion predicate here.
pub async fn apply_envelope_gc(
    conn: &Connection,
    authed: Option<&str>,
    body: &EnvelopeGcBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.actor_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    if authed.is_some() && !is_member(conn, &body.conversation_id, &actor).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    let sql = if body.is_dm {
        CLEANUP_DM_ENVELOPES
    } else {
        CLEANUP_CHANNEL_ENVELOPES
    };
    conn.execute(sql, libsql::params![body.conversation_id.clone()])
        .await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/attachments/register  &  /v1/attachments/delete ─────────────────

#[derive(Deserialize)]
pub struct AttachmentRegisterBody {
    pub content_hash: String,
    pub r2_key: String,
}

#[derive(Deserialize)]
pub struct AttachmentDeleteBody {
    pub content_hash: String,
}

pub async fn register_attachment(
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
    let parsed: AttachmentRegisterBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_register_attachment(&conn, authed.as_deref(), &parsed).await?)
}

pub async fn delete_attachment(
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
    let parsed: AttachmentDeleteBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_delete_attachment(&conn, authed.as_deref(), &parsed).await?)
}

/// Register a convergent-encryption dedup row (`content_hash → r2_key`). Authz:
/// any authenticated user. There is no conversation context at upload time —
/// the row is content-addressed and identical for every uploader — so there is
/// nothing finer to gate on. The signature still proves a real device, which is
/// all the no-token model can assert here.
pub async fn apply_register_attachment(
    conn: &Connection,
    _authed: Option<&str>,
    body: &AttachmentRegisterBody,
) -> anyhow::Result<WriteOutcome> {
    conn.execute(
        "INSERT OR IGNORE INTO attachment_object (content_hash, r2_key) VALUES (?1, ?2)",
        libsql::params![body.content_hash.clone(), body.r2_key.clone()],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

/// Delete a dedup row during message-delete attachment cleanup. Authz: any
/// authenticated user.
///
/// TODO(#419): this should be server-side reference-counted — a shared
/// convergent row must only be removed once NO member's message references the
/// hash. Current client semantics are best-effort local ref-counting + an
/// unconditional remote delete (convergent re-upload re-creates the row), and
/// this preserves them; promote to a counted delete in a later domain pass.
pub async fn apply_delete_attachment(
    conn: &Connection,
    _authed: Option<&str>,
    body: &AttachmentDeleteBody,
) -> anyhow::Result<WriteOutcome> {
    conn.execute(
        "DELETE FROM attachment_object WHERE content_hash = ?1",
        libsql::params![body.content_hash.clone()],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}
