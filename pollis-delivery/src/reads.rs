//! The MLS control plane's READ half (#987).
//!
//! Until this module the client answered these questions itself, with a
//! whole-database read token baked into the shipped binary. Moving them here
//! removes that credential — and, more importantly, restores two guarantees that
//! a client holding its own connection could keep only by accident:
//!
//! 1. **One snapshot per answer.** [`conversation_state`] reads the published
//!    GroupInfo, the pending-Welcome flag, the lineage heads and the commit batch
//!    inside ONE transaction. Split across two round trips, a device can observe
//!    a GroupInfo without the Welcome that `submit_commit` wrote atomically
//!    beside it, external-join while its own Welcome is inbound, and strand
//!    itself permanently. See `pollis_api::reads` for the full argument.
//! 2. **No torn commit batch.** The batch is verified contiguous
//!    ([`pollis_api::reads::check_contiguous`]) before it is served, because the
//!    client's response to a hole is to DELETE its MLS crypto state.
//!    `pruned_below` is what keeps a legitimate retention prune (#539)
//!    distinguishable from a torn read.
//!
//! Both endpoints are POST with signed bodies, never GET: the canonical signing
//! message covers the path WITHOUT its query string, so a signed
//! `GET …?since=N` carries unauthenticated parameters — the #681 shape, where an
//! unauthenticated `GET /v1/commits?user_id&device_id` was a remote commit-log
//! wipe. GETs are also exempt from rate limiting and absent from
//! `pollis_api::ENDPOINTS`, so a GET read endpoint would be invisible to the
//! route-coverage test that exists to catch exactly this omission.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, Uri},
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use libsql::Connection;

use crate::auth;
use crate::error::{AppError, AuthRejection};
use crate::writes::{bad_request, is_member, ok_response};
use crate::AppState;

pub use pollis_api::reads::*;

/// Cap on how many conversations one [`conversation_state`] call may ask about.
///
/// The batch exists so a 20-group cold launch is one round trip instead of
/// twenty (#987 §5); it is not an invitation to scan the whole log DB in one
/// request. A caller with more conversations than this pages — every entry is
/// independent, so paging costs correctness nothing (unlike splitting ONE
/// conversation's snapshot, which is the thing this endpoint exists to prevent).
pub const MAX_QUERIES: usize = 64;

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The authenticated `(user_id, device_id)` a read is answered for.
///
/// Both halves come from the VERIFIED signature when auth is on — never from the
/// body — because the Welcome lookup is device-scoped and a body-supplied
/// `device_id` would let any authenticated user drain another device's Welcome
/// queue. On the no-auth path they come from the body, which is what that path
/// means.
/// `pub` so `tests/conversation_snapshot.rs` can drive [`snapshot`] directly.
/// Constructing one is not authority — the handler still derives both halves
/// from a verified signature; this type only carries the result.
pub struct Reader {
    pub user_id: String,
    pub device_id: String,
}

/// [`reader`] for the sibling read modules — the reads that are scoped to a
/// DEVICE, not just a user (the key-package pool this device owns, the Welcome
/// queue addressed to it). Same gate, exported so there is one of it.
// The `Err` IS the response returned to the client, as elsewhere in this crate.
#[allow(clippy::result_large_err)]
pub(crate) async fn reader_identity(
    state: &AppState,
    headers: &HeaderMap,
    method: &Method,
    uri: &Uri,
    body: &Bytes,
) -> Result<Result<Reader, Response>, AppError> {
    reader(state, headers, method, uri, body, None, None).await
}

/// Authenticate a read and resolve the reader identity.
///
/// Mirrors `report_commit_since`'s gate: `verify_request_identity_cached` when
/// auth is enforced (both halves signature-bound), body fields when it is not.
// The `Err` IS the response returned to the client, as elsewhere in this crate.
#[allow(clippy::result_large_err)]
async fn reader(
    state: &AppState,
    headers: &HeaderMap,
    method: &Method,
    uri: &Uri,
    body: &Bytes,
    body_user_id: Option<&str>,
    body_device_id: Option<&str>,
) -> Result<Result<Reader, Response>, AppError> {
    if !state.require_auth {
        return Ok(match (body_user_id, body_device_id) {
            (Some(u), Some(d)) if !u.is_empty() && !d.is_empty() => Ok(Reader {
                user_id: u.to_string(),
                device_id: d.to_string(),
            }),
            _ => Err(bad_request(
                "user_id and device_id required when auth is disabled",
            )),
        });
    }
    let conn = state.db.conn().await?;
    match auth::verify_request_identity_cached(
        &state.device_keys,
        &conn,
        headers,
        method.as_str(),
        uri.path(),
        body,
        crate::util::now_unix() as i64,
    )
    .await
    {
        Ok((user_id, device_id)) => Ok(Ok(Reader { user_id, device_id })),
        Err(rej) => Ok(Err(rej.into_response())),
    }
}

/// Authenticate a read that needs only the USER half of the identity.
///
/// The device-scoped reads use [`reader`]; this is for the many that answer for
/// a user regardless of which of their devices asked. Same gate, same
/// no-auth-path contract: with auth on, the id comes from the verified
/// signature and a body that names a DIFFERENT user is a 403 rather than a
/// silently-ignored field.
// The `Err` IS the response returned to the client, as elsewhere in this crate.
#[allow(clippy::result_large_err)]
pub(crate) async fn authed_user(
    state: &AppState,
    headers: &HeaderMap,
    method: &Method,
    uri: &Uri,
    body: &Bytes,
    body_user_id: Option<&str>,
) -> Result<Result<String, Response>, AppError> {
    if !state.require_auth {
        return Ok(match body_user_id {
            Some(u) if !u.is_empty() => Ok(u.to_string()),
            _ => Err(bad_request("user_id required when auth is disabled")),
        });
    }
    let conn = state.db.conn().await?;
    match auth::verify_request_cached(
        &state.device_keys,
        &conn,
        headers,
        method.as_str(),
        uri.path(),
        body,
        crate::util::now_unix() as i64,
    )
    .await
    {
        Ok(user_id) => {
            if let Some(claimed) = body_user_id {
                if !claimed.is_empty() && claimed != user_id {
                    return Ok(Err(AuthRejection::Forbidden.into_response()));
                }
            }
            Ok(Ok(user_id))
        }
        Err(rej) => Ok(Err(rej.into_response())),
    }
}

// ── POST /v1/mls/conversation-state ──────────────────────────────────────────

/// POST `/v1/mls/conversation-state` — every input the client's MLS replay and
/// join-recovery paths need, batched across conversations, each conversation
/// answered from ONE log-DB read transaction.
///
/// Authorization is per entry (`authorized: false`), not per request: a sweep
/// naming one conversation the caller has since been removed from must not blind
/// it to the other nineteen. Non-membership therefore looks exactly like "no
/// state", which is also what it means — a removed member has no business
/// learning a group's head epoch.
pub async fn conversation_state(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let parsed: ConversationStateBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match reader(
        &state,
        &headers,
        &method,
        &uri,
        &body,
        parsed.user_id.as_deref(),
        parsed.device_id.as_deref(),
    )
    .await?
    {
        Ok(r) => r,
        Err(resp) => return Ok(resp),
    };
    if parsed.queries.len() > MAX_QUERIES {
        return Ok(bad_request("too many queries"));
    }

    let main = state.db.conn().await?;
    let log = state.log_db.conn().await?;

    let mut states = Vec::with_capacity(parsed.queries.len());
    for q in &parsed.queries {
        // Membership is the authorization gate AND one of the answers. It is read
        // once, on the main DB, and reported as `is_member` so the client's
        // fail-CLOSED external-join gate has an input rather than a guess.
        let member = is_member(&main, &q.conversation_id, &who.user_id).await;
        let authorized = matches!(member, Ok(true));
        if !authorized {
            states.push(ConversationState {
                conversation_id: q.conversation_id.clone(),
                authorized: false,
                group_info: None,
                welcome_pending: false,
                head: 0,
                head_generation: 0,
                generation: q.generation.unwrap_or(0),
                commits: Vec::new(),
                pruned_below: None,
                commit_at_epoch: None,
                // A confirmed non-member is `Some(false)`; a FAILED read is
                // `None`. The client fails closed on both, but only one of them
                // is a fact, and conflating them would hide a sick main DB
                // behind a stream of plausible "you were removed" verdicts.
                is_member: member.as_ref().ok().copied(),
                device_registered: None,
                added_identities: Vec::new(),
            });
            continue;
        }
        let registered = device_registered(&main, &who.user_id, &who.device_id)
            .await
            .ok();
        let mut st = snapshot(&log, q, &who, registered).await?;
        // Cross-signing inputs for the adds this batch carries, fetched ONCE per
        // batch instead of once per add-carrying commit. Deliberately after the
        // log-DB transaction closed: these rows live on the MAIN DB, so they were
        // never part of that snapshot and pretending otherwise would be a lie in
        // the type. Nothing pairs them with a log-DB fact — the client's
        // verification is a signature check over the cert chain, not a race.
        st.added_identities = added_identities(&main, &st.commits).await?;
        states.push(st);
    }

    Ok(ok_response::<ConversationStateBody>(
        ConversationStateResponse { states },
    ))
}

/// One conversation's log-DB snapshot, read inside ONE transaction.
///
/// The transaction is the entire point. `submit_commit` writes the commit, its
/// GroupInfo and its Welcomes in one `IMMEDIATE` transaction, so a reader inside
/// a single transaction that observes the GroupInfo necessarily observes the
/// Welcome — which is what makes the client's `ExternalJoin ⟹ !welcome_pending`
/// gate meaningful rather than vacuous. `head`, `head_generation` and `commits`
/// share it for the same reason from the other direction: the client treats a
/// commit batch that skips an epoch as a permanent gap and deletes its MLS state.
///
/// Deliberately NOT modelled on today's `GET /v1/commits/:conversation_id`
/// handler, which runs three separate queries on a bare connection. That handler
/// keeps its behaviour (old clients, operator debugging); extending its shape
/// onto the hot join path is the bug this avoids.
///
/// `pub` for `tests/conversation_snapshot.rs`, which asserts both properties
/// above directly — the atomicity one under concurrency, where only a caller
/// racing a writer can observe a tear at all (#987 P9/P10).
pub async fn snapshot(
    log: &Connection,
    q: &ConversationStateQuery,
    who: &Reader,
    device_registered: Option<bool>,
) -> Result<ConversationState, AppError> {
    let tx = log.transaction().await?;

    let head_generation = crate::commit::head_generation(&tx, &q.conversation_id).await?;
    let generation = q.generation.unwrap_or(head_generation);
    let head = crate::commit::head_epoch_in(&tx, &q.conversation_id, generation).await?;

    let group_info = read_group_info(&tx, &q.conversation_id, q.want_group_info_blob).await?;

    // Read AFTER the GroupInfo, inside the same transaction — the ordering the
    // client's source comment demands ("Welcome check evaluated at or after the
    // GroupInfo read"). Inside one transaction the two are the same instant
    // anyway; the order is kept so the code still reads as the argument it is.
    let welcome_pending =
        welcome_pending(&tx, &q.conversation_id, &who.user_id, &who.device_id).await?;

    let pruned_below = min_epoch_in(&tx, &q.conversation_id, generation).await?;

    let commits = match q.since {
        Some(since) => {
            crate::commit::fetch_commits(&tx, &q.conversation_id, generation, since).await?
        }
        None => Vec::new(),
    };

    let commit_at_epoch = match q.commit_at_epoch {
        Some(epoch) => commit_at(&tx, &q.conversation_id, generation, epoch)
            .await?
            .map(|b| b64(&b)),
        None => None,
    };

    tx.commit().await?;

    // Refuse rather than serve a hole. The client's handling of a
    // non-contiguous batch is `forget_local_mls_group_at` — it deletes the
    // device's MLS crypto state — so a batch we are not certain about must
    // surface as an error the client propagates, not as data it acts on.
    if let Err(hole) = check_contiguous(&commits) {
        return Err(AppError(anyhow::anyhow!(
            "commit log for {} generation {generation} has a hole: expected epoch {}, found {}",
            q.conversation_id,
            hole.expected,
            hole.found
        )));
    }

    Ok(ConversationState {
        conversation_id: q.conversation_id.clone(),
        authorized: true,
        group_info,
        welcome_pending,
        head,
        head_generation,
        generation,
        commits,
        pruned_below,
        commit_at_epoch,
        is_member: Some(true),
        device_registered,
        // Filled by the caller, from the MAIN DB, after this transaction closes.
        added_identities: Vec::new(),
    })
}

/// The published GroupInfo's `(generation, epoch)` head, plus its bytes when the
/// caller asked for them. The blob carries the whole ratchet tree — hundreds of
/// KB for a large roster — so the durability backstop, which only compares heads,
/// does not pay for it.
async fn read_group_info(
    tx: &Connection,
    conversation_id: &str,
    want_blob: bool,
) -> Result<Option<GroupInfoState>, AppError> {
    let sql = if want_blob {
        "SELECT generation, epoch, group_info FROM mls_group_info WHERE conversation_id = ?1"
    } else {
        "SELECT generation, epoch, NULL FROM mls_group_info WHERE conversation_id = ?1"
    };
    let mut rows = tx
        .query(sql, libsql::params![conversation_id.to_string()])
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let blob: Option<Vec<u8>> = if want_blob { row.get(2)? } else { None };
    Ok(Some(GroupInfoState {
        generation: row.get(0)?,
        epoch: row.get(1)?,
        group_info: blob.as_deref().map(b64),
    }))
}

/// Whether an undelivered Welcome for THIS user+device targets the conversation.
///
/// Mirrors `poll_mls_welcomes_inner`'s selection exactly, legacy
/// `recipient_device_id IS NULL` rows included — the two must agree, or the gate
/// suppresses an external join for a Welcome the drain will never hand back.
async fn welcome_pending(
    tx: &Connection,
    conversation_id: &str,
    user_id: &str,
    device_id: &str,
) -> Result<bool, AppError> {
    let mut rows = tx
        .query(
            "SELECT 1 FROM mls_welcome \
             WHERE conversation_id = ?1 AND recipient_id = ?2 AND delivered = 0 \
             AND (recipient_device_id = ?3 OR recipient_device_id IS NULL) \
             LIMIT 1",
            libsql::params![
                conversation_id.to_string(),
                user_id.to_string(),
                device_id.to_string()
            ],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// The lowest epoch still retained in a lineage, or `None` when it has no rows.
///
/// This is the whole of `pruned_below`: a batch that starts above what the caller
/// asked for is either a retention prune (#539) — where the client SHOULD drop
/// its local group and rejoin — or a torn read, where doing so destroys data.
/// Reading the floor in the same transaction as the batch is what tells them
/// apart.
async fn min_epoch_in(
    tx: &Connection,
    conversation_id: &str,
    generation: i64,
) -> Result<Option<i64>, AppError> {
    let mut rows = tx
        .query(
            "SELECT MIN(epoch) FROM mls_commit_log WHERE conversation_id = ?1 AND generation = ?2",
            libsql::params![conversation_id.to_string(), generation],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<Option<i64>>(0)?),
        None => Ok(None),
    }
}

/// The canonical commit bytes at one epoch — the input to the client's
/// ambiguous-failure resolution (`invariants::resolve`).
///
/// An INPUT, never a verdict: the adopt/rollback decision stays in the
/// Kani-proved pure function on the client. A DS that answered "adopt" would
/// silently delete a machine-checked proof.
async fn commit_at(
    tx: &Connection,
    conversation_id: &str,
    generation: i64,
    epoch: i64,
) -> Result<Option<Vec<u8>>, AppError> {
    let mut rows = tx
        .query(
            "SELECT commit_data FROM mls_commit_log \
             WHERE conversation_id = ?1 AND generation = ?2 AND epoch = ?3",
            libsql::params![conversation_id.to_string(), generation, epoch],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(row.get::<Vec<u8>>(0)?)),
        None => Ok(None),
    }
}

/// The cross-signing inputs for every distinct `added_user_id` a commit batch
/// names: the user's `account_id_pub` and the `user_device` rows for the devices
/// those commits added.
///
/// INPUTS ONLY. `verify_added_devices` — the loop that decides Verified /
/// Revoked / AbsentRetry — stays on the client, because it is a signature check
/// over a chain rooted in the user's own identity key and the DS is explicitly
/// not trusted to evaluate it. A device with no row is simply absent from
/// `devices`, which is how the client still reaches `AbsentRetry`.
async fn added_identities(
    main: &Connection,
    commits: &[pollis_api::commit::CommitWire],
) -> Result<Vec<AddedIdentity>, AppError> {
    // Group the added device ids by the user each add names. A user can appear
    // in several commits of one batch (a multi-device add is one commit per
    // device on some paths), so the ids accumulate rather than replace.
    let mut wanted: Vec<(String, Vec<String>)> = Vec::new();
    for c in commits {
        let Some(user) = c.added_user_id.as_deref() else {
            continue;
        };
        let ids: Vec<String> = c
            .added_device_ids
            .as_deref()
            .unwrap_or_default()
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        match wanted.iter_mut().find(|(u, _)| u == user) {
            Some((_, existing)) => {
                for id in ids {
                    if !existing.contains(&id) {
                        existing.push(id);
                    }
                }
            }
            None => wanted.push((user.to_string(), ids)),
        }
    }

    let mut out = Vec::with_capacity(wanted.len());
    for (user_id, device_ids) in wanted {
        let account_id_pub = account_id_pub(main, &user_id).await?.as_deref().map(b64);
        let devices = device_cert_rows(main, &user_id, &device_ids).await?;
        out.push(AddedIdentity {
            user_id,
            account_id_pub,
            devices,
        });
    }
    Ok(out)
}

/// A user's cross-signing root. `None` covers both "no such user" and "the
/// column is NULL"; the client treats both as retry, so they need not be
/// distinguished on the wire.
pub async fn account_id_pub(main: &Connection, user_id: &str) -> anyhow::Result<Option<Vec<u8>>> {
    let mut rows = main
        .query(
            "SELECT account_id_pub FROM users WHERE id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<Option<Vec<u8>>>(0)?),
        None => Ok(None),
    }
}

/// The cross-signing columns of the named devices, for one user.
///
/// Only rows that EXIST come back. Chunked so a large add cannot exceed SQLite's
/// bound-parameter limit, and bound by position — a device id is caller-supplied
/// and never goes near string-built SQL.
pub async fn device_cert_rows(
    main: &Connection,
    user_id: &str,
    device_ids: &[String],
) -> anyhow::Result<Vec<DeviceCertRow>> {
    let mut out = Vec::with_capacity(device_ids.len());
    // One slot reserved for the user id, so the two halves of that arithmetic
    // stay adjacent.
    for chunk in device_ids.chunks(BIND_CHUNK) {
        let mut params: Vec<libsql::Value> = Vec::with_capacity(chunk.len() + 1);
        params.push(user_id.to_string().into());
        for did in chunk {
            params.push(did.clone().into());
        }
        let placeholders = (0..chunk.len())
            .map(|i| format!("?{}", i + 2))
            .collect::<Vec<_>>()
            .join(", ");
        let mut rows = main
            .query(
                &format!(
                    "SELECT device_id, device_cert, cert_issued_at, cert_identity_version, \
                            mls_signature_pub, revoked_at, mls_signature_pub_pq \
                     FROM user_device WHERE user_id = ?1 AND device_id IN ({placeholders})"
                ),
                params,
            )
            .await?;
        while let Some(row) = rows.next().await? {
            out.push(DeviceCertRow {
                device_id: row.get(0)?,
                device_cert: row.get::<Option<Vec<u8>>>(1)?.as_deref().map(b64),
                cert_issued_at: row.get::<Option<String>>(2)?,
                cert_identity_version: row.get::<Option<i64>>(3)?,
                mls_signature_pub: row.get::<Option<Vec<u8>>>(4)?.as_deref().map(b64),
                revoked_at: row.get::<Option<String>>(5)?,
                mls_signature_pub_pq: row.get::<Option<Vec<u8>>>(6)?.as_deref().map(b64),
            });
        }
    }
    Ok(out)
}

/// Bound-parameter chunk size for the `IN (…)` lookups above. Well under
/// SQLite's default `SQLITE_MAX_VARIABLE_NUMBER`, with room for the fixed
/// leading parameter.
const BIND_CHUNK: usize = 100;

/// Is this device still registered (not revoked)? The main-DB half of the
/// client's external-join gate.
///
/// Reported separately from `is_member` and as an `Option`, because the two gates
/// have opposite consequences: a wrong `false` here delays a legitimate device's
/// recovery, a wrong `true` lets a revoked device climb back into a group. The
/// client keeps both biases at its own call sites.
pub async fn device_registered(
    main: &Connection,
    user_id: &str,
    device_id: &str,
) -> anyhow::Result<bool> {
    let mut rows = main
        .query(
            "SELECT 1 FROM user_device \
             WHERE user_id = ?1 AND device_id = ?2 AND revoked_at IS NULL LIMIT 1",
            libsql::params![user_id.to_string(), device_id.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

// ── POST /v1/welcomes/fetch ──────────────────────────────────────────────────

/// POST `/v1/welcomes/fetch` — drain this device's undelivered Welcomes.
///
/// The read counterpart of `POST /v1/welcomes/ack`, and device-scoped from the
/// VERIFIED signature: a body-supplied `device_id` would let any authenticated
/// user read another of their devices' Welcome queue, and (worse) let a
/// non-recipient learn that a Welcome exists at all.
pub async fn fetch_welcomes(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let parsed: FetchWelcomesBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match reader(
        &state,
        &headers,
        &method,
        &uri,
        &body,
        parsed.user_id.as_deref(),
        parsed.device_id.as_deref(),
    )
    .await?
    {
        Ok(r) => r,
        Err(resp) => return Ok(resp),
    };
    // A body that names a DIFFERENT recipient than the signature is a 403, not a
    // silently-ignored field: the client sends `user_id` for the no-auth path,
    // and "ignored when auth is on" must not shade into "accepted".
    if let Some(u) = parsed.user_id.as_deref() {
        if state.require_auth && u != who.user_id {
            return Ok(AuthRejection::Forbidden.into_response());
        }
    }

    let conn = state.log_db.conn().await?;
    let welcomes = pending_welcomes(&conn, &who.user_id, &who.device_id).await?;
    Ok(ok_response::<FetchWelcomesBody>(FetchWelcomesResponse {
        welcomes,
    }))
}

/// The undelivered Welcomes for one `(user, device)`, oldest first — byte-for-
/// byte the selection `poll_mls_welcomes_inner` used to run against its own
/// connection, legacy device-agnostic rows included.
///
/// `pub` so the integration harness drives the same query the handler does
/// rather than a copy of it.
pub async fn pending_welcomes(
    log: &Connection,
    user_id: &str,
    device_id: &str,
) -> anyhow::Result<Vec<PendingWelcome>> {
    let mut rows = log
        .query(
            "SELECT id, welcome_data FROM mls_welcome \
             WHERE recipient_id = ?1 AND delivered = 0 \
             AND (recipient_device_id = ?2 OR recipient_device_id IS NULL) \
             ORDER BY created_at ASC",
            libsql::params![user_id.to_string(), device_id.to_string()],
        )
        .await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        let bytes: Vec<u8> = row.get(1)?;
        out.push(PendingWelcome {
            id: row.get(0)?,
            welcome: b64(&bytes),
        });
    }
    Ok(out)
}
