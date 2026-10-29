//! Baseline security response headers for every DS reply (#345). One middleware
//! over the whole router, so error and 429 responses carry them too.

use axum::extract::Request;
use axum::http::header::{
    HeaderValue, CACHE_CONTROL, REFERRER_POLICY, STRICT_TRANSPORT_SECURITY,
    X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
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
    h.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    // Emitted unconditionally: TLS terminates at the edge, so the DS only ever sees
    // HTTP from the Worker. `preload` is omitted on purpose: it is hard to reverse
    // and also binds the apex.
    h.insert(
        STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=63072000; includeSubDomains"),
    );
    resp
}
