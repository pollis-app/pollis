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

/// How far back the durable claim budget counts.
const CLAIM_WINDOW_SECS: i64 = 3600;

/// Claims one account may make against ONE other account's pool per
/// [`CLAIM_WINDOW_SECS`].
///
/// Sized for the real add path: adding a user to a group claims one package per
/// device they have, a handful at most, and a retried add repeats that. Sixty an
/// hour covers a very device-heavy user being added to a dozen conversations by
/// the same person; it is nowhere near a drain.
const CLAIM_MAX_PER_PAIR: i64 = 60;

/// Claims ALL accounts together may make against one target device's pool per
/// [`CLAIM_WINDOW_SECS`]. The pair budget catches one attacker; this catches a
/// handful of accounts splitting the work between them.
///
/// Above the pair budget by a wide margin, because this one is shared by every
/// legitimate adder in the world: a popular account joining many groups at once
/// genuinely does have many different people claiming its packages.
const CLAIM_MAX_PER_TARGET: i64 = 300;

/// The result of a key-package claim.
///
/// [`ClaimOutcome::NoKeyPackage`] is a normal control-flow outcome (the add path
/// skips that device), NOT an error — it maps to 404, distinct from a 500 DB
/// failure. The two refusals below are about the CALLER, not the pool, and are
/// deliberately distinguishable from an empty pool: an honest adder needs to know
/// whether to move on or to back off.
#[derive(Debug)]
pub enum ClaimOutcome {
    Claimed { ref_hash: String, key_package: Vec<u8> },
    NoKeyPackage,
    /// The claimer may not draw from this target's pool at all (→ 403).
    Forbidden,
    /// The claimer is over a durable claim budget (→ 429).
    RateLimited,
}

/// POST /v1/key-packages/claim — atomically claim one of a TARGET user's
/// (optionally a specific device's) unclaimed key packages and return its bytes,
/// so the caller can build the MLS Add commit that brings that device into the
/// group.
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

/// Map a [`ClaimOutcome`] to its HTTP response: 200 + `{ ref_hash, key_package }`
/// (base64) on a claim, 404 + a typed error when the target has no unclaimed
/// package, 403 when the claimer may not draw from that pool, 429 when it is over
/// budget. Shared by the production handler and the integration harness so both
/// surface the same signals the client's control flow keys on.
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
/// Claiming is cross-account by construction — it is the mechanism for adding
/// someone — so the gate cannot be "the target is you". It used to be the
/// product's reachability rule instead (not blocked either way), on the argument
/// that anybody may start a conversation with anybody, so a claim from a
/// stranger is a claim from a prospective correspondent.
///
/// That argument does not survive contact with the numbers. A claim is a ONE-WAY
/// flip and a device publishes a pool of FIVE
/// (`pollis_core::commands::mls::key_packages`'s `TARGET`), while the per-pair
/// budget is sixty an hour — so the budget never bound anything, and any
/// unblocked stranger could empty a device's pool in five requests. A device with
/// an empty pool cannot be added to a group at all, so that is targeted exclusion
/// from every NEW conversation until the device next comes online to replenish —
/// indefinitely, for a device that is offline.
///
/// So the gate is now a RELATIONSHIP, not merely the absence of a block: the
/// claimer must already share a conversation with the target (or be the target).
/// That costs no honest flow, because every path that claims has already written
/// the roster row it is reconciling to:
///
///   * group add — the member row is written by `/v1/invites/accept`,
///     `/v1/join-requests/approve` or `/v1/invite-links/redeem`, and
///     `reconcile_group_mls_impl` then reads *that roster* and claims for the
///     devices missing from the tree;
///   * DM — `/v1/dm/create` writes `dm_channel` and every `dm_channel_member`
///     row in ONE transaction, and only then does the client initialise and
///     reconcile the MLS group. A DM request's recipient row exists from that
///     moment (un-accepted, which membership does not depend on);
///   * suite migration — the roster is the group's existing members;
///   * a user's own second device — `claimer == target`.
///
/// The block check stays underneath it: sharing a conversation with someone you
/// have since blocked is not a licence to drain their pool.
///
/// What this does NOT close is a co-member draining the pool of someone they
/// genuinely share a group with. No budget can, either, while the pool is five
/// and an honest adder legitimately spends one per add — any cap low enough to
/// matter would refuse real adds. The structural answer is a LAST-RESORT
/// KeyPackage (the RFC 9420 / X3DH device: one package handed out repeatedly
/// rather than consumed, so a pool can be depleted but never emptied), which is
/// a client and protocol change rather than a Delivery Service one.
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
    shares_a_conversation(conn, claimer, target).await
}

/// Do these two accounts sit in any of the same conversations?
///
/// Both shapes membership takes, matching `writes::is_member`'s legs: a group
/// (whose channels share its MLS group) and a DM channel. A channel of a group
/// is covered by the group row, so it needs no third leg here.
async fn shares_a_conversation(
    conn: &Connection,
    a: &str,
    b: &str,
) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 WHERE \
                EXISTS (SELECT 1 FROM group_member ga \
                        JOIN group_member gb ON gb.group_id = ga.group_id \
                        WHERE ga.user_id = ?1 AND gb.user_id = ?2) \
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
///
/// ## What `claimer` buys (#419's "KP exhaustion" note, now closed)
///
/// A claim is a ONE-WAY flip: the package is spent whether or not the claimer
/// ever builds the Add. With no bound, one authenticated account could empty a
/// target's pool in a loop, and a device with an empty pool cannot be added to
/// any group until it replenishes — a stranger holding an arbitrary user out of
/// every conversation they are invited to. So a claim now costs the claimer:
///
///   * it must be allowed to reach the target at all ([`may_claim_from`]);
///   * it is counted against a DURABLE per-(claimer, target) budget and a wider
///     per-target-device one, both read from `mls_key_package_claim` rather than
///     memory, so a rolling deploy does not hand out a fresh allowance and every
///     DS instance sees the same count (the #847 pattern);
///   * and a SUCCESSFUL claim is recorded. Only successes count: a claim against
///     an empty pool took nothing, and charging for it would let a target's own
///     exhaustion lock out the honest adders retrying behind it.
///
/// `claimer` is `None` only on the DS's no-auth (dev/test) path, which has no
/// signed identity to attribute a claim to and already trusts whoever asks; the
/// budget is skipped there exactly as every other authz check is.
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
    // #1090: the binding needs the SERVER-VERIFIED device, so this endpoint
    // gates with `gate_or_session_kind` rather than `gate_and_parse` — the
    // device id comes from the credential, never from the body.
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

/// Push tokens kept per user (#1090). A person has a phone and maybe a tablet;
/// anything past this is churn from reinstalls, and every stale row multiplies
/// one message into another Expo call.
pub const PUSH_TOKENS_PER_USER: i64 = 10;

/// Whether `token` is shaped like the Expo push token this fan-out can actually
/// deliver to (`push::EXPO_ENDPOINT` takes nothing else).
///
/// Shape-only, and that is the point: a token is a routing address, so the DS
/// cannot tell a live one from a dead one — but it can refuse the strings that
/// were never tokens at all, which is what stops the table being used as free
/// per-user storage or padded with junk to inflate fan-out.
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

/// Upsert the push token with `user_id = actor`. Authz: owner-scoped — the token
/// is bound to the actor, so a caller can never register a token under another
/// user (the body's `user_id`, if present, must equal the signer).
///
/// #1090, three bounds on top of that:
///
/// * **Shape.** A string that is not an Expo push token is refused outright, so
///   the table cannot be padded with junk that inflates every fan-out.
/// * **Device binding.** `token` is the primary key and the conflict branch used
///   to reassign `user_id` unconditionally, so anyone holding a victim's token
///   string could point it at their own account: the victim's phone then got the
///   attacker's notifications and none of its own. The row now records the
///   SERVER-VERIFIED registering device, and a re-register is only honoured from
///   that same device. This keeps the case the original design wanted —
///   switching accounts on one phone, where the same install re-registers under
///   a new user — while refusing the one it did not: a different device
///   presenting a stolen token. A legacy row with no binding adopts the first
///   device to re-register it.
/// * **Cap.** At most [`PUSH_TOKENS_PER_USER`], oldest evicted first, so a
///   reinstall loop cannot grow one message into an unbounded number of Expo
///   calls.
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
