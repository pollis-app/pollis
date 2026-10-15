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

/// `Debug` is derived so tests can `.expect(...)` on a `Result<_, AppError>`.
/// It forwards to `anyhow::Error`'s, which prints the whole cause chain — the
/// only rendering worth having when a test reports one.
#[derive(Debug)]
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
    /// The request is signed by the device's own key, and that device has been
    /// revoked (#1338). Told apart from [`AuthRejection::Unauthorized`] so the
    /// device can sign itself out instead of retrying forever. Only a holder of
    /// the revoked device's private key can earn this answer, so it tells the
    /// caller nothing it could not already infer; an unknown device or a bad
    /// signature still gets the bare `unauthorized`.
    DeviceRevoked,
}

/// Response header carried by a [`AuthRejection::DeviceRevoked`] answer, so a
/// client can recognise it without reading (and consuming) the body.
pub const AUTH_ERROR_HEADER: &str = "x-pollis-auth-error";

impl IntoResponse for AuthRejection {
    fn into_response(self) -> Response {
        let (code, msg) = match self {
            AuthRejection::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            AuthRejection::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            AuthRejection::DeviceRevoked => (StatusCode::UNAUTHORIZED, "device_revoked"),
        };
        let mut resp = (code, Json(serde_json::json!({ "error": msg }))).into_response();
        if matches!(self, AuthRejection::DeviceRevoked) {
            resp.headers_mut().insert(
                AUTH_ERROR_HEADER,
                axum::http::HeaderValue::from_static("device_revoked"),
            );
        }
        resp
    }
}
