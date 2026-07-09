//! Baseline security response headers for every DS reply (#345).
//!
//! The DS is a JSON API that returns session tokens and user metadata, so its
//! responses must not be cached by any intermediary and shouldn't be sniffed or
//! leak a referrer. Applied as one middleware over the whole router (including
//! error + 429 responses), so no handler has to remember to set them.

use axum::extract::Request;
use axum::http::header::{
    HeaderValue, CACHE_CONTROL, REFERRER_POLICY, X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::middleware::Next;
use axum::response::Response;

/// Axum middleware: set conservative security headers on every response.
/// `insert` overwrites, so these are authoritative for the DS.
pub async fn security_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    // Responses carry session tokens / user data — never cache them anywhere.
    h.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // Defence-in-depth even for a JSON API.
    h.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    resp
}
