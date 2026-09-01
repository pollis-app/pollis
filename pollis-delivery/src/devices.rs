//! Domain D — key-packages, device-cert re-signing, and push tokens.
//!
//! These are the **owner-scoped** client writes in the key-package / device /
//! push-token surface (#419 §D). Every endpoint copies the domain-A convention
//! ([`crate::messages`]): a thin axum handler `gate`s the request, parses the
//! body, calls a pure `apply_*(conn, authed, body) -> WriteOutcome` that embeds
//! BOTH authorization and the write, and maps the outcome to 200/403/400/500.
//! The handler and the integration harness share the `apply_*` fn, so the test
//! suite exercises the exact authz the production handler runs.
//!
//! ## Where the writes land
//!
//! Every domain-D table — `mls_key_package`, `user_device`, `push_token` — lives
//! in the **MAIN DB** (`state.db`), so all `apply_*` fns run on the main
//! connection.
//!
//! ## Authorization
//!
//! Every write here is **owner-scoped**: a user publishes/replenishes key
//! packages, re-signs device certs, and registers push tokens only for THEIR OWN
//! account. [`resolve_actor`] proves a signed request acts as itself (a
//! body-supplied `user_id` that differs from the authenticated user is
//! `Forbidden`), and the write then binds `user_id = actor` server-side so a
//! caller can never attach a row to another user. On the no-auth path
//! (`authed == None`) the actor comes from the body, mirroring `commit::submit`.
//!
//! ## What is NOT here — the bootstrap writes (kept direct in pollis-core)
//!
//! Two domain-D writes deliberately stay on the client's direct path and are
//! absent from this module:
//!
//!   - **device registration** (`auth.rs::register_device`, INSERT `user_device`)
//!     and **first device-cert publish** (`device.rs::ensure_device_cert`, the
//!     UPDATE that sets `user_device.mls_signature_pub`).
//!
//! These are the **bootstrap** writes. DS auth ([`crate::auth::verify_request`])
//! authenticates a request by looking up `user_device.mls_signature_pub` for the
//! `(user_id, device_id)` and verifying the Ed25519 signature against it. Until
//! that column is populated a device CANNOT produce a signature the DS will
//! accept (it would 401) — and the write that populates it is `ensure_device_cert`
//! itself. That is an irreducible chicken-and-egg: the write that establishes the
//! signing credential cannot be authenticated by that same credential. So device
//! registration + the cert publish remain direct; everything a device does
//! *after* it is enrolled (key packages, push tokens, cert re-signing) is signed
//! and routed here. Folding the bootstrap behind an OTP-session-gated DS endpoint
//! is the prerequisite for flipping clients to a read-only Turso token, and is
//! out of scope for this owner-scoped-signature slice.
//!
//! **Key-package CLAIM** (`POST /v1/key-packages/claim`) is the one domain-D
//! write that is NOT owner-scoped: you claim someone *else's* package while
//! adding their device, so it cannot use [`resolve_actor`]. The gate only
//! authenticates the claimer (any enrolled device may claim — claiming is how you
//! add a member); the target comes from the body. It returns the claimed package
//! bytes (not a [`WriteOutcome`]) so the caller can build the MLS Add commit, so
//! it has its own [`ClaimOutcome`] / `claim_outcome_response`. It is a standalone
//! endpoint (blocker C1, Goal B #419) rather than a DS-side step of `/v1/commits`
//! because the client needs the bytes BEFORE it can build the commit it submits.

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

// The request bodies for this module's endpoints live in `pollis-api`, the
// crate pollis-core builds its requests from — one declaration, both ends, so
// a client field that does not exist here is a compile error rather than a
// silently-absent JSON key. Re-exported so `pollis_delivery::devices::*Body`
// keeps resolving for handlers, tests and the flows harness.
pub use pollis_api::devices::*;

// ── Key-package entries ──────────────────────────────────────────────────────

// ── POST /v1/key-packages ────────────────────────────────────────────────────

/// POST /v1/key-packages — publish (insert-only) one or more key packages for
/// the actor's own device. Idempotent (`INSERT OR IGNORE` keyed on `ref_hash`),
/// so a retry is benign. Used by both the single-package publish and the
/// replenish-top-up client paths (neither deletes).
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

/// INSERT OR IGNORE each key package with `user_id = actor`. Authz: the actor is
/// the signer (a body `user_id` that differs is `Forbidden`); rows are bound to
/// the actor, so a caller can never publish a package under another user.
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
            // A malformed package is the whole write's problem; surface as 500
            // (the handler maps decode-at-parse to 400, but here it is a bad row).
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

// ── POST /v1/key-packages/replenish ──────────────────────────────────────────

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

/// DELETE the actor's stale unclaimed packages for `device_id` (and legacy
/// NULL-device rows), then INSERT the fresh pool — one transaction. Authz:
/// owner-scoped; the DELETE and every INSERT are bound to `user_id = actor`, so
/// a caller can only ever rotate their own device's pool.
pub async fn apply_replenish_key_packages(
    conn: &Connection,
    authed: Option<&str>,
    body: &ReplenishKeyPackagesBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    // Decode every package up front so a bad blob aborts before we touch the DB.
    let mut decoded: Vec<(String, Vec<u8>, i64)> = Vec::with_capacity(body.packages.len());
    for pkg in &body.packages {
        decoded.push((pkg.ref_hash.clone(), b64_decode(&pkg.key_package)?, pkg.suite()));
    }
    // A rotation replaces the pool for the SUITES it is publishing, and only
    // those. Today that is one suite (classic) and every stored row is classic,
    // so this is exactly the old whole-device wipe. It matters from #454 P2 on,
    // when a device keeps a classic and a hybrid pool side by side: an unscoped
    // delete would make the second rotation silently destroy the first pool.
    // An empty request keeps the historical whole-device semantics.
    let mut suites: Vec<i64> = decoded.iter().map(|(_, _, s)| *s).collect();
    suites.sort_unstable();
    suites.dedup();

    let tx = conn.transaction().await?;
    // Remove unclaimed packages for THIS device only — their private keys may no
    // longer exist in the device's current local DB (e.g. after a wipe). Also
    // clear legacy packages with NULL device_id for this user.
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

// ── POST /v1/key-packages/claim ──────────────────────────────────────────────

/// The result of a key-package claim: either a package was claimed (carries the
/// hash-ref + TLS bytes the caller builds the MLS Add from), or the target has no
/// unclaimed package available. The latter is a normal control-flow outcome (the
/// add path skips that device), NOT an error — it maps to 404, distinct from a
/// 500 DB failure.
pub enum ClaimOutcome {
    Claimed { ref_hash: String, key_package: Vec<u8> },
    NoKeyPackage,
}

/// POST /v1/key-packages/claim — atomically claim one of a TARGET user's
/// (optionally a specific device's) unclaimed key packages and return its bytes,
/// so the caller can build the MLS Add commit that brings that device into the
/// group.
///
/// Authz: ANY authenticated user may claim ANY target's package — claiming is the
/// protocol mechanism for adding someone, so it is inherently cross-account and
/// binds nothing from the body to the signer (the device signature on the request
/// IS the claimer's identity; `gate` having returned is the whole authorization).
/// On the no-auth test path the claim proceeds unauthenticated, mirroring the
/// other write handlers.
///
/// (KP exhaustion — a hostile peer draining a target's pool by claiming
/// repeatedly — is a known concern tracked for #419, but is deliberately NOT
/// rate-limited here.)
pub async fn claim_key_package(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    // The gate authenticates the claimer; its returned user_id is intentionally
    // unused — any authenticated caller may claim.
    if let Err(resp) = gate(&state, &req).await? {
        return Ok(resp);
    }
    let parsed: ClaimKeyPackageBody = match serde_json::from_slice(&req.body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    Ok(claim_outcome_response::<ClaimKeyPackageBody>(apply_claim_key_package(&conn, &parsed).await?))
}

/// Map a [`ClaimOutcome`] to its HTTP response: 200 + `{ ref_hash, key_package }`
/// (base64) on a claim, 404 + a typed error when the target has no unclaimed
/// package. Shared by the production handler and the integration harness so both
/// surface the same no-KP signal the client's control flow keys on.
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
    }
}

/// Atomically claim one unclaimed key package for the target and return its
/// hash-ref + TLS bytes (or [`ClaimOutcome::NoKeyPackage`]). This is the exact
/// `UPDATE … RETURNING` the client ran directly before the DS seam: it selects
/// the OLDEST unclaimed package (`ORDER BY created_at ASC LIMIT 1`) matching the
/// target — `user_id` only, or `user_id AND device_id` when a device is named —
/// and flips its `claimed` flag in one statement. The match is additionally
/// narrowed to the requested ciphersuite, so the suite pools stay disjoint. An
/// absent `ciphersuite` means the *current* suite ([`CIPHERSUITE_PQ`]) — #454
/// read it as classic, and #669 flipped it when the classic suite was retired,
/// so an old client's untagged claim now lands in the only pool anyone
/// publishes.
///
/// Atomicity: the `WHERE claimed = 0` subquery is re-evaluated under the single
/// libsql writer at statement-execution time, so two concurrent claims of the
/// same one-package pool can never both win — the first sets `claimed = 1`, the
/// second's subquery no longer sees an unclaimed row and `RETURNING` yields zero
/// rows (→ `NoKeyPackage`). Exactly one winner per row.
pub async fn apply_claim_key_package(
    conn: &Connection,
    body: &ClaimKeyPackageBody,
) -> anyhow::Result<ClaimOutcome> {
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
    match rows.next().await? {
        Some(row) => Ok(ClaimOutcome::Claimed {
            ref_hash: row.get::<String>(0)?,
            key_package: row.get::<Vec<u8>>(1)?,
        }),
        None => Ok(ClaimOutcome::NoKeyPackage),
    }
}

// ── POST /v1/devices/resign ──────────────────────────────────────────────────

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
    // Whole-fleet cert rewrite → evict the whole user's cached pubkeys (#658).
    // Today this UPDATE deliberately leaves `mls_signature_pub_pq` alone, so no
    // cached key can actually be wrong; the eviction is here so that stays true
    // if the statement ever grows to touch the key columns, and it costs one
    // re-read on an operation that happens once per identity rotation.
    if let Ok(owner) = resolve_actor(authed.as_deref(), parsed.user_id.as_deref()) {
        state.device_keys.invalidate_user(&owner);
    }
    outcome_response::<ResignDeviceCertsBody>(outcome)
}

/// UPDATE each device's cert columns, every statement scoped
/// `WHERE device_id = ? AND user_id = actor`. Authz: user-scoped — the actor may
/// re-sign certs for any device of THEIR OWN account (a whole-fleet operation
/// after rotation), but the `user_id = actor` bind makes another user's rows
/// untouchable.
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

// ── POST /v1/push-tokens ─────────────────────────────────────────────────────

/// POST /v1/push-tokens — upsert this device's Expo push token. Keyed on the
/// token (unique per install), so re-registering after an account switch
/// reassigns ownership rather than duplicating.
pub async fn register_push_token(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<PushTokenBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<PushTokenBody>(apply_register_push_token(&conn, authed.as_deref(), &parsed).await?)
}

/// Upsert the push token with `user_id = actor`. Authz: owner-scoped — the token
/// is bound to the actor, so a caller can never register a token under another
/// user (the body's `user_id`, if present, must equal the signer).
pub async fn apply_register_push_token(
    conn: &Connection,
    authed: Option<&str>,
    body: &PushTokenBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    conn.execute(
        "INSERT INTO push_token (token, user_id, platform, updated_at) \
         VALUES (?1, ?2, ?3, ?4) \
         ON CONFLICT(token) DO UPDATE SET \
             user_id = excluded.user_id, \
             platform = excluded.platform, \
             updated_at = excluded.updated_at",
        libsql::params![body.token.clone(), actor, body.platform.clone(), body.updated_at.clone()],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}
