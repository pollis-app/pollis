//! History transfer relay (#1735): two of one account's enrolled devices copy
//! message history between them, through the DS, sealed end to end.
//!
//! The DS's part is deliberately narrow — it is an untrusted relay:
//!
//! - It never sees a key or a row: `req`/`acc` are opaque signed blobs the two
//!   devices verify against each other's device certs, and chunks are sealed
//!   under a key only they derive.
//! - Every call is bound to the SIGNING device (`gate_identity`), never to a
//!   body field, and checked against the party it must be. A stranger, a device
//!   of another account, the wrong party and an expired transfer all get the
//!   same 404, so nothing here confirms that a transfer exists.
//! - Storage is bounded by the database itself (migration 000045): one open
//!   transfer per receiving device, an 8-chunk window above the receiver's ack,
//!   1 MiB per sealed chunk, immutable stored chunks, an ack that only moves
//!   forward. The handlers pre-check the same rules only to answer cleanly.
//! - Requests are budgeted per ACCOUNT, durably (`history_transfer_budget`);
//!   the per-IP limiter does not see accounts.
//!
//! Nudges to `inbox-<user>` are content-free hints (`{type, transfer_id,
//! event}`); clients honour them only from the DS and confirm over this API.
//!
//! Protocol and threat model: `docs/history-transfer-design.md`.

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use libsql::Connection;

use crate::error::{AppError, AuthRejection};
use crate::util::{b64, b64_decode};
use crate::writes::{bad_request, gate_identity_and_parse, ok_response, RawRequest};
use crate::AppState;

pub use pollis_api::history_transfer::*;

/// How long a request waits for the source's approval.
pub const REQUEST_TTL_SECS: i64 = 15 * 60;

/// How long an accepted transfer lives after its last activity, so a phone
/// backgrounded overnight can still resume.
pub const ACCEPTED_TTL_SECS: i64 = 24 * 60 * 60;

/// How long a finished transfer's row is kept, so the other device learns how
/// it ended (declined / cancelled / completed) instead of reading "expired".
pub const ENDED_TTL_SECS: i64 = 60 * 60;

/// Un-acknowledged chunks the DS holds per transfer (also a DB trigger).
pub const WINDOW: i64 = 8;

/// Largest sealed chunk (also a DB CHECK).
pub const MAX_SEALED_BYTES: usize = 1024 * 1024;

/// The HTTP body limit on `chunk/put`: a 1 MiB chunk in base64 plus JSON framing.
pub const PUT_BODY_LIMIT: usize = MAX_SEALED_BYTES / 3 * 4 + 64 * 1024;

/// Largest signed request (`req` carries the receiver's conversation list).
pub const MAX_REQ_BYTES: usize = 256 * 1024;

/// Largest signed acceptance.
pub const MAX_ACC_BYTES: usize = 64 * 1024;

/// Largest signature blob (ML-DSA-44 signatures are 2420 bytes).
pub const MAX_SIG_BYTES: usize = 8 * 1024;

/// Requests one account may file per [`BUDGET_WINDOW_SECS`].
pub const REQUESTS_PER_WINDOW: i64 = 12;

/// The per-account request budget's window.
pub const BUDGET_WINDOW_SECS: i64 = 60 * 60;

/// Open transfers one account may hold at once (one per receiving device is
/// also a DB index; this bounds an account with many devices).
pub const MAX_OPEN_PER_ACCOUNT: i64 = 4;

/// What a transfer call came to. Every refusal that could reveal a transfer's
/// existence is [`Outcome::NotFound`].
#[derive(Debug)]
pub enum Outcome<T> {
    Ok(T),
    /// Success that changed nothing (a repeat of a call that already took
    /// effect): answered like `Ok`, but nobody is nudged.
    Unchanged(T),
    /// Unknown, expired, another account's, or this device is not the party
    /// the call needs. One answer for all of them.
    NotFound,
    /// Malformed input; `&'static str` so no caller input is echoed.
    Invalid(&'static str),
    /// A stored index holds different bytes, or the transfer is not in the
    /// state the call needs.
    Conflict(&'static str),
    /// The window is full; retry after the receiver acknowledges.
    WindowFull,
    /// The per-account budget is spent.
    TooMany,
}

fn respond<B: pollis_api::DsRequest>(outcome: Outcome<B::Response>) -> Response {
    match outcome {
        Outcome::Ok(body) | Outcome::Unchanged(body) => ok_response::<B>(body),
        Outcome::NotFound => {
            (StatusCode::NOT_FOUND, Json(serde_json::json!({ "error": "not found" }))).into_response()
        }
        Outcome::Invalid(why) => bad_request(why),
        Outcome::Conflict(why) => {
            (StatusCode::CONFLICT, Json(serde_json::json!({ "error": why }))).into_response()
        }
        Outcome::WindowFull => {
            (StatusCode::CONFLICT, Json(serde_json::json!({ "error": "window_full" }))).into_response()
        }
        Outcome::TooMany => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": "too many history transfer requests" })),
        )
            .into_response(),
    }
}

/// Signed `(user_id, device_id)` and the parsed body, or the response to send.
/// The no-auth deployment has no device identity, so the feature refuses there.
async fn identify<B: serde::de::DeserializeOwned>(
    state: &AppState,
    req: &RawRequest,
) -> Result<Result<((String, String), B), Response>, AppError> {
    match gate_identity_and_parse::<B>(state, req).await? {
        Ok((Some(identity), body)) => Ok(Ok((identity, body))),
        Ok((None, _)) => Ok(Err(AuthRejection::Forbidden.into_response())),
        Err(resp) => Ok(Err(resp)),
    }
}

/// The nudge's whole body: the transfer and the step, nothing else.
pub fn nudge_payload(transfer_id: &str, event: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "history_transfer",
        "transfer_id": transfer_id,
        "event": event,
    })
}

/// Fire-and-forget, content-free nudge to the account's inbox room.
fn nudge(state: &AppState, user_id: &str, transfer_id: &str, event: &'static str) {
    let state = state.clone();
    let room = format!("inbox-{user_id}");
    let payload = nudge_payload(transfer_id, event);
    tokio::spawn(async move {
        if let Err(e) = crate::broker::room_send_data(&state, &room, &payload).await {
            tracing::debug!("history-transfer nudge failed (non-fatal): {e}");
        }
    });
}

fn id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// One stored transfer row.
#[derive(Debug, Clone)]
struct Row {
    transfer_id: String,
    src: String,
    dst: String,
    via: String,
    state: TransferState,
    req: Vec<u8>,
    req_sig: Vec<u8>,
    acc: Option<Vec<u8>>,
    acc_sig: Option<Vec<u8>>,
    acked_through: i64,
    expires_at: i64,
}

/// The transfer, if it exists for `user_id`, is unexpired and `device_id` is
/// one of its parties. Anything else is `None`, which every caller answers 404.
async fn load_for(
    conn: &Connection,
    transfer_id: &str,
    user_id: &str,
    device_id: &str,
    now: i64,
) -> anyhow::Result<Option<Row>> {
    let mut rows = conn
        .query(
            "SELECT transfer_id, src_device_id, dst_device_id, via, state, req, req_sig, \
                    acc, acc_sig, acked_through, expires_at \
             FROM history_transfer \
             WHERE transfer_id = ?1 AND user_id = ?2 \
               AND (src_device_id = ?3 OR dst_device_id = ?3) AND expires_at > ?4",
            libsql::params![transfer_id, user_id, device_id, now],
        )
        .await?;
    let Some(r) = rows.next().await? else {
        return Ok(None);
    };
    let state_raw: String = r.get(4)?;
    let Some(state) = TransferState::parse(&state_raw) else {
        return Ok(None);
    };
    Ok(Some(Row {
        transfer_id: r.get(0)?,
        src: r.get(1)?,
        dst: r.get(2)?,
        via: r.get(3)?,
        state,
        req: r.get(5)?,
        req_sig: r.get(6)?,
        acc: r.get::<Option<Vec<u8>>>(7)?,
        acc_sig: r.get::<Option<Vec<u8>>>(8)?,
        acked_through: r.get(9)?,
        expires_at: r.get(10)?,
    }))
}

async fn stored_through(conn: &Connection, transfer_id: &str) -> anyhow::Result<i64> {
    let mut rows = conn
        .query(
            "SELECT COALESCE(MAX(idx), -1) FROM history_transfer_chunk WHERE transfer_id = ?1",
            libsql::params![transfer_id],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(r) => r.get(0)?,
        None => -1,
    })
}

/// Delete one transfer's chunks and row.
async fn delete_transfer(conn: &Connection, transfer_id: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM history_transfer_chunk WHERE transfer_id = ?1",
        libsql::params![transfer_id],
    )
    .await?;
    conn.execute(
        "DELETE FROM history_transfer WHERE transfer_id = ?1",
        libsql::params![transfer_id],
    )
    .await?;
    Ok(())
}

/// Move a transfer from one of `from` to an ended state, atomically: the row
/// flips and its chunks go in one transaction, or nothing happens. Returns
/// whether it moved (`false` = it was not in any `from` state any more — a
/// concurrent call got there first).
async fn end_transfer(
    conn: &Connection,
    transfer_id: &str,
    from: &[TransferState],
    ended: TransferState,
    now: i64,
) -> anyhow::Result<bool> {
    let tx = conn.transaction().await?;
    let mut moved = 0;
    for f in from {
        moved += tx
            .execute(
                "UPDATE history_transfer SET state = ?2, updated_at = ?3, expires_at = ?4 \
                 WHERE transfer_id = ?1 AND state = ?5",
                libsql::params![transfer_id, ended.as_str(), now, now + ENDED_TTL_SECS, f.as_str()],
            )
            .await?;
    }
    if moved == 0 {
        tx.rollback().await?;
        return Ok(false);
    }
    tx.execute(
        "DELETE FROM history_transfer_chunk WHERE transfer_id = ?1",
        libsql::params![transfer_id],
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// request
// ---------------------------------------------------------------------------

pub async fn request(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let ((user, device), body) = match identify::<RequestTransferBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let now = crate::util::now_unix() as i64;
    let outcome = apply_request(&conn, &user, &device, &body, now).await?;
    if matches!(outcome, Outcome::Ok(_)) {
        nudge(&state, &user, &body.transfer_id, "requested");
    }
    Ok(respond::<RequestTransferBody>(outcome))
}

/// File a request from `device` (the receiver) to its sibling `src_device_id`.
/// Replaces `device`'s own earlier open transfer, if any.
pub async fn apply_request(
    conn: &Connection,
    user: &str,
    device: &str,
    body: &RequestTransferBody,
    now: i64,
) -> anyhow::Result<Outcome<RequestTransferResponse>> {
    if !id_ok(&body.transfer_id) || !id_ok(&body.src_device_id) {
        return Ok(Outcome::Invalid("bad id"));
    }
    if body.via != "link" && body.via != "sibling" {
        return Ok(Outcome::Invalid("via must be link or sibling"));
    }
    let Ok(req_bytes) = b64_decode(&body.req) else {
        return Ok(Outcome::Invalid("req is not base64"));
    };
    let Ok(sig) = b64_decode(&body.req_sig) else {
        return Ok(Outcome::Invalid("req_sig is not base64"));
    };
    if req_bytes.is_empty() || req_bytes.len() > MAX_REQ_BYTES || sig.is_empty() || sig.len() > MAX_SIG_BYTES {
        return Ok(Outcome::Invalid("req or signature size out of range"));
    }
    if body.src_device_id == device {
        return Ok(Outcome::Invalid("a device cannot copy history from itself"));
    }
    let tx = conn.transaction().await?;
    // The source must be a live, ENROLLED device (it has a signing key) of the
    // SAME account — read inside the transaction that files the request.
    // Another account's device, a revoked one, a half-registered one and an
    // unknown id are one answer.
    {
        let mut rows = tx
            .query(
                "SELECT 1 FROM user_device WHERE user_id = ?1 AND device_id = ?2 \
                 AND revoked_at IS NULL AND mls_signature_pub_pq IS NOT NULL",
                libsql::params![user, body.src_device_id.clone()],
            )
            .await?;
        if rows.next().await?.is_none() {
            drop(rows);
            tx.rollback().await?;
            return Ok(Outcome::NotFound);
        }
    }
    // Durable per-account budget.
    let used: Option<(i64, i64)> = {
        let mut r = tx
            .query(
                "SELECT window_start, used FROM history_transfer_budget WHERE user_id = ?1",
                libsql::params![user],
            )
            .await?;
        match r.next().await? {
            Some(row) => Some((row.get(0)?, row.get(1)?)),
            None => None,
        }
    };
    match used {
        Some((start, n)) if now - start < BUDGET_WINDOW_SECS => {
            if n >= REQUESTS_PER_WINDOW {
                tx.rollback().await?;
                return Ok(Outcome::TooMany);
            }
            tx.execute(
                "UPDATE history_transfer_budget SET used = used + 1 WHERE user_id = ?1",
                libsql::params![user],
            )
            .await?;
        }
        _ => {
            tx.execute(
                "INSERT INTO history_transfer_budget (user_id, window_start, used) VALUES (?1, ?2, 1) \
                 ON CONFLICT(user_id) DO UPDATE SET window_start = ?2, used = 1",
                libsql::params![user, now],
            )
            .await?;
        }
    }
    // This receiver's earlier transfer (open or ended) is replaced; an orphan
    // from a crashed or abandoned attempt must not block a new one.
    {
        let mut r = tx
            .query(
                "SELECT transfer_id FROM history_transfer WHERE dst_device_id = ?1 AND user_id = ?2",
                libsql::params![device, user],
            )
            .await?;
        let mut old = Vec::new();
        while let Some(row) = r.next().await? {
            old.push(row.get::<String>(0)?);
        }
        drop(r);
        for id in old {
            delete_transfer(&tx, &id).await?;
        }
    }
    let open: i64 = {
        let mut r = tx
            .query(
                "SELECT COUNT(*) FROM history_transfer \
                 WHERE user_id = ?1 AND state IN ('requested', 'accepted') AND expires_at > ?2",
                libsql::params![user, now],
            )
            .await?;
        match r.next().await? {
            Some(row) => row.get(0)?,
            None => 0,
        }
    };
    if open >= MAX_OPEN_PER_ACCOUNT {
        tx.rollback().await?;
        return Ok(Outcome::TooMany);
    }
    let expires_at = now + REQUEST_TTL_SECS;
    let inserted = tx
        .execute(
            "INSERT INTO history_transfer \
               (transfer_id, user_id, src_device_id, dst_device_id, via, state, req, req_sig, \
                created_at, updated_at, expires_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, 'requested', ?6, ?7, ?8, ?8, ?9) \
             ON CONFLICT(transfer_id) DO NOTHING",
            libsql::params![
                body.transfer_id.clone(),
                user,
                body.src_device_id.clone(),
                device,
                body.via.clone(),
                req_bytes,
                sig,
                now,
                expires_at
            ],
        )
        .await?;
    // An id another transfer (perhaps another account's) holds: the same answer
    // as an unknown source, so the refusal confirms nothing about it.
    if inserted == 0 {
        tx.rollback().await?;
        return Ok(Outcome::NotFound);
    }
    tx.commit().await?;
    Ok(Outcome::Ok(RequestTransferResponse { expires_at }))
}

// ---------------------------------------------------------------------------
// pending / status
// ---------------------------------------------------------------------------

pub async fn pending(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let ((user, device), _body) = match identify::<PendingTransfersBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let now = crate::util::now_unix() as i64;
    Ok(respond::<PendingTransfersBody>(apply_pending(&conn, &user, &device, now).await?))
}

/// The unexpired requests addressed to `device` as the source.
pub async fn apply_pending(
    conn: &Connection,
    user: &str,
    device: &str,
    now: i64,
) -> anyhow::Result<Outcome<PendingTransfersResponse>> {
    let mut rows = conn
        .query(
            "SELECT transfer_id, dst_device_id, via, req, req_sig, created_at, expires_at \
             FROM history_transfer \
             WHERE user_id = ?1 AND src_device_id = ?2 AND state = 'requested' AND expires_at > ?3 \
             ORDER BY created_at, transfer_id",
            libsql::params![user, device, now],
        )
        .await?;
    let mut requests = Vec::new();
    while let Some(r) = rows.next().await? {
        requests.push(PendingTransfer {
            transfer_id: r.get(0)?,
            dst_device_id: r.get(1)?,
            via: r.get(2)?,
            req: b64(&r.get::<Vec<u8>>(3)?),
            req_sig: b64(&r.get::<Vec<u8>>(4)?),
            created_at: r.get(5)?,
            expires_at: r.get(6)?,
        });
    }
    Ok(Outcome::Ok(PendingTransfersResponse { requests }))
}

pub async fn status(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let ((user, device), body) = match identify::<TransferStatusBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let now = crate::util::now_unix() as i64;
    Ok(respond::<TransferStatusBody>(apply_status(&conn, &user, &device, &body, now).await?))
}

pub async fn apply_status(
    conn: &Connection,
    user: &str,
    device: &str,
    body: &TransferStatusBody,
    now: i64,
) -> anyhow::Result<Outcome<TransferStatusResponse>> {
    let Some(row) = load_for(conn, &body.transfer_id, user, device, now).await? else {
        return Ok(Outcome::NotFound);
    };
    let stored = stored_through(conn, &row.transfer_id).await?;
    Ok(Outcome::Ok(TransferStatusResponse {
        transfer_id: row.transfer_id,
        src_device_id: row.src,
        dst_device_id: row.dst,
        via: row.via,
        state: row.state,
        req: b64(&row.req),
        req_sig: b64(&row.req_sig),
        acc: row.acc.as_deref().map(b64),
        acc_sig: row.acc_sig.as_deref().map(b64),
        acked_through: row.acked_through,
        stored_through: stored,
        expires_at: row.expires_at,
    }))
}

// ---------------------------------------------------------------------------
// accept / decline / cancel / complete
// ---------------------------------------------------------------------------

pub async fn accept(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let ((user, device), body) = match identify::<AcceptTransferBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let now = crate::util::now_unix() as i64;
    let outcome = apply_accept(&conn, &user, &device, &body, now).await?;
    if matches!(outcome, Outcome::Ok(_)) {
        nudge(&state, &user, &body.transfer_id, "accepted");
    }
    Ok(respond::<AcceptTransferBody>(outcome))
}

/// The source accepts. Only the source; only a requested (or, for a retry of
/// the same bytes, already accepted) transfer.
pub async fn apply_accept(
    conn: &Connection,
    user: &str,
    device: &str,
    body: &AcceptTransferBody,
    now: i64,
) -> anyhow::Result<Outcome<AcceptTransferResponse>> {
    let (Ok(acc), Ok(acc_sig)) = (b64_decode(&body.acc), b64_decode(&body.acc_sig)) else {
        return Ok(Outcome::Invalid("acc is not base64"));
    };
    if acc.is_empty() || acc.len() > MAX_ACC_BYTES || acc_sig.is_empty() || acc_sig.len() > MAX_SIG_BYTES {
        return Ok(Outcome::Invalid("acc or signature size out of range"));
    }
    let Some(row) = load_for(conn, &body.transfer_id, user, device, now).await? else {
        return Ok(Outcome::NotFound);
    };
    if row.src != device {
        return Ok(Outcome::NotFound);
    }
    match row.state {
        TransferState::Requested => {
            let expires_at = now + ACCEPTED_TTL_SECS;
            let n = conn
                .execute(
                    "UPDATE history_transfer SET state = 'accepted', acc = ?2, acc_sig = ?3, \
                         updated_at = ?4, expires_at = ?5 \
                     WHERE transfer_id = ?1 AND state = 'requested'",
                    libsql::params![body.transfer_id.clone(), acc, acc_sig, now, expires_at],
                )
                .await?;
            if n == 0 {
                return Ok(Outcome::Conflict("not awaiting approval"));
            }
            Ok(Outcome::Ok(AcceptTransferResponse { expires_at }))
        }
        // The same acceptance again (a resend after a lost answer, possibly
        // re-signed — ML-DSA signatures are randomized): a no-op, the stored
        // signature stands.
        TransferState::Accepted if row.acc.as_deref() == Some(acc.as_slice()) => {
            Ok(Outcome::Unchanged(AcceptTransferResponse { expires_at: row.expires_at }))
        }
        _ => Ok(Outcome::Conflict("not awaiting approval")),
    }
}

pub async fn decline(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let ((user, device), body) = match identify::<DeclineTransferBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let now = crate::util::now_unix() as i64;
    let outcome = apply_decline(&conn, &user, &device, &body, now).await?;
    if matches!(outcome, Outcome::Ok(_)) {
        nudge(&state, &user, &body.transfer_id, "declined");
    }
    Ok(respond::<DeclineTransferBody>(outcome))
}

pub async fn apply_decline(
    conn: &Connection,
    user: &str,
    device: &str,
    body: &DeclineTransferBody,
    now: i64,
) -> anyhow::Result<Outcome<pollis_api::StatusOk>> {
    let Some(row) = load_for(conn, &body.transfer_id, user, device, now).await? else {
        return Ok(Outcome::NotFound);
    };
    if row.src != device {
        return Ok(Outcome::NotFound);
    }
    match row.state {
        TransferState::Requested => {
            // Guarded on `requested`: an accept that landed since the read wins.
            if end_transfer(conn, &row.transfer_id, &[TransferState::Requested], TransferState::Declined, now).await? {
                return Ok(Outcome::Ok(pollis_api::StatusOk::Ok));
            }
            Ok(Outcome::Conflict("not awaiting approval"))
        }
        TransferState::Declined => Ok(Outcome::Unchanged(pollis_api::StatusOk::Ok)),
        _ => Ok(Outcome::Conflict("not awaiting approval")),
    }
}

pub async fn cancel(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let ((user, device), body) = match identify::<CancelTransferBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let now = crate::util::now_unix() as i64;
    let outcome = apply_cancel(&conn, &user, &device, &body, now).await?;
    if matches!(outcome, Outcome::Ok(_)) {
        nudge(&state, &user, &body.transfer_id, "cancelled");
    }
    Ok(respond::<CancelTransferBody>(outcome))
}

/// Either party ends an open transfer. Cancelling an ended one is a no-op.
pub async fn apply_cancel(
    conn: &Connection,
    user: &str,
    device: &str,
    body: &CancelTransferBody,
    now: i64,
) -> anyhow::Result<Outcome<pollis_api::StatusOk>> {
    let Some(row) = load_for(conn, &body.transfer_id, user, device, now).await? else {
        return Ok(Outcome::NotFound);
    };
    if row.state.is_open()
        && end_transfer(
            conn,
            &row.transfer_id,
            &[TransferState::Requested, TransferState::Accepted],
            TransferState::Cancelled,
            now,
        )
        .await?
    {
        return Ok(Outcome::Ok(pollis_api::StatusOk::Ok));
    }
    Ok(Outcome::Unchanged(pollis_api::StatusOk::Ok))
}

pub async fn complete(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let ((user, device), body) = match identify::<CompleteTransferBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let now = crate::util::now_unix() as i64;
    let outcome = apply_complete(&conn, &user, &device, &body, now).await?;
    if matches!(outcome, Outcome::Ok(_)) {
        nudge(&state, &user, &body.transfer_id, "completed");
    }
    Ok(respond::<CompleteTransferBody>(outcome))
}

/// The receiver reports the final chunk imported. Only the receiver; only an
/// accepted transfer (idempotent once completed).
pub async fn apply_complete(
    conn: &Connection,
    user: &str,
    device: &str,
    body: &CompleteTransferBody,
    now: i64,
) -> anyhow::Result<Outcome<pollis_api::StatusOk>> {
    let Some(row) = load_for(conn, &body.transfer_id, user, device, now).await? else {
        return Ok(Outcome::NotFound);
    };
    if row.dst != device {
        return Ok(Outcome::NotFound);
    }
    match row.state {
        TransferState::Accepted => {
            if end_transfer(conn, &row.transfer_id, &[TransferState::Accepted], TransferState::Completed, now).await? {
                return Ok(Outcome::Ok(pollis_api::StatusOk::Ok));
            }
            Ok(Outcome::Conflict("not transferring"))
        }
        TransferState::Completed => Ok(Outcome::Unchanged(pollis_api::StatusOk::Ok)),
        _ => Ok(Outcome::Conflict("not transferring")),
    }
}

// ---------------------------------------------------------------------------
// chunks
// ---------------------------------------------------------------------------

pub async fn put_chunk(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let ((user, device), body) = match identify::<PutChunkBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let now = crate::util::now_unix() as i64;
    let outcome = apply_put_chunk(&conn, &user, &device, &body, now).await?;
    if matches!(outcome, Outcome::Ok(_)) {
        nudge(&state, &user, &body.transfer_id, "chunk");
    }
    Ok(respond::<PutChunkBody>(outcome))
}

/// The source uploads chunk `idx`. Re-sending the same bytes is a no-op (as is
/// re-sending an index the receiver already acknowledged); different bytes at
/// a stored index are a 409 and the source must abort.
pub async fn apply_put_chunk(
    conn: &Connection,
    user: &str,
    device: &str,
    body: &PutChunkBody,
    now: i64,
) -> anyhow::Result<Outcome<PutChunkResponse>> {
    apply_put_chunk_interleaved(conn, user, device, body, now, || async {}).await
}

/// [`apply_put_chunk`] with `between` run after the transfer is read and before
/// the chunk is inserted — the window a concurrent ack can land in. Tests use
/// it to force that interleaving deterministically.
#[doc(hidden)]
pub async fn apply_put_chunk_interleaved<F, Fut>(
    conn: &Connection,
    user: &str,
    device: &str,
    body: &PutChunkBody,
    now: i64,
    between: F,
) -> anyhow::Result<Outcome<PutChunkResponse>>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    if body.idx < 0 || body.idx > u32::MAX as i64 {
        return Ok(Outcome::Invalid("idx out of range"));
    }
    let Ok(sealed) = b64_decode(&body.sealed) else {
        return Ok(Outcome::Invalid("sealed is not base64"));
    };
    if sealed.is_empty() || sealed.len() > MAX_SEALED_BYTES {
        return Ok(Outcome::Invalid("sealed chunk size out of range"));
    }
    let Some(row) = load_for(conn, &body.transfer_id, user, device, now).await? else {
        return Ok(Outcome::NotFound);
    };
    if row.src != device {
        return Ok(Outcome::NotFound);
    }
    if row.state != TransferState::Accepted {
        return Ok(Outcome::Conflict("not transferring"));
    }
    if body.idx <= row.acked_through {
        return Ok(Outcome::Unchanged(PutChunkResponse { acked_through: row.acked_through }));
    }
    between().await;
    let existing: Option<Vec<u8>> = {
        let mut r = conn
            .query(
                "SELECT sealed FROM history_transfer_chunk WHERE transfer_id = ?1 AND idx = ?2",
                libsql::params![body.transfer_id.clone(), body.idx],
            )
            .await?;
        match r.next().await? {
            Some(row) => Some(row.get(0)?),
            None => None,
        }
    };
    if let Some(existing) = existing {
        if existing == sealed {
            return Ok(Outcome::Unchanged(PutChunkResponse { acked_through: row.acked_through }));
        }
        return Ok(Outcome::Conflict("chunk index holds different bytes"));
    }
    if body.idx > row.acked_through + WINDOW {
        return Ok(Outcome::WindowFull);
    }
    let stored = stored_through(conn, &row.transfer_id).await?;
    if body.idx != row.acked_through + 1 && body.idx != stored + 1 {
        return Ok(Outcome::Conflict("chunk index not contiguous"));
    }
    // The trigger re-checks state, window and contiguity against the row as
    // it is NOW. The one race that is not the source's fault: the receiver
    // acknowledged this very index (a re-put after a lost answer) between our
    // read and the insert. Re-read: an index at or below the ack is done.
    let inserted = conn
        .execute(
            "INSERT INTO history_transfer_chunk (transfer_id, idx, sealed, created_at) VALUES (?1, ?2, ?3, ?4)",
            libsql::params![body.transfer_id.clone(), body.idx, sealed.clone(), now],
        )
        .await;
    if let Err(e) = inserted {
        let msg = e.to_string();
        if !(msg.contains("history_transfer_chunk:") || msg.contains("UNIQUE") || msg.contains("constraint")) {
            return Err(e.into());
        }
        let Some(again) = load_for(conn, &body.transfer_id, user, device, now).await? else {
            return Ok(Outcome::NotFound);
        };
        if body.idx <= again.acked_through {
            return Ok(Outcome::Unchanged(PutChunkResponse { acked_through: again.acked_through }));
        }
        let mut r = conn
            .query(
                "SELECT sealed FROM history_transfer_chunk WHERE transfer_id = ?1 AND idx = ?2",
                libsql::params![body.transfer_id.clone(), body.idx],
            )
            .await?;
        if let Some(existing) = r.next().await? {
            if existing.get::<Vec<u8>>(0)? == sealed {
                return Ok(Outcome::Unchanged(PutChunkResponse { acked_through: again.acked_through }));
            }
            return Ok(Outcome::Conflict("chunk index holds different bytes"));
        }
        if again.state != TransferState::Accepted {
            return Ok(Outcome::Conflict("not transferring"));
        }
        return Ok(Outcome::Conflict("chunk refused"));
    }
    conn.execute(
        "UPDATE history_transfer SET updated_at = ?2, expires_at = ?3 WHERE transfer_id = ?1 AND state = 'accepted'",
        libsql::params![body.transfer_id.clone(), now, now + ACCEPTED_TTL_SECS],
    )
    .await?;
    Ok(Outcome::Ok(PutChunkResponse { acked_through: row.acked_through }))
}

pub async fn get_chunk(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let ((user, device), body) = match identify::<GetChunkBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let now = crate::util::now_unix() as i64;
    Ok(respond::<GetChunkBody>(apply_get_chunk(&conn, &user, &device, &body, now).await?))
}

/// The receiver reads chunk `idx`, if the source has uploaded it.
pub async fn apply_get_chunk(
    conn: &Connection,
    user: &str,
    device: &str,
    body: &GetChunkBody,
    now: i64,
) -> anyhow::Result<Outcome<GetChunkResponse>> {
    let Some(row) = load_for(conn, &body.transfer_id, user, device, now).await? else {
        return Ok(Outcome::NotFound);
    };
    if row.dst != device {
        return Ok(Outcome::NotFound);
    }
    if row.state != TransferState::Accepted {
        return Ok(Outcome::Conflict("not transferring"));
    }
    let mut r = conn
        .query(
            "SELECT sealed FROM history_transfer_chunk WHERE transfer_id = ?1 AND idx = ?2",
            libsql::params![body.transfer_id.clone(), body.idx],
        )
        .await?;
    let sealed = match r.next().await? {
        Some(row) => Some(b64(&row.get::<Vec<u8>>(0)?)),
        None => None,
    };
    Ok(Outcome::Ok(GetChunkResponse { sealed, acked_through: row.acked_through }))
}

pub async fn ack_chunk(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let ((user, device), body) = match identify::<AckChunkBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let now = crate::util::now_unix() as i64;
    let outcome = apply_ack_chunk(&conn, &user, &device, &body, now).await?;
    if matches!(outcome, Outcome::Ok(_)) {
        nudge(&state, &user, &body.transfer_id, "acked");
    }
    Ok(respond::<AckChunkBody>(outcome))
}

/// The receiver acknowledges chunk `idx` (imported). Exactly the next index
/// moves the ack and deletes the chunk; an index already acknowledged is a
/// no-op; anything else is a 409.
pub async fn apply_ack_chunk(
    conn: &Connection,
    user: &str,
    device: &str,
    body: &AckChunkBody,
    now: i64,
) -> anyhow::Result<Outcome<AckChunkResponse>> {
    let Some(row) = load_for(conn, &body.transfer_id, user, device, now).await? else {
        return Ok(Outcome::NotFound);
    };
    if row.dst != device {
        return Ok(Outcome::NotFound);
    }
    if row.state != TransferState::Accepted {
        return Ok(Outcome::Conflict("not transferring"));
    }
    if body.idx <= row.acked_through {
        return Ok(Outcome::Unchanged(AckChunkResponse { acked_through: row.acked_through }));
    }
    if body.idx != row.acked_through + 1 {
        return Ok(Outcome::Conflict("ack out of order"));
    }
    let tx = conn.transaction().await?;
    let removed = tx
        .execute(
            "DELETE FROM history_transfer_chunk WHERE transfer_id = ?1 AND idx = ?2",
            libsql::params![body.transfer_id.clone(), body.idx],
        )
        .await?;
    if removed == 0 {
        tx.rollback().await?;
        return Ok(Outcome::Conflict("chunk not stored"));
    }
    let moved = tx
        .execute(
            "UPDATE history_transfer SET acked_through = ?2, updated_at = ?3, expires_at = ?4 \
             WHERE transfer_id = ?1 AND acked_through = ?2 - 1 AND state = 'accepted'",
            libsql::params![body.transfer_id.clone(), body.idx, now, now + ACCEPTED_TTL_SECS],
        )
        .await?;
    // A concurrent ack or an end got there first: nothing changes here.
    if moved != 1 {
        tx.rollback().await?;
        return Ok(Outcome::Conflict("ack raced another change"));
    }
    tx.commit().await?;
    Ok(Outcome::Ok(AckChunkResponse { acked_through: body.idx }))
}

// ---------------------------------------------------------------------------
// lifecycle: expiry sweep and purges
// ---------------------------------------------------------------------------

/// Delete every transfer (and its chunks) past `expires_at`, and spent budget
/// windows. Runs in the GC tick. Returns the transfers removed.
pub async fn sweep_expired(conn: &Connection, now: i64) -> anyhow::Result<u64> {
    conn.execute(
        "DELETE FROM history_transfer_chunk WHERE transfer_id IN \
           (SELECT transfer_id FROM history_transfer WHERE expires_at <= ?1)",
        libsql::params![now],
    )
    .await?;
    let n = conn
        .execute("DELETE FROM history_transfer WHERE expires_at <= ?1", libsql::params![now])
        .await?;
    conn.execute(
        "DELETE FROM history_transfer_budget WHERE window_start <= ?1",
        libsql::params![now - BUDGET_WINDOW_SECS],
    )
    .await?;
    Ok(n)
}

/// Account teardown: every transfer of `user_id`, its chunks and its budget row.
pub async fn purge_user(conn: &Connection, user_id: &str) -> anyhow::Result<()> {
    purge_transfers_of_user(conn, user_id).await?;
    conn.execute("DELETE FROM history_transfer_budget WHERE user_id = ?1", libsql::params![user_id])
        .await?;
    Ok(())
}

/// Identity reset: every transfer of `user_id` and its chunks. The reset
/// removes the account's other devices, so no transfer can still involve two
/// live devices; the request budget is kept (a reset is not a way around it).
pub async fn purge_transfers_of_user(conn: &Connection, user_id: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM history_transfer_chunk WHERE transfer_id IN \
           (SELECT transfer_id FROM history_transfer WHERE user_id = ?1)",
        libsql::params![user_id],
    )
    .await?;
    conn.execute("DELETE FROM history_transfer WHERE user_id = ?1", libsql::params![user_id])
        .await?;
    Ok(())
}

/// Revocation / logout of one device: every transfer it is a party to.
pub async fn purge_device(conn: &Connection, user_id: &str, device_id: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM history_transfer_chunk WHERE transfer_id IN \
           (SELECT transfer_id FROM history_transfer \
            WHERE user_id = ?1 AND (src_device_id = ?2 OR dst_device_id = ?2))",
        libsql::params![user_id, device_id],
    )
    .await?;
    conn.execute(
        "DELETE FROM history_transfer WHERE user_id = ?1 AND (src_device_id = ?2 OR dst_device_id = ?2)",
        libsql::params![user_id, device_id],
    )
    .await?;
    Ok(())
}
