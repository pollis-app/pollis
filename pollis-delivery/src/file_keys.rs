//! The per-conversation attachment key ring (#1382).
//!
//! One mutable row per MLS conversation in `file_keystate`: the conversation's
//! ring of random 32-byte file keys, WRAPPED under a KEK the members export
//! from the current MLS epoch, plus the ring's length (`key_version`). The DS
//! holds the wrap and can open neither it nor any attachment sealed under the
//! ring. Same shape and CAS as `pin_keystate` (`crate::pins`), with one
//! addition: the ring rotates on member removal, and `key_version` is what
//! lets the DS refuse every write that would fork it.
//!
//! ## The three writes
//!
//! - **mint** — insert-only, `key_version = 1`. A losing minter gets
//!   `updated: false`, refetches, and adopts the winner's ring.
//! - **re-wrap** — same ring, newer epoch's KEK: `key_version` must EQUAL the
//!   stored one and `(generation, epoch)` must be lexicographically strictly
//!   greater. A re-wrap racing a rotation carries the old length and is
//!   refused, so a rotation can never be rolled back.
//! - **rotate** — exactly one new key appended: `key_version` must be the
//!   stored one plus one, at a `(generation, epoch)` no older than the stored
//!   pair. Two concurrent rotations cannot both land.
//!
//! Every write and the read are membership-gated, and the `(generation,
//! epoch)` a write names is ceilinged at the commit log's head, like
//! `pin_keystate`, so nobody can freeze the row with an epoch that does not
//! exist yet.
//!
//! Each ring entry is a key plus the `(generation, epoch)` it was added at
//! (48 bytes before the wrap's tag). The DS checks only that the wrap's length
//! matches `key_version`; the epochs inside let a client tell whether a
//! rotation owed for a removal has already happened.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
};
use libsql::Connection;

use crate::error::{AppError, AuthRejection};
use crate::reads::authed_user;
use crate::writes::{bad_request, gate_and_parse, is_member, ok_response, resolve_actor, RawRequest};

use crate::AppState;
use crate::util::{b64, b64_decode};

pub use pollis_api::file_keys::*;

/// AES-256-GCM nonce length.
const NONCE_LEN: usize = 12;

/// One ring entry: a 32-byte key plus the `(generation, epoch)` it was added
/// at, two big-endian i64s. Mirrors `pollis_core::commands::file_keys`.
const RING_ENTRY_LEN: usize = 48;

/// AES-256-GCM tag.
const TAG_LEN: usize = 16;

/// Ceiling on the ring length (one key per removal batch), so a hostile member
/// cannot park a large blob here.
const MAX_KEY_VERSION: i64 = 1024;

#[derive(Debug)]
pub enum FileKeystateOutcome {
    Ok { updated: bool },
    Forbidden,
    Invalid(&'static str),
    AheadOfHead { head_generation: i64, head_epoch: i64 },
}

pub async fn upsert_keystate(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<UpsertFileKeystateBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let log_conn = state.log_db.conn().await?;
    Ok(
        match apply_upsert_keystate(&conn, &log_conn, authed.as_deref(), &parsed).await? {
            FileKeystateOutcome::Ok { updated } => {
                ok_response::<UpsertFileKeystateBody>(FileKeystateUpserted::Ok { updated })
            }
            FileKeystateOutcome::Forbidden => AuthRejection::Forbidden.into_response(),
            FileKeystateOutcome::Invalid(why) => bad_request(why),
            FileKeystateOutcome::AheadOfHead {
                head_generation,
                head_epoch,
            } => crate::writes::epoch_behind_response(head_generation, head_epoch),
        },
    )
}

/// Apply one keystate write. See the module docs for the three rules.
pub async fn apply_upsert_keystate(
    conn: &Connection,
    log_conn: &Connection,
    authed: Option<&str>,
    body: &UpsertFileKeystateBody,
) -> anyhow::Result<FileKeystateOutcome> {
    let actor = match resolve_actor(authed, body.actor_id.as_deref()) {
        Ok(a) => a,
        Err(_) => return Ok(FileKeystateOutcome::Forbidden),
    };
    if !is_member(conn, &body.conversation_id, &actor).await? {
        return Ok(FileKeystateOutcome::Forbidden);
    }
    if body.key_version < 1 || body.key_version > MAX_KEY_VERSION {
        return Ok(FileKeystateOutcome::Invalid("key_version out of range"));
    }
    if body.op == FileKeystateOp::Mint && body.key_version != 1 {
        return Ok(FileKeystateOutcome::Invalid("a mint is a ring of one key"));
    }
    let Ok(wrapped) = b64_decode(&body.wrapped_keyring) else {
        return Ok(FileKeystateOutcome::Invalid("wrapped_keyring is not valid base64"));
    };
    if wrapped.len() != body.key_version as usize * RING_ENTRY_LEN + TAG_LEN {
        return Ok(FileKeystateOutcome::Invalid(
            "wrapped_keyring length does not match key_version",
        ));
    }
    let Ok(nonce) = b64_decode(&body.nonce) else {
        return Ok(FileKeystateOutcome::Invalid("nonce is not valid base64"));
    };
    if nonce.len() != NONCE_LEN {
        return Ok(FileKeystateOutcome::Invalid("nonce must be 12 bytes"));
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
            return Ok(FileKeystateOutcome::Invalid(why));
        }
        Some(crate::writes::HeadBoundedOutcome::AheadOfHead {
            head_generation,
            head_epoch,
        }) => {
            return Ok(FileKeystateOutcome::AheadOfHead {
                head_generation,
                head_epoch,
            });
        }
        Some(crate::writes::HeadBoundedOutcome::Ok { .. }) | None => {}
    }

    let sql = match body.op {
        FileKeystateOp::Mint => {
            "INSERT INTO file_keystate \
                 (conversation_id, wrapped_keyring, nonce, key_version, generation, epoch, \
                  updated_by_device_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(conversation_id) DO NOTHING"
        }
        FileKeystateOp::Rewrap => {
            "UPDATE file_keystate SET \
                 wrapped_keyring = ?2, nonce = ?3, generation = ?5, epoch = ?6, \
                 updated_by_device_id = ?7, updated_at = datetime('now') \
             WHERE conversation_id = ?1 \
               AND key_version = ?4 \
               AND (?5 > generation OR (?5 = generation AND ?6 > epoch))"
        }
        FileKeystateOp::Rotate => {
            "UPDATE file_keystate SET \
                 wrapped_keyring = ?2, nonce = ?3, key_version = ?4, generation = ?5, \
                 epoch = ?6, updated_by_device_id = ?7, updated_at = datetime('now') \
             WHERE conversation_id = ?1 \
               AND key_version = ?4 - 1 \
               AND (?5 > generation OR (?5 = generation AND ?6 >= epoch))"
        }
    };
    let affected = conn
        .execute(
            sql,
            libsql::params![
                body.conversation_id.clone(),
                wrapped,
                nonce,
                body.key_version,
                body.generation,
                body.epoch,
                body.device_id.clone(),
            ],
        )
        .await?;
    Ok(FileKeystateOutcome::Ok {
        updated: affected > 0,
    })
}

/// POST `/v1/read/file-keystate` — the conversation's current wrapped ring.
/// Membership-gated; a missing row is `keystate: None`.
pub async fn read_keystate(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: FileKeystateBody = match serde_json::from_slice(&req.body) {
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
    Ok(ok_response::<FileKeystateBody>(FileKeystateResponse {
        keystate: read_stored(&conn, &parsed.conversation_id).await?,
    }))
}

/// The stored row, byte-exact.
pub async fn read_stored(
    conn: &Connection,
    conversation_id: &str,
) -> anyhow::Result<Option<StoredFileKeystate>> {
    let mut rows = conn
        .query(
            "SELECT wrapped_keyring, nonce, key_version, generation, epoch \
             FROM file_keystate WHERE conversation_id = ?1",
            libsql::params![conversation_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(StoredFileKeystate {
            wrapped_keyring: b64(&row.get::<Vec<u8>>(0)?),
            nonce: b64(&row.get::<Vec<u8>>(1)?),
            key_version: row.get::<i64>(2)?,
            generation: row.get::<i64>(3)?,
            epoch: row.get::<i64>(4)?,
        }),
        None => None,
    })
}
