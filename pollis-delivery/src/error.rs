//! HTTP error mapping. Any internal failure becomes a 500 with a terse JSON
//! body; the logged detail stays server-side. (A rejected commit is NOT an
//! error — it's a normal 409 response carrying the head + missing commits.)
//! A transient database failure — the connection to Turso dropped, an expired
//! hrana stream, `SQLITE_BUSY` — is a 503 `{"error":"unavailable"}` instead
//! (#1500): nothing about the request was wrong, and the same request may
//! succeed a moment later.
//!
//! [`AuthRejection`] is the one *expected* refusal: a write that failed
//! device-certificate-signature auth. It maps to 401 (couldn't prove which
//! device/user signed) or 403 (proved identity, but `sender_id` doesn't match
//! the authenticated user). We never fail open — every verification error on
//! the auth path is a refusal, never acceptance. A database error while looking
//! up the device's key is a refusal too, but a 503, not a 401: the DS could not
//! check the signature, which says nothing about whether it was valid.

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
        // Logged at error level either way: `error_log::record` stores it as the
        // failed request's detail.
        tracing::error!("delivery error: {:#}", self.0);
        if is_transient_db_error(&self.0) {
            return unavailable();
        }
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
        let err = err.into();
        // Here rather than in `into_response`: `?` converts while the handler's
        // connections are still checked out, so the pool hears about the fault
        // before they are released.
        note_transient(&err);
        Self(err)
    }
}

/// 503 `{"error":"unavailable"}`: the DS could not reach its database. Says
/// "try again", never "you are not signed in".
fn unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(axum::http::header::RETRY_AFTER, "1")],
        Json(serde_json::json!({ "error": "unavailable" })),
    )
        .into_response()
}

/// Whether `err` is a database failure that says nothing about the request: the
/// connection to Turso dropped or timed out, the hrana stream had expired, or
/// the database was busy. Anything else — a constraint, a bad column, SQL that
/// does not parse — is a real 500.
pub fn is_transient_db_error(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|cause| cause.downcast_ref::<libsql::Error>())
        .any(is_transient_libsql_error)
}

/// [`is_transient_db_error`], and when it is one, tell the connection pool so
/// that no connection checked out right now is reused
/// ([`crate::db::note_transport_fault`]).
pub fn note_transient(err: &anyhow::Error) -> bool {
    let transient = is_transient_db_error(err);
    if transient {
        crate::db::note_transport_fault();
    }
    transient
}

/// SQLite's primary result codes for "busy" and "locked".
const SQLITE_BUSY: i32 = 5;
const SQLITE_LOCKED: i32 = 6;

fn is_transient_libsql_error(err: &libsql::Error) -> bool {
    match err {
        libsql::Error::ConnectionFailed(_) => true,
        libsql::Error::SqliteFailure(code, _) | libsql::Error::RemoteSqliteFailure(code, _, _) => {
            matches!(code & 0xff, SQLITE_BUSY | SQLITE_LOCKED)
        }
        // libsql's hrana error type is private, so its rendering is all there is
        // to go on: `http error` is the transport (a dropped or timed-out
        // connection), `api error` a non-2xx answer from Turso, `stream error`
        // a statement the database refused.
        libsql::Error::Hrana(inner) => {
            let text = inner.to_string();
            text.starts_with("http error")
                || text.starts_with("stream closed")
                || text.contains("cursor stream ended prematurely")
                || text.contains("stream not found")
                || text.contains("SQLITE_BUSY")
                || text.contains("SQLITE_LOCKED")
                || text.starts_with("api error: `status=5")
        }
        _ => false,
    }
}

/// A write rejected by device-certificate-signature auth.
///
/// `Unauthorized` (401): the request couldn't be tied to a known device key —
/// missing headers, bad timestamp (replay window), unknown/revoked device, or
/// an invalid signature. Also the catch-all for any non-database error on the
/// auth path, so we never fail open.
///
/// `Forbidden` (403): the signature verified for `(user_id, device_id)`, but
/// the commit's `sender_id` is some *other* user — a validly-signed request
/// trying to write as someone else.
///
/// `Unavailable` (503): the device's key could not be read because the database
/// failed. Still a refusal — the request is not authenticated — but one the
/// client treats as "try again", not "signed out" (#1500).
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
    /// The device-key lookup failed on the database (#1500).
    Unavailable,
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
            AuthRejection::Unavailable => return unavailable(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hrana(text: &str) -> anyhow::Error {
        anyhow::Error::new(libsql::Error::Hrana(text.into()))
    }

    /// The renderings seen in the DS error log, and the local backend's codes.
    #[test]
    fn transient_database_failures_are_recognised() {
        for transient in [
            hrana("http error: `connection closed before message completed`"),
            hrana(r#"api error: `status=404 Not Found, body={"error":"stream not found: 4a:10"}`"#),
            hrana("api error: `status=502 Bad Gateway, body=`"),
            hrana(r#"stream error: `Error { message: "database is locked", code: "SQLITE_BUSY" }`"#),
            hrana("stream closed: `gone`"),
            anyhow::Error::new(libsql::Error::ConnectionFailed("refused".into())),
            anyhow::Error::new(libsql::Error::SqliteFailure(5, "database is locked".into())),
            anyhow::Error::new(libsql::Error::SqliteFailure(517, "busy snapshot".into())),
            anyhow::Error::new(libsql::Error::RemoteSqliteFailure(6, 6, "locked".into())),
            hrana("http error: `timed out`").context("claim key package"),
        ] {
            assert!(is_transient_db_error(&transient), "{transient:#}");
        }
    }

    #[test]
    fn real_failures_stay_500s() {
        for real in [
            hrana(r#"stream error: `Error { message: "UNIQUE constraint failed", code: "SQLITE_CONSTRAINT" }`"#),
            hrana("api error: `status=401 Unauthorized, body=`"),
            anyhow::Error::new(libsql::Error::SqliteFailure(19, "constraint".into())),
            anyhow::Error::new(libsql::Error::InvalidColumnType),
            anyhow::anyhow!("stream not found, but not from the database"),
        ] {
            assert!(!is_transient_db_error(&real), "{real:#}");
        }
    }

    async fn status_and_error(resp: Response) -> (StatusCode, String) {
        let status = resp.status();
        let body = axum::body::to_bytes(resp.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        (status, v["error"].as_str().unwrap().to_string())
    }

    #[tokio::test]
    async fn a_transient_failure_is_a_503_and_anything_else_a_500() {
        let transient =
            AppError::from(hrana("http error: `connection closed before message completed`"));
        assert_eq!(
            status_and_error(transient.into_response()).await,
            (StatusCode::SERVICE_UNAVAILABLE, "unavailable".to_string())
        );
        let real = AppError::from(anyhow::anyhow!("no such column: x"));
        assert_eq!(
            status_and_error(real.into_response()).await,
            (StatusCode::INTERNAL_SERVER_ERROR, "internal error".to_string())
        );
    }

    #[tokio::test]
    async fn an_unavailable_auth_lookup_is_a_503_not_a_401() {
        let resp = AuthRejection::Unavailable.into_response();
        assert!(resp.headers().get(AUTH_ERROR_HEADER).is_none());
        assert_eq!(
            status_and_error(resp).await,
            (StatusCode::SERVICE_UNAVAILABLE, "unavailable".to_string())
        );
    }
}
