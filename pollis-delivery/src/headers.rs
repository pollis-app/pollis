//! Baseline security response headers for every DS reply (#345).
//!
//! The DS is a JSON API that returns session tokens and user metadata, so its
//! responses must not be cached by any intermediary and shouldn't be sniffed or
//! leak a referrer. Applied as one middleware over the whole router (including
//! error + 429 responses), so no handler has to remember to set them.

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
    // Defence-in-depth even for a JSON API.
    h.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    // Pin the API to HTTPS for two years. The shipped clients build their URL
    // from a hardcoded `https://` scheme, so this is not what protects them —
    // it closes the gap for anything that reaches the DS through a browser or
    // by hand, where a single plaintext request is enough to be stripped. The
    // header is emitted unconditionally: TLS terminates at the edge, so the DS
    // itself only ever sees plain HTTP from the Worker, and a UA ignores HSTS
    // received over HTTP anyway. `includeSubDomains` scopes to `*.api…` only
    // (the apex is a different host and is unaffected). `preload` is
    // deliberately omitted — that is a separate, hard-to-reverse commitment
    // that also requires the apex, so it should be an explicit decision.
    h.insert(
        STRICT_TRANSPORT_SECURITY,
        HeaderValue::from_static("max-age=63072000; includeSubDomains"),
    );
    resp
}
