//! Server-side OTP: generation, salted-hash storage, attempt-limited
//! constant-time verification, and the Resend email send — all moved off the
//! client (which used to hold a baked-in Resend key + an in-process OTP map).
//! See `docs/otp-server-bootstrap-design.md`.
//!
//! This also FIXES the client-side OTP's unlimited-guess bug: each code now has
//! a per-email attempt counter, locks out (and is deleted) past
//! [`OtpConfig::max_attempts`], compares in constant time, and is deleted on the
//! first success (single-use).
//!
//! **Store:** in-memory (the DS is single-container — mirrors the OTP map the
//! client used to keep). Behind [`OtpStore`] so a scaled-out DS can swap it for a
//! Turso table without touching the handlers.
//!
//! **That store is not durable, and the guess budget depends on it (#1142).**
//! Single-container buys *consistency* — no second instance holding a divergent
//! counter — not persistence. The container scales to zero after
//! `PollisDelivery.sleepAfter` (`worker/index.ts`), so in a quiet hour every
//! mailbox's `failed` count and `locked_until` are dropped routinely, not only
//! across a redeploy.
//!
//! This is safe **only because `sleepAfter >= ttl_secs`**. Triggering the reset
//! costs an attacker total silence for `sleepAfter` — their own requests keep
//! the container warm — and that same silence expires the code they were
//! guessing, so they come back to a fresh counter AND a dead code. The budget
//! stays bounded by the code's lifetime rather than by the lockout, which is why
//! the practical exposure is a 300-second head start on re-requesting, not a
//! brute-force window.
//!
//! Invert the inequality and the reasoning inverts: the counter would reset
//! while the code is still live, giving repeated fresh [`OtpConfig::max_attempts`]
//! bursts against ONE code for the price of pausing between them.
//! `tests/otp_state_durability.rs` pins the inequality across the two languages.

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

// The request bodies for this module's endpoints live in `pollis-api`, the
// crate pollis-core builds its requests from — one declaration, both ends, so
// a client field that does not exist here is a compile error rather than a
// silently-absent JSON key. Re-exported so `pollis_delivery::otp::*Body`
// keeps resolving for handlers, tests and the flows harness.
pub use pollis_api::otp::*;

/// Tunables for the OTP + session machinery, read from DS env in
/// [`OtpConfig::from_env`].
#[derive(Clone)]
pub struct OtpConfig {
    /// Resend API key (DS env `RESEND_API_KEY`). `None` → email send is skipped
    /// (every request still 200s; useful only with `dev_otp`).
    pub resend_api_key: Option<String>,
    /// `DEV_OTP` override — when set, the email send is skipped and this exact
    /// code is the only one that verifies. Mirrors pollis-core's `DEV_OTP` so the
    /// integration harness + local dev keep working without a real mailbox.
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
    /// How many codes one mailbox may be sent inside a `ttl_secs` window. The
    /// cap exists because a re-request no longer invalidates the outstanding
    /// code (#1088) — without it an attacker could mint unbounded concurrently
    /// valid codes for a victim's mailbox.
    pub max_sends_per_window: u32,
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
        }
    }
}

impl OtpConfig {
    /// Build from DS environment. `RESEND_API_KEY` (the key the client no longer
    /// ships), `DEV_OTP` (harness/local override), `OTP_TTL_SECS` (optional).
    ///
    /// **Raising `OTP_TTL_SECS` above the container's `sleepAfter` re-opens
    /// #1142** — see the module header. `tests/otp_state_durability.rs` guards
    /// the compiled-in default, but it cannot see a deploy-time env override, so
    /// that one is on whoever sets it.
    pub fn from_env() -> Self {
        let defaults = Self::default();
        Self {
            resend_api_key: std::env::var("RESEND_API_KEY").ok().filter(|s| !s.is_empty()),
            dev_otp: std::env::var("DEV_OTP").ok().filter(|s| !s.is_empty()),
            ttl_secs: std::env::var("OTP_TTL_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(defaults.ttl_secs),
            ..defaults
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

/// State for ONE mailbox. The unit of throttling and lockout is the mailbox, not
/// the code (#1088): a fresh `request-otp` used to replace the record outright,
/// which reset the guess budget (so "5 attempts" was really 6 guesses per
/// requested code, unbounded in aggregate) and let anyone invalidate the code a
/// victim was about to type simply by asking for a new one.
///
/// So this record is created on the first request for the mailbox and then only
/// ever *amended*: `failed` and `locked_until` survive every subsequent
/// `prepare`, and a new code is ADDED to `codes` rather than replacing what is
/// there. Every code stays valid until its own TTL expires.
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

    /// `true` while a lockout is in force. An elapsed lockout is cleared along
    /// with the guess counter, so the mailbox is usable again without needing a
    /// successful sign-in to reset it.
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

/// Map size past which [`prepare`](OtpStore::prepare) sweeps records that carry
/// no state worth keeping. Records used to be self-clearing (an expired or
/// locked-out code deleted its own entry), but the whole point of #1088 is that
/// mailbox state outlives its codes — so something has to collect the mailboxes
/// nobody ever came back to, or the map grows with every address anyone asks
/// about. Amortised: the sweep is O(map) and runs only above this size.
const SWEEP_AT: usize = 1024;

/// In-memory OTP store keyed on the normalized email. `Clone` is shallow (shared
/// `Arc`) so it rides on the `Clone` `AppState`.
#[derive(Clone, Default)]
pub struct OtpStore {
    inner: Arc<Mutex<HashMap<String, MailboxState>>>,
}

/// Drop mailboxes holding nothing: no live code, no lockout in force, and a send
/// window that has already elapsed. Anything still carrying one of those is
/// load-bearing and stays.
///
/// The bound this sets on the guess budget: abandoning a mailbox for a full
/// `ttl_secs` with no outstanding code resets its failed-guess count. So the
/// budget refills at most once per OTP lifetime, versus once per *request*
/// before #1088 — and the per-IP window (`ratelimit.rs`) bounds how many
/// mailboxes one client can cycle that way.
fn sweep(map: &mut HashMap<String, MailboxState>, ttl_secs: u64, now: u64) {
    map.retain(|_, rec| {
        rec.prune(now);
        let locked = rec.locked_until.is_some_and(|until| now < until);
        let window_open = now.saturating_sub(rec.window_start) < ttl_secs;
        !rec.codes.is_empty() || locked || window_open
    });
}

/// Normalize an email for store keying so request/verify always agree: trim +
/// lowercase. (The `users` table is still queried with the as-typed address.)
/// `pub(crate)` so the email-change store keys its requester map identically.
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
    /// The mailbox is locked out by failed guesses. Do NOT email and do NOT
    /// store a code — the whole point of a persisted lockout is that asking for
    /// a new code cannot clear it. Still 200 the caller (anti-enumeration).
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
    /// Returns [`PrepareOutcome::LockedOut`] while a failed-guess lockout is in
    /// force and [`PrepareOutcome::Throttled`] inside the resend window or at the
    /// per-window send budget. In all three cases the caller still answers 200 —
    /// the outcome only decides whether an email goes out.
    pub fn prepare(&self, email: &str, code: &str, cfg: &OtpConfig, now: u64) -> PrepareOutcome {
        let key = normalize_email(email);
        let mut guard = self.inner.lock().expect("otp store mutex poisoned");
        if guard.len() >= SWEEP_AT {
            sweep(&mut guard, cfg.ttl_secs, now);
        }
        let rec = guard.entry(key).or_insert_with(|| MailboxState::new(now));
        rec.prune(now);

        // A persisted lockout is not clearable by requesting a new code — that
        // was the hole: `prepare` replaced the record and with it the counter.
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

    /// Check a submitted `code` against every code the mailbox has outstanding,
    /// WITHOUT consuming it on success. Constant-time compare; on a wrong guess
    /// it increments the mailbox's failed-guess counter and, past
    /// `cfg.max_attempts`, locks the mailbox for `cfg.lockout_secs` and drops
    /// every outstanding code.
    ///
    /// On a CORRECT code the record is **left in place** — the caller must call
    /// [`OtpStore::consume`] only after the dependent account-write + session
    /// mint succeed, so a transient/config failure downstream (e.g. a bad DB
    /// token) can't permanently burn a valid code and masquerade as "invalid
    /// code" (#518). Wrong-guess accounting is never rolled back.
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
            // Distinguish "the code you were given has run out of time" from
            // "this mailbox has no code at all" — the client renders them
            // differently — but keep the mailbox record either way, so the
            // guess counter survives.
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

    /// Consume (single-use) the OTPs for `email` once the dependent
    /// account-write and session mint have succeeded: a completed sign-in drops
    /// the whole mailbox record, which is also what resets the failed-guess
    /// counter. Idempotent. Pairs with [`OtpStore::check`] to make consumption
    /// contingent on the whole verify-otp operation succeeding (#518).
    pub fn consume(&self, email: &str) {
        let key = normalize_email(email);
        let mut guard = self.inner.lock().expect("otp store mutex poisoned");
        guard.remove(&key);
    }
}

// ── POST /v1/auth/request-otp ────────────────────────────────────────────────

/// POST /v1/auth/request-otp — generate + store a 6-digit OTP and email it via
/// Resend. **Always 200** regardless of whether the email maps to an account
/// (anti-enumeration). Honors `DEV_OTP` (skip send, force the code) so the
/// harness/local dev work without a mailbox.
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

/// Generate + store an OTP for `email` and (unless DEV_OTP is set or no Resend
/// key is configured) email it via Resend. Extracted from the handler so the
/// in-process integration harness drives the exact same store + throttle +
/// DEV_OTP logic against the shared OTP store.
pub async fn process_request_otp(otp: &OtpStore, cfg: &OtpConfig, email: &str) {
    let code = match &cfg.dev_otp {
        Some(dev) => dev.clone(),
        None => format!("{:06}", OsRng.gen_range(0..1_000_000u32)),
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
            if let Some(key) = &cfg.resend_api_key {
                if let Err(e) = send_otp_email(key, email, &code).await {
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
/// The caller (`process_request_otp`) AWAITS this before the `/v1/auth/request-otp`
/// handler answers, so before #913 — when this call had no deadline — a Resend
/// that accepted the connection and then went quiet held the sign-in request open
/// indefinitely, and held one of the shared client's pooled connections with it.
/// [`crate::util::Upstream::Resend`] bounds that. A timeout lands in the same
/// `Err` arm as any other send failure: logged, and the endpoint still answers
/// 200, because the response must not reveal whether an address exists.
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

// ── POST /v1/auth/verify-otp ─────────────────────────────────────────────────

/// POST /v1/auth/verify-otp — constant-time, attempt-limited code check; then
/// create-or-load the account and mint an OTP-session token. On success returns
/// `{user_id, username, is_new_account, has_identity, session_token,
/// session_expires_at}`. A wrong/expired/locked code → 401/429; never touches
/// the account on a failed code.
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

/// The outcome of [`apply_verify_otp`] — the handler maps it to the wire
/// response (the in-process harness maps it the same way).
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

/// Validate the submitted OTP, create-or-load the account, and mint a session
/// token — all the DB + store work behind `verify-otp`, extracted from the
/// handler so the in-process integration harness drives the identical logic
/// against the shared main DB + OTP/session stores.
pub async fn apply_verify_otp(
    conn: &libsql::Connection,
    otp: &OtpStore,
    sessions: &SessionStore,
    cfg: &OtpConfig,
    email: &str,
    code: &str,
    device_id: &str,
) -> anyhow::Result<VerifyOtpResult> {
    // Validate WITHOUT consuming: a correct code stays valid until the account-write
    // and session mint below succeed, so a transient/config DB failure returns a
    // clean 5xx and the *same* code still works on retry, instead of being burned
    // and disguised as "invalid code" (#518). Wrong/expired/locked codes are
    // rejected here and their attempt accounting stands.
    match otp.check(email, code, cfg, crate::util::now_unix()) {
        VerifyOutcome::Ok => {}
        VerifyOutcome::LockedOut => return Ok(VerifyOtpResult::LockedOut),
        VerifyOutcome::Invalid | VerifyOutcome::Expired | VerifyOutcome::NotFound => {
            return Ok(VerifyOtpResult::InvalidCode)
        }
    }

    // Code is good: create or load the account — server-gen ULID id + a default
    // username from the email prefix plus a 4-char ULID suffix for uniqueness.
    // Any error here `?`-propagates to a 5xx WITHOUT consuming the code (the
    // retry then heals).
    //
    // `pollis-core` used to carry a client twin of this
    // (`auth::resolve_or_create_user_by_email`); #910 deleted it along with the
    // dev-only login that was its sole caller, so this is now the ONE
    // implementation of resolve-or-create in the workspace.
    //
    // Canonicalize here rather than relying on the handler having done it:
    // `users.email` is `NOT NULL UNIQUE`, so the string IS the account identity,
    // and the only place that can guarantee "one address, one row" is the
    // function holding the INSERT. `apply_verify_otp` is also called directly by
    // the in-process test harnesses, which do not go through the handler's trim.
    //
    // Lowercase as well as trim (#1088). Mailboxes are case-insensitive in
    // practice, and the OTP store has always keyed on `trim(lower(..))` — so
    // `Alice@x.com` and `alice@x.com` were one mailbox to the code that sends
    // the code and two rows to the table that stores the account. Whoever typed
    // the second spelling landed in a fresh empty account while their real one,
    // its groups and its devices stayed where they were. Migration
    // `000026_email_case_insensitive` backfills the existing rows and adds a
    // unique index on `lower(trim(email))` so the database agrees.
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
            // Returned to the caller (#987): its orphan check compares these
            // bytes against the account key it holds locally, and this row read
            // is the one that already has them.
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

/// Map a [`VerifyOtpResult`] to the wire response. Shared by the production
/// handler and the in-process harness so both speak the same status codes.
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
/// The squeeze matters because the DS is the one issuing these, and a name it
/// mints must pass the rule it enforces on `/v1/profile/update` — a user whose
/// starting name is refused by their own profile form is a defect. So: the
/// whole thing is lower-cased (a ULID suffix is upper-case Crockford base32),
/// every character outside `[a-z0-9_.-]` in the local part becomes `_`
/// (`alice+work` → `alice_work`), and the local part is cut so the total fits
/// `USERNAME_MAX_LEN`. The suffix alone is `_xxxx` — five characters — so the
/// minimum length holds even for an address with an empty local part, and
/// because the local part cannot contain `@` the result never does either.
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

// ── small response helpers ───────────────────────────────────────────────────

/// `POST /v1/auth/request-otp` answers the shared `{"status":"ok"}` — and
/// answers it whether or not an account exists, which is the point (an OTP
/// request must not be an account-existence oracle).
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
    /// Returns the `TempDir` first so the caller binds and holds it: it deletes
    /// the directory the database lives in when it drops, and this used to be
    /// `std::mem::forget(dir)`, which kept the directory alive by never cleaning
    /// it up at all (#942).
    async fn conn_with(with_users: bool) -> (tempfile::TempDir, Db, crate::db::ConnGuard) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("otp-test.db");
        let db = Db::connect_local(path.to_str().unwrap()).await.expect("local db");
        let conn = db.conn().await.unwrap();
        if with_users {
            // Production is Turso, where foreign-key enforcement is off; libsql's LOCAL
        // backend turns it ON by default, so say so explicitly rather than
        // inherit a constraint no deploy has (`Db::connect_local` does the same).
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

    /// Defaults with the throttle off — the store tests below drive `now`
    /// explicitly, so wall-clock spacing is not what they are about.
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

    /// The #1088 bug: `prepare` used to replace the whole record, so the guess
    /// budget was per *requested code* rather than per mailbox — 5 guesses, ask
    /// for a new code, 5 more, forever. The counter must survive the request.
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

    /// And once locked, requesting a new code must not clear the lockout or mint
    /// a usable code — the old `!rec.locked` branch was dead because the record
    /// (and with it `locked`) was thrown away on every `prepare`.
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

    /// The other half of #1088: requesting a new code used to invalidate the one
    /// the victim was about to type, so anyone who knew an address could stall
    /// that person's sign-in indefinitely. Codes now accumulate and each stays
    /// valid for its own TTL.
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

    /// Mailbox state outliving its codes needs a collector, or the map grows
    /// with every address anyone asks about. A record with a live code, a
    /// standing lockout, or an open send window is load-bearing and must
    /// survive; one with none of the three is garbage.
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

    /// `users.email` is `NOT NULL UNIQUE`, so the address string IS the account's
    /// identity: two spellings of one address must land on one row. The handler
    /// trims, but `apply_verify_otp` is also called directly (the desktop and TUI
    /// in-process harnesses do), so the guarantee has to live here — at the
    /// function that holds the INSERT.
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

    /// #1088: the same address in different CASE is the same mailbox — the OTP
    /// store always thought so (it keys on `trim(lower(..))`), but `users.email`
    /// was byte-exact UNIQUE, so the second spelling opened a fresh empty
    /// account beside the real one.
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

    /// The default username contract: the email's local part, an underscore,
    /// and the last four characters of the account's ULID, lower-cased. An
    /// address with no local part yields `"_<suffix>"` — `split('@')` returns an
    /// empty first segment, never `None`.
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
        // Production is Turso, where foreign-key enforcement is off; libsql's LOCAL
        // backend turns it ON by default, so say so explicitly rather than
        // inherit a constraint no deploy has (`Db::connect_local` does the same).
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
        // The correct code is gone too — and the caller is told the mailbox is
        // locked rather than "invalid code", because the lockout is now persisted
        // on the mailbox instead of being implied by a deleted record (#1088).
        let after =
            apply_verify_otp(&conn, &otp, &sessions, &cfg, "a@x.com", "123456", "dev-1").await;
        assert!(matches!(after, Ok(VerifyOtpResult::LockedOut)));
    }
}
