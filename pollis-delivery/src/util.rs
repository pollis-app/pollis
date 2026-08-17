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

/// The upstreams the DS calls out to, and how long each is allowed to take.
///
/// **Every outbound request names one** — that is the point of the type. There
/// is no way to build a request through [`http_post`] without choosing a
/// deadline, and [`http_client`] is private, so "we forgot to set a timeout"
/// stopped being expressible (#913).
///
/// Before this, no outbound call had any deadline at all. `reqwest`'s default is
/// *no* timeout, so an upstream that accepts a connection and then never answers
/// pins the axum handler that called it for as long as the socket stays open —
/// and since #875 those calls all share one pooled `reqwest::Client`, a hung
/// upstream also occupies that pool's slots, so the blast radius is not limited
/// to the one handler that was unlucky.
///
/// The values are per-upstream because the calls are not alike: two of them sit
/// in front of a user waiting on a response, one is a fire-and-forget nudge, and
/// they talk to services with different latency profiles. Each is set well above
/// anything the upstream should ever need, because the deadline exists to bound a
/// *hang*, not to second-guess a slow-but-working upstream — if one of these ever
/// fires against a healthy service, raise it rather than retrying into it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Upstream {
    /// LiveKit's Twirp `RoomService` (`SendData`, `ListParticipants`) — our own
    /// self-hosted server, answering in tens of milliseconds. `ListParticipants`
    /// is a user-facing roster read, so a long stall is a visibly hung UI, and
    /// `SendData` is a fire-and-forget nudge whose whole value is being prompt.
    LiveKit,
    /// Resend, for the sign-in OTP email. Third-party and the slowest of the
    /// three, but the OTP handler awaits it before answering, so a caller who
    /// asked for a code is waiting on this.
    Resend,
    /// The Turso **Platform** API, minting a short-TTL read-only DB token. Not
    /// the data plane — this is api.turso.tech, called once per token refresh —
    /// and the client's DB access is blocked until it returns.
    TursoPlatform,
}

impl Upstream {
    /// The whole-request deadline: DNS + connect + TLS + write + read.
    pub const fn timeout(self) -> std::time::Duration {
        std::time::Duration::from_secs(match self {
            Upstream::LiveKit => 5,
            Upstream::Resend => 10,
            Upstream::TursoPlatform => 10,
        })
    }

    /// For the 504 body and the log line, so an operator reading either can tell
    /// which upstream stopped answering.
    pub const fn name(self) -> &'static str {
        match self {
            Upstream::LiveKit => "livekit",
            Upstream::Resend => "resend",
            Upstream::TursoPlatform => "turso-platform",
        }
    }
}

/// **The** way the DS makes an outbound POST. Private [`http_client`] +
/// mandatory [`Upstream`] deadline; there is no un-timed alternative.
pub fn http_post(upstream: Upstream, url: &str) -> reqwest::RequestBuilder {
    http_client().post(url).timeout(upstream.timeout())
}

/// The shared outbound HTTP client. **Private** — go through [`http_post`], which
/// is what makes the timeout unforgettable.
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
/// `connect_timeout` is a floor under the per-request deadlines rather than a
/// replacement for them: it bounds the DNS/TCP/TLS phase specifically, which is
/// the phase a black-holed upstream stalls in, and it applies to every request
/// regardless of which [`Upstream`] it names.
fn http_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .build()
                // Only fails if the TLS backend can't initialise, which is a
                // broken build rather than a runtime condition — and a DS that
                // silently fell back to an un-timed client would defeat the
                // point of this module.
                .expect("reqwest client builds")
        })
        .clone()
}
