//! Account, device, key-package, enrollment and object READ handlers (#987).
//!
//! The `/v1/read/*` family; the three that are not device-signed say why at the handler:
//!
//!   * [`account_probe`] is UNAUTHENTICATED, because it runs before PIN entry
//!     when no credential of any kind exists (#987 §6.5).
//!   * [`enrollment`] is gated on POSSESSION OF `request_id` (a session would expire first).
//!   * [`recovery_blob`] accepts an OTP SESSION ONLY (no device yet).
//!
//! Everything here supplies INPUTS; cryptographic judgements stay on the client.

use axum::{
    body::Bytes,
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use libsql::Connection;

use crate::error::{AppError, AuthRejection};
use crate::reads::authed_user;
use crate::writes::{bad_request, ok_response, RawRequest};
use crate::AppState;
use crate::util::{BIND_CHUNK, b64, placeholders};

pub use pollis_api::account_reads::*;

/// POST `/v1/read/account-keys` — cross-signing roots for a batch of users.
///
/// Any caller may read any `account_id_pub` (a PUBLIC key); TOFU decisions stay on the client.
pub async fn account_keys(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: AccountKeysBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    if let Err(resp) = authed_user(&state, &req, None).await? {
        return Ok(resp);
    }

    let conn = state.db.conn().await?;
    let mut keys = Vec::new();
    for chunk in parsed.user_ids.chunks(BIND_CHUNK) {
        let sql = format!(
            "SELECT id, account_id_pub, identity_version FROM users WHERE id IN ({})",
            placeholders(chunk.len(), 1)
        );
        let params: Vec<libsql::Value> = chunk.iter().map(|id| id.clone().into()).collect();
        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            // A NULL key is omitted: the client reads absence as "not provisioned, do not pin".
            let Some(pubkey) = row.get::<Option<Vec<u8>>>(1)? else {
                continue;
            };
            keys.push(AccountKeyRow {
                user_id: row.get(0)?,
                account_id_pub: b64(&pubkey),
                identity_version: row.get::<Option<i64>>(2)?.unwrap_or(0),
            });
        }
    }
    Ok(ok_response::<AccountKeysBody>(AccountKeysResponse { keys }))
}

/// POST `/v1/read/devices` — a user's devices, with the columns the caller is
/// entitled to.
///
/// Your OWN devices include management columns; anyone else's, only cross-signing material.
pub async fn devices(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let parsed: DevicesBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match authed_user(&state, &req, None).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };
    let target = parsed.user_id.clone().unwrap_or_else(|| who.clone());
    let is_self = target == who;

    let conn = state.db.conn().await?;
    let devices = device_rows(
        &conn,
        &target,
        &parsed.device_ids,
        parsed.include_revoked,
        is_self,
    )
    .await?;
    Ok(ok_response::<DevicesBody>(DevicesResponse { devices }))
}

/// The `user_device` rows for one user, optionally narrowed to named devices.
///
/// `with_management_columns` gates `device_name` / `last_seen` / `created_at`.
pub async fn device_rows(
    conn: &Connection,
    user_id: &str,
    device_ids: &[String],
    include_revoked: bool,
    with_management_columns: bool,
) -> anyhow::Result<Vec<DeviceRow>> {
    let revoked_clause = if include_revoked {
        ""
    } else {
        " AND revoked_at IS NULL"
    };
    let mut out = Vec::new();

    const COLUMNS: &str = "SELECT device_id, device_name, created_at, last_seen, revoked_at, \
                           device_cert, cert_issued_at, cert_identity_version, \
                           mls_signature_pub, mls_signature_pub_pq FROM user_device";

    // Empty `device_ids` means every device; otherwise chunked and bound by position.
    let mut queries: Vec<(String, Vec<libsql::Value>)> = Vec::new();
    if device_ids.is_empty() {
        queries.push((
            format!("{COLUMNS} WHERE user_id = ?1{revoked_clause} ORDER BY created_at ASC"),
            vec![user_id.to_string().into()],
        ));
    } else {
        for chunk in device_ids.chunks(BIND_CHUNK) {
            let sql = format!(
                "{COLUMNS} WHERE user_id = ?1 AND device_id IN ({}){revoked_clause} \
                 ORDER BY created_at ASC",
                placeholders(chunk.len(), 2)
            );
            let mut params: Vec<libsql::Value> = Vec::with_capacity(chunk.len() + 1);
            params.push(user_id.to_string().into());
            for d in chunk {
                params.push(d.clone().into());
            }
            queries.push((sql, params));
        }
    }

    for (sql, params) in queries {
        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            out.push(DeviceRow {
                device_id: row.get(0)?,
                device_name: if with_management_columns {
                    row.get(1)?
                } else {
                    None
                },
                created_at: if with_management_columns {
                    row.get(2)?
                } else {
                    None
                },
                last_seen: if with_management_columns {
                    row.get(3)?
                } else {
                    None
                },
                revoked_at: row.get(4)?,
                device_cert: row.get::<Option<Vec<u8>>>(5)?.as_deref().map(b64),
                cert_issued_at: row.get(6)?,
                cert_identity_version: row.get(7)?,
                mls_signature_pub: row.get::<Option<Vec<u8>>>(8)?.as_deref().map(b64),
                mls_signature_pub_pq: row.get::<Option<Vec<u8>>>(9)?.as_deref().map(b64),
            });
        }
    }
    Ok(out)
}

/// Unclaimed KeyPackages `(user_id, device_id)` still has in `suite` — the
/// replenish shortfall. The one definition behind both this endpoint's
/// `count_for_ciphersuite` and the count the Welcome drain reports (#1412).
///
/// Callers pass the AUTHENTICATED identity, never a body-named one.
pub async fn unclaimed_key_package_count(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
    suite: i64,
) -> anyhow::Result<i64> {
    // `last_resort = 0`: the last-resort package (#1571) isn't part of the refill pool.
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM mls_key_package \
             WHERE user_id = ?1 AND device_id = ?2 AND claimed = 0 AND ciphersuite = ?3 \
               AND last_resort = 0",
            libsql::params![user_id.to_string(), device_id.to_string(), suite],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(row.get::<i64>(0)?),
        None => Ok(0),
    }
}

/// Does `(user_id, device_id)` hold a last-resort KeyPackage in `suite` (#1571)?
pub async fn has_last_resort_key_package(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
    suite: i64,
) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM mls_key_package \
             WHERE user_id = ?1 AND device_id = ?2 AND ciphersuite = ?3 AND last_resort = 1 \
             LIMIT 1",
            libsql::params![user_id.to_string(), device_id.to_string(), suite],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// POST `/v1/read/key-packages` — the replenish shortfall and the roster's
/// claimable devices.
///
/// A short answer silently drops a device from a group, so failures are 500s, never trims.
pub async fn key_packages(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: KeyPackagesBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match crate::reads::reader_identity(&state, &req).await? {
        Ok(r) => r,
        Err(resp) => return Ok(resp),
    };

    let conn = state.db.conn().await?;

    let (unclaimed, has_last_resort) = match parsed.count_for_ciphersuite {
        // Scoped to the AUTHENTICATED device: the number drives what it publishes.
        Some(suite) => (
            Some(unclaimed_key_package_count(&conn, &who.user_id, &who.device_id, suite).await?),
            Some(has_last_resort_key_package(&conn, &who.user_id, &who.device_id, suite).await?),
        ),
        None => (None, None),
    };

    let mut pairs = Vec::new();
    if !parsed.pairs_for_user_ids.is_empty() {
        let suite = parsed.pairs_ciphersuite;
        for chunk in parsed.pairs_for_user_ids.chunks(BIND_CHUNK) {
            let suite_clause = if suite.is_some() {
                format!(" AND ciphersuite = ?{}", chunk.len() + 1)
            } else {
                String::new()
            };
            let sql = format!(
                "SELECT DISTINCT user_id, device_id FROM mls_key_package \
                 WHERE user_id IN ({}) AND claimed = 0{suite_clause}",
                placeholders(chunk.len(), 1)
            );
            let mut params: Vec<libsql::Value> =
                chunk.iter().map(|u| u.clone().into()).collect();
            if let Some(s) = suite {
                params.push(s.into());
            }
            let mut rows = conn.query(&sql, params).await?;
            while let Some(row) = rows.next().await? {
                pairs.push(DevicePair {
                    user_id: row.get(0)?,
                    device_id: row.get(1)?,
                });
            }
        }
    }

    Ok(ok_response::<KeyPackagesBody>(KeyPackagesResponse {
        unclaimed,
        has_last_resort,
        pairs,
    }))
}

/// POST `/v1/read/registered-devices` — every non-revoked `(user, device)` pair
/// for a roster.
///
/// Reconcile subtracts this from the tree; a short list would evict live devices.
pub async fn registered_devices(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: RegisteredDevicesBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    if let Err(resp) = authed_user(&state, &req, None).await? {
        return Ok(resp);
    }

    let conn = state.db.conn().await?;
    let pairs = registered_device_pairs(&conn, &parsed.user_ids).await?;
    Ok(ok_response::<RegisteredDevicesBody>(
        RegisteredDevicesResponse { pairs },
    ))
}

/// POST `/v1/read/roster-identities` — every roster user's `account_id_pub` and
/// all of their `user_device` rows, revoked included.
///
/// Committer-side cross-signing inputs; any failure is an ERROR, never a short list.
pub async fn roster_identities(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: RosterIdentitiesBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    if let Err(resp) = authed_user(&state, &req, None).await? {
        return Ok(resp);
    }

    let conn = state.db.conn().await?;
    let identities = crate::reads::roster_identities(&conn, &parsed.user_ids).await?;
    Ok(ok_response::<RosterIdentitiesBody>(
        RosterIdentitiesResponse { identities },
    ))
}

/// Every NON-REVOKED `(user_id, device_id)` pair for a roster.
///
/// `revoked_at IS NULL` is load-bearing (#679): revoked rows are tombstoned, not deleted.
///
/// Ids are BOUND, never interpolated.
pub async fn registered_device_pairs(
    conn: &Connection,
    user_ids: &[String],
) -> anyhow::Result<Vec<DevicePair>> {
    let mut pairs = Vec::new();
    for chunk in user_ids.chunks(BIND_CHUNK) {
        let sql = format!(
            "SELECT user_id, device_id FROM user_device \
             WHERE user_id IN ({}) AND revoked_at IS NULL",
            placeholders(chunk.len(), 1)
        );
        let params: Vec<libsql::Value> = chunk.iter().map(|u| u.clone().into()).collect();
        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            pairs.push(DevicePair {
                user_id: row.get(0)?,
                device_id: row.get(1)?,
            });
        }
    }
    Ok(pairs)
}

/// POST `/v1/read/enrollment` — poll one enrollment request.
///
/// # The credential is the `request_id`
///
/// A session minted at `verify-otp` dies with the request's 600s TTL, so a session poll would 401.
///
/// Sound: the payload is sealed to the requester's ephemeral key, which the id doesn't confer.
///
/// `verification_code` needs MORE: a device-signed sibling owning the account.
pub async fn enrollment(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: EnrollmentRequestBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    if parsed.request_id.is_empty() {
        return Ok(bad_request("request_id required"));
    }

    let conn = state.db.conn().await?;
    let mut rows = conn
        .query(
            "SELECT id, user_id, new_device_id, status, expires_at, created_at, \
                    wrapped_account_key, new_device_ephemeral_pub, verification_code \
             FROM device_enrollment_request WHERE id = ?1",
            libsql::params![parsed.request_id.clone()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(ok_response::<EnrollmentRequestBody>(
            EnrollmentRequestResponse { request: None },
        ));
    };
    let owner: String = row.get(1)?;
    let mut request = EnrollmentRequestRow {
        id: row.get(0)?,
        user_id: owner.clone(),
        new_device_id: row.get(2)?,
        status: row.get(3)?,
        expires_at: row.get(4)?,
        created_at: row.get(5)?,
        wrapped_account_key: row.get::<Option<Vec<u8>>>(6)?.as_deref().map(b64),
        new_device_ephemeral_pub: None,
        verification_code: None,
    };
    let ephemeral: Option<Vec<u8>> = row.get(7)?;
    let code: Option<String> = row.get(8)?;
    drop(rows);

    if parsed.want_approval_fields {
        // The code needs the STRONGER credential; a request id can be guessed or intercepted.
        let who = match authed_user(&state, &req, None).await? {
            Ok(u) => u,
            Err(resp) => return Ok(resp),
        };
        if who != owner {
            return Ok(AuthRejection::Forbidden.into_response());
        }
        request.new_device_ephemeral_pub = ephemeral.as_deref().map(b64);
        request.verification_code = code;
    }

    Ok(ok_response::<EnrollmentRequestBody>(
        EnrollmentRequestResponse {
            request: Some(request),
        },
    ))
}

/// POST `/v1/read/pending-enrollments` — the authenticated user's still-open
/// requests. Device-signed, because unlike [`enrollment`] this ENUMERATES.
pub async fn pending_enrollments(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: PendingEnrollmentsBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match authed_user(&state, &req, parsed.user_id.as_deref()).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };

    // QR-link requests (#1207) are approved on the link tag, never by typed code.
    let link_bound = state.links.bound_request_ids(&who, crate::util::now_unix());
    let conn = state.db.conn().await?;
    let mut rows = conn
        .query(
            "SELECT id, user_id, new_device_id, status, expires_at, created_at, verification_code \
             FROM device_enrollment_request \
             WHERE user_id = ?1 AND status = 'pending' \
               AND datetime(expires_at) > datetime('now') \
             ORDER BY created_at DESC",
            libsql::params![who],
        )
        .await?;
    let mut requests = Vec::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        if link_bound.contains(&id) {
            continue;
        }
        requests.push(EnrollmentRequestRow {
            id,
            user_id: row.get(1)?,
            new_device_id: row.get(2)?,
            status: row.get(3)?,
            expires_at: row.get(4)?,
            created_at: row.get(5)?,
            wrapped_account_key: None,
            new_device_ephemeral_pub: None,
            // Served here because this endpoint IS the approving device's list
            // and the code is what it displays for the human to compare.
            verification_code: row.get(6)?,
        });
    }
    Ok(ok_response::<PendingEnrollmentsBody>(
        PendingEnrollmentsResponse { requests },
    ))
}

/// POST `/v1/read/recovery-blob` — the Secret-Key-wrapped account identity.
///
/// **OTP session only**: never plain (last line of recovery), never device-signed (no device).
///
/// Every fetch records a security event: a fetch is what an owner wants to see.
pub async fn recovery_blob(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let parsed: RecoveryBlobBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };

    let user_id = if state.require_auth {
        let now = crate::util::now_unix();
        // OTP or device-link session (#1207): the new device's enrollment read.
        match crate::session::verify_session_for_enrollment(&headers, &state.sessions, now) {
            Ok(claims) => {
                if claims.user_id != parsed.user_id {
                    return Ok(AuthRejection::Forbidden.into_response());
                }
                claims.user_id
            }
            Err(rej) => return Ok(rej.into_response()),
        }
    } else {
        parsed.user_id.clone()
    };

    let conn = state.db.conn().await?;
    let mut rows = conn
        .query(
            "SELECT salt, nonce, wrapped_key FROM account_recovery WHERE user_id = ?1",
            libsql::params![user_id.clone()],
        )
        .await?;
    let blob = match rows.next().await? {
        Some(row) => Some(RecoveryBlob {
            salt: b64(&row.get::<Vec<u8>>(0)?),
            nonce: b64(&row.get::<Vec<u8>>(1)?),
            wrapped_key: b64(&row.get::<Vec<u8>>(2)?),
        }),
        None => None,
    };
    drop(rows);

    // Best-effort: the audit row is a notification, not a gate.
    if blob.is_some() {
        if let Err(e) = conn
            .execute(
                "INSERT INTO security_event (id, user_id, kind, device_id, metadata) \
                 VALUES (?1, ?2, 'recovery_blob_fetched', NULL, NULL)",
                libsql::params![ulid::Ulid::new().to_string(), user_id],
            )
            .await
        {
            tracing::warn!("recovery-blob security event failed: {e:#}");
        }
    }

    Ok(ok_response::<RecoveryBlobBody>(RecoveryBlobResponse {
        blob,
    }))
}

/// POST `/v1/read/account-status` — the caller's own account row.
///
/// Self-scoped by construction: the id in the body is only the no-auth path's
/// input and a mismatch is a 403, because `email` is on this row.
pub async fn account_status(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: AccountStatusBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match authed_user(&state, &req, parsed.user_id.as_deref()).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };

    let conn = state.db.conn().await?;
    let account = account_row(&conn, &who).await?;
    Ok(ok_response::<AccountStatusBody>(AccountStatusResponse {
        account,
    }))
}

pub async fn account_row(
    conn: &Connection,
    user_id: &str,
) -> anyhow::Result<Option<AccountStatus>> {
    let mut rows = conn
        .query(
            "SELECT identity_version, email, username, account_id_pub FROM users WHERE id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    match rows.next().await? {
        Some(row) => Ok(Some(AccountStatus {
            user_id: user_id.to_string(),
            // Nullable pre-migration-13; default 1 so a NULL can't stamp an unverifiable cert.
            identity_version: row.get::<Option<i64>>(0)?.unwrap_or(1),
            email: row.get(1)?,
            username: row.get(2)?,
            account_id_pub: row.get::<Option<Vec<u8>>>(3)?.as_deref().map(b64),
        })),
        None => Ok(None),
    }
}

/// POST `/v1/auth/account-probe` — does this account exist, and does it have an
/// identity?
///
/// **Unauthenticated, and it has to be:** it runs pre-PIN, when no credential exists.
///
/// Discloses less than `verify-otp` does: `user_id` is an unguessable ULID. Rate-limited per IP.
pub async fn account_probe(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, AppError> {
    let parsed: AccountProbeBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    if parsed.user_id.is_empty() {
        return Ok(bad_request("user_id required"));
    }

    let conn = state.db.conn().await?;
    let mut rows = conn
        .query(
            "SELECT account_id_pub, identity_version FROM users WHERE id = ?1",
            libsql::params![parsed.user_id.clone()],
        )
        .await?;
    // `identity_version` only for an account that HAS an identity.
    let (exists, has_identity, identity_version) = match rows.next().await? {
        Some(row) => {
            let has = row.get::<Option<Vec<u8>>>(0)?.is_some();
            let version = if has {
                Some(row.get::<Option<i64>>(1)?.unwrap_or(1))
            } else {
                None
            };
            (true, has, version)
        }
        None => (false, false, None),
    };
    Ok(ok_response::<AccountProbeBody>(AccountProbeResponse {
        exists,
        has_identity,
        identity_version,
    }))
}

/// POST `/v1/read/security-events` — the caller's own audit log.
pub async fn security_events(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: SecurityEventsBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match authed_user(&state, &req, parsed.user_id.as_deref()).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };
    // Clamped server-side, matching the client's previous clamp, so a body
    // cannot ask for the whole table.
    let limit = parsed.limit.unwrap_or(100).clamp(1, 500);
    let kinds = parsed.kinds.unwrap_or_default();
    if kinds.len() > SECURITY_EVENTS_MAX_KINDS {
        return Ok(bad_request("too many kinds"));
    }

    let (sql, params) = security_events_query(&who, parsed.before.as_ref(), &kinds, limit);
    let conn = state.db.conn().await?;
    let mut rows = conn.query(&sql, params).await?;
    let mut events = Vec::new();
    while let Some(row) = rows.next().await? {
        events.push(SecurityEventRow {
            id: row.get(0)?,
            kind: row.get(1)?,
            device_id: row.get(2)?,
            created_at: row.get(3)?,
            metadata: row.get(4)?,
        });
    }
    // One row past the page signals "there is more".
    let next_cursor = if events.len() as i64 > limit {
        events.truncate(limit as usize);
        events.last().map(|e| SecurityEventCursor {
            created_at: e.created_at.clone(),
            id: e.id.clone(),
        })
    } else {
        None
    };
    Ok(ok_response::<SecurityEventsBody>(SecurityEventsResponse {
        events,
        next_cursor,
    }))
}

/// The keyset page query (#1548):
///
/// `WHERE user_id = ?1 [AND kind IN (…)] [AND (created_at, id) < (?, ?)]
///  ORDER BY created_at DESC, id DESC LIMIT limit + 1`
///
/// Served by `idx_security_event_user_created_id` / `_user_kind` (000041). Never OFFSET.
pub fn security_events_query(
    user_id: &str,
    before: Option<&SecurityEventCursor>,
    kinds: &[String],
    limit: i64,
) -> (String, Vec<libsql::Value>) {
    let mut sql = String::from(
        "SELECT id, kind, device_id, created_at, metadata \
         FROM security_event WHERE user_id = ?1",
    );
    let mut params: Vec<libsql::Value> = vec![user_id.to_string().into()];
    if !kinds.is_empty() {
        sql.push_str(&format!(" AND kind IN ({})", placeholders(kinds.len(), params.len() + 1)));
        params.extend(kinds.iter().map(|k| libsql::Value::from(k.clone())));
    }
    if let Some(c) = before {
        let n = params.len();
        sql.push_str(&format!(" AND (created_at, id) < (?{}, ?{})", n + 1, n + 2));
        params.push(c.created_at.clone().into());
        params.push(c.id.clone().into());
    }
    sql.push_str(&format!(
        " ORDER BY created_at DESC, id DESC LIMIT ?{}",
        params.len() + 1
    ));
    params.push((limit + 1).into());
    (sql, params)
}

/// POST `/v1/read/emoji` — the usable set, and object resolution for rendering.
///
/// Enumerating is membership-derived; resolving a hash you were handed is ungated (#848).
pub async fn emoji(State(state): State<AppState>, req: RawRequest) -> Result<Response, AppError> {
    let parsed: EmojiReadBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let who = match authed_user(&state, &req, None).await? {
        Ok(u) => u,
        Err(resp) => return Ok(resp),
    };

    let conn = state.db.conn().await?;
    let usable = if parsed.want_usable {
        crate::directory::usable_emoji(&conn, &who).await?
    } else {
        Vec::new()
    };

    let mut objects = Vec::new();
    for chunk in parsed.content_hashes.chunks(BIND_CHUNK) {
        let sql = format!(
            "SELECT content_hash, r2_key, content_type FROM custom_emoji_object \
             WHERE content_hash IN ({})",
            placeholders(chunk.len(), 1)
        );
        let params: Vec<libsql::Value> = chunk.iter().map(|h| h.clone().into()).collect();
        let mut rows = conn.query(&sql, params).await?;
        while let Some(row) = rows.next().await? {
            objects.push(EmojiObject {
                content_hash: row.get(0)?,
                r2_key: row.get(1)?,
                content_type: row.get(2)?,
            });
        }
    }

    Ok(ok_response::<EmojiReadBody>(EmojiReadResponse {
        usable,
        objects,
    }))
}

/// POST `/v1/read/objects` — content-addressed dedup probe.
///
/// Answers a bare boolean about a hash the caller already computed from bytes it
/// already holds, so it discloses nothing the caller did not bring.
pub async fn object_exists(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: ObjectExistsBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    if let Err(resp) = authed_user(&state, &req, None).await? {
        return Ok(resp);
    }

    let sql = match parsed.kind {
        ObjectKind::Attachment => "SELECT 1 FROM attachment_object WHERE content_hash = ?1",
        ObjectKind::Emoji => "SELECT 1 FROM custom_emoji_object WHERE content_hash = ?1",
    };
    let conn = state.db.conn().await?;
    let mut rows = conn
        .query(sql, libsql::params![parsed.content_hash.clone()])
        .await?;
    Ok(ok_response::<ObjectExistsBody>(ObjectExistsResponse {
        exists: rows.next().await?.is_some(),
    }))
}

/// POST `/v1/read/read-cursors` — this account's synced read-cursor blob (#844).
///
/// Bound to the authenticated user: even the encrypted blob leaks size and change rate.
///
/// A missing row is `cursors: None`, not a 404.
pub async fn read_cursors(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let parsed: ReadCursorsBody = match serde_json::from_slice(&req.body) {
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
            "SELECT nonce, blob FROM read_cursor_sync WHERE user_id = ?1",
            libsql::params![who],
        )
        .await?;
    let cursors = match rows.next().await? {
        Some(row) => Some(EncryptedReadCursors {
            nonce: b64(&row.get::<Vec<u8>>(0)?),
            blob: b64(&row.get::<Vec<u8>>(1)?),
        }),
        None => None,
    };
    Ok(ok_response::<ReadCursorsBody>(ReadCursorsResponse {
        cursors,
    }))
}
