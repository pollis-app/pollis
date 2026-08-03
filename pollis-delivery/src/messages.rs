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
//!     (reusing [`crate::writes::is_member`]). Authorship is NOT checked here
//!     (Solution A, #607): under unconditional sealed sender the stored
//!     `sender_id` is a blinded sentinel, so the DS cannot prove who authored a
//!     message. Edit/self-delete are membership-gated only; the author check that
//!     used to live here is enforced CLIENT-side on ingest (the edit/redaction's
//!     MLS-authenticated credential must equal the target's author). Admin-delete
//!     still additionally requires the user be a group admin of the channel's
//!     owning group (a permission check, not an author check).
//!   - reactions: the user is a member, and may only write/remove their OWN
//!     reaction (`user_id` is bound to the authenticated user).
//!   - watermark: the row is per `(conversation, user, device)`; the user may
//!     only advance their own.
//!   - envelope GC: the user is a member. The deletion *decision* is many-member
//!     correct regardless of who triggers it — it is bounded by the MIN watermark
//!     over the whole current member-device roster and never by wall-clock age
//!     (invariant I3). See the TODO on [`apply_envelope_gc`] for the residual
//!     *trigger* concern, which is liveness-only.
//!
//! On the no-auth path (`authed == None`, only reachable when the DS runs with
//! `POLLIS_DS_REQUIRE_AUTH` off) the membership/identity checks are skipped and
//! the actor comes from the body — mirroring `commit::submit` and `writes.rs`.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, Uri},
    response::Response,
};
use libsql::Connection;
use serde::Deserialize;
use ulid::Ulid;

use crate::error::AppError;
use crate::writes::{
    bad_request, gate, is_member, outcome_response, resolve_actor, WriteOutcome,
};
use crate::AppState;

// ── Envelope GC SQL ──────────────────────────────────────────────────────────
//
// Envelope cleanup is gated on the DELIVERY WATERMARK and on nothing else: a row
// is deleted only when its `sent_at` sits strictly below the MINIMUM
// `last_fetched_at` over EVERY current member device of the conversation.
// Retention is therefore bounded by the slowest member device — never by
// wall-clock time. This is invariant I3 ("no TTL") in
// `docs/backend-core-invariants.md`.
//
// There used to be a second, OR'd arm: `sent_at < datetime('now', '-30 days')`.
// Because it was OR'd it deleted ON ITS OWN, without consulting a single
// watermark — so encrypted mail that no recipient device had ever collected was
// destroyed 30 days after it was sent, and a device offline for longer than that
// silently and permanently lost messages. That is failure mode F3, and it broke
// the product's headline guarantee (a member receives every message sent while
// they were a member; the only acceptable losses are pre-join history and a
// brand-new empty device). The arm is gone. Do not reintroduce a time-based — or
// any other delivery-blind — deletion gate here: age is not evidence of delivery.
//
// What remains fails CLOSED on every edge:
//   * `COUNT(ud.device_id) = COUNT(cw.last_fetched_at)` — every current member
//     device must have REPORTED a watermark. The LEFT JOIN yields NULL for a
//     device with no `conversation_watermark` row and `COUNT` skips NULLs, so a
//     never-reported device (brand-new, or long absent) breaks the equality, the
//     CASE returns NULL, and `sent_at < NULL` is NULL — nothing is deleted.
//   * `MIN(cw.last_fetched_at)` — the floor is the SLOWEST reporter's cursor, so
//     one eager device racing ahead cannot raise it.
//   * an EMPTY member-device set aggregates to `COUNT 0 = COUNT 0` with
//     `MIN(...) = NULL` over zero rows, so the CASE again returns NULL. A roster
//     that resolves to nobody is not evidence that everybody collected.
//
// The member-device roster is filtered by `ud.revoked_at IS NULL` (#685): a
// revoked device can never rejoin the MLS tree (I5), so it must not count toward
// the roster the watermark gate is measured against — otherwise it holds the
// floor down forever. Without the filter a revoked device wedges cleanup in
// either direction: its stale `last_fetched_at` pins `MIN(cw)` to a dead cursor,
// or its missing row breaks the "every device reported" check and disables
// pruning outright. This mirrors `commit::current_member_devices`, which excludes
// revoked devices from the commit-log retention floor for the same reason — keep
// the two rosters in agreement (I5).
//
// ## Why the roster lives in a macro (#722)
//
// The roster — "which member devices does the watermark gate measure against" —
// is the SAME rule in three places: these two DELETEs and
// [`crate::commit::current_member_devices`]. Nothing structural forces them to
// agree, and a divergence is an I5 violation surfacing as dropped mail (a roster
// too small deletes what a device still needs) or wedged retention (a roster too
// large never clears). So the join chain each DELETE aggregates over is factored
// out into a macro expanding to a string literal, and `concat!`-ed back into the
// statement — the SQL the DS executes is byte-for-byte what it always was, but
// the *roster half* of it now has exactly one definition that the parity test in
// `roster_parity_tests` can SELECT from directly. Editing the roster inside the
// DELETE now necessarily edits what the test reads, so any drift away from the
// Rust roster shows up as a red test rather than as silent divergence.
//
// The fragments deliberately include the `LEFT JOIN conversation_watermark`: it
// is what `COUNT(ud.device_id)` is counted over, so `SELECT ud.device_id` +
// fragment is *precisely* the row set the gate aggregates — including any
// accidental fan-out — not a paraphrase of it.

/// The channel roster: every non-revoked device of every member of the group
/// that owns channel `?1`, LEFT JOINed to that device's watermark for `?1`.
macro_rules! channel_member_device_rows {
    () => {
        "FROM group_member gm
       JOIN channels c ON c.id = ?1 AND c.group_id = gm.group_id
       JOIN user_device ud ON ud.user_id = gm.user_id AND ud.revoked_at IS NULL
       LEFT JOIN conversation_watermark cw
              ON cw.conversation_id = ?1
             AND cw.user_id = ud.user_id
             AND cw.device_id = ud.device_id"
    };
}

/// The DM roster: every non-revoked device of every member of DM channel `?1`,
/// LEFT JOINed to that device's watermark for `?1`.
macro_rules! dm_member_device_rows {
    () => {
        "FROM dm_channel_member dcm
       JOIN user_device ud ON ud.user_id = dcm.user_id AND ud.revoked_at IS NULL
       LEFT JOIN conversation_watermark cw
              ON cw.conversation_id = ?1
             AND cw.user_id = ud.user_id
             AND cw.device_id = ud.device_id
       WHERE dcm.dm_channel_id = ?1"
    };
}

const CLEANUP_CHANNEL_ENVELOPES: &str = concat!(
    "\
DELETE FROM message_envelope
 WHERE conversation_id = ?1
   AND sent_at < (
       SELECT CASE
                WHEN COUNT(ud.device_id) = COUNT(cw.last_fetched_at)
                THEN MIN(cw.last_fetched_at)
                ELSE NULL
              END
       ",
    channel_member_device_rows!(),
    "
     )"
);

const CLEANUP_DM_ENVELOPES: &str = concat!(
    "\
DELETE FROM message_envelope
 WHERE conversation_id = ?1
   AND sent_at < (
       SELECT CASE
                WHEN COUNT(ud.device_id) = COUNT(cw.last_fetched_at)
                THEN MIN(cw.last_fetched_at)
                ELSE NULL
              END
       ",
    dm_member_device_rows!(),
    "
     )"
);

// ── Shared authz helpers ─────────────────────────────────────────────────────

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

/// A DS-issued timestamp for the textual `sent_at` / `created_at` columns.
///
/// These columns are compared **lexically** — the message watermark advances on
/// `sent_at > cursor` — so a DS timestamp has to sort correctly against the
/// client-issued ones it shares a column with. Clients write
/// `chrono::Utc::now().to_rfc3339()`, which carries sub-second digits, so the DS
/// must emit them too: an earlier hand-rolled whole-second formatter produced
/// `…T09:00:00+00:00`, which sorts BELOW `…T09:00:00.123456789+00:00` because
/// `'+'` (0x2B) < `'.'` (0x2E). An admin delete tombstone written in the same
/// wall-clock second as the message it redacts therefore landed under every
/// recipient's watermark and was never fetched — the delete silently did
/// nothing. Always emitting nanoseconds restores the ordering (a shorter
/// fraction is a prefix of a longer one, so lexical order matches chronological
/// order across both precisions).
fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, false)
}

/// The `sent_at` an envelope must carry to be guaranteed visible to every
/// member of `conversation_id`, given `floor` — the greatest `sent_at` already
/// in that conversation.
///
/// Precision parity (see [`now_rfc3339`]) fixes ordering between two correct
/// clocks, but `sent_at` for ordinary messages comes from the *client's* clock
/// while a tombstone's comes from the *DS's*. A client running ahead would put
/// its messages — and so every recipient's watermark — in the DS's future, and
/// the tombstone would again sort underneath and never be fetched. Stamping the
/// tombstone strictly after everything already in the conversation removes the
/// dependence on the two clocks agreeing: the cursor cannot already be past it.
///
/// Falls back to `now` when the floor is absent or unparseable, which is the
/// pre-existing behaviour and never worse than it.
fn sent_at_after(now: String, floor: Option<String>) -> String {
    let Some(floor) = floor else {
        return now;
    };
    if floor < now {
        return now;
    }
    match chrono::DateTime::parse_from_rfc3339(&floor) {
        Ok(t) => (t + chrono::Duration::nanoseconds(1))
            .to_utc()
            .to_rfc3339_opts(chrono::SecondsFormat::Nanos, false),
        Err(_) => now,
    }
}

// ── POST /v1/messages/send ───────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SendMessageBody {
    pub id: String,
    pub conversation_id: String,
    /// Unsealed: bound to the authenticated user when signed (the no-auth
    /// fallback only). Sealed (issue #331): a non-identifying sentinel the client
    /// chose (e.g. the string `"sealed"`) — persisted as-is, NOT bound to the
    /// auth user (see `apply_send_message`).
    #[serde(default)]
    pub sender_id: Option<String>,
    /// The `"mls:<hex>"` ciphertext string the client persists — plain text, not
    /// binary, so no base64.
    pub ciphertext: String,
    #[serde(default)]
    pub reply_to_id: Option<String>,
    pub sent_at: String,
    /// Sealed sender flag (issue #331, `docs/metadata-minimization-design.md`
    /// §2). `1` → `sender_id` is a blinded sentinel; the true sender lives in the
    /// MLS credential. Absent (old clients / unsealed sends) → `0`.
    #[serde(default)]
    pub sealed: i64,
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

/// INSERT a `type='message'` envelope (the send). Authz: the authenticated user
/// is a current member of the conversation.
///
/// Sender binding depends on sealing (issue #331,
/// `docs/metadata-minimization-design.md` §2):
///   - **Unsealed** (`sealed = 0`): unchanged — a signed request may only send as
///     itself, so the stored `sender_id` is bound to the authenticated user via
///     [`resolve_actor`].
///   - **Sealed** (`sealed = 1`): the body's `sender_id` is a non-identifying
///     sentinel, deliberately NOT the authenticated user, so the
///     "send-as-yourself" equality check is relaxed and the sentinel is persisted
///     as-is. Membership authz is UNCHANGED — we still verify the *authenticated*
///     user is a member; sealing only blinds the stored sender column, it does
///     not weaken who is allowed to write.
pub async fn apply_send_message(
    conn: &Connection,
    authed: Option<&str>,
    body: &SendMessageBody,
) -> anyhow::Result<WriteOutcome> {
    let sealed = body.sealed != 0;
    // `member_check_user` is whose membership we verify; `stored_sender` is what
    // lands in the `sender_id` column.
    let (member_check_user, stored_sender) = if sealed {
        match authed {
            // Verify the authenticated writer's membership; store the blinded
            // sentinel the client sent (never bind it to the auth user).
            Some(u) => (u.to_string(), body.sender_id.clone().unwrap_or_default()),
            // No-auth fallback: keep the body's sender for both.
            None => {
                let s = body.sender_id.clone().unwrap_or_default();
                (s.clone(), s)
            }
        }
    } else {
        // Unsealed: bind the stored sender to the actor and require a signed
        // request to send only as itself.
        match resolve_actor(authed, body.sender_id.as_deref()) {
            Ok(s) => (s.clone(), s),
            Err(o) => return Ok(o),
        }
    };
    if authed.is_some() && !is_member(conn, &body.conversation_id, &member_check_user).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    conn.execute(
        "INSERT INTO message_envelope \
             (id, conversation_id, sender_id, ciphertext, reply_to_id, sent_at, sealed) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        libsql::params![
            body.id.clone(),
            body.conversation_id.clone(),
            stored_sender,
            body.ciphertext.clone(),
            body.reply_to_id.clone(),
            body.sent_at.clone(),
            body.sealed,
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
/// transaction. Authz: the editor is a current member of the conversation.
///
/// Authorship is NOT checked here (Solution A, #607): under unconditional sealed
/// sender the stored `sender_id` is always a blinded sentinel, so the DS cannot
/// verify the editor authored the target — and it deliberately does not try. The
/// edit's content only ever lands on a recipient whose ingest
/// (`pollis_core::commands::messages::ingest`) confirms the edit's
/// MLS-authenticated author (the credential inside the ciphertext) equals the
/// target message's author; a non-author's edit envelope is accepted for storage
/// here but dropped, cryptographically, on ingest. The DS's job is reduced to the
/// membership gate: only a member may write an edit envelope to the conversation.
pub async fn apply_edit_message(
    conn: &Connection,
    authed: Option<&str>,
    body: &EditMessageBody,
) -> anyhow::Result<WriteOutcome> {
    let sender = match resolve_actor(authed, body.sender_id.as_deref()) {
        Ok(s) => s,
        Err(o) => return Ok(o),
    };
    if authed.is_some() && !is_member(conn, &body.conversation_id, &sender).await? {
        return Ok(WriteOutcome::Forbidden);
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
    /// The original author the client resolved (from its local cache). Selects
    /// the self-vs-admin branch (Solution A, #607): the DS can no longer re-derive
    /// authorship from the sealed envelope, so it trusts this hint for BRANCHING
    /// only. It is never trusted for a permission grant — the admin branch
    /// re-derives the admin role independently, and the self branch grants nothing
    /// beyond envelope removal that membership doesn't already permit.
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

/// Delete a message. Two branches, chosen by the client's `msg_sender_id` hint
/// (Solution A, #607): under unconditional sealed sender the stored `sender_id`
/// is always a blinded sentinel, so the DS can no longer derive who authored the
/// message — it trusts the client to say whether this is a self-delete.
///
/// **Self-branch** (`msg_sender_id == actor`): gated on **membership** only.
/// Removes the original envelope (unscoped — a sealed row has no matchable
/// sender) and any pending edit; writes **no** tombstone. A non-author member can
/// thus remove a not-yet-fetched envelope (an accepted availability trade, #607),
/// but cannot forge a *delete appearance*: making other members drop an
/// already-fetched copy requires either a valid E2EE redaction (honored on ingest
/// only when its MLS-authenticated author matches the target's author) or an
/// admin tombstone (below) — neither of which a non-author can produce.
///
/// **Admin-branch** (`msg_sender_id != actor`): the actor must be a group admin
/// of the channel (a re-derived permission check, not an author check). Removes
/// the envelope + pending edit and writes a `type='delete'` tombstone so every
/// member soft-deletes on next ingest. Server-authorized moderation.
pub async fn apply_delete_message(
    conn: &Connection,
    authed: Option<&str>,
    body: &DeleteMessageBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.actor_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };

    // Branch selection comes from the client's hint — the DS cannot re-derive
    // authorship from the (sealed) envelope. Self-branch when the hint names the
    // actor as the author; admin-branch otherwise.
    let is_self_delete = body.msg_sender_id.as_deref() == Some(actor.as_str());

    if is_self_delete {
        // Self-branch authz is membership: any member may remove an envelope
        // they claim to have authored (authorship itself is enforced
        // client-side on ingest, never here). Skipped on the no-auth path.
        if authed.is_some() && !is_member(conn, &body.conversation_id, &actor).await? {
            return Ok(WriteOutcome::Forbidden);
        }
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
    // Read the conversation's high-water `sent_at` BEFORE the deletes below
    // remove the target — a recipient's watermark can be anywhere up to this
    // value, and the tombstone is only ever fetched if it sorts above it.
    let floor: Option<String> = {
        let mut rows = conn
            .query(
                "SELECT MAX(sent_at) FROM message_envelope WHERE conversation_id = ?1",
                libsql::params![body.conversation_id.clone()],
            )
            .await?;
        match rows.next().await? {
            Some(row) => row.get::<Option<String>>(0)?,
            None => None,
        }
    };
    let now = sent_at_after(now_rfc3339(), floor);
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

/// Run the watermark-gated envelope GC for a conversation. Authz: the actor is
/// a current member.
///
/// The deletion predicate is bounded by the SLOWEST current member device and by
/// nothing else — see the [`CLEANUP_CHANNEL_ENVELOPES`] block comment. The
/// 30-day TTL that used to be OR'd into it is gone (invariant I3, failure mode
/// F3): an envelope no member device has collected is retained however old it is.
///
/// Because the predicate consults the WHOLE current member-device roster, the
/// *decision* is the same no matter who fires it — so a single eager device can
/// never delete another device's mail, whatever the trigger.
///
/// TODO(#419): the *trigger* is still "whichever member happens to ingest calls
/// `/v1/envelopes/gc`", which is a liveness concern only: a conversation whose
/// members all stop ingesting simply retains envelopes longer than necessary. It
/// can no longer cause loss, so it is tracked separately from this fix. When it
/// is revisited, the predicate must stay watermark-bounded.
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

#[cfg(test)]
mod timestamp_tests {
    use super::*;

    /// The exact shape a client writes for `sent_at` (`pollis-core` sends
    /// `chrono::Utc::now().to_rfc3339()`, i.e. `SecondsFormat::AutoSi`).
    fn client_stamp(nanos: u32) -> String {
        chrono::DateTime::from_timestamp(1_800_000_000, nanos)
            .expect("valid instant")
            .to_rfc3339()
    }

    /// The regression: a DS timestamp must sort ABOVE a client timestamp taken
    /// earlier in the same wall-clock second. The old whole-second formatter
    /// emitted `…:20+00:00`, and `'+'` (0x2B) < `'.'` (0x2E), so it sorted
    /// BELOW `…:20.000000001+00:00` — an admin tombstone written in the same
    /// second as the message it redacts fell under every recipient's watermark
    /// and was never fetched, so the delete silently did nothing.
    #[test]
    fn a_ds_stamp_sorts_above_a_client_stamp_from_the_same_second() {
        let client = client_stamp(1);
        let ds = chrono::DateTime::from_timestamp(1_800_000_000, 500_000_000)
            .expect("valid instant")
            .to_rfc3339_opts(chrono::SecondsFormat::Nanos, false);
        assert!(
            ds > client,
            "DS stamp {ds} must sort above same-second client stamp {client}"
        );

        // What the old formatter produced, pinned as the thing we must not regress to.
        let whole_second = "2027-01-15T08:00:00+00:00";
        assert!(
            whole_second < client.as_str(),
            "a whole-second stamp sorts below a sub-second one — this is the bug"
        );
    }

    /// Lexical order must match chronological order across every fraction width
    /// chrono's `AutoSi` can emit (0, 3, 6, 9 digits), because both formats live
    /// in the same column and the watermark only ever compares them as text.
    #[test]
    fn lexical_order_matches_chronological_order_across_precisions() {
        let stamps = [
            client_stamp(0),
            client_stamp(1_000_000),
            client_stamp(1_001_000),
            client_stamp(1_001_001),
            client_stamp(999_999_999),
        ];
        for pair in stamps.windows(2) {
            assert!(
                pair[0] < pair[1],
                "{} must sort below {}",
                pair[0],
                pair[1]
            );
        }
    }

    /// A tombstone must outrank everything already in the conversation even when
    /// the client's clock runs ahead of the DS's — otherwise the recipient's
    /// watermark is already past it and it is never fetched.
    #[test]
    fn a_tombstone_outranks_a_conversation_stamped_in_the_future() {
        let now = now_rfc3339();
        let ahead = chrono::Utc::now() + chrono::Duration::hours(1);
        let floor = ahead.to_rfc3339();

        let stamped = sent_at_after(now.clone(), Some(floor.clone()));
        assert!(
            stamped > floor,
            "tombstone {stamped} must sort above the conversation's high-water mark {floor}"
        );
    }

    /// When nothing in the conversation is ahead of the DS clock, the tombstone
    /// keeps the plain current time — the guard only ever pushes forward.
    #[test]
    fn the_floor_guard_is_a_no_op_when_the_conversation_is_behind() {
        let now = now_rfc3339();
        let behind = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        assert_eq!(sent_at_after(now.clone(), Some(behind)), now);
        assert_eq!(sent_at_after(now.clone(), None), now);
        assert_eq!(sent_at_after(now.clone(), Some("not-a-timestamp".into())), now);
    }
}

#[cfg(test)]
mod gc_sql_tests {
    //! The envelope-GC cleanup SQL, driven directly against a real (in-memory)
    //! libsql DB. Two invariants are encoded here.
    //!
    //! **I3 — retention is bounded by the slowest member device, never a TTL.**
    //! The `no_ttl_*` tests below construct exactly the state the deleted 30-day
    //! TTL destroyed (failure mode F3): an envelope FAR older than 30 days that a
    //! current member device has not collected. It must survive. Ages are written
    //! as explicit `datetime('now', '-N days')` offsets, so "well over 30 days" is
    //! a property of the fixture rather than of when the suite happens to run.
    //!
    //! **#685 — a REVOKED device must not wedge deletion.** A revoked device can
    //! never rejoin the tree, so it must not count toward the roster the watermark
    //! gate is measured against. Both wedge modes are covered for each
    //! conversation shape:
    //!   * **stale watermark row present** — the revoked device's old
    //!     `last_fetched_at` pins `MIN(cw)` down, so an envelope above the LIVE
    //!     device's cursor is (wrongly) kept.
    //!   * **no watermark row at all** — the revoked device has no `cw` row, so
    //!     `COUNT(ud) != COUNT(cw)` and the CASE returns NULL, disabling the
    //!     watermark gate entirely.
    //!
    //! All timestamps use SQLite's `datetime()` format on BOTH `sent_at` and
    //! `last_fetched_at` so the lexical comparison the gate performs is
    //! unambiguous.

    use super::*;

    const SCHEMA: &str = "\
CREATE TABLE message_envelope (id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL, sent_at TEXT NOT NULL);\
CREATE TABLE user_device (user_id TEXT NOT NULL, device_id TEXT NOT NULL, revoked_at TEXT);\
CREATE TABLE group_member (group_id TEXT NOT NULL, user_id TEXT NOT NULL);\
CREATE TABLE channels (id TEXT PRIMARY KEY, group_id TEXT NOT NULL);\
CREATE TABLE dm_channel_member (dm_channel_id TEXT NOT NULL, user_id TEXT NOT NULL);\
CREATE TABLE conversation_watermark (\
  conversation_id TEXT NOT NULL, user_id TEXT NOT NULL, device_id TEXT NOT NULL, last_fetched_at TEXT NOT NULL);";

    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch(SCHEMA).await.unwrap();
        conn
    }

    async fn add_device(conn: &Connection, user: &str, device: &str, revoked: bool) {
        conn.execute(
            "INSERT INTO user_device (user_id, device_id, revoked_at) \
             VALUES (?1, ?2, CASE WHEN ?3 THEN datetime('now') ELSE NULL END)",
            libsql::params![user.to_string(), device.to_string(), revoked as i64],
        )
        .await
        .unwrap();
    }

    /// Seed a watermark row whose `last_fetched_at` is `datetime('now', offset)`.
    async fn seed_watermark(conn: &Connection, conv: &str, user: &str, device: &str, offset: &str) {
        conn.execute(
            "INSERT INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at) \
             VALUES (?1, ?2, ?3, datetime('now', ?4))",
            libsql::params![conv.to_string(), user.to_string(), device.to_string(), offset.to_string()],
        )
        .await
        .unwrap();
    }

    /// Insert one envelope at `datetime('now', offset)`.
    async fn add_envelope(conn: &Connection, id: &str, conv: &str, offset: &str) {
        conn.execute(
            "INSERT INTO message_envelope (id, conversation_id, sent_at) \
             VALUES (?1, ?2, datetime('now', ?3))",
            libsql::params![id.to_string(), conv.to_string(), offset.to_string()],
        )
        .await
        .unwrap();
    }

    async fn envelope_count(conn: &Connection, conv: &str) -> i64 {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM message_envelope WHERE conversation_id = ?1",
                libsql::params![conv.to_string()],
            )
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    // ── Channel ──────────────────────────────────────────────────────────────

    async fn channel_fixture(conn: &Connection) {
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice')", ())
            .await
            .unwrap();
        conn.execute("INSERT INTO channels (id, group_id) VALUES ('c1', 'g1')", ())
            .await
            .unwrap();
    }

    /// A revoked device with a STALE watermark row must not keep an envelope the
    /// live device has already read past.
    #[tokio::test]
    async fn channel_stale_revoked_watermark_does_not_pin_gc() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        add_device(&conn, "alice", "a-live", false).await;
        add_device(&conn, "alice", "a-old", true).await;
        // Live cursor is recent; the revoked device is stuck 10 days back.
        seed_watermark(&conn, "c1", "alice", "a-live", "-1 day").await;
        seed_watermark(&conn, "c1", "alice", "a-old", "-10 days").await;
        // The envelope sits between them: above the stale cursor, below the live one.
        add_envelope(&conn, "e1", "c1", "-5 days").await;

        conn.execute(CLEANUP_CHANNEL_ENVELOPES, libsql::params!["c1".to_string()])
            .await
            .unwrap();

        assert_eq!(
            envelope_count(&conn, "c1").await,
            0,
            "envelope below the LIVE device's cursor must be pruned; the revoked \
             device's stale watermark must not pin MIN(cw) (#685)"
        );
    }

    /// A revoked device with NO watermark row must not disable pruning via the
    /// `COUNT(ud) != COUNT(cw)` "every device reported" check.
    #[tokio::test]
    async fn channel_missing_revoked_watermark_does_not_disable_gc() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        add_device(&conn, "alice", "a-live", false).await;
        add_device(&conn, "alice", "a-old", true).await;
        // Only the live device has a watermark; the revoked device has none.
        seed_watermark(&conn, "c1", "alice", "a-live", "-1 day").await;
        add_envelope(&conn, "e1", "c1", "-5 days").await;

        conn.execute(CLEANUP_CHANNEL_ENVELOPES, libsql::params!["c1".to_string()])
            .await
            .unwrap();

        assert_eq!(
            envelope_count(&conn, "c1").await,
            0,
            "with the revoked device excluded, COUNT(ud) == COUNT(cw) and the \
             watermark gate prunes; its absent row must not disable GC (#685)"
        );
    }

    // ── DM ───────────────────────────────────────────────────────────────────

    async fn dm_fixture(conn: &Connection) {
        conn.execute("INSERT INTO dm_channel_member (dm_channel_id, user_id) VALUES ('d1', 'alice')", ())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn dm_stale_revoked_watermark_does_not_pin_gc() {
        let conn = conn().await;
        dm_fixture(&conn).await;
        add_device(&conn, "alice", "a-live", false).await;
        add_device(&conn, "alice", "a-old", true).await;
        seed_watermark(&conn, "d1", "alice", "a-live", "-1 day").await;
        seed_watermark(&conn, "d1", "alice", "a-old", "-10 days").await;
        add_envelope(&conn, "e1", "d1", "-5 days").await;

        conn.execute(CLEANUP_DM_ENVELOPES, libsql::params!["d1".to_string()])
            .await
            .unwrap();

        assert_eq!(
            envelope_count(&conn, "d1").await,
            0,
            "DM: the revoked device's stale watermark must not pin MIN(cw) (#685)"
        );
    }

    #[tokio::test]
    async fn dm_missing_revoked_watermark_does_not_disable_gc() {
        let conn = conn().await;
        dm_fixture(&conn).await;
        add_device(&conn, "alice", "a-live", false).await;
        add_device(&conn, "alice", "a-old", true).await;
        seed_watermark(&conn, "d1", "alice", "a-live", "-1 day").await;
        add_envelope(&conn, "e1", "d1", "-5 days").await;

        conn.execute(CLEANUP_DM_ENVELOPES, libsql::params!["d1".to_string()])
            .await
            .unwrap();

        assert_eq!(
            envelope_count(&conn, "d1").await,
            0,
            "DM: the revoked device's absent watermark must not disable GC (#685)"
        );
    }

    // ── I3 — no TTL: age alone never deletes ─────────────────────────────────

    /// **The regression test for F3.** A LIVE member device whose cursor sits
    /// below an envelope must keep that envelope alive no matter how old it is.
    /// The fixture is deliberately extreme — the envelope is 400 days old, more
    /// than 13× the deleted 30-day TTL — so the assertion cannot pass by accident
    /// of when the suite runs. Under the old `sent_at < datetime('now','-30 days')
    /// OR ...` predicate the TTL arm alone deleted this row and the recipient
    /// permanently lost the message.
    #[tokio::test]
    async fn no_ttl_channel_uncollected_ancient_envelope_survives() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob')", ())
            .await
            .unwrap();
        add_device(&conn, "alice", "a1", false).await;
        add_device(&conn, "bob", "b1", false).await;
        // Alice is fully caught up. Bob's device has been offline for 500 days —
        // its cursor is BELOW the envelope, so the envelope is still owed to it.
        seed_watermark(&conn, "c1", "alice", "a1", "-1 day").await;
        seed_watermark(&conn, "c1", "bob", "b1", "-500 days").await;
        add_envelope(&conn, "ancient", "c1", "-400 days").await;

        conn.execute(CLEANUP_CHANNEL_ENVELOPES, libsql::params!["c1".to_string()])
            .await
            .unwrap();

        assert_eq!(
            envelope_count(&conn, "c1").await,
            1,
            "a 400-day-old envelope BELOW a live member device's cursor must \
             survive — retention is bounded by the slowest member device, never by \
             wall-clock age (I3/F3). A TTL arm would have deleted it."
        );
    }

    /// The same property with the other never-collected shape: the absent member
    /// device has never reported a watermark AT ALL. `COUNT(ud) != COUNT(cw)` →
    /// CASE NULL → nothing deleted, however old the envelope is.
    #[tokio::test]
    async fn no_ttl_channel_never_reported_device_holds_ancient_envelope() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob')", ())
            .await
            .unwrap();
        add_device(&conn, "alice", "a1", false).await;
        add_device(&conn, "bob", "b-silent", false).await;
        // Only alice has ever reported; bob's device has no watermark row.
        seed_watermark(&conn, "c1", "alice", "a1", "-1 day").await;
        add_envelope(&conn, "ancient", "c1", "-400 days").await;

        conn.execute(CLEANUP_CHANNEL_ENVELOPES, libsql::params!["c1".to_string()])
            .await
            .unwrap();

        assert_eq!(
            envelope_count(&conn, "c1").await,
            1,
            "a member device that has never reported must hold even a 400-day-old \
             envelope: no watermark is no evidence of delivery (I3/F3)"
        );
    }

    /// DM shape, same regression.
    #[tokio::test]
    async fn no_ttl_dm_uncollected_ancient_envelope_survives() {
        let conn = conn().await;
        dm_fixture(&conn).await;
        conn.execute("INSERT INTO dm_channel_member (dm_channel_id, user_id) VALUES ('d1', 'bob')", ())
            .await
            .unwrap();
        add_device(&conn, "alice", "a1", false).await;
        add_device(&conn, "bob", "b1", false).await;
        seed_watermark(&conn, "d1", "alice", "a1", "-1 day").await;
        seed_watermark(&conn, "d1", "bob", "b1", "-500 days").await;
        add_envelope(&conn, "ancient", "d1", "-400 days").await;

        conn.execute(CLEANUP_DM_ENVELOPES, libsql::params!["d1".to_string()])
            .await
            .unwrap();

        assert_eq!(
            envelope_count(&conn, "d1").await,
            1,
            "DM: a 400-day-old envelope below the slowest member device's cursor \
             must survive (I3/F3)"
        );
    }

    // ── The positive leg: deletion still happens ─────────────────────────────

    /// GC is not simply disabled: once EVERY current member device has collected
    /// past an envelope, it goes — including one far younger than the old TTL, so
    /// this cannot be passing via the deleted arm.
    #[tokio::test]
    async fn channel_envelope_is_deleted_once_every_device_collected_past_it() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob')", ())
            .await
            .unwrap();
        add_device(&conn, "alice", "a1", false).await;
        add_device(&conn, "alice", "a2", false).await;
        add_device(&conn, "bob", "b1", false).await;
        // Every device's cursor is strictly above `collected`, and strictly below
        // `pending` — so exactly one of the two envelopes may go.
        add_envelope(&conn, "collected", "c1", "-3 days").await;
        add_envelope(&conn, "pending", "c1", "-1 hour").await;
        seed_watermark(&conn, "c1", "alice", "a1", "-2 days").await;
        seed_watermark(&conn, "c1", "alice", "a2", "-2 days").await;
        seed_watermark(&conn, "c1", "bob", "b1", "-2 days").await;

        conn.execute(CLEANUP_CHANNEL_ENVELOPES, libsql::params!["c1".to_string()])
            .await
            .unwrap();

        assert_eq!(
            envelope_count(&conn, "c1").await,
            1,
            "the envelope every member device collected must be pruned, and the \
             one still above the floor must remain — GC is bounded, not disabled"
        );
        let mut rows = conn
            .query("SELECT id FROM message_envelope WHERE conversation_id = 'c1'", ())
            .await
            .unwrap();
        let survivor = rows.next().await.unwrap().unwrap().get::<String>(0).unwrap();
        assert_eq!(survivor, "pending", "the wrong envelope was pruned");
    }

    /// The slowest device sets the floor: one device racing ahead must not raise
    /// it and evict mail a sibling device still needs.
    #[tokio::test]
    async fn channel_floor_is_the_minimum_not_the_maximum() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        add_device(&conn, "alice", "a-fast", false).await;
        add_device(&conn, "alice", "a-slow", false).await;
        seed_watermark(&conn, "c1", "alice", "a-fast", "-1 hour").await;
        seed_watermark(&conn, "c1", "alice", "a-slow", "-6 days").await;
        // Above the slow cursor, below the fast one: MIN keeps it, MAX would not.
        add_envelope(&conn, "between", "c1", "-3 days").await;

        conn.execute(CLEANUP_CHANNEL_ENVELOPES, libsql::params!["c1".to_string()])
            .await
            .unwrap();

        assert_eq!(
            envelope_count(&conn, "c1").await,
            1,
            "the floor must be MIN over member devices — the fast device's cursor \
             must not evict what the slow one has yet to collect"
        );
    }

    // ── Conservative edges ───────────────────────────────────────────────────

    /// A conversation with ZERO current member devices deletes NOTHING. The
    /// aggregate degenerates to `COUNT 0 = COUNT 0` with `MIN(...) = NULL` over
    /// zero rows, so the CASE returns NULL and `sent_at < NULL` is NULL. Pinned as
    /// a test because the alternative reading of an empty roster — "everybody has
    /// collected, delete it all" — is exactly the unbounded delete this ticket
    /// removes.
    #[tokio::test]
    async fn an_empty_member_device_set_deletes_nothing() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        // A member row exists but the member has no device at all.
        add_envelope(&conn, "ancient", "c1", "-400 days").await;
        add_envelope(&conn, "fresh", "c1", "-1 hour").await;

        conn.execute(CLEANUP_CHANNEL_ENVELOPES, libsql::params!["c1".to_string()])
            .await
            .unwrap();

        assert_eq!(
            envelope_count(&conn, "c1").await,
            2,
            "an empty member-device set must delete NOTHING, not everything"
        );
    }

    /// The same, one step further: the conversation id resolves to no membership
    /// row whatsoever (unknown/deleted conversation).
    #[tokio::test]
    async fn an_unknown_conversation_deletes_nothing() {
        let conn = conn().await;
        add_envelope(&conn, "ancient", "ghost", "-400 days").await;

        conn.execute(CLEANUP_CHANNEL_ENVELOPES, libsql::params!["ghost".to_string()])
            .await
            .unwrap();
        conn.execute(CLEANUP_DM_ENVELOPES, libsql::params!["ghost".to_string()])
            .await
            .unwrap();

        assert_eq!(
            envelope_count(&conn, "ghost").await,
            1,
            "a conversation with no resolvable membership must delete nothing"
        );
    }

    /// The revoked-device exclusion must not be able to *manufacture* an empty
    /// roster that deletes: when EVERY device is revoked the roster is empty, and
    /// an empty roster deletes nothing.
    #[tokio::test]
    async fn an_all_revoked_roster_deletes_nothing() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        add_device(&conn, "alice", "a-old", true).await;
        seed_watermark(&conn, "c1", "alice", "a-old", "-500 days").await;
        add_envelope(&conn, "ancient", "c1", "-400 days").await;

        conn.execute(CLEANUP_CHANNEL_ENVELOPES, libsql::params!["c1".to_string()])
            .await
            .unwrap();

        assert_eq!(
            envelope_count(&conn, "c1").await,
            1,
            "excluding revoked devices must never leave a roster that deletes by \
             virtue of being empty"
        );
    }
}

#[cfg(test)]
mod roster_parity_tests {
    //! **I5 — the three rosters must agree (#722).**
    //!
    //! "Which member devices count toward retention" is one rule with three
    //! implementations: [`CLEANUP_CHANNEL_ENVELOPES`], [`CLEANUP_DM_ENVELOPES`]
    //! (envelope GC, SQL) and [`crate::commit::current_member_devices`]
    //! (commit-log retention floor, Rust). #685/#686 brought them into agreement
    //! on revoked devices; nothing structural keeps them there. A divergence is
    //! not cosmetic:
    //!
    //!   * SQL roster **narrower** than the Rust one → envelope GC deletes mail a
    //!     device the commit log still serves has not collected (loss, F3-shaped).
    //!   * SQL roster **wider** → a device that cannot rejoin the tree holds the
    //!     watermark floor down forever and envelopes never clear (stuck
    //!     retention).
    //!
    //! ## How this test avoids being a restatement of the SQL
    //!
    //! The cleanup SQL embeds its roster inside a DELETE, so it cannot be read
    //! back directly. Rather than hand-copy an "equivalent" SELECT here — which
    //! would only ever prove *this file* agrees with *this file*, and would sit
    //! there passing while the DELETE drifted — the join chain is factored out of
    //! the DELETE into `channel_member_device_rows!` / `dm_member_device_rows!`
    //! and `concat!`-ed back in. The statements the DS executes are unchanged
    //! byte-for-byte (`the_extraction_did_not_change_the_cleanup_sql` pins that),
    //! and the SELECTs below are built from the SAME macro the DELETE is. There
    //! is exactly one copy of the roster SQL in the crate.
    //!
    //! So the drift this catches is the drift that matters: change the roster on
    //! either side — the JOIN inside the DELETE, or the query in
    //! `current_member_devices` — and the two sides disagree here. The expected
    //! rosters are additionally spelled out literally, so a change applied
    //! symmetrically to *both* implementations still fails rather than silently
    //! redefining the invariant.
    //!
    //! [`the_roster_is_what_the_delete_actually_gates_on`] closes the remaining
    //! gap between "the fragment" and "the statement": it proves the deletion
    //! outcome flips exactly on the watermark of a device the fragment lists, and
    //! does not move for devices it excludes.

    use super::*;
    use crate::commit::current_member_devices;

    /// The row set `COUNT(ud.device_id)` is counted over in the channel DELETE —
    /// same text, projected instead of aggregated.
    const CHANNEL_ROSTER_SELECT: &str = concat!(
        "SELECT ud.device_id
       ",
        channel_member_device_rows!()
    );

    /// The DM equivalent.
    const DM_ROSTER_SELECT: &str = concat!(
        "SELECT ud.device_id
       ",
        dm_member_device_rows!()
    );

    /// Production shapes, keys included — the PKs matter here. `user_device`'s
    /// PK is what makes a device id globally unique (so the Rust side's
    /// `DISTINCT` and the SQL side's raw rows are comparable), and
    /// `conversation_watermark`'s is what stops the LEFT JOIN fanning a roster
    /// row out into several.
    const SCHEMA: &str = "\
CREATE TABLE message_envelope (id TEXT PRIMARY KEY, conversation_id TEXT NOT NULL, sent_at TEXT NOT NULL);\
CREATE TABLE user_device (device_id TEXT PRIMARY KEY, user_id TEXT NOT NULL, revoked_at TEXT);\
CREATE TABLE group_member (group_id TEXT NOT NULL, user_id TEXT NOT NULL, PRIMARY KEY (group_id, user_id));\
CREATE TABLE channels (id TEXT PRIMARY KEY, group_id TEXT NOT NULL);\
CREATE TABLE dm_channel_member (\
  dm_channel_id TEXT NOT NULL, user_id TEXT NOT NULL, PRIMARY KEY (dm_channel_id, user_id));\
CREATE TABLE conversation_watermark (\
  conversation_id TEXT NOT NULL, user_id TEXT NOT NULL, device_id TEXT NOT NULL, \
  last_fetched_at TEXT NOT NULL, PRIMARY KEY (conversation_id, user_id, device_id));";

    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch(SCHEMA).await.unwrap();
        conn
    }

    async fn exec(conn: &Connection, sql: &str) {
        conn.execute(sql, ()).await.unwrap();
    }

    async fn device(conn: &Connection, user: &str, device: &str, revoked: bool) {
        conn.execute(
            "INSERT INTO user_device (device_id, user_id, revoked_at) \
             VALUES (?1, ?2, CASE WHEN ?3 THEN datetime('now') ELSE NULL END)",
            libsql::params![device.to_string(), user.to_string(), revoked as i64],
        )
        .await
        .unwrap();
    }

    async fn watermark(conn: &Connection, conv: &str, user: &str, device: &str, offset: &str) {
        conn.execute(
            "INSERT INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at) \
             VALUES (?1, ?2, ?3, datetime('now', ?4))",
            libsql::params![
                conv.to_string(),
                user.to_string(),
                device.to_string(),
                offset.to_string()
            ],
        )
        .await
        .unwrap();
    }

    async fn envelope(conn: &Connection, id: &str, conv: &str, offset: &str) {
        conn.execute(
            "INSERT INTO message_envelope (id, conversation_id, sent_at) \
             VALUES (?1, ?2, datetime('now', ?3))",
            libsql::params![id.to_string(), conv.to_string(), offset.to_string()],
        )
        .await
        .unwrap();
    }

    async fn envelope_count(conn: &Connection, conv: &str) -> i64 {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM message_envelope WHERE conversation_id = ?1",
                libsql::params![conv.to_string()],
            )
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    /// The roster the cleanup SQL measures against, as a sorted device list.
    /// Duplicates are deliberately NOT collapsed: a repeated row would inflate
    /// `COUNT(ud.device_id)` and break the "every device reported" gate, so it
    /// must show up as a mismatch rather than be normalised away.
    async fn sql_roster(conn: &Connection, select: &str, conv: &str) -> Vec<String> {
        let mut rows = conn
            .query(select, libsql::params![conv.to_string()])
            .await
            .unwrap();
        let mut out = Vec::new();
        while let Some(r) = rows.next().await.unwrap() {
            out.push(r.get::<String>(0).unwrap());
        }
        out.sort();
        out
    }

    async fn rust_roster(conn: &Connection, conv: &str) -> Vec<String> {
        let mut out = current_member_devices(conn, conv).await.unwrap();
        out.sort();
        out
    }

    /// Every drift-prone shape in one fixture, seeded identically for a channel
    /// (`c-main`, owned by group `g-main`) and a DM (`dm-main`):
    ///
    ///   * `alice` — a plain member with a single active device.
    ///   * `bob` — several devices, one of them revoked, and one active device
    ///     that has never reported a watermark.
    ///   * `carol` — a member whose devices are ALL revoked.
    ///   * `dave` — a member whose one active device has no watermark row.
    ///   * `mallory` — not a member of anything, with an active device (and a
    ///     stray watermark row for both conversations, so a roster that reached
    ///     through `conversation_watermark` instead of through membership would
    ///     be caught).
    ///   * `eve` — a member of a DIFFERENT group/DM, to pin conversation scoping.
    ///
    /// Watermarks are seeded only where a test needs them; membership and
    /// devices are the shared part.
    async fn fixture(conn: &Connection) {
        exec(conn, "INSERT INTO channels (id, group_id) VALUES ('c-main', 'g-main')").await;
        exec(conn, "INSERT INTO channels (id, group_id) VALUES ('c-other', 'g-other')").await;
        for user in ["alice", "bob", "carol", "dave"] {
            conn.execute(
                "INSERT INTO group_member (group_id, user_id) VALUES ('g-main', ?1)",
                libsql::params![user.to_string()],
            )
            .await
            .unwrap();
            conn.execute(
                "INSERT INTO dm_channel_member (dm_channel_id, user_id) VALUES ('dm-main', ?1)",
                libsql::params![user.to_string()],
            )
            .await
            .unwrap();
        }
        exec(conn, "INSERT INTO group_member (group_id, user_id) VALUES ('g-other', 'eve')").await;
        exec(
            conn,
            "INSERT INTO dm_channel_member (dm_channel_id, user_id) VALUES ('dm-other', 'eve')",
        )
        .await;

        device(conn, "alice", "alice-1", false).await;
        device(conn, "bob", "bob-live", false).await;
        device(conn, "bob", "bob-revoked", true).await;
        device(conn, "bob", "bob-silent", false).await;
        device(conn, "carol", "carol-old-1", true).await;
        device(conn, "carol", "carol-old-2", true).await;
        device(conn, "dave", "dave-1", false).await;
        device(conn, "eve", "eve-1", false).await;
        device(conn, "mallory", "mallory-1", false).await;
    }

    /// The devices the fixture's rosters must resolve to, for BOTH conversation
    /// shapes. Spelled out rather than derived, so a change made symmetrically to
    /// the SQL and the Rust roster still fails here instead of quietly
    /// redefining the invariant.
    const EXPECTED: [&str; 4] = ["alice-1", "bob-live", "bob-silent", "dave-1"];

    fn expected() -> Vec<String> {
        let mut v: Vec<String> = EXPECTED.iter().map(|s| s.to_string()).collect();
        v.sort();
        v
    }

    /// Seed the watermark spread the roster must be insensitive to: some roster
    /// devices reported, some never did, revoked devices carry ancient cursors,
    /// and a non-member has a row for the conversation.
    async fn seed_mixed_watermarks(conn: &Connection, conv: &str) {
        watermark(conn, conv, "alice", "alice-1", "-1 day").await;
        watermark(conn, conv, "bob", "bob-live", "-2 days").await;
        // `bob-silent` and `dave-1` deliberately have NO row.
        watermark(conn, conv, "bob", "bob-revoked", "-400 days").await;
        watermark(conn, conv, "carol", "carol-old-1", "-400 days").await;
        watermark(conn, conv, "carol", "carol-old-2", "-400 days").await;
        watermark(conn, conv, "mallory", "mallory-1", "-400 days").await;
    }

    // ── The parity assertions ────────────────────────────────────────────────

    #[tokio::test]
    async fn channel_sql_and_rust_rosters_agree() {
        let conn = conn().await;
        fixture(&conn).await;
        seed_mixed_watermarks(&conn, "c-main").await;

        let sql = sql_roster(&conn, CHANNEL_ROSTER_SELECT, "c-main").await;
        let rust = rust_roster(&conn, "c-main").await;

        assert_eq!(
            sql, rust,
            "CLEANUP_CHANNEL_ENVELOPES and commit::current_member_devices must \
             resolve the SAME member-device roster for a channel (I5, #722). \
             SQL: {sql:?} / Rust: {rust:?}"
        );
        assert_eq!(
            sql,
            expected(),
            "the agreed roster is not the intended one: active devices of current \
             members only — revoked devices excluded, a member with no live device \
             contributing nothing, and non-members absent"
        );
    }

    #[tokio::test]
    async fn dm_sql_and_rust_rosters_agree() {
        let conn = conn().await;
        fixture(&conn).await;
        seed_mixed_watermarks(&conn, "dm-main").await;

        let sql = sql_roster(&conn, DM_ROSTER_SELECT, "dm-main").await;
        let rust = rust_roster(&conn, "dm-main").await;

        assert_eq!(
            sql, rust,
            "CLEANUP_DM_ENVELOPES and commit::current_member_devices must resolve \
             the SAME member-device roster for a DM (I5, #722). \
             SQL: {sql:?} / Rust: {rust:?}"
        );
        assert_eq!(sql, expected(), "the agreed DM roster is not the intended one");
    }

    /// Scoping: the roster of a sibling conversation is its own, on both paths.
    /// A roster that leaked across conversations would delete one channel's mail
    /// on another channel's watermarks.
    #[tokio::test]
    async fn a_sibling_conversation_resolves_its_own_roster_on_both_paths() {
        let conn = conn().await;
        fixture(&conn).await;

        for (select, conv) in [
            (CHANNEL_ROSTER_SELECT, "c-other"),
            (DM_ROSTER_SELECT, "dm-other"),
        ] {
            let sql = sql_roster(&conn, select, conv).await;
            let rust = rust_roster(&conn, conv).await;
            assert_eq!(sql, rust, "{conv}: rosters diverge");
            assert_eq!(
                sql,
                vec!["eve-1".to_string()],
                "{conv} must see only its own member's device"
            );
        }
    }

    /// A member whose devices are ALL revoked, alone in the conversation: both
    /// paths must return the EMPTY roster. This is the case where disagreeing is
    /// most expensive — an empty roster disables envelope GC (conservative), but
    /// on the commit-log side it removes Tier-1's lower bound, so the two sides
    /// answering differently means one of them is acting on a device the other
    /// considers gone.
    #[tokio::test]
    async fn an_all_revoked_member_yields_the_empty_roster_on_both_paths() {
        let conn = conn().await;
        exec(&conn, "INSERT INTO channels (id, group_id) VALUES ('c-dead', 'g-dead')").await;
        exec(&conn, "INSERT INTO group_member (group_id, user_id) VALUES ('g-dead', 'carol')").await;
        exec(
            &conn,
            "INSERT INTO dm_channel_member (dm_channel_id, user_id) VALUES ('dm-dead', 'carol')",
        )
        .await;
        device(&conn, "carol", "carol-old-1", true).await;
        device(&conn, "carol", "carol-old-2", true).await;
        watermark(&conn, "c-dead", "carol", "carol-old-1", "-400 days").await;

        for (select, conv) in [
            (CHANNEL_ROSTER_SELECT, "c-dead"),
            (DM_ROSTER_SELECT, "dm-dead"),
        ] {
            let sql = sql_roster(&conn, select, conv).await;
            let rust = rust_roster(&conn, conv).await;
            assert_eq!(sql, rust, "{conv}: rosters diverge on an all-revoked member");
            assert!(
                sql.is_empty(),
                "{conv}: an all-revoked member contributes no devices, got {sql:?}"
            );
        }
    }

    /// An id that names no conversation: both paths must return nothing rather
    /// than, say, every device in the table.
    #[tokio::test]
    async fn an_unknown_conversation_yields_the_empty_roster_on_both_paths() {
        let conn = conn().await;
        fixture(&conn).await;

        for select in [CHANNEL_ROSTER_SELECT, DM_ROSTER_SELECT] {
            let sql = sql_roster(&conn, select, "ghost").await;
            assert!(sql.is_empty(), "unknown conversation resolved {sql:?}");
        }
        assert!(rust_roster(&conn, "ghost").await.is_empty());
    }

    // ── The fragment really is the statement ─────────────────────────────────

    /// Factoring the roster out of the DELETE must not have changed the SQL the
    /// DS executes. Pinned against the literal statements as they stood before
    /// the extraction, so the "safe, non-behavioural refactor" claim is checked
    /// rather than asserted.
    #[test]
    fn the_extraction_did_not_change_the_cleanup_sql() {
        assert_eq!(
            CLEANUP_CHANNEL_ENVELOPES,
            "\
DELETE FROM message_envelope
 WHERE conversation_id = ?1
   AND sent_at < (
       SELECT CASE
                WHEN COUNT(ud.device_id) = COUNT(cw.last_fetched_at)
                THEN MIN(cw.last_fetched_at)
                ELSE NULL
              END
       FROM group_member gm
       JOIN channels c ON c.id = ?1 AND c.group_id = gm.group_id
       JOIN user_device ud ON ud.user_id = gm.user_id AND ud.revoked_at IS NULL
       LEFT JOIN conversation_watermark cw
              ON cw.conversation_id = ?1
             AND cw.user_id = ud.user_id
             AND cw.device_id = ud.device_id
     )"
        );
        assert_eq!(
            CLEANUP_DM_ENVELOPES,
            "\
DELETE FROM message_envelope
 WHERE conversation_id = ?1
   AND sent_at < (
       SELECT CASE
                WHEN COUNT(ud.device_id) = COUNT(cw.last_fetched_at)
                THEN MIN(cw.last_fetched_at)
                ELSE NULL
              END
       FROM dm_channel_member dcm
       JOIN user_device ud ON ud.user_id = dcm.user_id AND ud.revoked_at IS NULL
       LEFT JOIN conversation_watermark cw
              ON cw.conversation_id = ?1
             AND cw.user_id = ud.user_id
             AND cw.device_id = ud.device_id
       WHERE dcm.dm_channel_id = ?1
     )"
        );
    }

    /// The parity above compares a FRAGMENT of the DELETE against the Rust
    /// roster; this ties that fragment to the DELETE's observable behaviour, so a
    /// roster condition smuggled in ELSEWHERE in the statement cannot hide.
    ///
    /// Every device the fragment lists gets a recent cursor and every device it
    /// excludes an ancient one, so an envelope in between is deleted. Then ONE
    /// listed device — and only that one — is moved below the envelope, and the
    /// envelope must survive: the outcome pivots on exactly the roster the
    /// fragment reports.
    #[tokio::test]
    async fn the_roster_is_what_the_delete_actually_gates_on() {
        for (conv, sql, select) in [
            ("c-main", CLEANUP_CHANNEL_ENVELOPES, CHANNEL_ROSTER_SELECT),
            ("dm-main", CLEANUP_DM_ENVELOPES, DM_ROSTER_SELECT),
        ] {
            // Leg 1 — every roster device collected past the envelope.
            let caught_up = conn().await;
            fixture(&caught_up).await;
            let roster = sql_roster(&caught_up, select, conv).await;
            assert_eq!(roster, expected(), "{conv}: unexpected roster");
            for d in &roster {
                let user = d.split('-').next().unwrap().to_string();
                watermark(&caught_up, conv, &user, d, "-1 day").await;
            }
            // Excluded devices are far behind — they must not be consulted.
            watermark(&caught_up, conv, "bob", "bob-revoked", "-400 days").await;
            watermark(&caught_up, conv, "carol", "carol-old-1", "-400 days").await;
            watermark(&caught_up, conv, "carol", "carol-old-2", "-400 days").await;
            watermark(&caught_up, conv, "mallory", "mallory-1", "-400 days").await;
            envelope(&caught_up, "e1", conv, "-5 days").await;
            caught_up
                .execute(sql, libsql::params![conv.to_string()])
                .await
                .unwrap();
            assert_eq!(
                envelope_count(&caught_up, conv).await,
                0,
                "{conv}: with every device the roster lists caught up, the envelope \
                 must go — a device the roster EXCLUDES must not hold it"
            );

            // Leg 2 — one roster device, and only it, falls behind.
            let held = conn().await;
            fixture(&held).await;
            for d in &roster {
                let user = d.split('-').next().unwrap().to_string();
                let offset = if d.as_str() == "bob-silent" {
                    "-400 days"
                } else {
                    "-1 day"
                };
                watermark(&held, conv, &user, d, offset).await;
            }
            watermark(&held, conv, "bob", "bob-revoked", "-1 day").await;
            watermark(&held, conv, "carol", "carol-old-1", "-1 day").await;
            watermark(&held, conv, "carol", "carol-old-2", "-1 day").await;
            watermark(&held, conv, "mallory", "mallory-1", "-1 day").await;
            envelope(&held, "e1", conv, "-5 days").await;
            held.execute(sql, libsql::params![conv.to_string()])
                .await
                .unwrap();
            assert_eq!(
                envelope_count(&held, conv).await,
                1,
                "{conv}: a single device the roster LISTS still holds the envelope, \
                 however caught-up the excluded devices are"
            );
        }
    }
}
