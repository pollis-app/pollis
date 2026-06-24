//! Pollis MLS Delivery Service.
//!
//! The sole writer to the MLS control-plane tables. It serializes commits per
//! conversation and serves the contiguous commit log. It sees only opaque
//! blobs — never plaintext or group/private keys — so it cannot decrypt or
//! forge a commit (RFC 9420 "Delivery Service" role). Clients keep all MLS
//! crypto; they submit commits here instead of writing the DB directly, which
//! is what makes forks/wedges structurally impossible (see [`commit`]).

pub mod commit;
pub mod db;
pub mod error;

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

use crate::commit::{CommitsResponse, SubmitBody, SubmitResponse};
use crate::db::Db;
use crate::error::AppError;

pub type AppState = Arc<Db>;

/// Build the HTTP router. Exposed so tests can drive it (and so `main` is thin).
pub fn build_router(db: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/commits", post(submit))
        .route("/v1/commits/:conversation_id", get(commits))
        .with_state(db)
}

async fn health() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

/// POST /v1/commits — submit a commit. 200 Accepted (won the epoch) or
/// 409 Rejected (not at head; body carries head + missing commits).
async fn submit(
    State(db): State<AppState>,
    Json(body): Json<SubmitBody>,
) -> Result<impl IntoResponse, AppError> {
    let conn = db.conn()?;
    let outcome = commit::submit_commit(&conn, &body).await?;
    let code = match &outcome {
        SubmitResponse::Accepted { .. } => StatusCode::OK,
        SubmitResponse::Rejected { .. } => StatusCode::CONFLICT,
    };
    Ok((code, Json(outcome)))
}

#[derive(Deserialize)]
struct Since {
    #[serde(default)]
    since: i64,
}

/// GET /v1/commits/:conversation_id?since=N — the contiguous commit log from
/// epoch N (default 0) to head.
async fn commits(
    State(db): State<AppState>,
    Path(conversation_id): Path<String>,
    Query(q): Query<Since>,
) -> Result<impl IntoResponse, AppError> {
    let conn = db.conn()?;
    let head = commit::head_epoch(&conn, &conversation_id).await?;
    let commits = commit::fetch_commits(&conn, &conversation_id, q.since).await?;
    Ok(Json(CommitsResponse { head, commits }))
}
