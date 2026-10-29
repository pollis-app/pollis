//! Server-side OTP: generation, salted-hash storage, attempt-limited constant-time checks, Resend sends.
//!
//! Each code is attempt-limited per mailbox, constant-time compared and single-use.
//!
//! **Store:** in-memory, behind [`OtpStore`] (the DS is single-container).
//!
//! **Not durable, and the guess budget depends on it (#1142):** scale-to-zero drops the counters.
//!
//! Safe **only because `sleepAfter >= ttl_secs`**: the silence that resets a counter expires the code.
//!
//! `tests/otp_state_durability.rs` pins that inequality across the two languages.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use rand::rngs::OsRng;
use rand::{Rng, RngCore};
use sha2::{Digest, Sha256};
use ulid::Ulid;

use crate::redact::mask_email;
use crate::session::SessionStore;
use crate::writes::bad_request;
use crate::AppState;

// Request bodies live in `pollis-api`, shared with pollis-core; re-exported here.
pub use pollis_api::otp::*;

/// Tunables for the OTP + session machinery, read from DS env in
/// [`OtpConfig::from_env`].
#[derive(Clone)]
pub struct OtpConfig {
    /// Resend API key (`RESEND_API_KEY`); `None` skips the send.
    pub resend_api_key: Option<String>,
    /// `DEV_OTP`: skip the send and accept only this exact code (harness, local dev).
    pub dev_otp: Option<String>,
    /// OTP lifetime, seconds (env `OTP_TTL_SECS`, default 600).
    pub ttl_secs: u64,
    /// Session-token lifetime, seconds (default 600).
    pub session_ttl_secs: u64,
    /// Minimum seconds between two emails for the same address.
    pub resend_throttle_secs: u64,
    /// Wrong-guess lockout threshold; the `(max+1)`-th wrong guess locks the
    /// MAILBOX (not merely the code) for `lockout_secs`.
    pub max_attempts: u32,
    /// How long a mailbox stays locked after `max_attempts` wrong guesses.
    /// Persisted on the mailbox record, so a fresh `request-otp` cannot clear it.
    pub lockout_secs: u64,
    /// Codes one mailbox may be sent per `ttl_secs` window; re-requests don't invalidate codes (#1088).
    pub max_sends_per_window: u32,
    /// The App Store / Play review account (`APP_REVIEW_LOGIN`): a fixed code, no email.
    pub review_login: Option<ReviewLogin>,
}

/// A store-review sign-in: one exact address and the fixed code that signs it
/// in. Parsed from `APP_REVIEW_LOGIN=<email>:<6 digits>` — one value rather
/// than two env vars so "an address with no code" is not configurable.
///
/// Only generation and the send change; storage, TTL, single use and lockout are as usual. Sign-in only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReviewLogin {
    email: String,
    code: String,
}

impl ReviewLogin {
    /// The only constructor: a normalized address and a six-digit code.
    pub fn parse(raw: &str) -> Result<Self, &'static str> {
        let (email, code) = raw.trim().rsplit_once(':').ok_or("expected <email>:<code>")?;
        let email = normalize_email(email);
        if !email.contains('@') {
            return Err("review email has no @");
        }
        if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err("review code must be exactly 6 digits");
        }
        Ok(Self { email, code: code.to_string() })
    }

    fn matches(&self, email: &str) -> bool {
        normalize_email(email) == self.email
    }
}

impl Default for OtpConfig {
    fn default() -> Self {
        Self {
            resend_api_key: None,
            dev_otp: None,
            ttl_secs: 600,
            session_ttl_secs: 600,
            resend_throttle_secs: 30,
            max_attempts: 5,
            lockout_secs: 900,
            max_sends_per_window: 3,
            review_login: None,
        }
    }
}

impl OtpConfig {
    /// Build from DS env: `RESEND_API_KEY`, `DEV_OTP`, `OTP_TTL_SECS`.
    ///
    /// **`OTP_TTL_SECS` above the container's `sleepAfter` re-opens #1142**; no test sees env overrides.
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            resend_api_key: std::env::var("RESEND_API_KEY").ok().filter(|s| !s.is_empty()),
            dev_otp: std::env::var("DEV_OTP").ok().filter(|s| !s.is_empty()),
            ttl_secs: std::env::var("OTP_TTL_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(defaults.ttl_secs),
            review_login: review_login_from_env(),
            ..defaults
        }
    }
}

/// A malformed `APP_REVIEW_LOGIN` disables the review login (at ERROR), not startup.
fn review_login_from_env() -> Option<ReviewLogin> {
    let raw = std::env::var("APP_REVIEW_LOGIN").ok().filter(|s| !s.trim().is_empty())?;
    match ReviewLogin::parse(&raw) {
        Ok(login) => {
            tracing::info!("store-review sign-in enabled for {}", mask_email(&login.email));
            Some(login)
        }
        Err(e) => {
            tracing::error!("APP_REVIEW_LOGIN ignored: {e}");
            None
        }
    }
}

/// One outstanding code. The code itself is never kept — only
/// `SHA-256(salt || code)`.
struct CodeSlot {
    code_hash: [u8; 32],
    salt: [u8; 16],
    expires_at: u64,
}

/// State for ONE mailbox, the unit of throttling and lockout (#1088).
///
/// Only ever amended: `failed`/`locked_until` survive every `prepare`, and codes accumulate.
struct MailboxState {
    /// Codes still inside their TTL, oldest first. Bounded by
    /// `max_sends_per_window`; expired slots are dropped lazily.
    codes: Vec<CodeSlot>,
    /// Wrong guesses since the last success. Reset only by
    /// [`OtpStore::consume`] (a completed sign-in) or by the lockout elapsing.
    failed: u32,
    /// When set and still in the future, every `check` is
    /// [`VerifyOutcome::LockedOut`] and every `prepare` is a silent no-op.
    locked_until: Option<u64>,
    /// Last time an email actually went out, for the resend throttle.
    last_sent_at: u64,
    /// Sends inside the current window, and when that window opened.
    sends: u32,
    window_start: u64,
}

impl MailboxState {
    fn new(now: u64) -> Self {
        Self {
            codes: Vec::new(),
            failed: 0,
            locked_until: None,
            last_sent_at: 0,
            sends: 0,
            window_start: now,
        }
    }

    /// `true` while a lockout is in force; an elapsed one clears the guess counter too.
    fn locked(&mut self, now: u64) -> bool {
        match self.locked_until {
            Some(until) if now < until => true,
            Some(_) => {
                self.locked_until = None;
                self.failed = 0;
                false
            }
            None => false,
        }
    }

    /// Drop codes past their TTL. Called before every read and write so an
    /// expired code is never counted as outstanding.
    fn prune(&mut self, now: u64) {
        self.codes.retain(|c| now <= c.expires_at);
    }

    /// Whether this mailbox is at its per-window send budget, rolling the window
    /// over first if it has elapsed.
    fn over_send_budget(&mut self, window_secs: u64, max_sends: u32, now: u64) -> bool {
        if now.saturating_sub(self.window_start) >= window_secs {
            self.window_start = now;
            self.sends = 0;
        }
        self.sends >= max_sends
    }
}

/// Map size past which [`prepare`](OtpStore::prepare) sweeps empty mailboxes (#1088).
const SWEEP_AT: usize = 1024;

/// In-memory OTP store keyed on the normalized email.
#[derive(Clone, Default)]
pub struct OtpStore {
    inner: Arc<Mutex<HashMap<String, MailboxState>>>,
}

/// Drop mailboxes holding nothing: no live code, no lockout in force, and a send
/// window that has already elapsed. Anything still carrying one of those is
/// load-bearing and stays.
///
/// So the guess budget refills at most once per OTP lifetime, not per request (#1088).
fn sweep(map: &mut HashMap<String, MailboxState>, ttl_secs: u64, now: u64) {
    map.retain(|_, rec| {
        rec.prune(now);
        let locked = rec.locked_until.is_some_and(|until| now < until);
        let window_open = now.saturating_sub(rec.window_start) < ttl_secs;
        !rec.codes.is_empty() || locked || window_open
    });
}

/// Normalize an email for store keying (trim + lowercase); shared with the email-change store.
pub(crate) fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

fn salted_hash(salt: &[u8; 16], code: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(salt);
    h.update(code.trim().as_bytes());
    h.finalize().into()
}

/// Constant-time byte compare — replicated from pollis-core's
/// `device_enrollment::constant_time_eq` to keep the dependency surface small.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Outcome of preparing a code for an email.
#[derive(Debug, PartialEq, Eq)]
pub enum PrepareOutcome {
    /// A fresh code was stored; email this plaintext.
    Send(String),
    /// Within the resend-throttle window, or at the per-mailbox send budget for
    /// this window; do NOT email, but still 200 the caller.
    Throttled,
    /// The mailbox is locked out: no email, no code (a request can't clear it). Still 200.
    LockedOut,
}

/// Outcome of verifying a submitted code.
#[derive(Debug, PartialEq, Eq)]
pub enum VerifyOutcome {
    Ok,
    Invalid,
    LockedOut,
    Expired,
    NotFound,
}

impl OtpStore {
    /// Store a fresh `code` for `email`, ADDING it to whatever codes the mailbox
    /// already has outstanding rather than replacing them.
    ///
    /// `LockedOut` / `Throttled` only decide whether an email goes out; the caller answers 200.
    pub fn prepare(&self, email: &str, code: &str, cfg: &OtpConfig, now: u64) -> PrepareOutcome {
        let key = normalize_email(email);
        let mut guard = self.inner.lock().expect("otp store mutex poisoned");
        if guard.len() >= SWEEP_AT {
            sweep(&mut guard, cfg.ttl_secs, now);
        }
        let rec = guard.entry(key).or_insert_with(|| MailboxState::new(now));
        rec.prune(now);

        // A persisted lockout is not clearable by requesting a new code.
        if rec.locked(now) {
            return PrepareOutcome::LockedOut;
        }
        if rec.last_sent_at > 0
            && now.saturating_sub(rec.last_sent_at) < cfg.resend_throttle_secs
        {
            return PrepareOutcome::Throttled;
        }
        if rec.over_send_budget(cfg.ttl_secs, cfg.max_sends_per_window, now) {
            return PrepareOutcome::Throttled;
        }

        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        rec.codes.push(CodeSlot {
            code_hash: salted_hash(&salt, code),
            salt,
            expires_at: now.saturating_add(cfg.ttl_secs),
        });
        rec.last_sent_at = now;
        rec.sends += 1;
        PrepareOutcome::Send(code.to_string())
    }

    /// Check `code` against every outstanding code WITHOUT consuming; wrong guesses count to a lockout.
    ///
    /// Success leaves the record: [`OtpStore::consume`] runs only after downstream success (#518).
    pub fn check(&self, email: &str, code: &str, cfg: &OtpConfig, now: u64) -> VerifyOutcome {
        let key = normalize_email(email);
        let mut guard = self.inner.lock().expect("otp store mutex poisoned");
        let rec = match guard.get_mut(&key) {
            Some(r) => r,
            None => return VerifyOutcome::NotFound,
        };
        if rec.locked(now) {
            return VerifyOutcome::LockedOut;
        }
        let had_codes = !rec.codes.is_empty();
        rec.prune(now);
        if rec.codes.is_empty() {
            // "Expired" and "no code" render differently; the mailbox record stays either way.
            return if had_codes {
                VerifyOutcome::Expired
            } else {
                VerifyOutcome::NotFound
            };
        }

        // Compare against every outstanding code. `fold` rather than `any` so the
        // work does not depend on which slot matched.
        let matched = rec.codes.iter().fold(false, |acc, slot| {
            acc | constant_time_eq(&salted_hash(&slot.salt, code), &slot.code_hash)
        });
        if matched {
            return VerifyOutcome::Ok;
        }

        rec.failed += 1;
        if rec.failed > cfg.max_attempts {
            rec.locked_until = Some(now.saturating_add(cfg.lockout_secs));
            rec.codes.clear();
            return VerifyOutcome::LockedOut;
        }
        VerifyOutcome::Invalid
    }

    /// Consume the mailbox's codes after a successful sign-in, resetting its counter (#518). Idempotent.
    pub fn consume(&self, email: &str) {
        let key = normalize_email(email);
        let mut guard = self.inner.lock().expect("otp store mutex poisoned");
        guard.remove(&key);
    }
}

/// POST /v1/auth/request-otp — store and email a 6-digit OTP. **Always 200** (anti-enumeration).
pub async fn request_otp(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let parsed: RequestOtpBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return bad_request("invalid body"),
    };
    let email = parsed.email.trim();
    // Empty email: nothing to do, but still 200 (don't reveal validation state).
    if email.is_empty() {
        return ok_200();
    }
    process_request_otp(&state.otp, &state.otp_config, email).await;
    ok_200()
}

/// Generate, store and (unless DEV_OTP or no Resend key) email an OTP; shared with the harness.
pub async fn process_request_otp(otp: &OtpStore, cfg: &OtpConfig, email: &str) {
    process_request_otp_for(otp, cfg, email, OtpPurpose::SignIn).await
}

/// What a code is being sent FOR — it selects the email the recipient reads, and
/// nothing else. The store, throttle, lockout and DEV_OTP behaviour are
/// identical for every purpose.
///
/// A code mailed to the address being LEFT must say what it authorizes, so a stray one alarms.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OtpPurpose {
    /// Signup / sign-in to the address that will own the session.
    SignIn,
    /// #1161: re-proving the CURRENT address before the account's email is
    /// changed away from it.
    EmailChangeChallenge,
}

/// [`process_request_otp`], with the mail template chosen explicitly.
pub async fn process_request_otp_for(
    otp: &OtpStore,
    cfg: &OtpConfig,
    email: &str,
    purpose: OtpPurpose,
) {
    let review = cfg
        .review_login
        .as_ref()
        .filter(|r| purpose == OtpPurpose::SignIn && r.matches(email));
    let code = match (&cfg.dev_otp, review) {
        (Some(dev), _) => dev.clone(),
        (None, Some(r)) => r.code.clone(),
        (None, None) => format!("{:06}", OsRng.gen_range(0..1_000_000u32)),
    };

    let outcome = otp.prepare(email, &code, cfg, crate::util::now_unix());

    match outcome {
        PrepareOutcome::Throttled => {}
        PrepareOutcome::LockedOut => {
            tracing::warn!(
                "OTP request for {} refused — mailbox locked out by failed guesses",
                mask_email(email)
            );
        }
        PrepareOutcome::Send(code) => {
            // DEV_OTP: skip the real send entirely.
            if cfg.dev_otp.is_some() {
                tracing::info!("DEV_OTP active — skipping email send for {}", mask_email(email));
                return;
            }
            // Nobody reads the review mailbox; the reviewer already has the code.
            if review.is_some() {
                tracing::info!("store-review sign-in requested for {}", mask_email(email));
                return;
            }
            if let Some(key) = &cfg.resend_api_key {
                let sent = match purpose {
                    OtpPurpose::SignIn => send_otp_email(key, email, &code).await,
                    OtpPurpose::EmailChangeChallenge => {
                        send_email_change_challenge(key, email, &code).await
                    }
                };
                if let Err(e) = sent {
                    // A send failure leaks nothing about account existence; log
                    // and still 200 so the response is uniform.
                    tracing::error!("OTP email send failed for {}: {e:#}", mask_email(email));
                }
            } else {
                tracing::warn!("RESEND_API_KEY unset — OTP email NOT sent for {}", mask_email(email));
            }
        }
    }
}

/// Hand the code to Resend.
///
/// Awaited by the handler, so [`crate::util::Upstream::Resend`] bounds it (#913); still 200.
async fn send_otp_email(api_key: &str, email: &str, code: &str) -> anyhow::Result<()> {
    let body = serde_json::json!({
        "from": "Pollis <noreply@mail.pollis.com>",
        "to": [email],
        "subject": "Your Pollis sign-in code",
        "text": format!("Your verification code is: {code}\n\nThis code expires in 10 minutes."),
    });
    let resp = crate::util::http_post(crate::util::Upstream::Resend, "https://api.resend.com/emails")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let txt = resp.text().await.unwrap_or_default();
        anyhow::bail!("Resend non-success: {txt}");
    }
    Ok(())
}

/// Ask the CURRENT address to authorize moving the account off it (#1161).
///
/// Sent BEFORE the change commits, naming the destination, so an unrequested code reads as an attack.
async fn send_email_change_challenge(
    api_key: &str,
    current_email: &str,
    code: &str,
) -> anyhow::Result<()> {
    let body = serde_json::json!({
        "from": "Pollis <noreply@mail.pollis.com>",
        "to": [current_email],
        "subject": "Confirm the email change on your Pollis account",
        "text": format!(
            "Someone asked to change the email address on your Pollis account away \
             from this one.\n\n\
             To approve it, enter this code in Pollis: {code}\n\n\
             It expires in 10 minutes.\n\n\
             If this was not you, do NOT enter the code. Someone has access to one \
             of your devices — revoke that device from Settings on a device you \
             still control, and contact support@pollis.com."
        ),
    });
    let resp = crate::util::http_post(crate::util::Upstream::Resend, "https://api.resend.com/emails")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let txt = resp.text().await.unwrap_or_default();
        anyhow::bail!("Resend non-success: {txt}");
    }
    Ok(())
}

/// Tell the address an account has just MOVED OFF that it happened (L3).
///
/// Sent after the change committed, never failing it; it names the new address.
///
/// Lives here so one place talks to Resend.
pub(crate) async fn send_email_change_notice(
    api_key: &str,
    old_email: &str,
    new_email: &str,
) -> anyhow::Result<()> {
    let body = serde_json::json!({
        "from": "Pollis <noreply@mail.pollis.com>",
        "to": [old_email],
        "subject": "Your Pollis email address was changed",
        "text": format!(
            "The email address on your Pollis account was changed to {new_email}.\n\n\
             If this was you, nothing further is needed.\n\n\
             If it was not, someone has access to one of your devices. Revoke that \
             device from Settings on a device you still control, and contact \
             support@pollis.com."
        ),
    });
    let resp = crate::util::http_post(crate::util::Upstream::Resend, "https://api.resend.com/emails")
        .header("Authorization", format!("Bearer {api_key}"))
        .json(&body)
        .send()
        .await?;
    if !resp.status().is_success() {
        let txt = resp.text().await.unwrap_or_default();
        anyhow::bail!("Resend non-success: {txt}");
    }
    Ok(())
}

/// POST /v1/auth/verify-otp — attempt-limited check, then create-or-load the account and a session.
pub async fn verify_otp(State(state): State<AppState>, body: axum::body::Bytes) -> Response {
    let parsed: VerifyOtpBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return bad_request("invalid body"),
    };
    let email = parsed.email.trim();
    let device_id = parsed.device_id.trim();
    if email.is_empty() || device_id.is_empty() {
        return bad_request("email and device_id required");
    }

    let conn = match state.db.conn().await {
        Ok(c) => c,
        Err(e) => return internal(e),
    };

    match apply_verify_otp(
        &conn,
        &state.otp,
        &state.sessions,
        &state.otp_config,
        email,
        &parsed.code,
        device_id,
    )
    .await
    {
        Ok(result) => verify_otp_response(result),
        Err(e) => internal(e),
    }
}

/// The outcome of [`apply_verify_otp`].
pub enum VerifyOtpResult {
    Ok {
        user_id: String,
        username: String,
        is_new_account: bool,
        has_identity: bool,
        /// Base64 `users.account_id_pub`, present iff `has_identity`.
        account_id_pub: Option<String>,
        session_token: String,
        session_expires_at: u64,
    },
    /// Wrong / expired / unknown code → 401.
    InvalidCode,
    /// Past the attempt limit → 429.
    LockedOut,
}

/// Validate the OTP, create-or-load the account, and mint a session; shared with the harness.
pub async fn apply_verify_otp(
    conn: &libsql::Connection,
    otp: &OtpStore,
    sessions: &SessionStore,
    cfg: &OtpConfig,
    email: &str,
    code: &str,
    device_id: &str,
) -> anyhow::Result<VerifyOtpResult> {
    // Validate WITHOUT consuming, so a DB failure below leaves the code valid for a retry (#518).
    match otp.check(email, code, cfg, crate::util::now_unix()) {
        VerifyOutcome::Ok => {}
        VerifyOutcome::LockedOut => return Ok(VerifyOtpResult::LockedOut),
        VerifyOutcome::Invalid | VerifyOutcome::Expired | VerifyOutcome::NotFound => {
            return Ok(VerifyOtpResult::InvalidCode)
        }
    }

    // Create or load the account; an error here is a 5xx WITHOUT consuming the code.
    //
    // Canonicalize here: `users.email` is the identity, and the harnesses bypass the handler.
    //
    // Lowercase too (#1088): one mailbox must be one row (migration `000026`).
    let canonical = normalize_email(email);
    let email = canonical.as_str();

    let mut rows = conn
        .query(
            "SELECT id, username, account_id_pub FROM users WHERE email = ?1",
            libsql::params![email.to_string()],
        )
        .await?;
    let existing = rows.next().await?;
    drop(rows);

    let (user_id, username, account_id_pub, is_new_account) = match existing {
        Some(row) => {
            let id: String = row.get(0)?;
            let uname: String = row
                .get(1)
                .unwrap_or_else(|_| email.split('@').next().unwrap_or("user").to_string());
            let pub_bytes: Option<Vec<u8>> = row.get::<Option<Vec<u8>>>(2).ok().flatten();
            // Returned for the caller's orphan check (#987).
            let encoded = pub_bytes.as_deref().map(crate::util::b64);
            (id, uname, encoded, false)
        }
        None => {
            let user_id = Ulid::new().to_string();
            let default_username = default_username(email, &user_id);
            conn.execute(
                "INSERT INTO users (id, email, username) VALUES (?1, ?2, ?3)",
                libsql::params![user_id.clone(), email.to_string(), default_username.clone()],
            )
            .await?;
            (user_id, default_username, None, true)
        }
    };
    let has_identity = account_id_pub.is_some();

    let now = crate::util::now_unix();
    let session_token = sessions.mint(&user_id, email, device_id, cfg.session_ttl_secs, now);

    // Single-use: consume ONLY now that the account exists and the session is
    // minted — everything that can fail has already succeeded (#518).
    otp.consume(email);

    Ok(VerifyOtpResult::Ok {
        user_id,
        username,
        is_new_account,
        has_identity,
        account_id_pub,
        session_token,
        session_expires_at: now + cfg.session_ttl_secs,
    })
}

/// Map a [`VerifyOtpResult`] to the wire response (handler and harness).
pub fn verify_otp_response(result: VerifyOtpResult) -> Response {
    match result {
        VerifyOtpResult::Ok {
            user_id,
            username,
            is_new_account,
            has_identity,
            account_id_pub,
            session_token,
            session_expires_at,
        } => crate::writes::ok_response::<VerifyOtpBody>(VerifyOtpResponse {
            user_id,
            username,
            is_new_account,
            has_identity,
            account_id_pub,
            session_token,
            // Unix seconds. `i64` on the wire type, matching every other
            // timestamp in the API; the DS computes it as `u64`.
            session_expires_at: session_expires_at as i64,
        }),
        VerifyOtpResult::LockedOut => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({ "error": "too many attempts" })),
        )
            .into_response(),
        VerifyOtpResult::InvalidCode => (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "error": "invalid code" })),
        )
            .into_response(),
    }
}

/// The username a brand-new account starts with: the email's local part, an
/// underscore, and the last four characters of the account's ULID, squeezed
/// into the shape `profile::is_valid_username` accepts.
///
/// It must pass `/v1/profile/update`'s rule: lowercased, `[a-z0-9_.-]`, length-capped, never `@`.
///
/// Uniqueness still rides on the ULID suffix; lower-casing it is a bijection
/// on the Crockford alphabet, so it loses nothing.
pub fn default_username(email: &str, user_id: &str) -> String {
    let suffix = &user_id[user_id.len().saturating_sub(4)..];
    let suffix = suffix.to_ascii_lowercase();
    let room = crate::profile::USERNAME_MAX_LEN - 1 - suffix.len();
    let local: String = email
        .split('@')
        .next()
        .unwrap_or("user")
        .chars()
        .map(|c| {
            let c = c.to_ascii_lowercase();
            if crate::profile::is_username_char(c) {
                c
            } else {
                '_'
            }
        })
        .take(room)
        .collect();
    format!("{local}_{suffix}")
}

/// `POST /v1/auth/request-otp` answers `{"status":"ok"}` whether or not an account exists.
fn ok_200() -> Response {
    crate::writes::ok_response::<RequestOtpBody>(pollis_api::StatusOk::Ok)
}

fn internal(e: anyhow::Error) -> Response {
    tracing::error!("verify-otp internal error: {e:#}");
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({ "error": "internal error" })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::session::SessionStore;


    // A local libsql connection for the account-write path. `with_users` toggles
    // whether the `users` table exists — omitting it makes the write fail, which is
    // exactly the transient/config DB failure #518 is about.
    /// Returns the `TempDir` first so the caller holds it (#942).
    async fn conn_with(with_users: bool) -> (tempfile::TempDir, Db, crate::db::ConnGuard) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("otp-test.db");
        let db = Db::connect_local(path.to_str().unwrap()).await.expect("local db");
        let conn = db.conn().await.unwrap();
        if with_users {
            // Turso has FK enforcement off; match it.
        conn.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        pollis_schema::apply::single_db(&conn).await.expect("schema");
        }
        (dir, db, conn)
    }

    /// A config with the resend throttle off, for tests that prepare several
    /// codes at one instant. Everything else is left at the real defaults.
    fn no_throttle(cfg: &OtpConfig) -> OtpConfig {
        OtpConfig { resend_throttle_secs: 0, ..cfg.clone() }
    }

    /// Defaults with the throttle off.
    fn cfg() -> OtpConfig {
        no_throttle(&OtpConfig::default())
    }

    #[test]
    fn check_does_not_consume_a_correct_code_consume_does() {
        let store = OtpStore::default();
        let cfg = cfg();
        store.prepare("a@x.com", "123456", &cfg, 1000);
        // A correct code checks Ok — and stays valid; checking again still Ok (the
        // #518 fix: check alone must not burn the code).
        assert_eq!(store.check("a@x.com", "123456", &cfg, 1000), VerifyOutcome::Ok);
        assert_eq!(store.check("a@x.com", "123456", &cfg, 1000), VerifyOutcome::Ok);
        // Single-use is enforced by consume, not by check.
        store.consume("a@x.com");
        assert_eq!(
            store.check("a@x.com", "123456", &cfg, 1000),
            VerifyOutcome::NotFound
        );
        // consume is idempotent.
        store.consume("a@x.com");
    }

    #[test]
    fn lockout_after_six_wrong_then_correct_fails() {
        let store = OtpStore::default();
        let cfg = cfg();
        store.prepare("a@x.com", "123456", &cfg, 1000);
        // 5 wrong guesses are merely invalid.
        for _ in 0..5 {
            assert_eq!(
                store.check("a@x.com", "000000", &cfg, 1000),
                VerifyOutcome::Invalid
            );
        }
        // The 6th locks out and drops the code.
        assert_eq!(
            store.check("a@x.com", "000000", &cfg, 1000),
            VerifyOutcome::LockedOut
        );
        // The correct code no longer works.
        assert_ne!(store.check("a@x.com", "123456", &cfg, 1000), VerifyOutcome::Ok);
    }

    /// #1088: the guess budget is per mailbox, so a new request must not reset it.
    #[test]
    fn a_fresh_request_does_not_refill_the_guess_budget() {
        let store = OtpStore::default();
        let cfg = cfg();
        store.prepare("a@x.com", "123456", &cfg, 1000);
        for _ in 0..5 {
            assert_eq!(
                store.check("a@x.com", "000000", &cfg, 1000),
                VerifyOutcome::Invalid
            );
        }
        // Ask for a new code — under the old store this reset `attempts` to 0.
        store.prepare("a@x.com", "654321", &cfg, 1000);
        assert_eq!(
            store.check("a@x.com", "000000", &cfg, 1000),
            VerifyOutcome::LockedOut,
            "the 6th wrong guess must lock out even across a re-request"
        );
    }

    /// Once locked, a new request must not clear the lockout or mint a usable code.
    #[test]
    fn a_locked_mailbox_cannot_be_unlocked_by_requesting_a_new_code() {
        let store = OtpStore::default();
        let cfg = cfg();
        store.prepare("a@x.com", "123456", &cfg, 1000);
        for _ in 0..6 {
            store.check("a@x.com", "000000", &cfg, 1000);
        }
        assert_eq!(
            store.prepare("a@x.com", "654321", &cfg, 1000),
            PrepareOutcome::LockedOut
        );
        assert_eq!(
            store.check("a@x.com", "654321", &cfg, 1000),
            VerifyOutcome::LockedOut,
            "no code may be stored while the mailbox is locked"
        );
        // The lockout is time-bounded, not permanent: past `lockout_secs` the
        // mailbox is usable again and the guess budget is fresh.
        let after = 1000 + cfg.lockout_secs + 1;
        assert!(matches!(
            store.prepare("a@x.com", "654321", &cfg, after),
            PrepareOutcome::Send(_)
        ));
        assert_eq!(store.check("a@x.com", "654321", &cfg, after), VerifyOutcome::Ok);
    }

    /// #1088: a new request must not invalidate the code a victim is about to type.
    #[test]
    fn a_new_request_leaves_the_outstanding_code_valid() {
        let store = OtpStore::default();
        let cfg = cfg();
        store.prepare("a@x.com", "111111", &cfg, 1000);
        store.prepare("a@x.com", "222222", &cfg, 1005);
        assert_eq!(store.check("a@x.com", "111111", &cfg, 1010), VerifyOutcome::Ok);
        assert_eq!(store.check("a@x.com", "222222", &cfg, 1010), VerifyOutcome::Ok);
    }

    /// Codes accumulating is only safe with a ceiling on how many a mailbox can
    /// be sent, or the attacker just mints unbounded valid codes instead.
    #[test]
    fn a_mailbox_is_capped_at_max_sends_per_window() {
        let store = OtpStore::default();
        let cfg = cfg();
        for i in 0..cfg.max_sends_per_window {
            assert!(
                matches!(store.prepare("a@x.com", "111111", &cfg, 1000 + i as u64), PrepareOutcome::Send(_)),
                "send {i} is inside the budget"
            );
        }
        assert_eq!(
            store.prepare("a@x.com", "222222", &cfg, 1000 + cfg.max_sends_per_window as u64),
            PrepareOutcome::Throttled
        );
        // The window rolls over.
        let next = 1000 + cfg.ttl_secs + 1;
        assert!(matches!(
            store.prepare("a@x.com", "222222", &cfg, next),
            PrepareOutcome::Send(_)
        ));
    }

    /// Mailbox state needs a collector; a live code, lockout or open send window keeps a record.
    #[test]
    fn the_sweep_drops_only_mailboxes_that_hold_nothing() {
        let store = OtpStore::default();
        let cfg = cfg();
        store.prepare("live@x.com", "111111", &cfg, 1000);
        store.prepare("locked@x.com", "111111", &cfg, 1000);
        for _ in 0..=cfg.max_attempts {
            store.check("locked@x.com", "000000", &cfg, 1000);
        }
        store.prepare("stale@x.com", "111111", &cfg, 1000);

        // Past `stale@x.com`'s code TTL and its send window, but still inside
        // `locked@x.com`'s lockout, and with a code freshly issued to
        // `live@x.com`.
        let later = 1000 + cfg.ttl_secs + 1;
        store.prepare("live@x.com", "222222", &cfg, later);

        let mut guard = store.inner.lock().unwrap();
        sweep(&mut guard, cfg.ttl_secs, later);
        let mut kept: Vec<&str> = guard.keys().map(|k| k.as_str()).collect();
        kept.sort();
        assert_eq!(kept, vec!["live@x.com", "locked@x.com"]);
    }

    fn review_cfg() -> OtpConfig {
        OtpConfig {
            review_login: Some(ReviewLogin::parse("Review@Pollis.com:424242").unwrap()),
            ..cfg()
        }
    }

    #[test]
    fn a_review_login_must_be_an_address_and_six_digits() {
        assert!(ReviewLogin::parse("review@pollis.com:424242").is_ok());
        for bad in [
            "review@pollis.com",
            "review@pollis.com:",
            "review@pollis.com:42424",
            "review@pollis.com:4242424",
            "review@pollis.com:42424a",
            "pollis.com:424242",
            ":424242",
        ] {
            assert!(ReviewLogin::parse(bad).is_err(), "{bad:?} must be refused");
        }
    }

    /// The review address signs in with the fixed code, matched in any case.
    #[tokio::test]
    async fn the_review_address_gets_the_fixed_code() {
        let store = OtpStore::default();
        let cfg = review_cfg();
        process_request_otp(&store, &cfg, " REVIEW@pollis.com ").await;
        assert_eq!(store.check("review@pollis.com", "424242", &cfg, crate::util::now_unix()), VerifyOutcome::Ok);
    }

    /// Every other address still gets a random code — the fixed code is not a
    /// master key.
    #[tokio::test]
    async fn the_fixed_code_does_not_sign_in_any_other_address() {
        let store = OtpStore::default();
        let cfg = review_cfg();
        process_request_otp(&store, &cfg, "alice@x.com").await;
        assert_ne!(store.check("alice@x.com", "424242", &cfg, crate::util::now_unix()), VerifyOutcome::Ok);
    }

    /// Only sign-in is fixed; an email-change challenge to the review address
    /// is an ordinary random code.
    #[tokio::test]
    async fn an_email_change_challenge_to_the_review_address_is_not_fixed() {
        let store = OtpStore::default();
        let cfg = review_cfg();
        process_request_otp_for(&store, &cfg, "review@pollis.com", OtpPurpose::EmailChangeChallenge).await;
        assert_ne!(store.check("review@pollis.com", "424242", &cfg, crate::util::now_unix()), VerifyOutcome::Ok);
    }

    /// The fixed code buys no extra guesses: the review mailbox locks out like
    /// any other, and while locked even the right code is refused.
    #[tokio::test]
    async fn the_review_mailbox_still_locks_out() {
        let store = OtpStore::default();
        let cfg = review_cfg();
        process_request_otp(&store, &cfg, "review@pollis.com").await;
        let now = crate::util::now_unix();
        for _ in 0..=cfg.max_attempts {
            store.check("review@pollis.com", "000000", &cfg, now);
        }
        assert_eq!(store.check("review@pollis.com", "424242", &cfg, now), VerifyOutcome::LockedOut);
    }

    /// Single-use and unrequested-code rules still hold: without a request
    /// there is nothing to verify, and a consumed code does not replay.
    #[tokio::test]
    async fn the_review_code_is_single_use_and_needs_a_request() {
        let store = OtpStore::default();
        let cfg = review_cfg();
        let now = crate::util::now_unix();
        assert_eq!(store.check("review@pollis.com", "424242", &cfg, now), VerifyOutcome::NotFound);
        process_request_otp(&store, &cfg, "review@pollis.com").await;
        assert_eq!(store.check("review@pollis.com", "424242", &cfg, now), VerifyOutcome::Ok);
        store.consume("review@pollis.com");
        assert_eq!(store.check("review@pollis.com", "424242", &cfg, now), VerifyOutcome::NotFound);
    }

    #[test]
    fn expired_code_rejected() {
        let store = OtpStore::default();
        let cfg = cfg();
        store.prepare("a@x.com", "123456", &cfg, 1000);
        assert_eq!(
            store.check("a@x.com", "123456", &cfg, 2000),
            VerifyOutcome::Expired
        );
    }

    #[test]
    fn throttle_skips_resend() {
        let store = OtpStore::default();
        let cfg = OtpConfig::default();
        assert!(matches!(
            store.prepare("a@x.com", "111111", &cfg, 1000),
            PrepareOutcome::Send(_)
        ));
        assert!(matches!(
            store.prepare("a@x.com", "222222", &cfg, 1010),
            PrepareOutcome::Throttled
        ));
    }

    /// Count the `users` rows, so "did this create a second account?" is a
    /// direct observation rather than an inference.
    async fn user_count(conn: &crate::db::ConnGuard) -> i64 {
        let mut rows = conn.query("SELECT COUNT(*) FROM users", ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    async fn verify(
        conn: &crate::db::ConnGuard,
        otp: &OtpStore,
        sessions: &SessionStore,
        cfg: &OtpConfig,
        email: &str,
    ) -> VerifyOtpResult {
        otp.prepare(email, "123456", &no_throttle(cfg), crate::util::now_unix());
        apply_verify_otp(conn, otp, sessions, cfg, email, "123456", "dev-1")
            .await
            .expect("verify-otp must succeed")
    }

    /// Two spellings of one address must land on one row, at the INSERT.
    #[tokio::test]
    async fn the_same_address_spelled_two_ways_resolves_to_one_account() {
        let (_dir, _db, conn) = conn_with(true).await;
        let (otp, sessions, cfg) = (OtpStore::default(), SessionStore::default(), OtpConfig::default());

        let first = verify(&conn, &otp, &sessions, &cfg, "alice@x.com").await;
        let padded = verify(&conn, &otp, &sessions, &cfg, "  alice@x.com \n").await;

        let (id_a, new_a) = match first {
            VerifyOtpResult::Ok { user_id, is_new_account, .. } => (user_id, is_new_account),
            _ => panic!("expected Ok"),
        };
        let (id_b, new_b) = match padded {
            VerifyOtpResult::Ok { user_id, is_new_account, .. } => (user_id, is_new_account),
            _ => panic!("expected Ok"),
        };

        assert!(new_a, "the first sign-in creates the account");
        assert!(!new_b, "the padded spelling must find the account, not make one");
        assert_eq!(id_a, id_b);
        assert_eq!(user_count(&conn).await, 1, "one address must never own two accounts");
    }

    /// #1088: the same address in a different CASE is the same mailbox.
    #[tokio::test]
    async fn the_same_address_in_a_different_case_resolves_to_one_account() {
        let (_dir, _db, conn) = conn_with(true).await;
        let (otp, sessions, cfg) = (OtpStore::default(), SessionStore::default(), OtpConfig::default());

        let first = verify(&conn, &otp, &sessions, &cfg, "alice@x.com").await;
        let shouty = verify(&conn, &otp, &sessions, &cfg, "Alice@X.COM").await;

        let (id_a, new_a) = match first {
            VerifyOtpResult::Ok { user_id, is_new_account, .. } => (user_id, is_new_account),
            _ => panic!("expected Ok"),
        };
        let (id_b, new_b) = match shouty {
            VerifyOtpResult::Ok { user_id, is_new_account, .. } => (user_id, is_new_account),
            _ => panic!("expected Ok"),
        };

        assert!(new_a);
        assert!(!new_b, "a different capitalisation must find the account, not make one");
        assert_eq!(id_a, id_b);
        assert_eq!(user_count(&conn).await, 1);

        // Stored canonically, so the byte-exact UNIQUE and the migration's
        // `lower(trim(email))` index agree.
        let mut rows = conn
            .query("SELECT email FROM users", ())
            .await
            .unwrap();
        let stored: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(stored, "alice@x.com");

        // And the directory resolves either spelling to that one row, so an
        // invite or DM addressed to `Alice@X.COM` reaches Alice.
        for spelling in ["alice@x.com", "Alice@X.COM", "  ALICE@x.com "] {
            let found = crate::directory::user_by_identifier(&conn, spelling)
                .await
                .unwrap()
                .unwrap_or_else(|| panic!("{spelling} must resolve"));
            assert_eq!(found.id, id_a);
        }
    }

    /// Default username: local part, `_`, the ULID's last four, lowercased; empty local part → `"_<suffix>"`.
    #[tokio::test]
    async fn the_default_username_is_the_email_prefix_and_a_ulid_suffix() {
        let (_dir, _db, conn) = conn_with(true).await;
        let (otp, sessions, cfg) = (OtpStore::default(), SessionStore::default(), OtpConfig::default());

        match verify(&conn, &otp, &sessions, &cfg, "alice@x.com").await {
            VerifyOtpResult::Ok { user_id, username, .. } => {
                assert_eq!(
                    username,
                    format!("alice_{}", &user_id[user_id.len() - 4..].to_ascii_lowercase())
                );
            }
            _ => panic!("expected Ok"),
        }

        match verify(&conn, &otp, &sessions, &cfg, "@x.com").await {
            VerifyOtpResult::Ok { user_id, username, .. } => {
                assert_eq!(
                    username,
                    format!("_{}", &user_id[user_id.len() - 4..].to_ascii_lowercase())
                );
            }
            _ => panic!("expected Ok"),
        }
    }

    /// A minted default must pass the rule `/v1/profile/update` enforces —
    /// otherwise the account's own starting name is refused by its own form.
    /// Pins the three squeezes: case, character class, and length.
    #[test]
    fn a_default_username_always_satisfies_the_username_rule() {
        use crate::profile::{is_valid_username, USERNAME_MAX_LEN};

        let ulid = "01J8ZK2M4N6P8Q0R2S4T6V8WXY";
        for email in [
            "Alice.Smith@x.com",
            "alice+work@gmail.com",
            "ALICE@X.COM",
            "a b@x.com",
            "émile@x.fr",
            "@x.com",
            "x@x.com",
            "an-extremely-long-local-part-that-runs-past-the-limit-by-a-mile@x.com",
        ] {
            let name = default_username(email, ulid);
            assert!(
                is_valid_username(&name),
                "{email:?} produced {name:?}, which the profile rule refuses"
            );
            assert!(name.len() <= USERNAME_MAX_LEN, "{name:?} is over the cap");
            assert!(name.ends_with("_8wxy"), "{name:?} lost its suffix");
        }
        assert_eq!(default_username("alice+work@gmail.com", ulid), "alice_work_8wxy");
        assert_eq!(default_username("Alice.Smith@x.com", ulid), "alice.smith_8wxy");
    }

    /// A returning account reports whether it has published an identity key —
    /// the flag the client's enrollment gate turns on. A NULL column is "not
    /// yet", never an error.
    #[tokio::test]
    async fn has_identity_reflects_the_stored_key_and_null_reads_as_absent() {
        let (_dir, _db, conn) = conn_with(true).await;
        let (otp, sessions, cfg) = (OtpStore::default(), SessionStore::default(), OtpConfig::default());

        // No key yet.
        match verify(&conn, &otp, &sessions, &cfg, "bob@x.com").await {
            VerifyOtpResult::Ok { has_identity, is_new_account, .. } => {
                assert!(is_new_account);
                assert!(!has_identity);
            }
            _ => panic!("expected Ok"),
        }

        conn.execute(
            "UPDATE users SET account_id_pub = ?1 WHERE email = 'bob@x.com'",
            libsql::params![vec![7u8; 32]],
        )
        .await
        .unwrap();

        match verify(&conn, &otp, &sessions, &cfg, "bob@x.com").await {
            VerifyOtpResult::Ok { has_identity, is_new_account, .. } => {
                assert!(!is_new_account);
                assert!(has_identity);
            }
            _ => panic!("expected Ok"),
        }
    }

    // #518: a correct code + a failing account-write must surface as an error (5xx),
    // NOT "invalid code", and must leave the code usable so an immediate retry
    // succeeds once the DB is healthy.
    #[tokio::test]
    async fn correct_code_with_failing_db_write_errors_then_retry_succeeds() {
        let (_dir, _db, conn) = conn_with(false).await; // no `users` table → the write fails
        let otp = OtpStore::default();
        let sessions = SessionStore::default();
        let cfg = OtpConfig::default();
        otp.prepare("a@x.com", "123456", &no_throttle(&cfg), crate::util::now_unix());

        let first =
            apply_verify_otp(&conn, &otp, &sessions, &cfg, "a@x.com", "123456", "dev-1").await;
        assert!(
            first.is_err(),
            "a failed account-write must surface as an error (5xx), not consume the code"
        );

        // Heal the DB and retry the SAME code — it must succeed (was not burned).
        // Turso has FK enforcement off; match it.
        conn.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        pollis_schema::apply::single_db(&conn).await.expect("schema");
        let second =
            apply_verify_otp(&conn, &otp, &sessions, &cfg, "a@x.com", "123456", "dev-1").await;
        assert!(
            matches!(second, Ok(VerifyOtpResult::Ok { is_new_account: true, .. })),
            "the same code must verify once the DB write can succeed"
        );
    }

    // On the success path the code is consumed exactly once — a replay is rejected.
    #[tokio::test]
    async fn correct_code_is_single_use_on_success() {
        let (_dir, _db, conn) = conn_with(true).await;
        let otp = OtpStore::default();
        let sessions = SessionStore::default();
        let cfg = OtpConfig::default();
        otp.prepare("a@x.com", "123456", &no_throttle(&cfg), crate::util::now_unix());

        let first =
            apply_verify_otp(&conn, &otp, &sessions, &cfg, "a@x.com", "123456", "dev-1").await;
        assert!(matches!(first, Ok(VerifyOtpResult::Ok { .. })));
        // Replay of the now-consumed code is rejected.
        let replay =
            apply_verify_otp(&conn, &otp, &sessions, &cfg, "a@x.com", "123456", "dev-1").await;
        assert!(matches!(replay, Ok(VerifyOtpResult::InvalidCode)));
    }

    // Wrong guesses still count toward lockout and are never rolled back, even
    // though the account-write (which would fail here) is never reached for them.
    #[tokio::test]
    async fn wrong_guesses_still_lock_out_regardless_of_db() {
        let (_dir, _db, conn) = conn_with(false).await; // a correct code's write would fail
        let otp = OtpStore::default();
        let sessions = SessionStore::default();
        let cfg = OtpConfig::default();
        otp.prepare("a@x.com", "123456", &no_throttle(&cfg), crate::util::now_unix());

        for _ in 0..cfg.max_attempts {
            let r =
                apply_verify_otp(&conn, &otp, &sessions, &cfg, "a@x.com", "000000", "dev-1").await;
            assert!(matches!(r, Ok(VerifyOtpResult::InvalidCode)));
        }
        let locked =
            apply_verify_otp(&conn, &otp, &sessions, &cfg, "a@x.com", "000000", "dev-1").await;
        assert!(matches!(locked, Ok(VerifyOtpResult::LockedOut)));
        // The correct code is gone too, and the answer is "locked" (persisted on the mailbox, #1088).
        let after =
            apply_verify_otp(&conn, &otp, &sessions, &cfg, "a@x.com", "123456", "dev-1").await;
        assert!(matches!(after, Ok(VerifyOtpResult::LockedOut)));
    }
}
