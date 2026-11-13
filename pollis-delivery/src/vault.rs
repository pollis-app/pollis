//! The Vault (#107) — personal encrypted storage, synced across a user's
//! devices as ciphertext the server cannot read.
//!
//! Every row is an opaque AES-256-GCM blob; the DS checks only owner and envelope, never content.

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

// Request bodies live in `pollis-api`, shared with pollis-core; re-exported here.
pub use pollis_api::vault::*;

/// AES-256-GCM nonce length — the one length the construction has.
const VAULT_NONCE_LEN: usize = 12;

/// Hard ceiling on one entry's blob; attachment bytes live in R2, never here.
const VAULT_BLOB_MAX_BYTES: usize = 256 * 1024;

/// Hard ceiling on attachment references per entry.
const VAULT_MAX_ATTACHMENT_REFS: usize = 32;

/// The result of a vault write. Wider than [`WriteOutcome`] because a
/// malformed envelope is the caller's mistake (400), not a permission problem.
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
    let outcome = apply_save_vault_message(&conn, authed.as_deref(), &parsed).await?;
    // An edit that drops a file may release it (#1881).
    if matches!(outcome, VaultOutcome::Ok) {
        crate::messages::spawn_attachment_reclaim(&state);
    }
    vault_outcome_response::<SaveVaultMessageBody>(outcome)
}

/// UPSERT one vault entry, owner-bound on BOTH arms, and REPLACE its
/// attachment reference set.
///
/// The update arm's `WHERE vault_message.user_id = ?2` stops a colliding ULID being a cross-user write.
///
/// Refs are replaced only after the entry write is accepted, so a forged save can't touch others'.
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
    // A reference is exactly 64 lowercase hex (a SHA-256).
    for h in &body.attachment_hashes {
        if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(VaultOutcome::Invalid("attachment hash is not sha-256 hex"));
        }
    }

    // One transaction: the entry and its reference set change together, so a
    // failure part-way never leaves the entry without the files it holds.
    let tx = conn.transaction().await?;
    let affected = tx
        .execute(
            // `updated_at` = `created_at` on insert; it changes only on a real edit (#1316, #1497).
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
    // Zero rows: the id is someone else's entry.
    if affected == 0 {
        tx.rollback().await?;
        return Ok(VaultOutcome::Forbidden);
    }

    // Replace the entry's reference set; an object goes only once nothing references it.
    let before = crate::messages::drop_vault_references(&tx, &body.id).await?;
    for h in &body.attachment_hashes {
        let h = h.to_ascii_lowercase();
        // A file whose bytes the DS is deleting cannot be kept by this entry (#1881).
        if crate::messages::object_state(&tx, &h).await?.as_deref() == Some("deleting") {
            tx.rollback().await?;
            return Ok(VaultOutcome::Invalid("an attachment is being deleted"));
        }
        tx.execute(
            "INSERT OR IGNORE INTO vault_attachment_ref (content_hash, vault_message_id) \
             VALUES (?1, ?2)",
            libsql::params![h.clone(), body.id.clone()],
        )
        .await?;
        // A file another delete just released is held again (#1881).
        crate::messages::revive_released_object(&tx, &h).await?;
    }
    // A file this edit dropped is released, if nothing else holds it.
    crate::messages::mark_released_objects(&tx, &before).await?;
    tx.commit().await?;
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
    let outcome = apply_delete_vault_message(&conn, authed.as_deref(), &parsed).await?;
    if matches!(outcome, WriteOutcome::Ok) {
        crate::messages::spawn_attachment_reclaim(&state);
    }
    outcome_response::<DeleteVaultMessageBody>(outcome)
}

/// Hard-DELETE one entry and its refs, owner-bound; an already-gone entry is a no-op.
///
/// The ref delete has the same owner guard, so a replayed id can't strip another user's refs.
pub async fn apply_delete_vault_message(
    conn: &Connection,
    authed: Option<&str>,
    body: &DeleteVaultMessageBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, Some(body.user_id.as_str())) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    let tx = conn.transaction().await?;
    let removed = tx
        .execute(
            "DELETE FROM vault_message WHERE id = ?1 AND user_id = ?2",
            libsql::params![body.id.clone(), user],
        )
        .await?;
    if removed > 0 {
        let held = crate::messages::drop_vault_references(&tx, &body.id).await?;
        // Deleting the entry deletes it from the only history it is in (#1881).
        crate::messages::mark_released_objects(&tx, &held).await?;
    }
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

/// POST `/v1/read/vault` — every entry this account holds, newest first.
///
/// Bound to the authenticated user: even undecryptable rows leak counts and change rates.
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
