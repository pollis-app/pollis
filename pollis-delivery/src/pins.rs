//! Pinned messages (#99) — the wrapped pin-key state and the pin rows.
//!
//! Follows the domain-module shape verbatim: one `*Body` per endpoint in
//! `pollis-api`, a pure `apply_*(conn, authed, body)` embedding BOTH the
//! authorization decision and the write, and a thin axum handler that gates,
//! parses, and maps the outcome. Reads follow `account_reads`: authenticate,
//! membership-gate, serve ciphertext back byte-exactly.
//!
//! ## What the DS holds, in one paragraph
//!
//! Two ciphertexts per pinning conversation, keys to neither. `pin_keystate`
//! is the conversation's 32-byte pin master key (`Kpin`) WRAPPED under a KEK
//! the members derive from the current MLS epoch's exporter secret — one
//! mutable row per MLS conversation, re-wrapped (~60 bytes) whenever the epoch
//! advances, guarded by the same lexicographic `(generation, epoch)` CAS as
//! `mls_group_info` so a stale wrap can never roll the row back to an epoch a
//! removed member could still derive. `pinned_message` rows are content
//! snapshots sealed directly under `Kpin`, written once at pin time and never
//! re-encrypted; a removed member stops being able to read FUTURE pins because
//! it can no longer unwrap the re-wrapped `Kpin`, which is the honest
//! guarantee (plaintexts it saw while a member were always beyond recall).
//!
//! ## Authorization
//!
//! Everything here is membership-gated via [`crate::writes::is_member`]: the
//! keystate is addressed by the MLS conversation id (group or DM), pins by the
//! channel or DM id, and `is_member` accepts all three id kinds. Any current
//! member may pin and unpin (Slack's model — a pin is conversation furniture,
//! not the pinner's property). On the no-auth path the identity comes from the
//! body actor, mirroring the other write modules.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use libsql::Connection;

use crate::error::{AppError, AuthRejection};
use crate::reads::authed_user;
use crate::writes::{bad_request, gate_and_parse, is_member, ok_response, outcome_response, resolve_actor, RawRequest};

use crate::AppState;

// The request bodies for this module's endpoints live in `pollis-api`, the
// crate pollis-core builds its requests from — one declaration, both ends, so
// a client field that does not exist here is a compile error rather than a
// silently-absent JSON key. Re-exported so `pollis_delivery::pins::*Body`
// keeps resolving for handlers, tests and the flows harness.
pub use pollis_api::pins::*;

// ── Bounds ───────────────────────────────────────────────────────────────────

/// AES-256-GCM nonce length — the one length the construction has.
const PIN_NONCE_LEN: usize = 12;

/// Hard ceiling on one wrapped `Kpin` blob. The real thing is 48 bytes
/// (32-byte key + 16-byte tag); anything much larger is not a wrap.
const KEYSTATE_BLOB_MAX_BYTES: usize = 256;

/// Hard ceiling on one pin's encrypted content snapshot. A snapshot is one
/// message's padded JSON — the same bound the read-cursor blob uses is roomy
/// enough and keeps a hostile client from parking megabytes per pin.
const PIN_BLOB_MAX_BYTES: usize = 256 * 1024;

/// The result of a pin-domain write. Wider than [`crate::writes::WriteOutcome`]
/// for the same reason `profile::ReadCursorOutcome` is: a malformed envelope is
/// the caller's mistake (400), not a permission problem.
#[derive(Debug)]
pub enum PinOutcome {
    Ok,
    Forbidden,
    /// `&'static str` so no caller input is ever echoed back.
    Invalid(&'static str),
}

fn pin_outcome_response<B>(outcome: PinOutcome) -> Result<Response, AppError>
where
    B: pollis_api::DsRequest<Response = pollis_api::StatusOk>,
{
    Ok(match outcome {
        PinOutcome::Ok => ok_response::<B>(pollis_api::StatusOk::Ok),
        PinOutcome::Forbidden => AuthRejection::Forbidden.into_response(),
        PinOutcome::Invalid(why) => bad_request(why),
    })
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ── POST /v1/pins/keystate ───────────────────────────────────────────────────

/// The keystate CAS answer: did the wrap land? Separate from [`PinOutcome`]
/// because a refused CAS is a normal race outcome the client acts on, not an
/// error.
#[derive(Debug)]
pub enum KeystateOutcome {
    Ok { updated: bool },
    Forbidden,
    Invalid(&'static str),
}

pub async fn upsert_keystate(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<UpsertPinKeystateBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    Ok(
        match apply_upsert_keystate(&conn, authed.as_deref(), &parsed).await? {
            KeystateOutcome::Ok { updated } => {
                ok_response::<UpsertPinKeystateBody>(PinKeystateUpserted::Ok { updated })
            }
            KeystateOutcome::Forbidden => AuthRejection::Forbidden.into_response(),
            KeystateOutcome::Invalid(why) => bad_request(why),
        },
    )
}

/// Insert-or-CAS the conversation's wrapped `Kpin`.
///
/// Authz: the actor must be a current member of the MLS conversation. The CAS
/// mirrors `writes::upsert_group_info` exactly — lexicographic on
/// `(generation, epoch)`, strictly greater or refused — because the wrap is
/// derived from the same object those columns version (the group's exporter),
/// and an epoch-only guard would strand a suite migration's successor lineage.
/// `updated: false` (zero rows) is the race answer, not an error: it tells a
/// minting loser to refetch and adopt the winner's key, and a re-wrapping
/// device that somebody newer already covered it.
pub async fn apply_upsert_keystate(
    conn: &Connection,
    authed: Option<&str>,
    body: &UpsertPinKeystateBody,
) -> anyhow::Result<KeystateOutcome> {
    let actor = match resolve_actor(authed, body.actor_id.as_deref()) {
        Ok(a) => a,
        Err(_) => return Ok(KeystateOutcome::Forbidden),
    };
    if !is_member(conn, &body.conversation_id, &actor).await? {
        return Ok(KeystateOutcome::Forbidden);
    }
    let wrapped = match b64_decode(&body.wrapped_kpin) {
        Some(w) => w,
        None => return Ok(KeystateOutcome::Invalid("wrapped_kpin is not valid base64")),
    };
    if wrapped.is_empty() || wrapped.len() > KEYSTATE_BLOB_MAX_BYTES {
        return Ok(KeystateOutcome::Invalid("wrapped_kpin size out of range"));
    }
    let nonce = match b64_decode(&body.nonce) {
        Some(n) => n,
        None => return Ok(KeystateOutcome::Invalid("nonce is not valid base64")),
    };
    if nonce.len() != PIN_NONCE_LEN {
        return Ok(KeystateOutcome::Invalid("nonce must be 12 bytes"));
    }
    if body.generation < 0 || body.epoch < 0 {
        return Ok(KeystateOutcome::Invalid("generation/epoch out of range"));
    }

    // A MINT is insert-only: if any row exists, the caller lost the race and
    // must adopt the stored key, even when its own epoch is newer — updating
    // here would replace a Kpin that pins may already be sealed under,
    // forking the key lineage. A RE-WRAP is the monotone CAS: same key, new
    // epoch's KEK, never rolled backwards.
    let sql = if body.mint {
        "INSERT INTO pin_keystate \
             (conversation_id, wrapped_kpin, nonce, generation, epoch, updated_by_device_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(conversation_id) DO NOTHING"
    } else {
        "INSERT INTO pin_keystate \
             (conversation_id, wrapped_kpin, nonce, generation, epoch, updated_by_device_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(conversation_id) DO UPDATE SET \
             wrapped_kpin = excluded.wrapped_kpin, \
             nonce = excluded.nonce, \
             generation = excluded.generation, \
             epoch = excluded.epoch, \
             updated_by_device_id = excluded.updated_by_device_id, \
             updated_at = datetime('now') \
         WHERE excluded.generation > pin_keystate.generation \
            OR (excluded.generation = pin_keystate.generation \
                AND excluded.epoch > pin_keystate.epoch)"
    };
    let affected = conn
        .execute(
            sql,
            libsql::params![
                body.conversation_id.clone(),
                wrapped,
                nonce,
                body.generation,
                body.epoch,
                body.device_id.clone(),
            ],
        )
        .await?;
    Ok(KeystateOutcome::Ok {
        updated: affected > 0,
    })
}

// ── POST /v1/pins/pin ────────────────────────────────────────────────────────

pub async fn pin_message(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<PinMessageBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    pin_outcome_response::<PinMessageBody>(apply_pin_message(&conn, authed.as_deref(), &parsed).await?)
}

/// Store one pin's encrypted snapshot. Idempotent per
/// `(conversation_id, message_id)`: the second pin of the same message is a
/// no-op rather than an error, whoever got there first keeps the row.
///
/// Authz: current membership of the pin's conversation. What is validated is
/// the ENVELOPE — base64 that decodes, the one nonce length AES-GCM has, a
/// bounded blob — and nothing about the content, because the DS holds no key
/// for it and never will.
pub async fn apply_pin_message(
    conn: &Connection,
    authed: Option<&str>,
    body: &PinMessageBody,
) -> anyhow::Result<PinOutcome> {
    let actor = match resolve_actor(authed, body.pinned_by.as_deref()) {
        Ok(a) => a,
        Err(_) => return Ok(PinOutcome::Forbidden),
    };
    if !is_member(conn, &body.conversation_id, &actor).await? {
        return Ok(PinOutcome::Forbidden);
    }
    let content = match b64_decode(&body.encrypted_content) {
        Some(c) => c,
        None => {
            return Ok(PinOutcome::Invalid(
                "encrypted_content is not valid base64",
            ))
        }
    };
    if content.is_empty() || content.len() > PIN_BLOB_MAX_BYTES {
        return Ok(PinOutcome::Invalid("encrypted_content size out of range"));
    }
    let nonce = match b64_decode(&body.nonce) {
        Some(n) => n,
        None => return Ok(PinOutcome::Invalid("nonce is not valid base64")),
    };
    if nonce.len() != PIN_NONCE_LEN {
        return Ok(PinOutcome::Invalid("nonce must be 12 bytes"));
    }

    conn.execute(
        "INSERT INTO pinned_message \
             (id, conversation_id, message_id, pinned_by, encrypted_content, nonce) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(conversation_id, message_id) DO NOTHING",
        libsql::params![
            body.id.clone(),
            body.conversation_id.clone(),
            body.message_id.clone(),
            actor,
            content,
            nonce,
        ],
    )
    .await?;
    Ok(PinOutcome::Ok)
}

// ── POST /v1/pins/unpin ──────────────────────────────────────────────────────

pub async fn unpin_message(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<UnpinMessageBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<UnpinMessageBody>(apply_unpin_message(&conn, authed.as_deref(), &parsed).await?)
}

/// Remove a pin. Any current member may unpin; deleting a pin that does not
/// exist is a no-op (idempotent, like pinning twice).
pub async fn apply_unpin_message(
    conn: &Connection,
    authed: Option<&str>,
    body: &UnpinMessageBody,
) -> anyhow::Result<crate::writes::WriteOutcome> {
    let actor = match resolve_actor(authed, body.actor_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    if !is_member(conn, &body.conversation_id, &actor).await? {
        return Ok(crate::writes::WriteOutcome::Forbidden);
    }
    conn.execute(
        "DELETE FROM pinned_message WHERE conversation_id = ?1 AND message_id = ?2",
        libsql::params![body.conversation_id.clone(), body.message_id.clone()],
    )
    .await?;
    Ok(crate::writes::WriteOutcome::Ok)
}

// ── POST /v1/read/pin-keystate ───────────────────────────────────────────────

/// POST `/v1/read/pin-keystate` — the conversation's current wrapped `Kpin`.
///
/// Membership-gated: the wrap is undecryptable without the epoch's exporter,
/// but serving it to a stranger would still leak that the conversation pins
/// things and how recently its epoch moved. A missing row is
/// `keystate: None`, not a 404 — a conversation that predates #99 simply has
/// no key yet, and the first pinner mints one.
pub async fn read_keystate(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: PinKeystateBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match authed_user(&state, &req, parsed.user_id.as_deref()).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };

    let conn = state.db.conn().await?;
    if !is_member(&conn, &parsed.conversation_id, &who).await? {
        return Ok(AuthRejection::Forbidden.into_response());
    }
    let mut rows = conn
        .query(
            "SELECT wrapped_kpin, nonce, generation, epoch \
             FROM pin_keystate WHERE conversation_id = ?1",
            libsql::params![parsed.conversation_id.clone()],
        )
        .await?;
    let keystate = match rows.next().await? {
        Some(row) => Some(StoredPinKeystate {
            wrapped_kpin: b64(&row.get::<Vec<u8>>(0)?),
            nonce: b64(&row.get::<Vec<u8>>(1)?),
            generation: row.get::<i64>(2)?,
            epoch: row.get::<i64>(3)?,
        }),
        None => None,
    };
    Ok(ok_response::<PinKeystateBody>(PinKeystateResponse {
        keystate,
    }))
}

// ── POST /v1/read/pins ───────────────────────────────────────────────────────

/// POST `/v1/read/pins` — one conversation's pins, newest first.
/// Membership-gated like the keystate read; rows come back ciphertext and all,
/// byte-exact, for the client to open under `Kpin`.
pub async fn read_pins(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: ListPinsBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match authed_user(&state, &req, parsed.user_id.as_deref()).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };

    let conn = state.db.conn().await?;
    if !is_member(&conn, &parsed.conversation_id, &who).await? {
        return Ok(AuthRejection::Forbidden.into_response());
    }
    let mut rows = conn
        .query(
            "SELECT id, conversation_id, message_id, pinned_by, encrypted_content, nonce, \
                    pinned_at \
             FROM pinned_message WHERE conversation_id = ?1 \
             ORDER BY pinned_at DESC, id DESC",
            libsql::params![parsed.conversation_id.clone()],
        )
        .await?;
    let mut pins = Vec::new();
    while let Some(row) = rows.next().await? {
        pins.push(StoredPin {
            id: row.get::<String>(0)?,
            conversation_id: row.get::<String>(1)?,
            message_id: row.get::<String>(2)?,
            pinned_by: row.get::<String>(3)?,
            encrypted_content: b64(&row.get::<Vec<u8>>(4)?),
            nonce: b64(&row.get::<Vec<u8>>(5)?),
            pinned_at: row.get::<String>(6)?,
        });
    }
    Ok(ok_response::<ListPinsBody>(ListPinsResponse { pins }))
}
