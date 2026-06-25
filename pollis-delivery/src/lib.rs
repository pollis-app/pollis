//! Pollis MLS Delivery Service.
//!
//! The sole writer to the MLS control-plane tables. It serializes commits per
//! conversation and serves the contiguous commit log. It sees only opaque
//! blobs — never plaintext or group/private keys — so it cannot decrypt or
//! forge a commit (RFC 9420 "Delivery Service" role). Clients keep all MLS
//! crypto; they submit commits here instead of writing the DB directly, which
//! is what makes forks/wedges structurally impossible (see [`commit`]).
//!
//! ## Write authentication ([`auth`])
//!
//! Writes (`POST /v1/commits`) can be gated behind device-certificate-signature
//! auth: the client signs each request with its Ed25519 device key and the DS
//! verifies against the registered `user_device.mls_signature_pub`. Reads
//! (`GET /v1/commits/:id`) and `/health` stay open.
//!
//! Enforcement is **config-gated and default OFF** via `POLLIS_DS_REQUIRE_AUTH`
//! (`true`/`1` → enforce; unset/anything else → today's no-auth behavior).
//! That keeps existing unsigned clients and the integration harness working
//! until a follow-up makes the pollis-core client sign + send the headers.

pub mod auth;
pub mod commit;
pub mod db;
pub mod error;

use std::sync::Arc;

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

use crate::commit::{CommitsResponse, SubmitBody, SubmitResponse};
use crate::db::Db;
use crate::error::{AppError, AuthRejection};

/// Shared handler state: the DB plus whether write auth is enforced.
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Db>,
    /// When true, `POST /v1/commits` requires a valid device signature.
    pub require_auth: bool,
}

impl AppState {
    pub fn new(db: Arc<Db>, require_auth: bool) -> Self {
        Self { db, require_auth }
    }
}

/// Read the `POLLIS_DS_REQUIRE_AUTH` gate. `true`/`1` (case-insensitive) → on;
/// unset or anything else → off (today's no-auth behavior).
pub fn require_auth_from_env() -> bool {
    matches!(
        std::env::var("POLLIS_DS_REQUIRE_AUTH").ok().as_deref(),
        Some("true") | Some("TRUE") | Some("True") | Some("1")
    )
}

/// Build the HTTP router from a bare DB, reading the auth gate from the
/// environment. Used by `main`; logs the enforcement state at startup.
pub fn build_router(db: Arc<Db>) -> Router {
    let require_auth = require_auth_from_env();
    tracing::info!(
        require_auth,
        "pollis-delivery write auth: {}",
        if require_auth {
            "ENFORCED (device-cert signature required on POST /v1/commits)"
        } else {
            "OFF (POLLIS_DS_REQUIRE_AUTH unset — writes accepted unauthenticated)"
        }
    );
    build_router_with_state(AppState::new(db, require_auth))
}

/// Build the HTTP router from an explicit [`AppState`]. Exposed so tests can
/// drive the real router with auth toggled either way.
pub fn build_router_with_state(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/commits", post(submit))
        .route("/v1/commits/:conversation_id", get(commits))
        .with_state(state)
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

/// POST /v1/commits — submit a commit. When auth is enforced, the request must
/// carry a valid device-cert signature and the commit's `sender_id` must equal
/// the authenticated user; otherwise 401/403. On success: 200 Accepted (won the
/// epoch) or 409 Rejected (not at head; body carries head + missing commits).
///
/// Takes the raw [`Bytes`] (not `Json<SubmitBody>`) because the signature binds
/// `sha256(body)` — we must hash the exact wire bytes before deserializing.
async fn submit(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    // Verify the signature over the *raw* body before parsing. The path is
    // taken from the matched URI (no query on this route).
    let authed_user = if state.require_auth {
        let conn = state.db.conn()?;
        match auth::verify_request(
            &conn,
            &headers,
            method.as_str(),
            uri.path(),
            &body,
            auth::now_unix(),
        )
        .await
        {
            Ok(user_id) => Some(user_id),
            Err(rej) => return Ok(rej.into_response()),
        }
    } else {
        None
    };

    // Parse after the signature check so a forged body can't even reach the DB.
    let parsed: SubmitBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        // Malformed JSON is a client error, not a server error.
        Err(_) => {
            return Ok((
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": "invalid body" })),
            )
                .into_response())
        }
    };

    // Identity binding: a validly-signed request may only write as itself.
    if let Some(user_id) = &authed_user {
        if parsed.sender_id != *user_id {
            return Ok(AuthRejection::Forbidden.into_response());
        }
    }

    let conn = state.db.conn()?;
    let outcome = commit::submit_commit(&conn, &parsed).await?;
    let code = match &outcome {
        SubmitResponse::Accepted { .. } => StatusCode::OK,
        SubmitResponse::Rejected { .. } => StatusCode::CONFLICT,
    };
    Ok((code, Json(outcome)).into_response())
}

#[derive(Deserialize)]
struct Since {
    #[serde(default)]
    since: i64,
}

/// GET /v1/commits/:conversation_id?since=N — the contiguous commit log from
/// epoch N (default 0) to head. Reads are open (unauthenticated).
async fn commits(
    State(state): State<AppState>,
    Path(conversation_id): Path<String>,
    Query(q): Query<Since>,
) -> Result<impl IntoResponse, AppError> {
    let conn = state.db.conn()?;
    let head = commit::head_epoch(&conn, &conversation_id).await?;
    let commits = commit::fetch_commits(&conn, &conversation_id, q.since).await?;
    Ok(Json(CommitsResponse { head, commits }))
}
