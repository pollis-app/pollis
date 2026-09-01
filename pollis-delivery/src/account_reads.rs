//! Account, device, key-package, enrollment and object READ handlers (#987).
//!
//! The `/v1/read/*` family. Most of it is ordinary device-signed reads; the three
//! that are not each carry a specific reason, stated at the handler:
//!
//!   * [`account_probe`] is UNAUTHENTICATED, because it runs before PIN entry
//!     when no credential of any kind exists (#987 §6.5).
//!   * [`enrollment`] is gated on POSSESSION OF `request_id`, because a
//!     session-gated enrollment poll is arithmetically guaranteed to 401 before
//!     the request it polls expires.
//!   * [`recovery_blob`] accepts an OTP SESSION ONLY, because its caller by
//!     definition has no device yet and the blob is the account's last line of
//!     recovery.
//!
//! Everything here supplies INPUTS. No handler in this module evaluates a
//! certificate chain, decides whether a key changed, or answers a
//! safety-number question: those are cryptographic judgements rooted in the
//! user's own identity key, and the DS is outside the trust boundary that makes
//! them meaningful.

use axum::{
    body::Bytes,
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use base64::Engine as _;
use libsql::Connection;

use crate::error::{AppError, AuthRejection};
use crate::reads::authed_user;
use crate::writes::{bad_request, ok_response, RawRequest};
use crate::AppState;

pub use pollis_api::account_reads::*;

const BIND_CHUNK: usize = 100;

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn placeholders(n: usize, first: usize) -> String {
    (0..n)
        .map(|i| format!("?{}", i + first))
        .collect::<Vec<_>>()
        .join(", ")
}

// ── POST /v1/read/account-keys ───────────────────────────────────────────────

/// POST `/v1/read/account-keys` — cross-signing roots for a batch of users.
///
/// Any authenticated caller may ask for any user's `account_id_pub`: it is a
/// PUBLIC key, it is what safety numbers are computed from, and it is published
/// to the transparency log. What this does not do is decide anything with it —
/// the TOFU comparison, the pin update and the `KeyChanged` signal all stay on
/// the client, against the client's own local pin, which is the only copy the DS
/// cannot influence.
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
            // A NULL `account_id_pub` is a user who has not established an
            // identity yet. Omitted rather than sent as null, because the client
            // reads absence as "not provisioned, do not pin" and a present-but-
            // empty key would be a different (wrong) answer.
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

// ── POST /v1/read/devices ────────────────────────────────────────────────────

/// POST `/v1/read/devices` — a user's devices, with the columns the caller is
/// entitled to.
///
/// Asking about YOUR OWN devices returns the management columns (`device_name`,
/// `last_seen`). Asking about someone else's returns only the cross-signing
/// material, which any group member can already observe through a commit's add
/// metadata — a device name is a human-chosen string ("Dan's work laptop") and
/// has no business crossing that line.
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
/// `with_management_columns` gates `device_name` / `last_seen` / `created_at`;
/// the cert columns are always included, because verifying a device's cert is
/// something every peer legitimately does.
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

    // An empty `device_ids` means "every device"; a non-empty one is chunked so
    // a large fleet cannot exceed SQLite's bound-parameter ceiling. Device ids
    // are caller-supplied and bound by position, never interpolated.
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

// ── POST /v1/read/key-packages ───────────────────────────────────────────────

/// POST `/v1/read/key-packages` — the replenish shortfall and the roster's
/// claimable devices.
///
/// The `pairs` half decides WHICH devices reconcile adds to the MLS tree, so a
/// short answer is not a degraded answer — it is a device silently left out of a
/// group with no error anywhere. Any failure below therefore propagates as a 500
/// rather than being trimmed into a plausible-looking list.
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

    let unclaimed = match parsed.count_for_ciphersuite {
        Some(suite) => {
            // Scoped to the AUTHENTICATED device, never a body-named one: this
            // number drives how many packages that device publishes, and
            // answering for someone else's pool would make it publish into the
            // wrong one.
            let mut rows = conn
                .query(
                    "SELECT COUNT(*) FROM mls_key_package \
                     WHERE user_id = ?1 AND device_id = ?2 AND claimed = 0 AND ciphersuite = ?3",
                    libsql::params![who.user_id.clone(), who.device_id.clone(), suite],
                )
                .await?;
            match rows.next().await? {
                Some(row) => Some(row.get::<i64>(0)?),
                None => Some(0),
            }
        }
        None => None,
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
        pairs,
    }))
}

// ── POST /v1/read/registered-devices ─────────────────────────────────────────

/// POST `/v1/read/registered-devices` — every non-revoked `(user, device)` pair
/// for a roster.
///
/// Reconcile subtracts this from the MLS tree to find leaves whose device row was
/// revoked while the user remained a member. A short list here would evict live
/// devices, so a failure is an error, never a trim.
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

/// Every NON-REVOKED `(user_id, device_id)` pair for a roster.
///
/// `revoked_at IS NULL` is the load-bearing clause (#679): `revoke_device`
/// TOMBSTONES the row rather than deleting it — deliberately, so a peer can tell
/// "revoked, drop the rejoin" from "not replicated yet, retry" — so a revoked
/// device has to be excluded by the PREDICATE, never by the row's absence.
/// Without it, reconcile keeps the revoked leaf in `desired`, never places it in
/// `to_remove`, and the revoked device goes on decrypting.
///
/// Ids are BOUND, never interpolated: an id containing a quote must match itself
/// exactly or not at all. It used to be spliced in behind a character filter
/// that silently mangled anything else, which made the query answer a question
/// nobody asked — and an answer to the wrong question is not a trustworthy place
/// to put a revocation check.
///
/// An empty roster short-circuits rather than building `IN ()`.
///
/// `pub` so the #679 regression suite drives the real query rather than a copy.
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

// ── POST /v1/read/enrollment ─────────────────────────────────────────────────

/// POST `/v1/read/enrollment` — poll one enrollment request.
///
/// # The credential is the `request_id`
///
/// Not a device signature: the caller has no signing key, which is the entire
/// reason it is enrolling. Not an OTP session either, and this is the part worth
/// stating because the obvious design fails silently. The DS session TTL is 600
/// seconds and the enrollment TTL is 600 seconds, and the session is minted at
/// `verify-otp`, STRICTLY BEFORE the enrollment request is created — so a
/// session-gated poll is guaranteed to 401 while the request it is polling is
/// still valid. The poll runs for `TTL + 5`, which makes it certain rather than
/// likely.
///
/// Possession of the id is a sound gate because the payload is sealed to
/// something the id does not confer: `wrapped_account_key` is encrypted to the
/// requesting device's ephemeral X25519 public key, whose private half never
/// leaves that process's memory. A holder of the id learns a status string and a
/// blob they cannot open.
///
/// The approval-side columns are the exception, and they need MORE than the
/// capability: `verification_code` is what a human reads aloud to authorize the
/// enrollment, so it is served only to a device-signed sibling that owns the
/// request's account.
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
        // The approval columns require the STRONGER credential. A bare
        // capability holder — which is anyone who can guess or intercept a
        // request id — must never read the verification code, because reading
        // it out is what a human does to authorize the enrollment.
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
        requests.push(EnrollmentRequestRow {
            id: row.get(0)?,
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

// ── POST /v1/read/recovery-blob ──────────────────────────────────────────────

/// POST `/v1/read/recovery-blob` — the Secret-Key-wrapped account identity.
///
/// **OTP session only.** Never plain (this is the account's last line of
/// recovery), and never device-signed (the caller has no device — that is the
/// situation it is recovering from). The credential is the email OTP the caller
/// just proved, which is exactly the authorization the recovery flow has always
/// rested on; before #987 it rested on it *implicitly*, because possession of
/// the baked read token was all the database asked for.
///
/// Every fetch records a security event. The blob is useless without the Secret
/// Key, but a FETCH is precisely the signal an account owner wants to see if it
/// was not them.
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
        match crate::session::verify_session(&headers, &state.sessions, now) {
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

    // Best-effort: an audit row that fails to write must not fail a recovery the
    // user is legitimately performing, and the event is a notification, not a
    // gate.
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

// ── POST /v1/read/account-status ─────────────────────────────────────────────

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
            // The column is nullable pre-migration-13; the client's own default
            // was 1, kept here so a NULL cannot become a 0 that stamps an
            // unverifiable cert.
            identity_version: row.get::<Option<i64>>(0)?.unwrap_or(1),
            email: row.get(1)?,
            username: row.get(2)?,
            account_id_pub: row.get::<Option<Vec<u8>>>(3)?.as_deref().map(b64),
        })),
        None => Ok(None),
    }
}

// ── POST /v1/auth/account-probe ──────────────────────────────────────────────

/// POST `/v1/auth/account-probe` — does this account exist, and does it have an
/// identity?
///
/// **Unauthenticated, and it has to be.** This runs at every app launch, before
/// PIN entry: the local SQLCipher database is closed, so `ds_post` cannot load
/// the device signer; `device_id` is not set; and no OTP has run, so there is no
/// session bearer either. There is no credential in existence at that moment,
/// and the answer gates the PRE-UNLOCK UI — a device that needs enrollment has no
/// PIN to unlock with — so it cannot be deferred past the point where one exists.
///
/// The disclosure is strictly smaller than one already shipped. `user_id` is a
/// 128-bit ULID the caller read out of its OWN `accounts.json`: not guessable,
/// not an enumeration surface. Meanwhile `verify-otp` already answers
/// `has_identity` for an EMAIL ADDRESS, which is guessable. Rate-limited per IP
/// by the router, like the other unauthenticated endpoints.
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
    // `identity_version` rides along for the bootstrap pivot — see the field's
    // doc on `AccountProbeResponse`. It is served only for an account that HAS
    // an identity, because a version without a key is not a fact any caller can
    // use, and withholding it keeps this response's shape honest.
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

// ── POST /v1/read/security-events ────────────────────────────────────────────

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

    let conn = state.db.conn().await?;
    let mut rows = conn
        .query(
            "SELECT id, kind, device_id, created_at, metadata \
             FROM security_event WHERE user_id = ?1 \
             ORDER BY created_at DESC LIMIT ?2",
            libsql::params![who, limit],
        )
        .await?;
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
    Ok(ok_response::<SecurityEventsBody>(SecurityEventsResponse {
        events,
    }))
}

// ── POST /v1/read/emoji ──────────────────────────────────────────────────────

/// POST `/v1/read/emoji` — the usable set, and object resolution for rendering.
///
/// The two halves have deliberately different rules (#848): enumerating is
/// membership-derived (the `usable` query JOINs `group_member`, so the picker's
/// source and the send-permission rule are literally the same set), while
/// resolving a hash you were already handed is part of reading a message you can
/// already read and is ungated. Keeping both here does not merge the rules — it
/// puts them next to each other where the difference is visible.
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

// ── POST /v1/read/objects ────────────────────────────────────────────────────

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

// ── POST /v1/read/read-cursors ───────────────────────────────────────────────

/// POST `/v1/read/read-cursors` — this account's synced read-cursor blob (#844).
///
/// Device-signed and bound to the authenticated user. Serving the row to any
/// caller would be *cryptographically* harmless — it is AES-256-GCM under a key
/// derived from the account identity key, which the DS has never held — but it
/// would still hand a stranger the blob's size and change rate, which is exactly
/// the metadata this feature works to blunt. So it is scoped like every other
/// per-account read in this module.
///
/// A missing row is `cursors: None`, not a 404: a first-run device has nothing
/// to merge, and that is an ordinary state rather than an error.
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
