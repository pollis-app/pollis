//! HTTP error mapping. Any internal failure becomes a 500 with a terse JSON
//! body; the logged detail stays server-side. (A rejected commit is NOT an
//! error — it's a normal 409 response carrying the head + missing commits.)
//!
//! [`AuthRejection`] is the one *expected* refusal: a write that failed
//! device-certificate-signature auth. It maps to 401 (couldn't prove which
//! device/user signed) or 403 (proved identity, but `sender_id` doesn't match
//! the authenticated user). We never fail open — every verification or DB
//! error on the auth path becomes a 401, never acceptance.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

pub struct AppError(pub anyhow::Error);

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        tracing::error!("delivery error: {:#}", self.0);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "internal error" })),
        )
            .into_response()
    }
}

impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

/// A write rejected by device-certificate-signature auth.
///
/// `Unauthorized` (401): the request couldn't be tied to a known device key —
/// missing headers, bad timestamp (replay window), unknown/revoked device, or
/// an invalid signature. Also the catch-all for any error on the auth path, so
/// we never fail open.
///
/// `Forbidden` (403): the signature verified for `(user_id, device_id)`, but
/// the commit's `sender_id` is some *other* user — a validly-signed request
/// trying to write as someone else.
#[derive(Debug, PartialEq, Eq)]
pub enum AuthRejection {
    Unauthorized,
    Forbidden,
}

impl IntoResponse for AuthRejection {
    fn into_response(self) -> Response {
        let (code, msg) = match self {
            AuthRejection::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            AuthRejection::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
        };
        (code, Json(serde_json::json!({ "error": msg }))).into_response()
    }
}
