//! The Vault (#107) — personal encrypted storage, synced across a user's
//! devices as ciphertext the server cannot read.
//!
//! Copies the read-cursor-sync shape ([`crate::profile::save_read_cursors`])
//! per entry instead of per account: every row is an opaque AES-256-GCM blob
//! under a key derived client-side from the ACCOUNT identity key, which the
//! DS has never held. The DS's whole job here is byte-exact custody, owner
//! binding, and bounds — it validates the ENVELOPE (base64 that decodes, the
//! one nonce length AES-GCM has, a bounded blob) and nothing about content,
//! because the entry body, the pinned flag, and any attachment references all
//! live INSIDE the ciphertext. What the server learns is stated in migration
//! `000020_vault_message.sql`: row count, approximate (padded) sizes, change
//! times. Not content, never content.

use axum::{
    extract::State,
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use libsql::Connection;

use crate::error::{AppError, AuthRejection};
use crate::reads::authed_user;
use crate::writes::{bad_request, gate_and_parse, ok_response, outcome_response, resolve_actor, RawRequest, WriteOutcome};
use crate::AppState;

// The request bodies for this module's endpoints live in `pollis-api`, the
// crate pollis-core builds its requests from — one declaration, both ends, so
// a client field that does not exist here is a compile error rather than a
// silently-absent JSON key. Re-exported so `pollis_delivery::vault::*Body`
// keeps resolving for handlers, tests and the flows harness.
pub use pollis_api::vault::*;

/// AES-256-GCM nonce length — the one length the construction has.
const VAULT_NONCE_LEN: usize = 12;

/// Hard ceiling on one entry's blob. Matches the read-cursor bound: entries
/// are padded JSON (text + attachment references — the attachment BYTES live
/// in R2, never here), so a megabyte-scale blob is a bug or an abuse, not a
/// note.
const VAULT_BLOB_MAX_BYTES: usize = 256 * 1024;

/// Hard ceiling on attachment references per entry. Mirrors the composer's
/// per-message attachment cap with headroom; a save declaring hundreds of
/// hashes is not a note, it is somebody using the ref table as a database.
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

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

// ── POST /v1/vault/save ──────────────────────────────────────────────────────

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

/// UPSERT one vault entry, owner-bound on BOTH arms, and REPLACE its
/// attachment reference set.
///
/// The insert stamps the authenticated user as owner; the update arm's
/// `WHERE vault_message.user_id = ?2` is the row-level guard that makes
/// "overwrite a stranger's entry by minting the same ULID" unrepresentable —
/// the conflict target is the primary key alone, so without that predicate a
/// colliding id would be a cross-user write. `created_at` is kept from the
/// original row on an edit so the entry does not jump position in the list.
///
/// The reference replacement (delete-then-insert on `vault_attachment_ref`)
/// runs only after the entry write is accepted, so a forged save can never
/// touch another user's reference rows. Refs keep the shared R2 objects
/// alive against attachment GC — see `crate::messages::LIVE_REF_EXISTS_SQL`.
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
    let nonce = match b64_decode(&body.nonce) {
        Some(n) => n,
        None => return Ok(VaultOutcome::Invalid("nonce is not valid base64")),
    };
    if nonce.len() != VAULT_NONCE_LEN {
        return Ok(VaultOutcome::Invalid("nonce must be 12 bytes"));
    }
    let blob = match b64_decode(&body.blob) {
        Some(b) => b,
        None => return Ok(VaultOutcome::Invalid("blob is not valid base64")),
    };
    if blob.is_empty() || blob.len() > VAULT_BLOB_MAX_BYTES {
        return Ok(VaultOutcome::Invalid("blob size out of range"));
    }
    if body.attachment_hashes.len() > VAULT_MAX_ATTACHMENT_REFS {
        return Ok(VaultOutcome::Invalid("too many attachment references"));
    }
    // A reference names a content-addressed object: exactly 64 lowercase hex.
    // Anything else is not a SHA-256 and has no business in the ref table.
    for h in &body.attachment_hashes {
        if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(VaultOutcome::Invalid("attachment hash is not sha-256 hex"));
        }
    }

    let affected = conn
        .execute(
            "INSERT INTO vault_message (id, user_id, nonce, blob, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(id) DO UPDATE SET \
                 nonce = excluded.nonce, \
                 blob = excluded.blob, \
                 updated_at = datetime('now') \
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
    // Zero rows means the id exists and belongs to someone else — the exact
    // state the WHERE guard exists to refuse. Answer as the permission problem
    // it is rather than a silent success.
    if affected == 0 {
        return Ok(VaultOutcome::Forbidden);
    }

    // Replace the entry's reference set. An edit that dropped a file releases
    // it here; the object itself is collected later only if NOTHING else
    // (message or vault, any user) still references it.
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

// ── POST /v1/vault/delete ────────────────────────────────────────────────────

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

/// Hard-DELETE one entry, owner-bound in the statement itself, releasing its
/// attachment references with it. Deleting an entry that is already gone is a
/// no-op — the state the caller wanted.
///
/// The ref delete is guarded by the same owner predicate (it only fires when
/// the entry row was actually the caller's), so a stranger cannot strip
/// another user's references by replaying an id.
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

// ── POST /v1/read/vault ──────────────────────────────────────────────────────

/// POST `/v1/read/vault` — every entry this account holds, newest first.
///
/// Device-signed and bound to the authenticated user, like the read-cursor
/// read: the rows are undecryptable without the account key, but serving them
/// to a stranger would still hand out entry counts and change rates. An empty
/// vault is an empty list, not an error.
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
