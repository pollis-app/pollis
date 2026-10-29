//! Baseline security response headers for every DS reply (#345).

use axum::extract::Request;
use axum::http::header::{
    HeaderValue, CACHE_CONTROL, REFERRER_POLICY, STRICT_TRANSPORT_SECURITY,
    X_CONTENT_TYPE_OPTIONS, X_FRAME_OPTIONS,
};
use axum::middleware::Next;
use axum::response::Response;

/// Axum middleware: set conservative security headers on every response.
pub async fn security_headers(req: Request, next: Next) -> Response {
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    // Responses carry session tokens / user data — never cache them anywhere.
    h.insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    // HSTS unconditionally: TLS ends at the edge, so the DS only sees HTTP. No `preload`, on purpose.
    h.insert(
        STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=63072000; includeSubDomains"),
    );
    resp
}
