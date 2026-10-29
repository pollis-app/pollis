//! HTTP error mapping. Any internal failure becomes a 500 with a terse JSON body.
//! A transient database failure is a 503 `{"error":"unavailable"}` instead (#1500).
//!
//! [`AuthRejection`] is the one *expected* refusal; it never fails open.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};

/// `Debug` forwards to `anyhow::Error`'s, which prints the whole cause chain.
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
        // Here, not in `into_response`: the pool must hear of the fault before connections are released.
        note_transient(&err);
        Self(err)
    }
}

/// 503 `{"error":"unavailable"}`: "try again", never "you are not signed in".
fn unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [(axum::http::header::RETRY_AFTER, "1")],
        Json(serde_json::json!({ "error": "unavailable" })),
    )
        .into_response()
}

/// Whether `err` is a database failure that says nothing about the request.
pub fn is_transient_db_error(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|cause| cause.downcast_ref::<libsql::Error>())
        .any(is_transient_libsql_error)
}

/// [`is_transient_db_error`], also telling the pool not to reuse checked-out connections.
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
        // libsql's hrana error type is private, so its rendering is all there is to go on.
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
/// `Unauthorized` (401): not tied to a live device key; also the catch-all, so never fail open.
///
/// `Forbidden` (403): the signature verified, but `sender_id` is another user.
///
/// `Unavailable` (503): the key lookup failed on the database (#1500).
#[derive(Debug, PartialEq, Eq)]
pub enum AuthRejection {
    Unauthorized,
    Forbidden,
    /// Signed by a revoked device's own key (#1338), so it can sign out instead of retrying.
    DeviceRevoked,
    /// The device-key lookup failed on the database (#1500).
    Unavailable,
}

/// Header on a [`AuthRejection::DeviceRevoked`] answer, readable without the body.
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
