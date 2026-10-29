//! Key packages, device-cert re-signing and push tokens (#419 §D). All tables
//! are on the main DB.
//!
//! Every write is owner-scoped: [`resolve_actor`] proves a signed request acts
//! as itself and the write binds `user_id = actor`. The exception is the
//! key-package claim, which draws from someone else's pool by construction and
//! is gated by `may_claim_from` plus a durable budget instead.
//!
//! The bootstrap writes (device registration, first cert publish) cannot be
//! signed by the credential they establish, so they live behind the OTP-session
//! gate in [`crate::bootstrap`].

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

pub use pollis_api::devices::*;

/// POST /v1/key-packages: publish (insert-only, idempotent on `ref_hash`) key
/// packages for the actor's own device.
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

pub async fn apply_publish_key_packages(
    conn: &Connection,
    authed: Option<&str>,
    body: &PublishKeyPackagesBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    for pkg in &body.packages {
        let kp = match b64_decode(&pkg.key_package) {
            Ok(b) => b,
            Err(e) => return Err(e),
        };
        conn.execute(
            "INSERT OR IGNORE INTO mls_key_package \
             (ref_hash, user_id, key_package, device_id, ciphersuite) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            libsql::params![
                pkg.ref_hash.clone(),
                actor.clone(),
                kp,
                body.device_id.clone(),
                pkg.suite(),
            ],
        )
        .await?;
    }
    Ok(WriteOutcome::Ok)
}

/// POST /v1/key-packages/replenish: replace this device's unclaimed pool in one
/// transaction, so the pool is never observed empty mid-refill.
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

pub async fn apply_replenish_key_packages(
    conn: &Connection,
    authed: Option<&str>,
    body: &ReplenishKeyPackagesBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    // Decode up front so a bad blob aborts before the DB is touched.
    let mut decoded: Vec<(String, Vec<u8>, i64)> = Vec::with_capacity(body.packages.len());
    for pkg in &body.packages {
        decoded.push((pkg.ref_hash.clone(), b64_decode(&pkg.key_package)?, pkg.suite()));
    }
    // A rotation replaces the pool only for the suites it publishes, so pools
    // of different suites stay disjoint (#454). An empty request clears the
    // whole device.
    let mut suites: Vec<i64> = decoded.iter().map(|(_, _, s)| *s).collect();
    suites.sort_unstable();
    suites.dedup();

    let tx = conn.transaction().await?;
    // This device's unclaimed packages (their private keys may be gone after a
    // wipe), plus legacy NULL-device rows.
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
    for (ref_hash, kp, suite) in &decoded {
        tx.execute(
            "INSERT OR IGNORE INTO mls_key_package \
             (ref_hash, user_id, key_package, device_id, ciphersuite) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            libsql::params![
                ref_hash.clone(),
                actor.clone(),
                kp.clone(),
                body.device_id.clone(),
                *suite,
            ],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

/// How far back the durable claim budget counts.
const CLAIM_WINDOW_SECS: i64 = 3600;

/// Claims one account may make against one other account's pool per
/// [`CLAIM_WINDOW_SECS`].
const CLAIM_MAX_PER_PAIR: i64 = 60;

/// Claims all accounts together may make against one target device's pool per
/// [`CLAIM_WINDOW_SECS`]; catches several accounts splitting the work. Wide
/// because every legitimate adder shares it.
const CLAIM_MAX_PER_TARGET: i64 = 300;

/// The result of a key-package claim. `NoKeyPackage` (404) is normal control
/// flow; the refusals are distinguishable from it so an honest adder knows
/// whether to move on or back off.
#[derive(Debug)]
pub enum ClaimOutcome {
    Claimed { ref_hash: String, key_package: Vec<u8> },
    NoKeyPackage,
    Forbidden,
    RateLimited,
}

/// POST /v1/key-packages/claim: atomically claim one of a target user's (or
/// device's) unclaimed key packages and return its bytes for an MLS Add.
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

/// Map a [`ClaimOutcome`] to 200 / 404 / 403 / 429.
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

/// May `claimer` draw from `target`'s key-package pool at all? Only when they
/// are the same account or share a conversation's desired roster, and neither
/// has blocked the other. Mere reachability let any stranger empty a five-package
/// pool and exclude the device from new conversations. Every claim path writes
/// the roster row before it claims. See `.codesight/wiki/mls.md`.
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

/// Do these two accounts sit in any one conversation's desired roster (a group,
/// counting pending invitees as [`crate::directory::desired_roster`] does, or a
/// DM channel)?
async fn shares_a_desired_roster(
    conn: &Connection,
    a: &str,
    b: &str,
) -> anyhow::Result<bool> {
    // Explicit EXISTS legs, not a UNION CTE, so `idx_gm_user` and
    // `idx_invite_invitee` are used instead of materializing both tables. No
    // `group_invite.status` filter: this must agree with
    // `directory::desired_roster`, which has none.
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

/// Atomically claim the oldest unclaimed key package of the target (user, or
/// user and device) in the requested ciphersuite, defaulting to
/// [`CIPHERSUITE_PQ`] (#669). One `UPDATE … RETURNING` under the single writer,
/// so two concurrent claims of one package cannot both win.
///
/// With a `claimer`, the claim must pass [`may_claim_from`] and the durable
/// budgets in `mls_key_package_claim`, and only a successful claim is recorded
/// (see `.codesight/wiki/database.md`). `claimer` is `None` only on the no-auth
/// path.
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
    let mut rows = match &body.target_device_id {
        Some(device_id) => {
            conn.query(
                "UPDATE mls_key_package \
                 SET claimed = 1 \
                 WHERE ref_hash = ( \
                     SELECT ref_hash FROM mls_key_package \
                     WHERE user_id = ?1 AND device_id = ?2 AND claimed = 0 \
                       AND ciphersuite = ?3 \
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
                     ORDER BY created_at ASC LIMIT 1 \
                 ) \
                 RETURNING ref_hash, key_package",
                libsql::params![body.target_user_id.clone(), suite],
            )
            .await?
        }
    };
    let claimed = match rows.next().await? {
        Some(row) => Some((row.get::<String>(0)?, row.get::<Vec<u8>>(1)?)),
        None => None,
    };
    drop(rows);

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

/// POST /v1/devices/resign: re-stamp cross-signing certs onto the actor's own
/// `user_device` rows after an identity rotation. Touches only the cert
/// columns, never a device's auth credential.
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
    // Defensive (#658): the UPDATE leaves the key columns alone, but evicting
    // keeps the cache correct if it ever touches them.
    if let Ok(owner) = resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        state.device_keys.invalidate_user(&owner);
    }
    outcome_response::<ResignDeviceCertsBody>(outcome)
}

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

/// POST /v1/push-tokens: upsert this device's Expo push token.
pub async fn register_push_token(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    // The binding needs the server-verified device id, never a body field (#1090).
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

/// Push tokens kept per user (#1090); every stale row is another Expo call per
/// message.
pub const PUSH_TOKENS_PER_USER: i64 = 10;

/// Whether `token` is shaped like an Expo push token. Shape-only: it stops the
/// table being padded with junk or used as free storage.
pub fn is_expo_push_token(token: &str) -> bool {
    let inner = token
        .strip_prefix("ExponentPushToken[")
        .or_else(|| token.strip_prefix("ExpoPushToken["))
        .and_then(|rest| rest.strip_suffix(']'));
    match inner {
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

/// Upsert the push token with `user_id = actor`. Another account may take a
/// token over only from the device bound to it, so a stolen token string cannot
/// redirect a victim's notifications (#1090; see `push_token` in
/// `.codesight/wiki/database.md`).
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
        // `None` is a pre-#1090 row with no binding: adopt it.
        let same_device = match (bound_device.as_deref(), device_id) {
            (Some(bound), Some(asking)) => bound == asking,
            (None, _) => true,
            (Some(_), None) => false,
        };
        if owner != &actor && !same_device {
            return Ok(WriteOutcome::Forbidden);
        }
    }

    conn.execute(
        "INSERT INTO push_token (token, user_id, platform, updated_at, device_id) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT(token) DO UPDATE SET \
             user_id = excluded.user_id, \
             platform = excluded.platform, \
             updated_at = excluded.updated_at, \
             device_id = COALESCE(excluded.device_id, push_token.device_id)",
        libsql::params![
            body.token.clone(),
            actor.clone(),
            body.platform.clone(),
            body.updated_at.clone(),
            device_id.map(|d| d.to_string()),
        ],
    )
    .await?;

    // Oldest-first eviction; `token` breaks ties deterministically.
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
