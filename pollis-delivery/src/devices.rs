//! Domain D — key-packages, device-cert re-signing, and push tokens.
//!
//! **Owner-scoped** client writes (#419 §D), in the domain-A shape of [`crate::messages`].
//!
//! ## Authorization
//!
//! Every write binds `user_id = actor` ([`resolve_actor`]); on the no-auth path, the body actor.
//!
//! **Key-package CLAIM** is NOT owner-scoped: you claim someone else's package to add them.

use axum::{
    extract::State,
    response::Response,
};
use libsql::Connection;

use crate::error::AppError;
use crate::writes::{
    bad_request,
    gate,
    gate_and_parse,
    outcome_response,
    resolve_actor,
    RawRequest,
    WriteOutcome,
};
use crate::AppState;
use crate::util::b64_decode;

// Request bodies live in `pollis-api`, shared with pollis-core; re-exported here.
pub use pollis_api::devices::*;

/// POST /v1/key-packages — publish (insert-only, idempotent) packages for the actor's own device.
pub async fn publish_key_packages(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<PublishKeyPackagesBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<PublishKeyPackagesBody>(apply_publish_key_packages(&conn, authed.as_deref(), &parsed).await?)
}

/// INSERT OR IGNORE each key package bound to the actor; a mismatched body `user_id` is `Forbidden`.
pub async fn apply_publish_key_packages(
    conn: &Connection,
    authed: Option<&str>,
    body: &PublishKeyPackagesBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    if let Some(why) = last_resort_shape_error(&body.packages) {
        return Ok(WriteOutcome::Invalid(why));
    }
    // Decode every package before touching the DB, so a bad blob writes nothing.
    let mut decoded: Vec<(&KeyPackageEntry, Vec<u8>)> = Vec::with_capacity(body.packages.len());
    for pkg in &body.packages {
        decoded.push((pkg, b64_decode(&pkg.key_package)?));
    }
    let tx = conn.transaction().await?;
    for (pkg, kp) in decoded {
        if pkg.last_resort {
            // A device has ONE last-resort package per suite (the partial
            // unique index enforces it). Publishing a new one replaces the old.
            tx.execute(
                "DELETE FROM mls_key_package \
                 WHERE user_id = ?1 AND device_id = ?2 AND ciphersuite = ?3 AND last_resort = 1",
                libsql::params![actor.clone(), body.device_id.clone(), pkg.suite()],
            )
            .await?;
        }
        tx.execute(
            "INSERT OR IGNORE INTO mls_key_package \
             (ref_hash, user_id, key_package, device_id, ciphersuite, last_resort) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            libsql::params![
                pkg.ref_hash.clone(),
                actor.clone(),
                kp,
                body.device_id.clone(),
                pkg.suite(),
                i64::from(pkg.last_resort),
            ],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

/// At most ONE last-resort package per suite per request (#1571): a clean 400, not a 500.
fn last_resort_shape_error(packages: &[KeyPackageEntry]) -> Option<&'static str> {
    let mut suites: Vec<i64> = packages
        .iter()
        .filter(|p| p.last_resort)
        .map(KeyPackageEntry::suite)
        .collect();
    let n = suites.len();
    suites.sort_unstable();
    suites.dedup();
    if suites.len() != n {
        return Some("more than one last-resort key package per ciphersuite");
    }
    None
}

/// POST /v1/key-packages/replenish — atomically clear this device's stale
/// unclaimed key packages and publish a fresh pool. One transaction so the pool
/// is never observed empty mid-refill.
pub async fn replenish_key_packages(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<ReplenishKeyPackagesBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<ReplenishKeyPackagesBody>(apply_replenish_key_packages(&conn, authed.as_deref(), &parsed).await?)
}

/// DELETE the actor's stale unclaimed packages for `device_id`, then INSERT the pool, atomically.
pub async fn apply_replenish_key_packages(
    conn: &Connection,
    authed: Option<&str>,
    body: &ReplenishKeyPackagesBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    if let Some(why) = last_resort_shape_error(&body.packages) {
        return Ok(WriteOutcome::Invalid(why));
    }
    // Decode every package up front so a bad blob aborts before we touch the DB.
    let mut decoded: Vec<(String, Vec<u8>, i64, bool)> = Vec::with_capacity(body.packages.len());
    for pkg in &body.packages {
        decoded.push((
            pkg.ref_hash.clone(),
            b64_decode(&pkg.key_package)?,
            pkg.suite(),
            pkg.last_resort,
        ));
    }
    // A rotation replaces only the SUITES it publishes; an empty request clears the whole device.
    let mut suites: Vec<i64> = decoded.iter().map(|(_, _, s, _)| *s).collect();
    suites.sort_unstable();
    suites.dedup();

    let tx = conn.transaction().await?;
    // Remove THIS device's unclaimed packages (their keys may be gone), plus legacy NULL-device rows.
    // Including the last-resort package (#1571); a current client sends a fresh one alongside.
    if suites.is_empty() {
        tx.execute(
            "DELETE FROM mls_key_package WHERE user_id = ?1 AND claimed = 0 \
             AND (device_id = ?2 OR device_id IS NULL)",
            libsql::params![actor.clone(), body.device_id.clone()],
        )
        .await?;
    }
    for suite in &suites {
        tx.execute(
            "DELETE FROM mls_key_package WHERE user_id = ?1 AND claimed = 0 \
             AND (device_id = ?2 OR device_id IS NULL) AND ciphersuite = ?3",
            libsql::params![actor.clone(), body.device_id.clone(), *suite],
        )
        .await?;
    }
    for (ref_hash, kp, suite, last_resort) in &decoded {
        tx.execute(
            "INSERT OR IGNORE INTO mls_key_package \
             (ref_hash, user_id, key_package, device_id, ciphersuite, last_resort) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            libsql::params![
                ref_hash.clone(),
                actor.clone(),
                kp.clone(),
                body.device_id.clone(),
                *suite,
                i64::from(*last_resort),
            ],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

/// How far back the durable claim budget counts.
const CLAIM_WINDOW_SECS: i64 = 3600;

/// Claims one account may make against ONE other account's pool per
/// [`CLAIM_WINDOW_SECS`].
const CLAIM_MAX_PER_PAIR: i64 = 60;

/// Claims ALL accounts together may make against one target device's pool per
/// [`CLAIM_WINDOW_SECS`]. The pair budget catches one attacker; this catches a
/// handful of accounts splitting the work between them.
const CLAIM_MAX_PER_TARGET: i64 = 300;

/// The result of a key-package claim.
///
/// `NoKeyPackage` is normal control flow (404); the two refusals are about the CALLER.
#[derive(Debug)]
pub enum ClaimOutcome {
    Claimed { ref_hash: String, key_package: Vec<u8> },
    NoKeyPackage,
    /// The claimer may not draw from this target's pool at all (→ 403).
    Forbidden,
    /// The claimer is over a durable claim budget (→ 429).
    RateLimited,
}

/// POST /v1/key-packages/claim — atomically claim one of a TARGET's unclaimed packages.
pub async fn claim_key_package(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let claimer = match gate(&state, &req).await? {
        Ok(c) => c,
        Err(resp) => return Ok(resp),
    };
    let parsed: ClaimKeyPackageBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    Ok(claim_outcome_response::<ClaimKeyPackageBody>(
        apply_claim_key_package(&conn, claimer.as_deref(), &parsed).await?,
    ))
}

/// Map a [`ClaimOutcome`] to its HTTP response (200 / 404 / 403 / 429).
pub fn claim_outcome_response<B>(outcome: ClaimOutcome) -> Response
where
    B: pollis_api::DsRequest<Response = ClaimKeyPackageResponse>,
{
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use base64::Engine as _;
    match outcome {
        ClaimOutcome::Claimed { ref_hash, key_package } => {
            let b64 = base64::engine::general_purpose::STANDARD.encode(key_package);
            crate::writes::ok_response::<B>(ClaimKeyPackageResponse {
                ref_hash,
                key_package: b64,
            })
        }
        ClaimOutcome::NoKeyPackage => (
            StatusCode::NOT_FOUND,
            axum::Json(serde_json::json!({ "error": "no_key_package" })),
        )
            .into_response(),
        ClaimOutcome::Forbidden => crate::error::AuthRejection::Forbidden.into_response(),
        ClaimOutcome::RateLimited => (
            StatusCode::TOO_MANY_REQUESTS,
            axum::Json(serde_json::json!({ "error": "too_many_claims" })),
        )
            .into_response(),
    }
}

/// May `claimer` draw from `target`'s key-package pool at all?
///
/// Claiming is cross-account by construction, so the gate cannot be "the target is you".
///
/// A pool is five one-way claims, so any unblocked stranger could empty it.
///
/// So the two must share a conversation's DESIRED ROSTER (or be one account):
///
///   * group invite: the invitee is in the roster before accepting;
///   * join-request approval and invite-link redemption write the member row
///     first, then the client reconciles;
///   * DM: `/v1/dm/create` writes every member row before the client reconciles;
///   * suite migration — the roster is the group's existing members;
///   * a user's own second device — `claimer == target`.
///
/// The block check stays underneath it: being in a roster with someone you have
/// since blocked is not a licence to drain their pool.
///
/// An inviter can still claim, but visibly and rate-limited; last-resort packages (#1571) do the rest.
async fn may_claim_from(
    conn: &Connection,
    claimer: &str,
    target: &str,
) -> anyhow::Result<bool> {
    if claimer == target {
        return Ok(true);
    }
    if crate::profile::is_blocked_either_way(conn, claimer, target).await? {
        return Ok(false);
    }
    shares_a_desired_roster(conn, claimer, target).await
}

/// Do these two accounts sit in any one conversation's desired roster?
///
/// Groups (pending invitees count, either direction) and DM channels.
async fn shares_a_desired_roster(
    conn: &Connection,
    a: &str,
    b: &str,
) -> anyhow::Result<bool> {
    // Four EXISTS legs, not a `UNION` CTE, so `idx_gm_user` / `idx_invite_invitee` are used.
    // No `status` filter on `group_invite`: this must agree with `directory::desired_roster`.
    let mut rows = conn
        .query(
            "SELECT 1 WHERE \
                EXISTS (SELECT 1 FROM group_member ga \
                        JOIN group_member gb ON gb.group_id = ga.group_id \
                        WHERE ga.user_id = ?1 AND gb.user_id = ?2) \
             OR EXISTS (SELECT 1 FROM group_member ga \
                        JOIN group_invite gi ON gi.group_id = ga.group_id \
                        WHERE ga.user_id = ?1 AND gi.invitee_id = ?2) \
             OR EXISTS (SELECT 1 FROM group_invite gi \
                        JOIN group_member gb ON gb.group_id = gi.group_id \
                        WHERE gi.invitee_id = ?1 AND gb.user_id = ?2) \
             OR EXISTS (SELECT 1 FROM group_invite gi \
                        JOIN group_invite gj ON gj.group_id = gi.group_id \
                        WHERE gi.invitee_id = ?1 AND gj.invitee_id = ?2) \
             OR EXISTS (SELECT 1 FROM dm_channel_member da \
                        JOIN dm_channel_member db ON db.dm_channel_id = da.dm_channel_id \
                        WHERE da.user_id = ?1 AND db.user_id = ?2) \
             LIMIT 1",
            libsql::params![a.to_string(), b.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// Recent claims by `claimer` against `target`, and against that target device
/// from everyone.
async fn recent_claims(
    conn: &Connection,
    claimer: &str,
    body: &ClaimKeyPackageBody,
) -> anyhow::Result<(i64, i64)> {
    let cutoff = format!("-{CLAIM_WINDOW_SECS} seconds");
    let mut rows = conn
        .query(
            "SELECT \
                 COUNT(*) FILTER (WHERE claimer_id = ?1), \
                 COUNT(*) FILTER (WHERE ?3 IS NULL OR target_device_id = ?3) \
             FROM mls_key_package_claim \
             WHERE target_user_id = ?2 \
               AND datetime(claimed_at) > datetime('now', ?4)",
            libsql::params![
                claimer.to_string(),
                body.target_user_id.clone(),
                body.target_device_id.clone(),
                cutoff,
            ],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => (row.get(0)?, row.get(1)?),
        None => (0, 0),
    })
}

/// Atomically claim one unclaimed package for the target: oldest first, requested suite (default PQ).
///
/// Exactly one winner per row: the `WHERE claimed = 0` subquery runs under the single writer.
///
/// A claim is a ONE-WAY flip, so it costs the claimer:
///
///   * it must be allowed to reach the target at all ([`may_claim_from`]);
///   * a DURABLE per-(claimer, target) and per-device budget (`mls_key_package_claim`, #847);
///   * only SUCCESSFUL claims count, so an empty pool doesn't lock out honest adders.
///
/// `claimer` is `None` only on the no-auth path, which skips the budget.
pub async fn apply_claim_key_package(
    conn: &Connection,
    claimer: Option<&str>,
    body: &ClaimKeyPackageBody,
) -> anyhow::Result<ClaimOutcome> {
    if let Some(claimer) = claimer {
        if !may_claim_from(conn, claimer, &body.target_user_id).await? {
            return Ok(ClaimOutcome::Forbidden);
        }
        let (by_pair, by_target) = recent_claims(conn, claimer, body).await?;
        if by_pair >= CLAIM_MAX_PER_PAIR || by_target >= CLAIM_MAX_PER_TARGET {
            return Ok(ClaimOutcome::RateLimited);
        }
    }

    let suite = body.ciphersuite.unwrap_or(CIPHERSUITE_PQ);
    let claimed = match claim_regular(conn, body, suite).await? {
        Some(hit) => Some(hit),
        // Empty pool: fall back to the last-resort package (#1571), read, never flipped. Device-scoped only.
        None => match &body.target_device_id {
            Some(device_id) => {
                last_resort_package(conn, &body.target_user_id, device_id, suite).await?
            }
            None => None,
        },
    };

    let Some((ref_hash, key_package)) = claimed else {
        return Ok(ClaimOutcome::NoKeyPackage);
    };

    if let Some(claimer) = claimer {
        conn.execute(
            "INSERT INTO mls_key_package_claim \
                 (id, claimer_id, target_user_id, target_device_id) \
             VALUES (?1, ?2, ?3, ?4)",
            libsql::params![
                ulid::Ulid::new().to_string(),
                claimer.to_string(),
                body.target_user_id.clone(),
                body.target_device_id.clone(),
            ],
        )
        .await?;
    }

    Ok(ClaimOutcome::Claimed {
        ref_hash,
        key_package,
    })
}

/// Atomically claim the OLDEST unclaimed REGULAR package for the target: the
/// pre-#1571 claim, narrowed to `last_resort = 0` so the pool is spent before the
/// package that never may be.
async fn claim_regular(
    conn: &Connection,
    body: &ClaimKeyPackageBody,
    suite: i64,
) -> anyhow::Result<Option<(String, Vec<u8>)>> {
    let mut rows = match &body.target_device_id {
        Some(device_id) => {
            conn.query(
                "UPDATE mls_key_package \
                 SET claimed = 1 \
                 WHERE ref_hash = ( \
                     SELECT ref_hash FROM mls_key_package \
                     WHERE user_id = ?1 AND device_id = ?2 AND claimed = 0 \
                       AND ciphersuite = ?3 AND last_resort = 0 \
                     ORDER BY created_at ASC LIMIT 1 \
                 ) \
                 RETURNING ref_hash, key_package",
                libsql::params![body.target_user_id.clone(), device_id.clone(), suite],
            )
            .await?
        }
        None => {
            conn.query(
                "UPDATE mls_key_package \
                 SET claimed = 1 \
                 WHERE ref_hash = ( \
                     SELECT ref_hash FROM mls_key_package \
                     WHERE user_id = ?1 AND claimed = 0 AND ciphersuite = ?2 \
                       AND last_resort = 0 \
                     ORDER BY created_at ASC LIMIT 1 \
                 ) \
                 RETURNING ref_hash, key_package",
                libsql::params![body.target_user_id.clone(), suite],
            )
            .await?
        }
    };
    Ok(match rows.next().await? {
        Some(row) => Some((row.get::<String>(0)?, row.get::<Vec<u8>>(1)?)),
        None => None,
    })
}

/// The device's last-resort package in `suite` (#1571); a plain SELECT, so it serves repeatedly.
async fn last_resort_package(
    conn: &Connection,
    user_id: &str,
    device_id: &str,
    suite: i64,
) -> anyhow::Result<Option<(String, Vec<u8>)>> {
    let mut rows = conn
        .query(
            "SELECT ref_hash, key_package FROM mls_key_package \
             WHERE user_id = ?1 AND device_id = ?2 AND ciphersuite = ?3 \
               AND last_resort = 1 \
             LIMIT 1",
            libsql::params![user_id.to_string(), device_id.to_string(), suite],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some((row.get::<String>(0)?, row.get::<Vec<u8>>(1)?)),
        None => None,
    })
}

/// POST /v1/devices/resign — re-stamp the cross-signing certs the client signed
/// (with the account identity key) onto the actor's own `user_device` rows after
/// an identity rotation. Does NOT touch `mls_signature_pub` — only the cert
/// columns — so it cannot change any device's auth credential.
pub async fn resign_device_certs(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<ResignDeviceCertsBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let outcome = apply_resign_device_certs(&conn, authed.as_deref(), &parsed).await?;
    // Evict the user's cached pubkeys (#658), in case this ever touches the key columns.
    if let Ok(owner) = resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        state.device_keys.invalidate_user(&owner);
    }
    outcome_response::<ResignDeviceCertsBody>(outcome)
}

/// UPDATE each device's cert columns, scoped `WHERE device_id = ? AND user_id = actor`.
pub async fn apply_resign_device_certs(
    conn: &Connection,
    authed: Option<&str>,
    body: &ResignDeviceCertsBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    let tx = conn.transaction().await?;
    for cert in &body.certs {
        let cert_bytes = b64_decode(&cert.device_cert)?;
        tx.execute(
            "UPDATE user_device \
             SET device_cert = ?1, cert_issued_at = ?2, cert_identity_version = ?3 \
             WHERE device_id = ?4 AND user_id = ?5",
            libsql::params![
                cert_bytes,
                cert.cert_issued_at.clone(),
                cert.cert_identity_version,
                cert.device_id.clone(),
                actor.clone(),
            ],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

/// POST /v1/push-tokens — upsert this device's Expo push token. Keyed on the
/// token (unique per install), so re-registering after an account switch
/// reassigns ownership rather than duplicating.
pub async fn register_push_token(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    // #1090: the device id comes from the verified credential, never the body.
    let (authed, cred) = match crate::writes::gate_or_session_kind(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let parsed: PushTokenBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(crate::writes::bad_request("invalid body")),
    };
    let device_id = match &cred {
        crate::writes::GateCredential::Signature { device_id }
        | crate::writes::GateCredential::Session { device_id } => Some(device_id.as_str()),
        crate::writes::GateCredential::None => None,
    };
    let conn = state.db.conn().await?;
    outcome_response::<PushTokenBody>(
        apply_register_push_token(&conn, authed.as_deref(), device_id, &parsed).await?,
    )
}

/// Push tokens kept per user (#1090); extras are reinstall churn.
pub const PUSH_TOKENS_PER_USER: i64 = 10;

/// Whether `token` is shaped like the Expo push token this fan-out can actually
/// deliver to (`push::EXPO_ENDPOINT` takes nothing else).
///
/// Shape-only: it can't tell live from dead, but refuses junk that would pad fan-out.
pub fn is_expo_push_token(token: &str) -> bool {
    let inner = token
        .strip_prefix("ExponentPushToken[")
        .or_else(|| token.strip_prefix("ExpoPushToken["))
        .and_then(|rest| rest.strip_suffix(']'));
    match inner {
        // Expo's opaque id: non-empty, bounded, and no structural characters.
        Some(id) => {
            !id.is_empty()
                && id.len() <= 128
                && id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
        }
        None => false,
    }
}

/// Upsert the push token with `user_id = actor`.
///
/// #1090, three bounds on top of that:
///
/// * **Shape.** Non-Expo strings are refused.
/// * **Device binding.** Only the registering device may re-register a token (legacy rows adopt).
/// * **Cap.** At most [`PUSH_TOKENS_PER_USER`], oldest evicted first.
///
/// `caps` (#1599) is reduced to the known capabilities; anything else is dropped.
pub async fn apply_register_push_token(
    conn: &Connection,
    authed: Option<&str>,
    device_id: Option<&str>,
    body: &PushTokenBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    if !is_expo_push_token(&body.token) {
        return Ok(WriteOutcome::Forbidden);
    }

    // Who, if anyone, already holds this token.
    let mut rows = conn
        .query(
            "SELECT user_id, device_id FROM push_token WHERE token = ?1",
            libsql::params![body.token.clone()],
        )
        .await?;
    let existing: Option<(String, Option<String>)> = match rows.next().await? {
        Some(row) => Some((row.get(0)?, row.get::<Option<String>>(1)?)),
        None => None,
    };
    drop(rows);

    if let Some((owner, bound_device)) = &existing {
        // A different account may only take the token over from the device that
        // registered it. `None` is a pre-#1090 row with no binding: adopt it.
        let same_device = match (bound_device.as_deref(), device_id) {
            (Some(bound), Some(asking)) => bound == asking,
            (None, _) => true,
            (Some(_), None) => false,
        };
        if owner != &actor && !same_device {
            return Ok(WriteOutcome::Forbidden);
        }
    }

    // #1599: `caps` is overwritten on every register, never merged, so an install
    // that goes back to a build without the capability goes back to today's payload.
    conn.execute(
        "INSERT INTO push_token (token, user_id, platform, updated_at, device_id, caps) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(token) DO UPDATE SET \
             user_id = excluded.user_id, \
             platform = excluded.platform, \
             updated_at = excluded.updated_at, \
             device_id = COALESCE(excluded.device_id, push_token.device_id), \
             caps = excluded.caps",
        libsql::params![
            body.token.clone(),
            actor.clone(),
            body.platform.clone(),
            body.updated_at.clone(),
            device_id.map(|d| d.to_string()),
            crate::push::PushCaps::from_wire(&body.caps).bits(),
        ],
    )
    .await?;

    // Cap with oldest-first eviction. Ordered by `updated_at` then `token` so the
    // victim is deterministic when several rows share a stamp.
    conn.execute(
        "DELETE FROM push_token WHERE user_id = ?1 AND token NOT IN ( \
             SELECT token FROM push_token WHERE user_id = ?1 \
             ORDER BY updated_at DESC, token DESC LIMIT ?2 \
         )",
        libsql::params![actor, PUSH_TOKENS_PER_USER],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}
