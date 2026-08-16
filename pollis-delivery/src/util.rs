//! Tiny crate-wide primitives that have no better home.
//!
//! Until #875 the DS carried six copies of the wall-clock helper below —
//! `auth`, `bootstrap`, `broker`, `email_change`, `otp` and `ratelimit` each had
//! their own — in two different return types. Identical copies are cheap right
//! up until one of them is edited.

/// Current wall-clock time, whole seconds since the Unix epoch.
///
/// A clock behind the epoch reads as `0` rather than panicking: every caller
/// uses this for TTLs, lockouts and rate-limit stamps, where "a very old
/// timestamp" degrades gracefully and a panic takes the DS down.
///
/// Returns `u64` because a Unix second is not negative. [`crate::auth`] casts at
/// its boundary — request-signature skew is compared against a *client*-supplied
/// timestamp, so that arithmetic has to be signed or a client ahead of the
/// server wraps into "valid".
pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// **The** outbound HTTP client for the DS. Use this instead of
/// `reqwest::Client::new()`.
///
/// `reqwest::Client` is `Arc`-backed — cloning is cheap and shares the
/// connection pool — but a *freshly built* one starts with an EMPTY pool, so
/// constructing per call silently pays a full DNS + TCP + TLS handshake on
/// every single request. Until #875 the DS did exactly that at four sites, all
/// on request-serving paths: the OTP email send (`otp`), and LiveKit `SendData`,
/// LiveKit `ListParticipants` and the Turso read-only token mint (`broker`). The
/// client half of the same anti-pattern was already retired by
/// `pollis_relay::http` — whose docs put the measured cost at 3-4.5s per POST on
/// a mobile dev build — and this is the server half.
///
/// Deliberately configured EXACTLY like `reqwest::Client::new()`, i.e. with no
/// request timeout, so this change is a pooling change and nothing else. (The
/// DS having no timeout on outbound LiveKit/Resend calls is a separate hazard —
/// a hung upstream pins an axum handler — but giving it one is a behaviour
/// change and belongs in its own ticket.)
pub fn http_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(reqwest::Client::new).clone()
}
