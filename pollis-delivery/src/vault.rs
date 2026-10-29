//! The Vault (#107): personal storage synced across a user's devices as
//! per-entry AES-256-GCM blobs under a key derived from the account identity
//! key, which the DS never holds. The DS only does custody, owner binding and
//! envelope bounds; what it learns is stated in migration
//! `000020_vault_message.sql`.

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
    ok_response,
    outcome_response,
    resolve_actor,
    RawRequest,
    WriteOutcome,
};
use crate::AppState;
use crate::util::{b64, b64_decode};

// Request bodies are declared once in `pollis-api`, shared with pollis-core.
pub use pollis_api::vault::*;

const VAULT_NONCE_LEN: usize = 12;

/// Entries are padded JSON (attachment bytes live in R2), so a larger blob is a
/// bug or abuse.
const VAULT_BLOB_MAX_BYTES: usize = 256 * 1024;

/// The composer's per-message attachment cap, with headroom.
const VAULT_MAX_ATTACHMENT_REFS: usize = 32;

/// [`WriteOutcome`] plus `Invalid`: a malformed envelope is a 400, not a 403.
#[derive(Debug)]
pub enum VaultOutcome {
    Ok,
    Forbidden,
    /// `&'static str` so no caller input is ever echoed back.
    Invalid(&'static str),
}

fn vault_outcome_response<B>(outcome: VaultOutcome) -> Result<Response, AppError>
where
    B: pollis_api::DsRequest<Response = pollis_api::StatusOk>,
{
    Ok(match outcome {
        VaultOutcome::Ok => ok_response::<B>(pollis_api::StatusOk::Ok),
        VaultOutcome::Forbidden => AuthRejection::Forbidden.into_response(),
        VaultOutcome::Invalid(why) => bad_request(why),
    })
}

pub async fn save_vault_message(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<SaveVaultMessageBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    vault_outcome_response::<SaveVaultMessageBody>(
        apply_save_vault_message(&conn, authed.as_deref(), &parsed).await?,
    )
}

/// UPSERT one vault entry and replace its attachment reference set.
///
/// The conflict target is the primary key alone, so the update arm's
/// `user_id` predicate is what stops a colliding id from overwriting another
/// user's entry. The refs are replaced only after that write is accepted; they
/// keep shared R2 objects alive against GC (`crate::messages::LIVE_REF_EXISTS_SQL`).
pub async fn apply_save_vault_message(
    conn: &Connection,
    authed: Option<&str>,
    body: &SaveVaultMessageBody,
) -> anyhow::Result<VaultOutcome> {
    let user = match resolve_actor(authed, Some(body.user_id.as_str())) {
        Ok(u) => u,
        Err(_) => return Ok(VaultOutcome::Forbidden),
    };
    if body.id.is_empty() || body.id.len() > 64 {
        return Ok(VaultOutcome::Invalid("id length out of range"));
    }
    if body.created_at.is_empty() || body.created_at.len() > 64 {
        return Ok(VaultOutcome::Invalid("created_at length out of range"));
    }
    let Ok(nonce) = b64_decode(&body.nonce) else {
        return Ok(VaultOutcome::Invalid("nonce is not valid base64"));
    };
    if nonce.len() != VAULT_NONCE_LEN {
        return Ok(VaultOutcome::Invalid("nonce must be 12 bytes"));
    }
    let Ok(blob) = b64_decode(&body.blob) else {
        return Ok(VaultOutcome::Invalid("blob is not valid base64"));
    };
    if blob.is_empty() || blob.len() > VAULT_BLOB_MAX_BYTES {
        return Ok(VaultOutcome::Invalid("blob size out of range"));
    }
    if body.attachment_hashes.len() > VAULT_MAX_ATTACHMENT_REFS {
        return Ok(VaultOutcome::Invalid("too many attachment references"));
    }
    for h in &body.attachment_hashes {
        if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(VaultOutcome::Invalid("attachment hash is not sha-256 hex"));
        }
    }

    let affected = conn
        .execute(
            // Clients show "(edited)" when `updated_at` differs from
            // `created_at` (#1316), so only a real change stamps it: a
            // byte-identical save is an idempotent retry (#1497).
            "INSERT INTO vault_message (id, user_id, nonce, blob, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?5) \
             ON CONFLICT(id) DO UPDATE SET \
                 nonce = excluded.nonce, \
                 blob = excluded.blob, \
                 updated_at = CASE \
                     WHEN vault_message.nonce = excluded.nonce \
                      AND vault_message.blob = excluded.blob \
                     THEN vault_message.updated_at \
                     ELSE datetime('now') END \
             WHERE vault_message.user_id = ?2",
            libsql::params![
                body.id.clone(),
                user,
                nonce,
                blob,
                body.created_at.clone()
            ],
        )
        .await?;
    // Zero rows: the id belongs to someone else.
    if affected == 0 {
        return Ok(VaultOutcome::Forbidden);
    }

    conn.execute(
        "DELETE FROM vault_attachment_ref WHERE vault_message_id = ?1",
        libsql::params![body.id.clone()],
    )
    .await?;
    for h in &body.attachment_hashes {
        conn.execute(
            "INSERT OR IGNORE INTO vault_attachment_ref (content_hash, vault_message_id) \
             VALUES (?1, ?2)",
            libsql::params![h.to_ascii_lowercase(), body.id.clone()],
        )
        .await?;
    }
    Ok(VaultOutcome::Ok)
}

pub async fn delete_vault_message(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<DeleteVaultMessageBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<DeleteVaultMessageBody>(
        apply_delete_vault_message(&conn, authed.as_deref(), &parsed).await?,
    )
}

/// Hard-DELETE one of the actor's entries and its attachment references. The
/// ref delete runs only if the entry was the caller's, so replaying an id
/// cannot strip another user's refs.
pub async fn apply_delete_vault_message(
    conn: &Connection,
    authed: Option<&str>,
    body: &DeleteVaultMessageBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, Some(body.user_id.as_str())) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    let removed = conn
        .execute(
            "DELETE FROM vault_message WHERE id = ?1 AND user_id = ?2",
            libsql::params![body.id.clone(), user],
        )
        .await?;
    if removed > 0 {
        conn.execute(
            "DELETE FROM vault_attachment_ref WHERE vault_message_id = ?1",
            libsql::params![body.id.clone()],
        )
        .await?;
    }
    Ok(WriteOutcome::Ok)
}

/// POST `/v1/read/vault` — every entry this account holds, newest first.
/// Owner-bound: even undecryptable, the rows reveal counts and change rates.
pub async fn read_vault(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: VaultMessagesBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match authed_user(&state, &req, Some(parsed.user_id.as_str())).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };

    let conn = state.db.conn().await?;
    let mut rows = conn
        .query(
            "SELECT id, nonce, blob, created_at, updated_at \
             FROM vault_message WHERE user_id = ?1 \
             ORDER BY created_at DESC, id DESC",
            libsql::params![who],
        )
        .await?;
    let mut messages = Vec::new();
    while let Some(row) = rows.next().await? {
        messages.push(StoredVaultMessage {
            id: row.get::<String>(0)?,
            nonce: b64(&row.get::<Vec<u8>>(1)?),
            blob: b64(&row.get::<Vec<u8>>(2)?),
            created_at: row.get::<String>(3)?,
            updated_at: row.get::<String>(4)?,
        });
    }
    Ok(ok_response::<VaultMessagesBody>(VaultMessagesResponse {
        messages,
    }))
}
