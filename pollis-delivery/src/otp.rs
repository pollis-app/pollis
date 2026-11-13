//! Server-side OTP: generation, salted-hash storage, attempt-limited constant-time checks, Resend sends.
//!
//! Each code is attempt-limited per mailbox, constant-time compared and single-use.
//!
//! **Store:** [`OtpStore`]. The per-mailbox limits (resend throttle, send budget,
//! guess count, lockout) live in the `otp_mailbox` table under a keyed hash of
//! the address, one statement per decision; the codes live in memory.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use hmac::{Hmac, Mac};
use pollis_device_cert::constant_time_eq;
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
    /// Stored on the mailbox's row, so a fresh `request-otp` cannot clear it.
    pub lockout_secs: u64,
    /// Codes one mailbox may be sent per `ttl_secs` window; re-requests don't invalidate codes (#1088).
    pub max_sends_per_window: u32,
    /// The App Store / Play review account (`APP_REVIEW_LOGIN`): a fixed code, no email.
    pub review_login: Option<ReviewLogin>,
    /// The key mailbox rows are stored under; see [`MailboxKey`].
    pub mailbox_key: MailboxKey,
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
            mailbox_key: MailboxKey::random(),
        }
    }
}

impl OtpConfig {
    /// Build from DS env: `RESEND_API_KEY`, `DEV_OTP`, `OTP_TTL_SECS`, and the
    /// mailbox key from `LIVEKIT_API_SECRET`.
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
            mailbox_key: mailbox_key_from_env(),
            ..defaults
        }
    }
}

/// The mailbox key, derived from `LIVEKIT_API_SECRET`: a DS-only secret every
/// deployment has, so every process of one deployment computes the same rows.
/// Rotating that secret starts every mailbox afresh, as a new key must.
///
/// Without it (local dev) the key is per process, logged at WARN.
fn mailbox_key_from_env() -> MailboxKey {
    match std::env::var("LIVEKIT_API_SECRET").ok().filter(|s| !s.is_empty()) {
        Some(secret) => MailboxKey::derive(&secret),
        None => {
            tracing::warn!("LIVEKIT_API_SECRET unset — OTP mailbox limits are kept per process");
            MailboxKey::random()
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

type HmacSha256 = Hmac<Sha256>;

/// Domain separation for [`MailboxKey::derive`]: the secret it starts from has
/// another use, and the label keeps the two from ever colliding.
const MAILBOX_KEY_LABEL: &[u8] = b"pollis-otp-mailbox-v1";

/// The key a mailbox's limits are stored under in `otp_mailbox`: HMAC-SHA256
/// of the normalized address, so the table never holds an address.
///
/// Stable across restarts (that is the point of the table), so it is derived
/// from a DS secret rather than drawn per process. The key itself is never
/// stored.
#[derive(Clone)]
pub struct MailboxKey(Arc<HmacSha256>);

impl MailboxKey {
    /// A key derived from a DS-held secret, the same for every process given
    /// the same secret.
    pub fn derive(secret: &str) -> Self {
        let mut root =
            HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
        root.update(MAILBOX_KEY_LABEL);
        let key = root.finalize().into_bytes();
        Self(Arc::new(
            HmacSha256::new_from_slice(&key).expect("HMAC accepts any key length"),
        ))
    }

    /// A fresh random key, for a DS with no secret to derive from (local dev,
    /// tests). The limits then hold for this process only.
    pub fn random() -> Self {
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        let mac = HmacSha256::new_from_slice(&key).expect("HMAC accepts any key length");
        key.fill(0);
        Self(Arc::new(mac))
    }

    /// The `otp_mailbox.mailbox` value for `email` in `scope`: 64 hex characters.
    fn mailbox(&self, scope: OtpScope, email: &str) -> String {
        let mut mac = (*self.0).clone();
        mac.update(scope.label());
        mac.update(&[0]);
        mac.update(normalize_email(email).as_bytes());
        hex::encode(mac.finalize().into_bytes())
    }
}

impl Default for MailboxKey {
    fn default() -> Self {
        Self::random()
    }
}

/// Which code family a store holds. Each has its own limits per mailbox, so a
/// sign-in and an email change to one address never share a counter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OtpScope {
    SignIn,
    /// The address an account is moving TO.
    EmailChangeNew,
    /// The address an account is moving off (#1161).
    EmailChangeCurrent,
}

impl OtpScope {
    fn label(self) -> &'static [u8] {
        match self {
            OtpScope::SignIn => b"sign-in",
            OtpScope::EmailChangeNew => b"email-change-new",
            OtpScope::EmailChangeCurrent => b"email-change-current",
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

/// Map size past which [`OtpStore::prepare`] drops mailboxes with no live code.
const SWEEP_AT: usize = 1024;

/// Per-mailbox OTP state. Two halves:
///
/// - **Limits** (resend throttle, send budget, wrong-guess count, lockout) live
///   in `otp_mailbox` (migration `000047`), keyed by [`MailboxKey`]. Every
///   decision about them is one SQL statement, so they hold across restarts
///   and concurrent requests are serialized by the database.
/// - **Codes** (salted hashes) live in this process only. A code that dies
///   with the process is simply requested again; a stored hash of a six-digit
///   code would be one a database reader could reverse.
///
/// Codes accumulate (a re-request never invalidates the code being typed) and
/// are bounded by the send budget.
#[derive(Clone)]
pub struct OtpStore {
    scope: OtpScope,
    /// Mailbox key → live codes, oldest first.
    codes: Arc<Mutex<HashMap<String, Vec<CodeSlot>>>>,
}

impl Default for OtpStore {
    fn default() -> Self {
        Self::new(OtpScope::SignIn)
    }
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

/// Outcome of preparing a code for an email.
#[derive(Debug, PartialEq, Eq)]
pub enum PrepareOutcome {
    /// A fresh code was stored; email this plaintext.
    Send(String),
    /// Inside the resend throttle, at the send budget, or locked out: no email,
    /// no code. The caller still answers 200.
    Refused,
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

/// A correct code's outcome, carrying whatever the accept statement returned.
enum Verified {
    Accepted(libsql::Row),
    Rejected(VerifyOutcome),
}

/// The statement that accepts a correct code: an [`accept_sql!`] and the
/// parameters it binds from `?3`.
struct Accept {
    sql: &'static str,
    extra: Vec<libsql::Value>,
}

/// What this process's codes say about a submitted code.
enum CodeMatch {
    Matched,
    Wrong,
    /// No live code: `had_codes` when some just expired.
    NoLiveCode { had_codes: bool },
}

// Every statement below treats a row past `expires_at` as absent, whether or
// not a trigger has deleted it yet, so a decision never depends on pruning.
// In DO UPDATE, a bare column is the row's value before the statement.

/// Send a code if the mailbox may have one. `?1` mailbox, `?2` now, `?3` code
/// TTL (also the send window), `?4` resend throttle, `?5` sends per window.
/// Returns a row iff a code may go out.
///
/// Refused when locked, inside the throttle, or at the budget of a window
/// still open. An elapsed lockout clears the guess count; the guess count
/// otherwise survives a new code.
const PREPARE_SQL: &str = "\
INSERT INTO otp_mailbox (mailbox, failed, locked_until, last_sent_at, sends, window_start, updated_at, expires_at) \
VALUES (?1, 0, NULL, ?2, 1, ?2, ?2, ?2 + ?3) \
ON CONFLICT(mailbox) DO UPDATE SET \
    failed = CASE WHEN expires_at < ?2 OR locked_until <= ?2 THEN 0 ELSE failed END, \
    locked_until = NULL, \
    last_sent_at = ?2, \
    sends = CASE WHEN expires_at < ?2 OR window_start IS NULL OR ?2 - window_start >= ?3 \
        THEN 1 ELSE sends + 1 END, \
    window_start = CASE WHEN expires_at < ?2 OR window_start IS NULL OR ?2 - window_start >= ?3 \
        THEN ?2 ELSE window_start END, \
    updated_at = ?2, \
    expires_at = ?2 + ?3 \
WHERE expires_at < ?2 \
   OR ((locked_until IS NULL OR locked_until <= ?2) \
       AND (last_sent_at = 0 OR ?2 - last_sent_at >= ?4) \
       AND (window_start IS NULL OR ?2 - window_start >= ?3 OR sends < ?5)) \
RETURNING 1";

/// Count one wrong guess. `?1` mailbox, `?2` now, `?3` code TTL, `?4` max
/// attempts, `?5` lockout. Returns `locked_until` (NULL unless this guess
/// locked the mailbox); no row means it was already locked.
const WRONG_GUESS_SQL: &str = "\
INSERT INTO otp_mailbox (mailbox, failed, locked_until, last_sent_at, sends, window_start, updated_at, expires_at) \
VALUES (?1, 1, CASE WHEN 1 > ?4 THEN ?2 + ?5 END, 0, 0, NULL, ?2, \
    CASE WHEN 1 > ?4 THEN max(?2 + ?3, ?2 + ?5) ELSE ?2 + ?3 END) \
ON CONFLICT(mailbox) DO UPDATE SET \
    failed = CASE WHEN expires_at < ?2 OR locked_until <= ?2 THEN 1 ELSE failed + 1 END, \
    locked_until = CASE WHEN (CASE WHEN expires_at < ?2 OR locked_until <= ?2 THEN 1 ELSE failed + 1 END) > ?4 \
        THEN ?2 + ?5 END, \
    last_sent_at = CASE WHEN expires_at < ?2 THEN 0 ELSE last_sent_at END, \
    sends = CASE WHEN expires_at < ?2 THEN 0 ELSE sends END, \
    window_start = CASE WHEN expires_at < ?2 THEN NULL ELSE window_start END, \
    updated_at = ?2, \
    expires_at = max( \
        CASE WHEN expires_at < ?2 THEN ?2 + ?3 ELSE expires_at END, \
        CASE WHEN (CASE WHEN expires_at < ?2 OR locked_until <= ?2 THEN 1 ELSE failed + 1 END) > ?4 \
            THEN ?2 + ?5 ELSE 0 END) \
WHERE expires_at < ?2 OR locked_until IS NULL OR locked_until <= ?2 \
RETURNING locked_until";

/// Accept a correct code: unless locked, reset the mailbox to fresh (no
/// guesses, no sends; the row expires now). `?1` mailbox, `?2` now. Returns a
/// row iff accepted, carrying the given RETURNING columns so a caller can fold
/// its own read into this round trip.
macro_rules! accept_sql {
    ($returning:literal) => {
        concat!(
            "INSERT INTO otp_mailbox (mailbox, failed, locked_until, last_sent_at, sends, window_start, updated_at, expires_at) \
             VALUES (?1, 0, NULL, 0, 0, NULL, ?2, ?2) \
             ON CONFLICT(mailbox) DO UPDATE SET \
                 failed = 0, locked_until = NULL, last_sent_at = 0, sends = 0, \
                 window_start = NULL, updated_at = ?2, expires_at = ?2 \
             WHERE expires_at < ?2 OR locked_until IS NULL OR locked_until <= ?2 \
             RETURNING ",
            $returning
        )
    };
}

/// Whether the mailbox is locked now. `?1` mailbox, `?2` now.
const LOCKED_SQL: &str = "SELECT 1 FROM otp_mailbox WHERE mailbox = ?1 AND locked_until > ?2";

impl OtpStore {
    /// An empty store for one code family.
    pub fn new(scope: OtpScope) -> Self {
        Self {
            scope,
            codes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Store a fresh `code` for `email`, ADDING it to whatever codes the mailbox
    /// already has outstanding rather than replacing them. One statement.
    ///
    /// `Refused` only decides that no email goes out; the caller answers 200.
    pub async fn prepare(
        &self,
        conn: &libsql::Connection,
        email: &str,
        code: &str,
        cfg: &OtpConfig,
        now: u64,
    ) -> anyhow::Result<PrepareOutcome> {
        let mailbox = cfg.mailbox_key.mailbox(self.scope, email);
        let mut rows = conn
            .query(
                PREPARE_SQL,
                libsql::params![
                    mailbox.clone(),
                    now as i64,
                    cfg.ttl_secs as i64,
                    cfg.resend_throttle_secs as i64,
                    i64::from(cfg.max_sends_per_window),
                ],
            )
            .await?;
        let allowed = rows.next().await?.is_some();
        drop(rows);
        if !allowed {
            return Ok(PrepareOutcome::Refused);
        }

        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        let slot = CodeSlot {
            code_hash: salted_hash(&salt, code),
            salt,
            expires_at: now.saturating_add(cfg.ttl_secs),
        };
        let mut guard = self.codes.lock().expect("otp store mutex poisoned");
        if guard.len() >= SWEEP_AT {
            guard.retain(|_, slots| {
                slots.retain(|c| now <= c.expires_at);
                !slots.is_empty()
            });
        }
        guard.entry(mailbox).or_default().push(slot);
        Ok(PrepareOutcome::Send(code.to_string()))
    }

    /// Check `code` against every outstanding code WITHOUT consuming; wrong guesses count to a lockout.
    ///
    /// Success leaves the codes: [`OtpStore::consume`] runs only after downstream success (#518).
    pub async fn check(
        &self,
        conn: &libsql::Connection,
        email: &str,
        code: &str,
        cfg: &OtpConfig,
        now: u64,
    ) -> anyhow::Result<VerifyOutcome> {
        let verified = self
            .verify(conn, email, code, cfg, now, Accept { sql: accept_sql!("1"), extra: Vec::new() })
            .await?;
        Ok(match verified {
            Verified::Accepted(_) => VerifyOutcome::Ok,
            Verified::Rejected(outcome) => outcome,
        })
    }

    /// [`Self::check`], running `accept` when the code is right; its row comes
    /// back with the acceptance.
    ///
    /// A correct code costs that one statement; a wrong one costs one too.
    async fn verify(
        &self,
        conn: &libsql::Connection,
        email: &str,
        code: &str,
        cfg: &OtpConfig,
        now: u64,
        accept: Accept,
    ) -> anyhow::Result<Verified> {
        let mailbox = cfg.mailbox_key.mailbox(self.scope, email);
        match self.match_code(&mailbox, code, now) {
            CodeMatch::Matched => {
                let mut params: Vec<libsql::Value> = vec![mailbox.clone().into(), (now as i64).into()];
                params.extend(accept.extra);
                let mut rows = conn.query(accept.sql, params).await?;
                match rows.next().await? {
                    Some(row) => Ok(Verified::Accepted(row)),
                    None => {
                        self.drop_codes(&mailbox);
                        Ok(Verified::Rejected(VerifyOutcome::LockedOut))
                    }
                }
            }
            CodeMatch::Wrong => {
                let mut rows = conn
                    .query(
                        WRONG_GUESS_SQL,
                        libsql::params![
                            mailbox.clone(),
                            now as i64,
                            cfg.ttl_secs as i64,
                            i64::from(cfg.max_attempts),
                            cfg.lockout_secs as i64,
                        ],
                    )
                    .await?;
                let locked = match rows.next().await? {
                    Some(row) => row.get::<Option<i64>>(0)?.is_some(),
                    None => true,
                };
                drop(rows);
                if !locked {
                    return Ok(Verified::Rejected(VerifyOutcome::Invalid));
                }
                // A lockout drops every outstanding code.
                self.drop_codes(&mailbox);
                tracing::warn!(
                    "OTP check for {} refused — mailbox locked out by failed guesses",
                    mask_email(email)
                );
                Ok(Verified::Rejected(VerifyOutcome::LockedOut))
            }
            CodeMatch::NoLiveCode { had_codes } => {
                let mut rows = conn
                    .query(LOCKED_SQL, libsql::params![mailbox, now as i64])
                    .await?;
                let outcome = if rows.next().await?.is_some() {
                    VerifyOutcome::LockedOut
                } else if had_codes {
                    // "Expired" and "no code" render differently.
                    VerifyOutcome::Expired
                } else {
                    VerifyOutcome::NotFound
                };
                Ok(Verified::Rejected(outcome))
            }
        }
    }

    /// Compare `code` against this process's live codes for `mailbox`.
    fn match_code(&self, mailbox: &str, code: &str, now: u64) -> CodeMatch {
        let mut guard = self.codes.lock().expect("otp store mutex poisoned");
        let Some(slots) = guard.get_mut(mailbox) else {
            return CodeMatch::NoLiveCode { had_codes: false };
        };
        let had_codes = !slots.is_empty();
        slots.retain(|c| now <= c.expires_at);
        if slots.is_empty() {
            guard.remove(mailbox);
            return CodeMatch::NoLiveCode { had_codes };
        }
        // Compare against every outstanding code. `fold` rather than `any` so the
        // work does not depend on which slot matched.
        let matched = slots.iter().fold(false, |acc, slot| {
            acc | constant_time_eq(&salted_hash(&slot.salt, code), &slot.code_hash)
        });
        if matched {
            CodeMatch::Matched
        } else {
            CodeMatch::Wrong
        }
    }

    fn drop_codes(&self, mailbox: &str) {
        self.codes
            .lock()
            .expect("otp store mutex poisoned")
            .remove(mailbox);
    }

    /// Consume the mailbox's codes after a successful sign-in (#518). Idempotent.
    ///
    /// The mailbox's limits were already reset when its code was accepted.
    pub fn consume(&self, email: &str, cfg: &OtpConfig) {
        self.drop_codes(&cfg.mailbox_key.mailbox(self.scope, email));
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
    let conn = match state.db.conn().await {
        Ok(c) => c,
        Err(e) => return internal(e),
    };
    match process_request_otp(&conn, &state.otp, &state.otp_config, email).await {
        Ok(()) => ok_200(),
        Err(e) => internal(e),
    }
}

/// Generate, store and (unless DEV_OTP or no Resend key) email an OTP; shared with the harness.
pub async fn process_request_otp(
    conn: &libsql::Connection,
    otp: &OtpStore,
    cfg: &OtpConfig,
    email: &str,
) -> anyhow::Result<()> {
    process_request_otp_for(conn, otp, cfg, email, OtpPurpose::SignIn).await
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
///
/// One database statement (the mailbox's limits), then the send.
pub async fn process_request_otp_for(
    conn: &libsql::Connection,
    otp: &OtpStore,
    cfg: &OtpConfig,
    email: &str,
    purpose: OtpPurpose,
) -> anyhow::Result<()> {
    let review = cfg
        .review_login
        .as_ref()
        .filter(|r| purpose == OtpPurpose::SignIn && r.matches(email));
    let code = match (&cfg.dev_otp, review) {
        (Some(dev), _) => dev.clone(),
        (None, Some(r)) => r.code.clone(),
        (None, None) => format!("{:06}", OsRng.gen_range(0..1_000_000u32)),
    };

    let outcome = otp
        .prepare(conn, email, &code, cfg, crate::util::now_unix())
        .await?;

    match outcome {
        PrepareOutcome::Refused => {
            tracing::info!(
                "OTP request for {} not sent — throttled or locked out",
                mask_email(email)
            );
        }
        PrepareOutcome::Send(code) => {
            // DEV_OTP: skip the real send entirely.
            if cfg.dev_otp.is_some() {
                tracing::info!("DEV_OTP active — skipping email send for {}", mask_email(email));
                return Ok(());
            }
            // Nobody reads the review mailbox; the reviewer already has the code.
            if review.is_some() {
                tracing::info!("store-review sign-in requested for {}", mask_email(email));
                return Ok(());
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
    Ok(())
}

/// Send one plain-text email from `noreply@mail.pollis.com` through Resend: the one
/// place this crate talks to it.
pub(crate) async fn send_email(
    api_key: &str,
    to: &str,
    subject: &str,
    text: &str,
) -> anyhow::Result<()> {
    let body = serde_json::json!({
        "from": "Pollis <noreply@mail.pollis.com>",
        "to": [to],
        "subject": subject,
        "text": text,
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

/// Hand the code to Resend.
///
/// Awaited by the handler, so [`crate::util::Upstream::Resend`] bounds it (#913); still 200.
async fn send_otp_email(api_key: &str, email: &str, code: &str) -> anyhow::Result<()> {
    let text = format!("Your verification code is: {code}\n\nThis code expires in 10 minutes.");
    send_email(api_key, email, "Your Pollis sign-in code", &text).await
}

/// Ask the CURRENT address to authorize moving the account off it (#1161).
///
/// Sent BEFORE the change commits, naming the destination, so an unrequested code reads as an attack.
async fn send_email_change_challenge(
    api_key: &str,
    current_email: &str,
    code: &str,
) -> anyhow::Result<()> {
    let text = format!(
        "Someone asked to change the email address on your Pollis account away \
         from this one.\n\n\
         To approve it, enter this code in Pollis: {code}\n\n\
         It expires in 10 minutes.\n\n\
         If this was not you, do NOT enter the code. Someone has access to one \
         of your devices — revoke that device from Settings on a device you \
         still control, and contact support@pollis.com."
    );
    let subject = "Confirm the email change on your Pollis account";
    send_email(api_key, current_email, subject, &text).await
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
    let text = format!(
        "The email address on your Pollis account was changed to {new_email}.\n\n\
         If this was you, nothing further is needed.\n\n\
         If it was not, someone has access to one of your devices. Revoke that \
         device from Settings on a device you still control, and contact \
         support@pollis.com."
    );
    send_email(
        api_key,
        old_email,
        "Your Pollis email address was changed",
        &text,
    )
    .await
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
    // Canonicalize here: `users.email` is the identity, and the harnesses bypass the handler.
    //
    // Lowercase too (#1088): one mailbox must be one row (migration `000026`).
    let canonical = normalize_email(email);
    let email = canonical.as_str();

    // Validate WITHOUT consuming, so a DB failure below leaves the code valid for a retry (#518).
    //
    // The account lookup rides on the code's accept statement: one round trip for both.
    let verified = otp
        .verify(
            conn,
            email,
            code,
            cfg,
            crate::util::now_unix(),
            Accept {
                sql: accept_sql!(
                    "(SELECT id FROM users WHERE email = ?3), \
                     (SELECT username FROM users WHERE email = ?3), \
                     (SELECT account_id_pub FROM users WHERE email = ?3)"
                ),
                extra: vec![email.to_string().into()],
            },
        )
        .await?;
    let row = match verified {
        Verified::Accepted(row) => row,
        Verified::Rejected(VerifyOutcome::LockedOut) => return Ok(VerifyOtpResult::LockedOut),
        Verified::Rejected(_) => return Ok(VerifyOtpResult::InvalidCode),
    };

    // Create or load the account; an error here is a 5xx WITHOUT consuming the code.
    let (user_id, username, account_id_pub, is_new_account) = match row.get::<Option<String>>(0)? {
        Some(id) => {
            let uname: String = row
                .get::<Option<String>>(1)
                .ok()
                .flatten()
                .unwrap_or_else(|| email.split('@').next().unwrap_or("user").to_string());
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
    otp.consume(email, cfg);

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
    use crate::db::{ConnGuard, Db};
    use crate::session::SessionStore;

    /// A local libsql DB with the full schema. Returns the `TempDir` first so
    /// the caller holds it (#942).
    async fn db() -> (tempfile::TempDir, Db, ConnGuard) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("otp-test.db");
        let db = Db::connect_local(path.to_str().unwrap()).await.expect("local db");
        let conn = db.conn().await.unwrap();
        // Turso has FK enforcement off; match it.
        conn.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        pollis_schema::apply::single_db(&conn).await.expect("schema");
        (dir, db, conn)
    }

    /// Make every `users` INSERT fail until [`heal_users`] — the transient
    /// account-write failure #518 is about.
    async fn break_users(conn: &ConnGuard) {
        conn.execute_batch(
            "CREATE TRIGGER users_down BEFORE INSERT ON users \
             BEGIN SELECT RAISE(ABORT, 'users down'); END;",
        )
        .await
        .unwrap();
    }

    async fn heal_users(conn: &ConnGuard) {
        conn.execute_batch("DROP TRIGGER users_down;").await.unwrap();
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

    async fn prepare(s: &OtpStore, c: &ConnGuard, email: &str, code: &str, cfg: &OtpConfig, now: u64) -> PrepareOutcome {
        s.prepare(c, email, code, cfg, now).await.expect("prepare")
    }

    async fn check(s: &OtpStore, c: &ConnGuard, email: &str, code: &str, cfg: &OtpConfig, now: u64) -> VerifyOutcome {
        s.check(c, email, code, cfg, now).await.expect("check")
    }

    async fn rows(c: &ConnGuard) -> Vec<String> {
        let mut out = Vec::new();
        let mut r = c.query("SELECT mailbox FROM otp_mailbox ORDER BY mailbox", ()).await.unwrap();
        while let Some(row) = r.next().await.unwrap() {
            out.push(row.get(0).unwrap());
        }
        out
    }

    #[tokio::test]
    async fn check_does_not_consume_a_correct_code_consume_does() {
        let (_d, _db, c) = db().await;
        let store = OtpStore::default();
        let cfg = cfg();
        prepare(&store, &c, "a@x.com", "123456", &cfg, 1000).await;
        // A correct code checks Ok — and stays valid; checking again still Ok (the
        // #518 fix: check alone must not burn the code).
        assert_eq!(check(&store, &c, "a@x.com", "123456", &cfg, 1000).await, VerifyOutcome::Ok);
        assert_eq!(check(&store, &c, "a@x.com", "123456", &cfg, 1000).await, VerifyOutcome::Ok);
        // Single-use is enforced by consume, not by check.
        store.consume("a@x.com", &cfg);
        assert_eq!(check(&store, &c, "a@x.com", "123456", &cfg, 1000).await, VerifyOutcome::NotFound);
        // consume is idempotent.
        store.consume("a@x.com", &cfg);
    }

    #[tokio::test]
    async fn lockout_after_six_wrong_then_correct_fails() {
        let (_d, _db, c) = db().await;
        let store = OtpStore::default();
        let cfg = cfg();
        prepare(&store, &c, "a@x.com", "123456", &cfg, 1000).await;
        // 5 wrong guesses are merely invalid.
        for _ in 0..5 {
            assert_eq!(check(&store, &c, "a@x.com", "000000", &cfg, 1000).await, VerifyOutcome::Invalid);
        }
        // The 6th locks out and drops the code.
        assert_eq!(check(&store, &c, "a@x.com", "000000", &cfg, 1000).await, VerifyOutcome::LockedOut);
        // The correct code no longer works.
        assert_eq!(check(&store, &c, "a@x.com", "123456", &cfg, 1000).await, VerifyOutcome::LockedOut);
    }

    /// #1088: the guess budget is per mailbox, so a new request must not reset it.
    #[tokio::test]
    async fn a_fresh_request_does_not_refill_the_guess_budget() {
        let (_d, _db, c) = db().await;
        let store = OtpStore::default();
        let cfg = cfg();
        prepare(&store, &c, "a@x.com", "123456", &cfg, 1000).await;
        for _ in 0..5 {
            assert_eq!(check(&store, &c, "a@x.com", "000000", &cfg, 1000).await, VerifyOutcome::Invalid);
        }
        prepare(&store, &c, "a@x.com", "654321", &cfg, 1000).await;
        assert_eq!(
            check(&store, &c, "a@x.com", "000000", &cfg, 1000).await,
            VerifyOutcome::LockedOut,
            "the 6th wrong guess must lock out even across a re-request"
        );
    }

    /// Once locked, a new request must not clear the lockout or mint a usable code.
    #[tokio::test]
    async fn a_locked_mailbox_cannot_be_unlocked_by_requesting_a_new_code() {
        let (_d, _db, c) = db().await;
        let store = OtpStore::default();
        let cfg = cfg();
        prepare(&store, &c, "a@x.com", "123456", &cfg, 1000).await;
        for _ in 0..6 {
            check(&store, &c, "a@x.com", "000000", &cfg, 1000).await;
        }
        assert_eq!(prepare(&store, &c, "a@x.com", "654321", &cfg, 1000).await, PrepareOutcome::Refused);
        assert_eq!(
            check(&store, &c, "a@x.com", "654321", &cfg, 1000).await,
            VerifyOutcome::LockedOut,
            "no code may be stored while the mailbox is locked"
        );
        // The lockout is time-bounded, not permanent: past `lockout_secs` the
        // mailbox is usable again and the guess budget is fresh.
        let after = 1000 + cfg.lockout_secs + 1;
        assert!(matches!(prepare(&store, &c, "a@x.com", "654321", &cfg, after).await, PrepareOutcome::Send(_)));
        for _ in 0..cfg.max_attempts {
            assert_eq!(check(&store, &c, "a@x.com", "000000", &cfg, after).await, VerifyOutcome::Invalid);
        }
        assert_eq!(check(&store, &c, "a@x.com", "654321", &cfg, after).await, VerifyOutcome::Ok);
    }

    /// A code that matched before a lockout landed is still refused: the
    /// database decides, not this process's codes.
    #[tokio::test]
    async fn a_correct_code_is_refused_while_the_row_is_locked() {
        let (_d, _db, c) = db().await;
        let store = OtpStore::default();
        let cfg = cfg();
        prepare(&store, &c, "a@x.com", "123456", &cfg, 1000).await;
        c.execute("UPDATE otp_mailbox SET locked_until = 5000, expires_at = 5000", ())
            .await
            .unwrap();
        assert_eq!(check(&store, &c, "a@x.com", "123456", &cfg, 1000).await, VerifyOutcome::LockedOut);
        // And the refusal dropped the code.
        assert_eq!(
            check(&store, &c, "a@x.com", "123456", &cfg, 5001).await,
            VerifyOutcome::NotFound
        );
    }

    /// #1088: a new request must not invalidate the code a victim is about to type.
    #[tokio::test]
    async fn a_new_request_leaves_the_outstanding_code_valid() {
        let (_d, _db, c) = db().await;
        let store = OtpStore::default();
        let cfg = cfg();
        prepare(&store, &c, "a@x.com", "111111", &cfg, 1000).await;
        prepare(&store, &c, "a@x.com", "222222", &cfg, 1005).await;
        assert_eq!(check(&store, &c, "a@x.com", "111111", &cfg, 1010).await, VerifyOutcome::Ok);
        assert_eq!(check(&store, &c, "a@x.com", "222222", &cfg, 1010).await, VerifyOutcome::Ok);
    }

    /// Codes accumulating is only safe with a ceiling on how many a mailbox can
    /// be sent, or the attacker just mints unbounded valid codes instead.
    #[tokio::test]
    async fn a_mailbox_is_capped_at_max_sends_per_window() {
        let (_d, _db, c) = db().await;
        let store = OtpStore::default();
        let cfg = cfg();
        for i in 0..cfg.max_sends_per_window {
            assert!(
                matches!(prepare(&store, &c, "a@x.com", "111111", &cfg, 1000 + i as u64).await, PrepareOutcome::Send(_)),
                "send {i} is inside the budget"
            );
        }
        let at = 1000 + cfg.max_sends_per_window as u64;
        assert_eq!(prepare(&store, &c, "a@x.com", "222222", &cfg, at).await, PrepareOutcome::Refused);
        // The window rolls over.
        let next = 1000 + cfg.ttl_secs + 1;
        assert!(matches!(prepare(&store, &c, "a@x.com", "222222", &cfg, next).await, PrepareOutcome::Send(_)));
    }

    #[tokio::test]
    async fn throttle_skips_resend() {
        let (_d, _db, c) = db().await;
        let store = OtpStore::default();
        let cfg = OtpConfig::default();
        assert!(matches!(prepare(&store, &c, "a@x.com", "111111", &cfg, 1000).await, PrepareOutcome::Send(_)));
        assert_eq!(prepare(&store, &c, "a@x.com", "222222", &cfg, 1010).await, PrepareOutcome::Refused);
        assert!(matches!(
            prepare(&store, &c, "a@x.com", "222222", &cfg, 1000 + cfg.resend_throttle_secs).await,
            PrepareOutcome::Send(_)
        ));
    }

    #[tokio::test]
    async fn expired_code_rejected() {
        let (_d, _db, c) = db().await;
        let store = OtpStore::default();
        let cfg = cfg();
        prepare(&store, &c, "a@x.com", "123456", &cfg, 1000).await;
        assert_eq!(check(&store, &c, "a@x.com", "123456", &cfg, 2000).await, VerifyOutcome::Expired);
    }

    /// A completed sign-in starts the mailbox afresh: no guesses held against
    /// it and a full send budget.
    #[tokio::test]
    async fn an_accepted_code_resets_the_mailbox() {
        let (_d, _db, c) = db().await;
        let store = OtpStore::default();
        let cfg = cfg();
        for i in 0..cfg.max_sends_per_window {
            prepare(&store, &c, "a@x.com", "123456", &cfg, 1000 + i as u64).await;
        }
        for _ in 0..cfg.max_attempts {
            check(&store, &c, "a@x.com", "000000", &cfg, 1010).await;
        }
        assert_eq!(check(&store, &c, "a@x.com", "123456", &cfg, 1010).await, VerifyOutcome::Ok);
        store.consume("a@x.com", &cfg);
        for i in 0..cfg.max_sends_per_window {
            assert!(matches!(
                prepare(&store, &c, "a@x.com", "654321", &cfg, 1011 + i as u64).await,
                PrepareOutcome::Send(_)
            ));
        }
        for _ in 0..cfg.max_attempts {
            assert_eq!(check(&store, &c, "a@x.com", "000000", &cfg, 1020).await, VerifyOutcome::Invalid);
        }
    }

    /// Sign-in and the two email-change challenges count separately, even for one address.
    #[tokio::test]
    async fn each_scope_has_its_own_limits() {
        let (_d, _db, c) = db().await;
        let cfg = OtpConfig::default();
        let stores = [
            OtpStore::new(OtpScope::SignIn),
            OtpStore::new(OtpScope::EmailChangeNew),
            OtpStore::new(OtpScope::EmailChangeCurrent),
        ];
        for s in &stores {
            assert!(matches!(prepare(s, &c, "a@x.com", "111111", &cfg, 1000).await, PrepareOutcome::Send(_)));
        }
        assert_eq!(rows(&c).await.len(), 3);
    }

    /// The table holds a keyed hash, never the address, and refuses anything else.
    #[tokio::test]
    async fn rows_hold_a_keyed_hash_never_the_address() {
        let (_d, _db, c) = db().await;
        let store = OtpStore::default();
        let cfg = cfg();
        prepare(&store, &c, " Alice@Example.com ", "111111", &cfg, 1000).await;
        let stored = rows(&c).await;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].len(), 64);
        assert!(!stored[0].contains("alice") && !stored[0].contains('@'));
        // Same address under another key: another row.
        let other = OtpConfig { mailbox_key: MailboxKey::derive("another secret"), ..cfg.clone() };
        prepare(&store, &c, "alice@example.com", "111111", &other, 1000).await;
        assert_eq!(rows(&c).await.len(), 2);
        // The schema itself refuses an address.
        let raw = c
            .execute(
                "INSERT INTO otp_mailbox (mailbox, updated_at, expires_at) VALUES ('alice@example.com', 1, 2)",
                (),
            )
            .await;
        assert!(raw.is_err(), "an address must not fit the mailbox column");
    }

    #[test]
    fn a_derived_key_is_stable_and_secret_dependent() {
        let a = MailboxKey::derive("s1").mailbox(OtpScope::SignIn, "a@x.com");
        assert_eq!(a, MailboxKey::derive("s1").mailbox(OtpScope::SignIn, " A@X.com"));
        assert_ne!(a, MailboxKey::derive("s2").mailbox(OtpScope::SignIn, "a@x.com"));
        assert_ne!(a, MailboxKey::derive("s1").mailbox(OtpScope::EmailChangeNew, "a@x.com"));
    }

    /// Rows past their expiry go with the next write; a live code, a lockout or
    /// an open send window keeps a row.
    #[tokio::test]
    async fn the_next_write_prunes_only_rows_that_hold_nothing() {
        let (_d, _db, c) = db().await;
        let store = OtpStore::default();
        let cfg = cfg();
        prepare(&store, &c, "stale@x.com", "111111", &cfg, 1000).await;
        prepare(&store, &c, "locked@x.com", "111111", &cfg, 1000).await;
        for _ in 0..=cfg.max_attempts {
            check(&store, &c, "locked@x.com", "000000", &cfg, 1000).await;
        }
        let stale = cfg.mailbox_key.mailbox(OtpScope::SignIn, "stale@x.com");
        let locked = cfg.mailbox_key.mailbox(OtpScope::SignIn, "locked@x.com");
        let live = cfg.mailbox_key.mailbox(OtpScope::SignIn, "live@x.com");

        // Past `stale@x.com`'s code TTL and send window, still inside the lockout.
        let later = 1000 + cfg.ttl_secs + 1;
        prepare(&store, &c, "live@x.com", "222222", &cfg, later).await;
        let mut want = vec![live, locked.clone()];
        want.sort();
        assert_eq!(rows(&c).await, want);
        assert!(!rows(&c).await.contains(&stale));

        // Past the lockout too: only the newest row is left.
        let much_later = 1000 + cfg.lockout_secs + 1;
        prepare(&store, &c, "live@x.com", "333333", &cfg, much_later).await;
        assert_eq!(rows(&c).await.len(), 1);
    }

    /// A row past expiry counts as absent even before anything prunes it.
    #[tokio::test]
    async fn an_expired_row_reads_as_fresh() {
        let (_d, _db, c) = db().await;
        let store = OtpStore::default();
        let cfg = cfg();
        prepare(&store, &c, "a@x.com", "111111", &cfg, 1000).await;
        for _ in 0..cfg.max_attempts {
            check(&store, &c, "a@x.com", "000000", &cfg, 1000).await;
        }
        let later = 1000 + cfg.ttl_secs + 1;
        prepare(&store, &c, "a@x.com", "222222", &cfg, later).await;
        for _ in 0..cfg.max_attempts {
            assert_eq!(check(&store, &c, "a@x.com", "000000", &cfg, later).await, VerifyOutcome::Invalid);
        }
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
        let (_d, _db, c) = db().await;
        let store = OtpStore::default();
        let cfg = review_cfg();
        process_request_otp(&c, &store, &cfg, " REVIEW@pollis.com ").await.unwrap();
        let now = crate::util::now_unix();
        assert_eq!(check(&store, &c, "review@pollis.com", "424242", &cfg, now).await, VerifyOutcome::Ok);
    }

    /// Every other address still gets a random code — the fixed code is not a
    /// master key.
    #[tokio::test]
    async fn the_fixed_code_does_not_sign_in_any_other_address() {
        let (_d, _db, c) = db().await;
        let store = OtpStore::default();
        let cfg = review_cfg();
        process_request_otp(&c, &store, &cfg, "alice@x.com").await.unwrap();
        let now = crate::util::now_unix();
        assert_ne!(check(&store, &c, "alice@x.com", "424242", &cfg, now).await, VerifyOutcome::Ok);
    }

    /// Only sign-in is fixed; an email-change challenge to the review address
    /// is an ordinary random code.
    #[tokio::test]
    async fn an_email_change_challenge_to_the_review_address_is_not_fixed() {
        let (_d, _db, c) = db().await;
        let store = OtpStore::new(OtpScope::EmailChangeCurrent);
        let cfg = review_cfg();
        process_request_otp_for(&c, &store, &cfg, "review@pollis.com", OtpPurpose::EmailChangeChallenge)
            .await
            .unwrap();
        let now = crate::util::now_unix();
        assert_ne!(check(&store, &c, "review@pollis.com", "424242", &cfg, now).await, VerifyOutcome::Ok);
    }

    /// The fixed code buys no extra guesses: the review mailbox locks out like
    /// any other, and while locked even the right code is refused.
    #[tokio::test]
    async fn the_review_mailbox_still_locks_out() {
        let (_d, _db, c) = db().await;
        let store = OtpStore::default();
        let cfg = review_cfg();
        process_request_otp(&c, &store, &cfg, "review@pollis.com").await.unwrap();
        let now = crate::util::now_unix();
        for _ in 0..=cfg.max_attempts {
            check(&store, &c, "review@pollis.com", "000000", &cfg, now).await;
        }
        assert_eq!(check(&store, &c, "review@pollis.com", "424242", &cfg, now).await, VerifyOutcome::LockedOut);
    }

    /// Single-use and unrequested-code rules still hold: without a request
    /// there is nothing to verify, and a consumed code does not replay.
    #[tokio::test]
    async fn the_review_code_is_single_use_and_needs_a_request() {
        let (_d, _db, c) = db().await;
        let store = OtpStore::default();
        let cfg = review_cfg();
        let now = crate::util::now_unix();
        assert_eq!(check(&store, &c, "review@pollis.com", "424242", &cfg, now).await, VerifyOutcome::NotFound);
        process_request_otp(&c, &store, &cfg, "review@pollis.com").await.unwrap();
        assert_eq!(check(&store, &c, "review@pollis.com", "424242", &cfg, now).await, VerifyOutcome::Ok);
        store.consume("review@pollis.com", &cfg);
        assert_eq!(check(&store, &c, "review@pollis.com", "424242", &cfg, now).await, VerifyOutcome::NotFound);
    }

    /// Count the `users` rows, so "did this create a second account?" is a
    /// direct observation rather than an inference.
    async fn user_count(conn: &ConnGuard) -> i64 {
        let mut rows = conn.query("SELECT COUNT(*) FROM users", ()).await.unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    async fn verify(
        conn: &ConnGuard,
        otp: &OtpStore,
        sessions: &SessionStore,
        cfg: &OtpConfig,
        email: &str,
    ) -> VerifyOtpResult {
        otp.prepare(conn, email, "123456", &no_throttle(cfg), crate::util::now_unix())
            .await
            .unwrap();
        apply_verify_otp(conn, otp, sessions, cfg, email, "123456", "dev-1")
            .await
            .expect("verify-otp must succeed")
    }

    /// Two spellings of one address must land on one row, at the INSERT.
    #[tokio::test]
    async fn the_same_address_spelled_two_ways_resolves_to_one_account() {
        let (_dir, _db, conn) = db().await;
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
        let (_dir, _db, conn) = db().await;
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
        let (_dir, _db, conn) = db().await;
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
        let (_dir, _db, conn) = db().await;
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
            VerifyOtpResult::Ok { has_identity, is_new_account, account_id_pub, .. } => {
                assert!(!is_new_account);
                assert!(has_identity);
                assert_eq!(account_id_pub, Some(crate::util::b64(&[7u8; 32])));
            }
            _ => panic!("expected Ok"),
        }
    }

    // #518: a correct code + a failing account-write must surface as an error (5xx),
    // NOT "invalid code", and must leave the code usable so an immediate retry
    // succeeds once the DB is healthy.
    #[tokio::test]
    async fn correct_code_with_failing_db_write_errors_then_retry_succeeds() {
        let (_dir, _db, conn) = db().await;
        break_users(&conn).await;
        let otp = OtpStore::default();
        let sessions = SessionStore::default();
        let cfg = OtpConfig::default();
        otp.prepare(&conn, "a@x.com", "123456", &no_throttle(&cfg), crate::util::now_unix())
            .await
            .unwrap();

        let first =
            apply_verify_otp(&conn, &otp, &sessions, &cfg, "a@x.com", "123456", "dev-1").await;
        assert!(
            first.is_err(),
            "a failed account-write must surface as an error (5xx), not consume the code"
        );

        // Heal the DB and retry the SAME code — it must succeed (was not burned).
        heal_users(&conn).await;
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
        let (_dir, _db, conn) = db().await;
        let otp = OtpStore::default();
        let sessions = SessionStore::default();
        let cfg = OtpConfig::default();
        otp.prepare(&conn, "a@x.com", "123456", &no_throttle(&cfg), crate::util::now_unix())
            .await
            .unwrap();

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
        let (_dir, _db, conn) = db().await;
        break_users(&conn).await;
        let otp = OtpStore::default();
        let sessions = SessionStore::default();
        let cfg = OtpConfig::default();
        otp.prepare(&conn, "a@x.com", "123456", &no_throttle(&cfg), crate::util::now_unix())
            .await
            .unwrap();

        for _ in 0..cfg.max_attempts {
            let r =
                apply_verify_otp(&conn, &otp, &sessions, &cfg, "a@x.com", "000000", "dev-1").await;
            assert!(matches!(r, Ok(VerifyOtpResult::InvalidCode)));
        }
        let locked =
            apply_verify_otp(&conn, &otp, &sessions, &cfg, "a@x.com", "000000", "dev-1").await;
        assert!(matches!(locked, Ok(VerifyOtpResult::LockedOut)));
        // The correct code is gone too, and the answer is "locked" (stored on the mailbox, #1088).
        let after =
            apply_verify_otp(&conn, &otp, &sessions, &cfg, "a@x.com", "123456", "dev-1").await;
        assert!(matches!(after, Ok(VerifyOtpResult::LockedOut)));
    }
}
