//! The alerts webhook listener (#1182, "Built-in alerts handler").
//!
//! `POST /hooks/<installation_id>` with `{"channel": "...", "text": "..."}`.
//! Off until configured; binds loopback by default. Authenticated per
//! installation, by either:
//!
//! - `Authorization: Bearer <secret>`, or
//! - `X-Pollis-Signature: sha256=<hex HMAC-SHA256(secret, raw body)>`, for
//!   senders that sign.
//!
//! An optional `X-Pollis-Delivery: <id>` names the delivery; a sender that
//! posts the same alert to several hosts of one bot sends the same id to each,
//! and exactly one of them posts it. Without it each call is its own delivery.
//!
//! Checked in this order: installation known (404), authenticated (401), within
//! the per-installation rate limit (429 + `Retry-After`), well-formed (400).
//! Only then does a job run.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::json;
use sha2::Sha256;
use subtle::ConstantTimeEq;
use tokio::sync::oneshot;
use zeroize::Zeroizing;

use crate::job::{JobError, JobReport, TriggerOutcome};
use crate::ratelimit::TokenBucket;
use crate::trigger::WebhookCall;

/// Largest request body accepted.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// How long a caller waits for the job's answer before getting `202`; the job
/// still runs to completion.
pub const ANSWER_TIMEOUT: Duration = Duration::from_secs(30);

pub const SIGNATURE_HEADER: &str = "x-pollis-signature";
pub const DELIVERY_HEADER: &str = "x-pollis-delivery";

/// Hands a webhook call to the job loop. The job runs on its own task, so a
/// caller that disconnects never cancels it halfway.
pub trait JobSubmitter: Send + Sync {
    fn submit_webhook(
        &self,
        installation_id: &str,
        call: WebhookCall,
    ) -> oneshot::Receiver<Result<JobReport, JobError>>;
}

/// One installation's webhook credentials and limiter.
pub struct HookEntry {
    secret: Zeroizing<Vec<u8>>,
    limiter: std::sync::Mutex<TokenBucket>,
}

impl HookEntry {
    pub fn new(secret: Zeroizing<Vec<u8>>, limiter: TokenBucket) -> Self {
        Self { secret, limiter: std::sync::Mutex::new(limiter) }
    }
}

pub struct WebhookState {
    hooks: HashMap<String, HookEntry>,
    submitter: Arc<dyn JobSubmitter>,
    answer_timeout: Duration,
}

impl WebhookState {
    pub fn new(hooks: HashMap<String, HookEntry>, submitter: Arc<dyn JobSubmitter>) -> Self {
        Self { hooks, submitter, answer_timeout: ANSWER_TIMEOUT }
    }

    #[cfg(test)]
    pub(crate) fn with_answer_timeout(mut self, timeout: Duration) -> Self {
        self.answer_timeout = timeout;
        self
    }
}

pub fn router(state: Arc<WebhookState>) -> Router {
    Router::new()
        .route("/hooks/{installation_id}", post(handle_hook))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

#[derive(Deserialize)]
struct HookBody {
    channel: String,
    text: String,
}

/// HMAC-SHA256 of `body` under `secret`, as the `X-Pollis-Signature` value.
pub fn sign(secret: &[u8], body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret).expect("HMAC takes any key length");
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

fn authenticated(secret: &[u8], headers: &HeaderMap, body: &[u8]) -> bool {
    if let Some(sig) = headers.get(SIGNATURE_HEADER) {
        let Some(hex_sig) = sig.to_str().ok().and_then(|s| s.strip_prefix("sha256=")) else {
            return false;
        };
        let Ok(given) = hex::decode(hex_sig.trim()) else {
            return false;
        };
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(secret).expect("HMAC takes any key length");
        mac.update(body);
        return mac.verify_slice(&given).is_ok();
    }
    let Some(bearer) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return false;
    };
    bool::from(bearer.trim().as_bytes().ct_eq(secret))
}

fn valid_delivery_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn answer(status: StatusCode, body: serde_json::Value) -> Response {
    (status, Json(body)).into_response()
}

async fn handle_hook(
    State(state): State<Arc<WebhookState>>,
    Path(installation_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(entry) = state.hooks.get(&installation_id) else {
        return answer(
            StatusCode::NOT_FOUND,
            json!({"status": "unknown_installation", "fix": "use an installation id from `pollis-bot status` on this host"}),
        );
    };
    if !authenticated(&entry.secret, &headers, &body) {
        crate::log::failure(
            &crate::log::scope(&installation_id),
            "webhook call with a wrong secret or signature",
            "configure the sender with this installation's secret (`pollis-bot rotate-secret` issues a new one)",
        );
        return answer(
            StatusCode::UNAUTHORIZED,
            json!({"status": "unauthorized", "fix": "send `Authorization: Bearer <secret>` or `X-Pollis-Signature: sha256=<hmac>`"}),
        );
    }
    let limited = {
        let mut limiter = entry.limiter.lock().unwrap_or_else(|p| p.into_inner());
        limiter.try_take(Instant::now())
    };
    if let Err(wait) = limited {
        let secs = wait.as_secs().max(1);
        let mut response = answer(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"status": "rate_limited", "retry_after_secs": secs}),
        );
        if let Ok(v) = HeaderValue::from_str(&secs.to_string()) {
            response.headers_mut().insert(axum::http::header::RETRY_AFTER, v);
        }
        return response;
    }
    let Ok(parsed) = serde_json::from_slice::<HookBody>(&body) else {
        return answer(
            StatusCode::BAD_REQUEST,
            json!({"status": "bad_request", "fix": "send a JSON body {\"channel\": \"...\", \"text\": \"...\"}"}),
        );
    };
    let delivery_id = match headers.get(DELIVERY_HEADER) {
        Some(v) => match v.to_str() {
            Ok(id) if valid_delivery_id(id) => id.to_string(),
            _ => {
                return answer(
                    StatusCode::BAD_REQUEST,
                    json!({"status": "bad_request", "fix": "X-Pollis-Delivery must be 1-128 characters of A-Z a-z 0-9 . _ : -"}),
                )
            }
        },
        None => ulid::Ulid::new().to_string(),
    };

    let call = WebhookCall { delivery_id: delivery_id.clone(), channel: parsed.channel, text: parsed.text };
    let rx = state.submitter.submit_webhook(&installation_id, call);
    match tokio::time::timeout(state.answer_timeout, rx).await {
        Err(_) => answer(StatusCode::ACCEPTED, json!({"status": "accepted", "delivery_id": delivery_id})),
        Ok(Err(_)) => answer(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"status": "failed", "delivery_id": delivery_id, "error": "the runtime is shutting down", "fix": "retry with the same X-Pollis-Delivery once it is running"}),
        ),
        Ok(Ok(result)) => job_answer(&delivery_id, result),
    }
}

fn job_answer(delivery_id: &str, result: Result<JobReport, JobError>) -> Response {
    let failed = |e: &JobError| {
        answer(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"status": "failed", "delivery_id": delivery_id, "error": e.to_string(), "fix": e.fix()}),
        )
    };
    let report = match result {
        Ok(report) => report,
        Err(e) => return failed(&e),
    };
    let Some(webhook) = report.webhook() else {
        return answer(StatusCode::INTERNAL_SERVER_ERROR, json!({"status": "failed", "delivery_id": delivery_id}));
    };
    match &webhook.result {
        Err(e) => failed(e),
        Ok(TriggerOutcome::NoActions) => {
            answer(StatusCode::OK, json!({"status": "ignored", "delivery_id": delivery_id}))
        }
        Ok(TriggerOutcome::ClaimedElsewhere) => {
            answer(StatusCode::OK, json!({"status": "claimed_elsewhere", "delivery_id": delivery_id}))
        }
        Ok(TriggerOutcome::HandlerFailed { error }) => answer(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({"status": "failed", "delivery_id": delivery_id, "error": error}),
        ),
        Ok(TriggerOutcome::Applied { posted: 0, refused }) => {
            let reasons: Vec<String> = refused.iter().map(|r| r.to_string()).collect();
            answer(
                StatusCode::UNPROCESSABLE_ENTITY,
                json!({"status": "refused", "delivery_id": delivery_id, "refused": reasons}),
            )
        }
        Ok(TriggerOutcome::Applied { posted, refused }) => {
            let reasons: Vec<String> = refused.iter().map(|r| r.to_string()).collect();
            answer(
                StatusCode::OK,
                json!({"status": "posted", "delivery_id": delivery_id, "posted": posted, "refused": reasons}),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::TriggerResult;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use std::sync::Mutex;
    use tower::ServiceExt;

    const SECRET: &[u8] = b"pbw_test-secret";

    /// Records submitted calls and answers `posted`.
    #[derive(Default)]
    struct FakeSubmitter {
        calls: Mutex<Vec<(String, WebhookCall)>>,
    }

    impl JobSubmitter for FakeSubmitter {
        fn submit_webhook(
            &self,
            installation_id: &str,
            call: WebhookCall,
        ) -> oneshot::Receiver<Result<JobReport, JobError>> {
            let key = crate::trigger::Trigger::Webhook(call.clone()).key();
            self.calls.lock().unwrap().push((installation_id.to_string(), call));
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(Ok(JobReport {
                triggers: vec![TriggerResult {
                    key,
                    kind: "webhook",
                    result: Ok(TriggerOutcome::Applied { posted: 1, refused: vec![] }),
                }],
            }));
            rx
        }
    }

    fn app(burst: u32) -> (Router, Arc<FakeSubmitter>) {
        let submitter = Arc::new(FakeSubmitter::default());
        let mut hooks = HashMap::new();
        hooks.insert(
            "INST".to_string(),
            HookEntry::new(Zeroizing::new(SECRET.to_vec()), TokenBucket::new(1, burst, Instant::now())),
        );
        let state = WebhookState::new(hooks, submitter.clone());
        (router(Arc::new(state)), submitter)
    }

    const BODY: &str = r#"{"channel":"alerts","text":"disk full"}"#;

    fn request(path: &str, headers: &[(&str, &str)], body: &str) -> Request<Body> {
        let mut b = Request::post(path).header("content-type", "application/json");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        b.body(Body::from(body.to_string())).unwrap()
    }

    async fn send(app: &Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
        let res = app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null))
    }

    fn bearer() -> String {
        format!("Bearer {}", std::str::from_utf8(SECRET).unwrap())
    }

    #[tokio::test]
    async fn the_right_secret_posts() {
        let (app, submitter) = app(10);
        let (status, body) = send(&app, request("/hooks/INST", &[("authorization", &bearer())], BODY)).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["status"], "posted");
        let calls = submitter.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "INST");
        assert_eq!(calls[0].1.channel, "alerts");
        assert_eq!(calls[0].1.text, "disk full");
    }

    #[tokio::test]
    async fn a_wrong_or_missing_secret_is_refused_and_runs_nothing() {
        let (app, submitter) = app(10);
        for headers in [
            vec![("authorization", "Bearer pbw_wrong")],
            vec![("authorization", "pbw_test-secret")],
            vec![],
        ] {
            let (status, _) = send(&app, request("/hooks/INST", &headers, BODY)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{headers:?}");
        }
        assert!(submitter.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_valid_hmac_posts_and_a_bad_one_is_refused() {
        let (app, submitter) = app(10);
        let good = sign(SECRET, BODY.as_bytes());
        let (status, _) = send(&app, request("/hooks/INST", &[(SIGNATURE_HEADER, &good)], BODY)).await;
        assert_eq!(status, StatusCode::OK);

        let wrong_key = sign(b"pbw_other", BODY.as_bytes());
        let other_body = sign(SECRET, br#"{"channel":"alerts","text":"other"}"#);
        for sig in [wrong_key.as_str(), other_body.as_str(), "sha256=zz", "md5=abcd"] {
            let (status, _) = send(&app, request("/hooks/INST", &[(SIGNATURE_HEADER, sig)], BODY)).await;
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{sig}");
        }
        // A bad signature is not rescued by a correct bearer alongside it.
        let (status, _) = send(
            &app,
            request("/hooks/INST", &[(SIGNATURE_HEADER, &wrong_key), ("authorization", &bearer())], BODY),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
        assert_eq!(submitter.calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_unknown_installation_is_not_found() {
        let (app, submitter) = app(10);
        let (status, body) = send(&app, request("/hooks/NOPE", &[("authorization", &bearer())], BODY)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["status"], "unknown_installation");
        assert!(submitter.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn over_the_rate_limit_is_429_with_retry_after() {
        let (app, submitter) = app(2);
        for _ in 0..2 {
            let (status, _) = send(&app, request("/hooks/INST", &[("authorization", &bearer())], BODY)).await;
            assert_eq!(status, StatusCode::OK);
        }
        let res = app
            .clone()
            .oneshot(request("/hooks/INST", &[("authorization", &bearer())], BODY))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        assert!(res.headers().get("retry-after").is_some());
        assert_eq!(submitter.calls.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_malformed_body_or_delivery_id_is_400() {
        let (app, submitter) = app(10);
        let (status, _) = send(&app, request("/hooks/INST", &[("authorization", &bearer())], "{\"text\":1}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (status, _) = send(
            &app,
            request("/hooks/INST", &[("authorization", &bearer()), (DELIVERY_HEADER, "has space")], BODY),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(submitter.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn the_delivery_header_names_the_trigger() {
        let (app, submitter) = app(10);
        let (_, body) = send(
            &app,
            request("/hooks/INST", &[("authorization", &bearer()), (DELIVERY_HEADER, "grafana:42")], BODY),
        )
        .await;
        assert_eq!(body["delivery_id"], "grafana:42");
        assert_eq!(submitter.calls.lock().unwrap()[0].1.delivery_id, "grafana:42");
    }

    /// A submitter whose job never answers in time.
    struct SlowSubmitter(Mutex<Vec<oneshot::Sender<Result<JobReport, JobError>>>>);

    impl JobSubmitter for SlowSubmitter {
        fn submit_webhook(&self, _: &str, _: WebhookCall) -> oneshot::Receiver<Result<JobReport, JobError>> {
            let (tx, rx) = oneshot::channel();
            self.0.lock().unwrap().push(tx);
            rx
        }
    }

    #[tokio::test]
    async fn a_slow_job_answers_202_and_keeps_running() {
        let submitter = Arc::new(SlowSubmitter(Mutex::new(Vec::new())));
        let mut hooks = HashMap::new();
        hooks.insert(
            "INST".to_string(),
            HookEntry::new(Zeroizing::new(SECRET.to_vec()), TokenBucket::new(60, 10, Instant::now())),
        );
        let state = WebhookState::new(hooks, submitter.clone()).with_answer_timeout(Duration::from_millis(20));
        let app = router(Arc::new(state));
        let (status, body) = send(&app, request("/hooks/INST", &[("authorization", &bearer())], BODY)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{body}");
        assert_eq!(submitter.0.lock().unwrap().len(), 1);
    }
}
