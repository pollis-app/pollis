//! Tiny crate-wide primitives that have no better home.

/// Moved to `pollis-api` (#1305) so the client's diagnostics log and the DS's
/// failed-request log scrub with the same rule; re-exported so every existing
/// caller here keeps its path.
pub use pollis_api::redact::redact_ip_literals;

/// Runs one background job at a time in this process, without dropping a
/// request: a [`SingleFlight::kick`] while the job is running makes the running
/// one go round again instead of starting a second. Clones share the gate.
#[derive(Clone, Default)]
pub struct SingleFlight {
    running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl SingleFlight {
    /// Make sure `job` runs (again) after this call, at most one at a time.
    pub fn kick<F, Fut>(&self, job: F)
    where
        F: Fn() -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        use std::sync::atomic::Ordering::SeqCst;
        self.pending.store(true, SeqCst);
        if self.running.swap(true, SeqCst) {
            return;
        }
        let gate = self.clone();
        tokio::spawn(async move {
            loop {
                while gate.pending.swap(false, SeqCst) {
                    job().await;
                }
                gate.running.store(false, SeqCst);
                // A kick that landed between the last check and the release.
                if !gate.pending.load(SeqCst) || gate.running.swap(true, SeqCst) {
                    break;
                }
            }
        });
    }
}

/// Every row of a query whose first column is `TEXT`, in order.
pub async fn query_strings(
    conn: &libsql::Connection,
    sql: &str,
    params: impl libsql::params::IntoParams,
) -> anyhow::Result<Vec<String>> {
    let mut rows = conn.query(sql, params).await?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().await? {
        out.push(row.get::<String>(0)?);
    }
    Ok(out)
}

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
    /// The published client policy on the CDN (#1611); refreshed in the background.
    ClientPolicy,
    /// R2's S3 API, for the object deletes of attachment reclaim (#1881); a
    /// background pass, retried on the next one.
    R2,
}

impl Upstream {
    /// The whole-request deadline: DNS + connect + TLS + write + read.
    pub const fn timeout(self) -> std::time::Duration {
        std::time::Duration::from_secs(match self {
            // PER ATTEMPT (#1751): `livekit::twirp` may make two. LiveKit itself takes ~3.1s
            // to answer SendData for a room nobody is in (its "no response from servers"
            // 503), so 5s left under 2s for DNS + connect + TLS + the round trip.
            Upstream::LiveKit => 6,
            Upstream::Resend => 10,
            Upstream::ExpoPush => 5,
            Upstream::ClientPolicy => 5,
            Upstream::R2 => 10,
        })
    }

    /// For the 504 body and the log line.
    pub const fn name(self) -> &'static str {
        match self {
            Upstream::LiveKit => "livekit",
            Upstream::Resend => "resend",
            Upstream::ExpoPush => "expo-push",
            Upstream::ClientPolicy => "client-policy",
            Upstream::R2 => "r2",
        }
    }
}

/// **The** way the DS makes an outbound POST. Private [`http_client`] +
/// mandatory [`Upstream`] deadline; there is no un-timed alternative.
pub fn http_post(upstream: Upstream, url: &str) -> reqwest::RequestBuilder {
    http_client().post(url).timeout(upstream.timeout())
}

/// [`http_post`]'s GET twin, with the same mandatory deadline.
pub fn http_get(upstream: Upstream, url: &str) -> reqwest::RequestBuilder {
    http_client().get(url).timeout(upstream.timeout())
}

/// [`http_post`]'s DELETE twin, with the same mandatory deadline.
pub fn http_delete(upstream: Upstream, url: &str) -> reqwest::RequestBuilder {
    http_client().delete(url).timeout(upstream.timeout())
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

/// How long LiveKit's DNS + TCP + TLS phase may take (#1751). Well under the
/// per-attempt [`Upstream::LiveKit`] deadline, so a stalled connect is told
/// apart from a slow answer (`reqwest::Error::is_connect`) and leaves time for
/// the retry on a fresh connection.
pub const LIVEKIT_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// An idle pooled LiveKit connection is dropped after this long: below nginx's
/// 75s `keepalive_timeout` in front of LiveKit, so the DS closes first and never
/// writes a request into a connection the far end already gave up on.
const LIVEKIT_POOL_IDLE: std::time::Duration = std::time::Duration::from_secs(30);

/// The LiveKit client and its generation; [`reset_livekit_client`] swaps both.
static LIVEKIT_CLIENT: std::sync::RwLock<Option<(u64, reqwest::Client)>> =
    std::sync::RwLock::new(None);

/// LiveKit's own outbound client (#1751): separate from [`http_client`] so it
/// can be thrown away, with every pooled connection in it, without touching
/// the other upstreams. Same mandatory deadline as [`http_post`].
///
/// Returns the client's generation, to hand back to [`reset_livekit_client`].
pub fn livekit_post(url: &str) -> (u64, reqwest::RequestBuilder) {
    let current = LIVEKIT_CLIENT
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let (generation, client) = match current {
        Some(c) => c,
        None => {
            let mut slot = LIVEKIT_CLIENT.write().unwrap_or_else(|e| e.into_inner());
            slot.get_or_insert_with(|| (1, build_livekit_client(true))).clone()
        }
    };
    (
        generation,
        client.post(url).timeout(Upstream::LiveKit.timeout()),
    )
}

/// Replace the LiveKit client, dropping its pool, unless someone already
/// replaced generation `seen`: N concurrent failures cost one rebuild, not N.
/// Returns whether this call did the rebuild.
pub fn reset_livekit_client(seen: u64) -> bool {
    let mut slot = LIVEKIT_CLIENT.write().unwrap_or_else(|e| e.into_inner());
    match slot.as_ref() {
        Some((generation, _)) if *generation != seen => false,
        _ => {
            *slot = Some((seen + 1, build_livekit_client(true)));
            true
        }
    }
}

/// A single-use LiveKit client with no pool, for the operator probe: every
/// request it makes pays a full DNS + TCP + TLS handshake, which is the point.
pub fn livekit_probe_get(url: &str) -> reqwest::RequestBuilder {
    build_livekit_client(false)
        .get(url)
        .timeout(Upstream::LiveKit.timeout())
}

fn build_livekit_client(pooled: bool) -> reqwest::Client {
    let builder = reqwest::Client::builder()
        .connect_timeout(LIVEKIT_CONNECT_TIMEOUT)
        // Probes a connection the network silently dropped, so it errors
        // instead of absorbing a request until the deadline.
        .tcp_keepalive(std::time::Duration::from_secs(15))
        .pool_idle_timeout(LIVEKIT_POOL_IDLE)
        .pool_max_idle_per_host(if pooled { 8 } else { 0 });
    builder
        .build()
        // Only fails on a broken TLS backend; an un-timed fallback would defeat the point.
        .expect("reqwest client builds")
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
