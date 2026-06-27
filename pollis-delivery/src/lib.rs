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

pub mod account;
pub mod auth;
pub mod commit;
pub mod db;
pub mod devices;
pub mod error;
pub mod groups;
pub mod messages;
pub mod profile;
pub mod writes;

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

/// Shared handler state: the DBs plus whether write auth is enforced.
#[derive(Clone)]
pub struct AppState {
    /// Main DB — user/device metadata (auth lookups live here).
    pub db: Arc<Db>,
    /// Commit-log DB — the MLS control-plane tables (`mls_commit_log`,
    /// `mls_group_info`, `mls_welcome`). The DS is its sole writer. Defaults to
    /// the same handle as `db` when no separate log DB is configured.
    pub log_db: Arc<Db>,
    /// When true, `POST /v1/commits` requires a valid device signature.
    pub require_auth: bool,
}

impl AppState {
    /// Single-DB state: the commit log shares `db`.
    pub fn new(db: Arc<Db>, require_auth: bool) -> Self {
        let log_db = Arc::clone(&db);
        Self {
            db,
            log_db,
            require_auth,
        }
    }

    /// State with a separate commit-log DB for the MLS control-plane tables.
    pub fn new_with_log_db(db: Arc<Db>, log_db: Arc<Db>, require_auth: bool) -> Self {
        Self {
            db,
            log_db,
            require_auth,
        }
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
/// environment. Used by `main`; logs the enforcement state at startup. The
/// commit log shares the single `db` (no separate log DB).
pub fn build_router(db: Arc<Db>) -> Router {
    let log_db = Arc::clone(&db);
    build_router_with_log_db(db, log_db)
}

/// Like [`build_router`], but with a separate commit-log DB for the MLS
/// control-plane tables. `log_db` may be the same handle as `db` (single-DB
/// fallback). Reads the auth gate from the environment.
pub fn build_router_with_log_db(db: Arc<Db>, log_db: Arc<Db>) -> Router {
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
    build_router_with_state(AppState::new_with_log_db(db, log_db, require_auth))
}

/// Build the HTTP router from an explicit [`AppState`]. Exposed so tests can
/// drive the real router with auth toggled either way.
pub fn build_router_with_state(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/commits", post(submit))
        .route("/v1/commits/:conversation_id", get(commits))
        .route("/v1/group-info", post(writes::group_info))
        .route("/v1/welcomes/ack", post(writes::welcomes_ack))
        .route("/v1/welcomes/reset", post(writes::welcomes_reset))
        .route("/v1/welcomes/purge", post(writes::welcomes_purge))
        // Domain A (#419) — messages / envelopes / watermarks / reactions /
        // attachments. All land on the MAIN DB.
        .route("/v1/messages/send", post(messages::send_message))
        .route("/v1/messages/edit", post(messages::edit_message))
        .route("/v1/messages/delete", post(messages::delete_message))
        .route("/v1/reactions/add", post(messages::add_reaction))
        .route("/v1/reactions/remove", post(messages::remove_reaction))
        .route("/v1/watermarks/advance", post(messages::advance_watermark))
        .route("/v1/envelopes/gc", post(messages::envelope_gc))
        .route("/v1/attachments/register", post(messages::register_attachment))
        .route("/v1/attachments/delete", post(messages::delete_attachment))
        // Domain B (#419) — groups / channels / membership / invites /
        // join-requests. All land on the MAIN DB.
        .route("/v1/groups/create", post(groups::create_group))
        .route("/v1/groups/update", post(groups::update_group))
        .route("/v1/groups/delete", post(groups::delete_group))
        .route("/v1/groups/leave", post(groups::leave_group))
        .route("/v1/channels/create", post(groups::create_channel))
        .route("/v1/channels/update", post(groups::update_channel))
        .route("/v1/channels/delete", post(groups::delete_channel))
        .route("/v1/members/remove", post(groups::remove_member))
        .route("/v1/members/role", post(groups::set_member_role))
        .route("/v1/invites/create", post(groups::create_invite))
        .route("/v1/invites/accept", post(groups::accept_invite))
        .route("/v1/invites/decline", post(groups::decline_invite))
        .route("/v1/join-requests/create", post(groups::create_join_request))
        .route("/v1/join-requests/approve", post(groups::approve_join_request))
        .route("/v1/join-requests/reject", post(groups::reject_join_request))
        // Domain C (#419) — profile / preferences / blocks / DMs. All land on
        // the MAIN DB.
        .route("/v1/profile/update", post(profile::update_profile))
        .route("/v1/profile/preferences", post(profile::save_preferences))
        .route("/v1/blocks/add", post(profile::block_user))
        .route("/v1/blocks/remove", post(profile::unblock_user))
        .route("/v1/dm/create", post(profile::create_dm))
        .route("/v1/dm/accept", post(profile::accept_dm))
        .route("/v1/dm/add", post(profile::add_dm_member))
        .route("/v1/dm/remove", post(profile::remove_dm_member))
        .route("/v1/dm/leave", post(profile::leave_dm))
        // Domain D (#419) — key-packages / device-cert re-sign / push tokens.
        // All land on the MAIN DB. Device registration + the FIRST cert publish
        // are bootstrap writes that stay on the client's direct path (see
        // `devices` module docs).
        .route("/v1/key-packages", post(devices::publish_key_packages))
        .route("/v1/key-packages/replenish", post(devices::replenish_key_packages))
        .route("/v1/devices/resign", post(devices::resign_device_certs))
        .route("/v1/push-tokens", post(devices::register_push_token))
        // Domains E + G (#419) — account lifecycle / identity rotation /
        // recovery / device-enrollment / security audit. All land on the MAIN
        // DB. The account-identity bootstrap (signup version-1 establishment),
        // device registration, the enrollment *request*, and logout device
        // removal stay on the client's direct path (see `account` module docs).
        .route("/v1/account/rotate-identity", post(account::rotate_identity))
        .route("/v1/account/delete", post(account::delete_account))
        .route("/v1/account/reset-recover", post(account::reset_recover))
        .route("/v1/security-events", post(account::record_security_event))
        .route("/v1/enrollment/approve", post(account::approve_enrollment))
        .route("/v1/enrollment/reject", post(account::reject_enrollment))
        .route("/v1/devices/revoke", post(account::revoke_device))
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

    // The MLS control-plane tables live on the commit-log DB (== main DB when no
    // separate log DB is configured).
    let conn = state.log_db.conn()?;
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
    let conn = state.log_db.conn()?;
    let head = commit::head_epoch(&conn, &conversation_id).await?;
    let commits = commit::fetch_commits(&conn, &conversation_id, q.since).await?;
    Ok(Json(CommitsResponse { head, commits }))
}
