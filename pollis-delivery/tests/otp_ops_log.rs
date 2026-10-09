//! The dev-only request-otp / rate-limit ops log (`src/ops_log.rs`), driven
//! through the real axum router with `tower::oneshot`.
//!
//! Three things must hold, and each has a test:
//! 1. **Prod is untouched.** Off by default, the limiter answers exactly as it
//!    always did and the operator route does not exist; and `wrangler.prod.jsonc`
//!    can never switch it on.
//! 2. **On, it observes without changing a verdict.** The same sequence of
//!    requests gets the same statuses and the same 429 body with the log on.
//! 3. **On, it records who spent the budget — never an IP or an address.** Each
//!    event carries a client pseudonym, the user agent, the email's domain and a
//!    keyed tag, and the operator view contains no raw IP and no full email.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use pollis_delivery::otp::OtpConfig;
use pollis_delivery::ratelimit::RateLimitConfig;
use pollis_delivery::{build_router_with_state, AppState};
use tower::ServiceExt as _;

mod common;

const TOKEN: &str = "ops-token";
const IP_A: &str = "203.0.113.41";
const IP_B: &str = "198.51.100.42";

/// request-otp never touches a table, so a bare DB is enough.
async fn fresh_db() -> common::TempDb {
    common::TempDb::open("ops.db").await
}

/// `request_otp_max` 2 per 10 min, `write` 1 per minute, `DEV_OTP` so nothing
/// is mailed, and no per-email throttle so only the per-client limit is in play.
fn state(db: Arc<pollis_delivery::db::Db>, ops_log: bool) -> AppState {
    AppState::new(db, false)
        .with_otp_config(OtpConfig {
            resend_api_key: None,
            dev_otp: Some("123456".to_string()),
            resend_throttle_secs: 0,
            ..OtpConfig::default()
        })
        .with_ratelimit_config(RateLimitConfig {
            request_otp_max: 2,
            request_otp_window_secs: 600,
            write_max: 1,
            write_window_secs: 60,
            ..RateLimitConfig::default()
        })
        .with_metrics_token(Some(TOKEN.into()))
        .with_ops_log(ops_log)
}

async fn call(state: &AppState, req: Request<Body>) -> (StatusCode, Vec<u8>) {
    let resp = build_router_with_state(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, body)
}

fn post(path: &str, ip: &str, ua: Option<&str>, body: serde_json::Value) -> Request<Body> {
    let mut b = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("CF-Connecting-IP", ip);
    if let Some(ua) = ua {
        b = b.header("user-agent", ua);
    }
    b.body(Body::from(serde_json::to_vec(&body).unwrap())).unwrap()
}

fn otp(ip: &str, ua: Option<&str>, email: &str) -> Request<Body> {
    post("/v1/auth/request-otp", ip, ua, serde_json::json!({ "email": email }))
}

fn ops_view(token: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method("GET").uri("/v1/ops/otp-requests");
    if let Some(t) = token {
        b = b.header("Authorization", format!("Bearer {t}"));
    }
    b.body(Body::empty()).unwrap()
}

/// The sequence both on/off tests drive: three request-otps from A (the third
/// over budget), one from B, and two writes from A (the second over budget).
async fn drive(state: &AppState) -> Vec<(StatusCode, Vec<u8>)> {
    let mut out = Vec::new();
    for (i, email) in ["e2e-1@example.com", "e2e-2@example.com", "e2e-3@example.com"].iter().enumerate() {
        let ua = if i == 0 { Some("curl/8.7.1") } else { None };
        out.push(call(state, otp(IP_A, ua, email)).await);
    }
    out.push(call(state, otp(IP_B, Some("probe/2.0"), "loop@example.org")).await);
    for _ in 0..2 {
        out.push(call(state, post("/v1/profile/update", IP_A, None, serde_json::json!({}))).await);
    }
    out
}

/// Off (the production state): the limiter's answers are the ones it always
/// gave, and the operator route is not served even with the right token.
#[tokio::test]
async fn off_by_default_the_route_does_not_exist() {
    let db = fresh_db().await;
    let st = state(db.arc(), false);
    assert!(st.ops_log.is_none(), "no recorder exists unless the gate is on");
    let results = drive(&st).await;
    let statuses: Vec<StatusCode> = results.iter().map(|(s, _)| *s).collect();
    assert_eq!(statuses[..4], [StatusCode::OK, StatusCode::OK, StatusCode::TOO_MANY_REQUESTS, StatusCode::OK]);
    assert_eq!(statuses[5], StatusCode::TOO_MANY_REQUESTS);

    let (status, body) = call(&st, ops_view(Some(TOKEN))).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(body.is_empty(), "an off route must not describe itself");
}

/// On, every response — status AND body — is identical to off. The log
/// observes the limiter; it never decides anything.
#[tokio::test]
async fn on_it_changes_no_status_and_no_body() {
    let db_off = fresh_db().await;
    let db_on = fresh_db().await;
    let off = drive(&state(db_off.arc(), false)).await;
    let on = drive(&state(db_on.arc(), true)).await;
    assert_eq!(off, on, "the ops log altered a response");
}

/// On, the operator view attributes the budget: per-client counts, outcomes,
/// user agents and email domains — and no raw IP or full address anywhere.
#[tokio::test]
async fn on_it_records_who_spent_the_budget_without_ips_or_emails() {
    let db = fresh_db().await;
    let st = state(db.arc(), true);
    drive(&st).await;

    let (status, body) = call(&st, ops_view(Some(TOKEN))).await;
    assert_eq!(status, StatusCode::OK);
    let raw = String::from_utf8(body.clone()).unwrap();
    for secret in [IP_A, IP_B, "e2e-1@", "e2e-2@", "e2e-3@", "loop@", TOKEN] {
        assert!(!raw.contains(secret), "the ops view leaked {secret}: {raw}");
    }
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();

    // 4 request-otps + the 1 rate-limited write; the allowed write is not an
    // OTP request and was not limited, so it is not recorded.
    assert_eq!(v["total_recorded"], 5);
    let events = v["events"].as_array().unwrap();
    assert_eq!(events.len(), 5);

    // Newest first: the limited write, then B's request-otp, then A's three.
    let write = &events[0];
    assert_eq!(write["endpoint"], "/v1/profile/update");
    assert_eq!(write["tier"], "write");
    assert_eq!(write["outcome"], "rate_limited");
    assert_eq!(write["status"], 429);
    assert!(write["email_domain"].is_null() && write["email_tag"].is_null());

    let b = &events[1];
    assert_eq!(b["outcome"], "accepted");
    assert_eq!(b["user_agent"], "probe/2.0");
    assert_eq!(b["email_domain"], "example.org");

    let a3 = &events[2];
    assert_eq!(a3["outcome"], "rate_limited", "A's third request-otp was over budget");
    assert_eq!(a3["status"], 429);
    assert_eq!(a3["email_domain"], "example.com", "a limited request-otp still records its domain");
    assert!(a3["user_agent"].is_null(), "no agent sent → none recorded");
    assert_eq!(events[4]["user_agent"], "curl/8.7.1");

    // A is one client across tiers; B is another. Pseudonyms are 32 hex chars.
    let client_a = a3["client"].as_str().unwrap();
    assert_eq!(client_a.len(), 32);
    assert!(client_a.bytes().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(write["client"], client_a, "one client, one pseudonym, every tier");
    assert_ne!(b["client"], client_a);

    // Three fresh addresses → three distinct tags.
    let tags: std::collections::BTreeSet<&str> =
        events[2..].iter().map(|e| e["email_tag"].as_str().unwrap()).collect();
    assert_eq!(tags.len(), 3);

    // The summary puts the busiest client first with its outcome split.
    let top = &v["clients"][0];
    assert_eq!(top["client"], client_a);
    assert_eq!(top["requests"], 4);
    assert_eq!(top["accepted"], 2);
    assert_eq!(top["rate_limited"], 2);
    assert_eq!(top["distinct_emails"], 3);
    assert_eq!(top["endpoints"]["/v1/auth/request-otp"], 3);
    assert_eq!(top["user_agents"], serde_json::json!(["(none)", "curl/8.7.1"]));
}

/// A retry loop is visible as such: many requests, ONE email tag.
#[tokio::test]
async fn a_retry_loop_shows_as_one_address_many_requests() {
    let db = fresh_db().await;
    let st = state(db.arc(), true);
    for _ in 0..4 {
        call(&st, otp(IP_B, None, " Same@Example.com ")).await;
    }
    let (_, body) = call(&st, ops_view(Some(TOKEN))).await;
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let top = &v["clients"][0];
    assert_eq!(top["requests"], 4);
    assert_eq!(top["distinct_emails"], 1);
    assert_eq!(top["rate_limited"], 2);
}

/// The route has the same operator gate as `/v1/config`: 401 without the token.
#[tokio::test]
async fn the_ops_view_requires_the_operator_token() {
    let db = fresh_db().await;
    let st = state(db.arc(), true);
    assert_eq!(call(&st, ops_view(None)).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(call(&st, ops_view(Some("wrong"))).await.0, StatusCode::UNAUTHORIZED);
    // On but no operator token configured → not served at all.
    let st = st.with_metrics_token(None);
    assert_eq!(call(&st, ops_view(Some(TOKEN))).await.0, StatusCode::NOT_FOUND);
}

/// `/v1/config` reports the OTP tiers in force and whether the log is on, so
/// "is dev's raised budget actually live?" is checkable from outside.
#[tokio::test]
async fn config_reports_the_otp_tiers_and_the_ops_log() {
    let db = fresh_db().await;
    for on in [false, true] {
        let st = state(db.arc(), on);
        let req = Request::builder()
            .method("GET")
            .uri("/v1/config")
            .header("Authorization", format!("Bearer {TOKEN}"))
            .body(Body::empty())
            .unwrap();
        let (status, body) = call(&st, req).await;
        assert_eq!(status, StatusCode::OK);
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["request_otp_max"], 2);
        assert_eq!(v["request_otp_window_secs"], 600);
        assert_eq!(v["ops_log"], on);
    }
}

fn wrangler_vars(env: &str) -> serde_json::Map<String, serde_json::Value> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(format!("wrangler.{env}.jsonc"));
    let raw = std::fs::read_to_string(path).expect("read wrangler config");
    let json: String = raw
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let cfg: serde_json::Value = serde_json::from_str(&json).expect("parse wrangler config");
    cfg["vars"].as_object().cloned().unwrap_or_default()
}

/// The hard gate: production never sets the ops log (nor anything that would
/// change its limits — prod keeps the compiled-in defaults), and dev does.
#[test]
fn only_dev_turns_the_ops_log_on() {
    let prod = wrangler_vars("prod");
    assert!(
        !prod.contains_key("POLLIS_DS_OPS_LOG"),
        "wrangler.prod.jsonc must not set POLLIS_DS_OPS_LOG"
    );
    let tiers: Vec<&String> = prod.keys().filter(|k| k.starts_with("RL_")).collect();
    assert!(tiers.is_empty(), "wrangler.prod.jsonc must keep the default rate limits, found {tiers:?}");

    let dev = wrangler_vars("dev");
    assert_eq!(dev.get("POLLIS_DS_OPS_LOG").and_then(|v| v.as_str()), Some("true"));
    // Dev runs request-otp unlimited for the parallel e2e suites, but never
    // verify-otp: `DEV_OTP` is one code for every mailbox, so that tier is the
    // only bound on guessing it across rotated addresses.
    assert_eq!(dev.get("RL_REQUEST_OTP_MAX").and_then(|v| v.as_str()), Some("off"));
    let verify = dev.get("RL_VERIFY_OTP_MAX").and_then(|v| v.as_str());
    assert!(
        verify.is_some_and(|v| v.parse::<u32>().is_ok()),
        "dev must keep a numeric verify-otp limit, found {verify:?}"
    );
}
