//! Tiny crate-wide primitives that have no better home.

/// Moved to `pollis-api` (#1305) so the client's diagnostics log and the DS's
/// failed-request log scrub with the same rule; re-exported so every existing
/// caller here keeps its path.
pub use pollis_api::redact::redact_ip_literals;

pub fn now_unix() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The upstreams the DS calls out to, and how long each is allowed to take.
///
/// **Every outbound request names one**, so a missing timeout is inexpressible (#913).
///
/// Each bounds a *hang*, well above need; if one fires on a healthy service, raise it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Upstream {
    /// LiveKit's Twirp `RoomService`, self-hosted; a stall is a hung UI.
    LiveKit,
    /// Resend, for the OTP email, which the handler awaits.
    Resend,
    /// Expo push (#987); nobody waits on it, hence the tightest deadline.
    ExpoPush,
}

impl Upstream {
    /// The whole-request deadline: DNS + connect + TLS + write + read.
    pub const fn timeout(self) -> std::time::Duration {
        std::time::Duration::from_secs(match self {
            Upstream::LiveKit => 5,
            Upstream::Resend => 10,
            Upstream::ExpoPush => 5,
        })
    }

    /// For the 504 body and the log line.
    pub const fn name(self) -> &'static str {
        match self {
            Upstream::LiveKit => "livekit",
            Upstream::Resend => "resend",
            Upstream::ExpoPush => "expo-push",
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
/// A freshly built client has an empty pool, so one per call pays a full handshake (#875).
///
/// `connect_timeout` bounds the DNS/TCP/TLS phase, where a black-holed upstream stalls.
fn http_client() -> reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(5))
                .build()
                // Only fails on a broken TLS backend; an un-timed fallback would defeat the point.
                .expect("reqwest client builds")
        })
        .clone()
}

// Bounded `IN (…)` lists (#916): the DS's copy of `pollis_core::db::chunk`. Keep them in step.
// `ack_welcomes` binds a caller-sized list, so it must be chunked.

/// Maximum number of values bound into a single statement. Mirrors
/// `pollis_core::db::chunk::MAX_BIND_PARAMS`; see there for why it is this low.
pub const MAX_BIND_PARAMS: usize = 500;

/// Split `items` into chunks that fit within [`MAX_BIND_PARAMS`], leaving
/// `reserved` slots for the statement's fixed (non-list) parameters.
pub fn bind_chunks<T>(items: &[T], reserved: usize) -> std::slice::Chunks<'_, T> {
    let per_chunk = MAX_BIND_PARAMS.saturating_sub(reserved).max(1);
    items.chunks(per_chunk)
}

/// The read paths' `IN (…)` chunk size — deliberately far below
/// [`MAX_BIND_PARAMS`], with room for the fixed leading parameters.
pub const BIND_CHUNK: usize = 100;

/// `?first,?first+1,…` for `count` values — the `IN (…)` body.
pub fn placeholders(count: usize, first: usize) -> String {
    let mut out = String::with_capacity(count * 5);
    for i in 0..count {
        if i > 0 {
            out.push(',');
        }
        out.push('?');
        out.push_str(&(first + i).to_string());
    }
    out
}

/// Encode bytes as STANDARD base64 — the encoding every binary wire field uses.
pub fn b64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Decode STANDARD base64. Callers that only care whether it parsed use `.ok()`.
pub fn b64_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    use base64::Engine as _;
    Ok(base64::engine::general_purpose::STANDARD.decode(s)?)
}

#[cfg(test)]
mod tests {
    use super::redact_ip_literals;

    #[test]
    fn ip_literals_are_redacted_wherever_they_hide() {
        let cases = [
            ("203.0.113.7 (linux)", "[redacted] (linux)"),
            ("ip-10-0-0-12 (linux)", "[redacted] (linux)"),
            ("c-73-162-1-2.hsd1.ca.comcast.net (macos)", "[redacted] (macos)"),
            ("192_168_1_5.lan (windows)", "[redacted] (windows)"),
            ("name=ip-10-0-0-5 (linux)", "name=[redacted] (linux)"),
            ("2001:db8::1", "[redacted]"),
            ("fe80::1%en0 box", "[redacted] box"),
            ("[2001:db8::1]:443", "[redacted]"),
            ("10.0.0.1:8080", "[redacted]"),
        ];
        for (input, want) in cases {
            assert_eq!(redact_ip_literals(input), want, "input {input:?}");
        }
    }

    #[test]
    fn ordinary_names_are_untouched() {
        for name in [
            "macOS desktop",
            "Pollis on Windows",
            "dans-macbook-pro (macos)",
            "Pixel 8",
            "name=iPhone 15",
            "via=qr,approver=01HQ7Z",
            "v1.2.3 build",
            "credential=signature,new_identity_version=3",
        ] {
            assert_eq!(redact_ip_literals(name), name);
        }
    }
}
