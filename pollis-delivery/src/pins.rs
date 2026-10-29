//! Pinned messages (#99) — the wrapped pin-key state and the pin rows.
//!
//! Two ciphertexts, keys to neither: `pin_keystate` (`Kpin` wrapped per epoch, CAS'd) and pins sealed under `Kpin`.
//! All membership-gated via [`crate::writes::is_member`]; any current member may pin and unpin.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
};
use libsql::Connection;

use crate::error::{AppError, AuthRejection};
use crate::reads::authed_user;
use crate::writes::{
    bad_request,
    gate_and_parse,
    is_member,
    ok_response,
    outcome_response,
    resolve_actor,
    RawRequest,
};

use crate::AppState;
use crate::util::{b64, b64_decode};

// Request bodies live in `pollis-api`, shared with pollis-core; re-exported here.
pub use pollis_api::pins::*;

/// AES-256-GCM nonce length — the one length the construction has.
const PIN_NONCE_LEN: usize = 12;

/// Hard ceiling on one wrapped `Kpin` blob (really 48 bytes).
const KEYSTATE_BLOB_MAX_BYTES: usize = 256;

/// Hard ceiling on one pin's encrypted snapshot.
const PIN_BLOB_MAX_BYTES: usize = 256 * 1024;

/// A pin-domain write result; a malformed envelope is a 400, not a permission problem.
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

/// The keystate CAS answer; a refused CAS is a normal race outcome, not an error.
#[derive(Debug)]
pub enum KeystateOutcome {
    Ok { updated: bool },
    Forbidden,
    Invalid(&'static str),
    /// The `(generation, epoch)` is ABOVE the commit log's head; answered with the head.
    AheadOfHead {
        head_generation: i64,
        head_epoch: i64,
    },
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
    // Two databases: membership lives on the main DB, the commit log that
    // ceilings the CAS lives on the log DB (#420 split them).
    let log_conn = state.log_db.conn().await?;
    Ok(
        match apply_upsert_keystate(&conn, &log_conn, authed.as_deref(), &parsed).await? {
            KeystateOutcome::Ok { updated } => {
                ok_response::<UpsertPinKeystateBody>(PinKeystateUpserted::Ok { updated })
            }
            KeystateOutcome::Forbidden => AuthRejection::Forbidden.into_response(),
            KeystateOutcome::Invalid(why) => bad_request(why),
            KeystateOutcome::AheadOfHead {
                head_generation,
                head_epoch,
            } => crate::writes::epoch_behind_response(head_generation, head_epoch),
        },
    )
}

/// Insert-or-CAS the conversation's wrapped `Kpin`.
///
/// Authz: current member. Lexicographic `(generation, epoch)` CAS; `updated: false` is the race answer.
///
/// Ceilinged at the commit log's head, or one huge `generation` would freeze the row forever.
pub async fn apply_upsert_keystate(
    conn: &Connection,
    log_conn: &Connection,
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
    let Ok(wrapped) = b64_decode(&body.wrapped_kpin) else {
        return Ok(KeystateOutcome::Invalid("wrapped_kpin is not valid base64"));
    };
    if wrapped.is_empty() || wrapped.len() > KEYSTATE_BLOB_MAX_BYTES {
        return Ok(KeystateOutcome::Invalid("wrapped_kpin size out of range"));
    }
    let Ok(nonce) = b64_decode(&body.nonce) else {
        return Ok(KeystateOutcome::Invalid("nonce is not valid base64"));
    };
    if nonce.len() != PIN_NONCE_LEN {
        return Ok(KeystateOutcome::Invalid("nonce must be 12 bytes"));
    }
    match crate::writes::refuse_above_head(
        log_conn,
        &body.conversation_id,
        body.generation,
        body.epoch,
    )
    .await?
    {
        Some(crate::writes::HeadBoundedOutcome::Invalid(why)) => {
            return Ok(KeystateOutcome::Invalid(why));
        }
        Some(crate::writes::HeadBoundedOutcome::AheadOfHead {
            head_generation,
            head_epoch,
        }) => {
            return Ok(KeystateOutcome::AheadOfHead {
                head_generation,
                head_epoch,
            });
        }
        Some(crate::writes::HeadBoundedOutcome::Ok { .. }) | None => {}
    }

    // A MINT is insert-only (the loser adopts the stored key); a RE-WRAP is the monotone CAS.
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

/// Store one pin's encrypted snapshot; idempotent per `(conversation_id, message_id)`.
///
/// Authz: membership. Only the envelope is validated; the DS has no key for the content.
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
    let Ok(content) = b64_decode(&body.encrypted_content) else {
        return Ok(PinOutcome::Invalid("encrypted_content is not valid base64"));
    };
    if content.is_empty() || content.len() > PIN_BLOB_MAX_BYTES {
        return Ok(PinOutcome::Invalid("encrypted_content size out of range"));
    }
    let Ok(nonce) = b64_decode(&body.nonce) else {
        return Ok(PinOutcome::Invalid("nonce is not valid base64"));
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

/// POST `/v1/read/pin-keystate` — the conversation's current wrapped `Kpin`.
///
/// Membership-gated (the wrap still leaks pinning activity); a missing row is `keystate: None`.
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

/// POST `/v1/read/pins` — one conversation's pins, newest first, membership-gated.
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
