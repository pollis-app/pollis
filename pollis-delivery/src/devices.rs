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
//! **Key-package CLAIM** (`fetch_mls_key_package`, the `claimed = 1` UPDATE on a
//! *peer's* row) is likewise absent: it is not owner-scoped (you claim someone
//! else's package while adding their device) and per the migration plan it folds
//! into `/v1/commits` as a DS-side step of the add path, never a standalone
//! client endpoint.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, Uri},
    response::Response,
};
use libsql::Connection;
use serde::Deserialize;

use crate::error::AppError;
use crate::writes::{bad_request, gate, outcome_response, resolve_actor, WriteOutcome};
use crate::AppState;

fn b64_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    use base64::Engine as _;
    Ok(base64::engine::general_purpose::STANDARD.decode(s)?)
}

// ── Key-package entries ──────────────────────────────────────────────────────

/// One published key package: its hex hash-ref and the TLS-serialized
/// `KeyPackage` bytes, base64 (STANDARD) since they are binary.
#[derive(Deserialize)]
pub struct KeyPackageEntry {
    pub ref_hash: String,
    /// base64 (STANDARD) of the TLS-serialized `KeyPackage`.
    pub key_package: String,
}

// ── POST /v1/key-packages ────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct PublishKeyPackagesBody {
    /// The publishing device. Must be one of the actor's own devices; the rows
    /// are written with `user_id = actor` regardless, so this only labels which
    /// device produced them.
    pub device_id: String,
    pub packages: Vec<KeyPackageEntry>,
    /// No-auth fallback for the actor; when signed it must equal the
    /// authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

/// POST /v1/key-packages — publish (insert-only) one or more key packages for
/// the actor's own device. Idempotent (`INSERT OR IGNORE` keyed on `ref_hash`),
/// so a retry is benign. Used by both the single-package publish and the
/// replenish-top-up client paths (neither deletes).
pub async fn publish_key_packages(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: PublishKeyPackagesBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_publish_key_packages(&conn, authed.as_deref(), &parsed).await?)
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
            "INSERT OR IGNORE INTO mls_key_package (ref_hash, user_id, key_package, device_id) \
             VALUES (?1, ?2, ?3, ?4)",
            libsql::params![pkg.ref_hash.clone(), actor.clone(), kp, body.device_id.clone()],
        )
        .await?;
    }
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/key-packages/replenish ──────────────────────────────────────────

#[derive(Deserialize)]
pub struct ReplenishKeyPackagesBody {
    pub device_id: String,
    pub packages: Vec<KeyPackageEntry>,
    #[serde(default)]
    pub user_id: Option<String>,
}

/// POST /v1/key-packages/replenish — atomically clear this device's stale
/// unclaimed key packages and publish a fresh pool. One transaction so the pool
/// is never observed empty mid-refill.
pub async fn replenish_key_packages(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: ReplenishKeyPackagesBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_replenish_key_packages(&conn, authed.as_deref(), &parsed).await?)
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
    let mut decoded: Vec<(String, Vec<u8>)> = Vec::with_capacity(body.packages.len());
    for pkg in &body.packages {
        decoded.push((pkg.ref_hash.clone(), b64_decode(&pkg.key_package)?));
    }
    let tx = conn.transaction().await?;
    // Remove unclaimed packages for THIS device only — their private keys may no
    // longer exist in the device's current local DB (e.g. after a wipe). Also
    // clear legacy packages with NULL device_id for this user.
    tx.execute(
        "DELETE FROM mls_key_package WHERE user_id = ?1 AND claimed = 0 \
         AND (device_id = ?2 OR device_id IS NULL)",
        libsql::params![actor.clone(), body.device_id.clone()],
    )
    .await?;
    for (ref_hash, kp) in &decoded {
        tx.execute(
            "INSERT OR IGNORE INTO mls_key_package (ref_hash, user_id, key_package, device_id) \
             VALUES (?1, ?2, ?3, ?4)",
            libsql::params![ref_hash.clone(), actor.clone(), kp.clone(), body.device_id.clone()],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/devices/resign ──────────────────────────────────────────────────

/// One re-signed cross-signing cert for a device of the actor's account.
#[derive(Deserialize)]
pub struct ResignedCert {
    pub device_id: String,
    /// base64 (STANDARD) of the signed cert bytes.
    pub device_cert: String,
    /// Unix seconds, decimal ASCII (stored as TEXT for lossless round-trip).
    pub cert_issued_at: String,
    pub cert_identity_version: i64,
}

#[derive(Deserialize)]
pub struct ResignDeviceCertsBody {
    pub certs: Vec<ResignedCert>,
    #[serde(default)]
    pub user_id: Option<String>,
}

/// POST /v1/devices/resign — re-stamp the cross-signing certs the client signed
/// (with the account identity key) onto the actor's own `user_device` rows after
/// an identity rotation. Does NOT touch `mls_signature_pub` — only the cert
/// columns — so it cannot change any device's auth credential.
pub async fn resign_device_certs(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: ResignDeviceCertsBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_resign_device_certs(&conn, authed.as_deref(), &parsed).await?)
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

#[derive(Deserialize)]
pub struct PushTokenBody {
    pub token: String,
    pub platform: String,
    /// RFC3339 timestamp the client stamped. Plain text (never parsed here).
    pub updated_at: String,
    #[serde(default)]
    pub user_id: Option<String>,
}

/// POST /v1/push-tokens — upsert this device's Expo push token. Keyed on the
/// token (unique per install), so re-registering after an account switch
/// reassigns ownership rather than duplicating.
pub async fn register_push_token(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: PushTokenBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn()?;
    outcome_response(apply_register_push_token(&conn, authed.as_deref(), &parsed).await?)
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
