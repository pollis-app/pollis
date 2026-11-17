//! The one place Pollis fetches a URL a person typed (#1987): the SENDER's
//! device, building a preview of a link it is about to send.
//!
//! Every rule here exists because the URL is arbitrary text:
//!
//! - **https only, on 443 only**, at every hop. Redirects are followed by hand
//!   (at most [`MAX_REDIRECTS`]) so each hop is checked like the first.
//! - **Public addresses only.** The host is resolved here, every address it
//!   resolves to must be public ([`is_public`]), and the connection is pinned to
//!   the checked address, so a second DNS answer cannot swap in a private one.
//!   A link can never make the sender's device probe their own network.
//! - **Direct, never proxied.** The relay overlay has no open-internet exit, and
//!   a system proxy would see every link a person previews. Strict overlay mode
//!   never gets here (`super::fetch_link_preview`).
//! - **Bounded.** One deadline for the whole preview, byte caps on what is read,
//!   no cookies, a generic user agent.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use tokio::time::Instant;

/// Redirects followed before giving up.
pub const MAX_REDIRECTS: usize = 5;
/// The whole preview — page, redirects and image — must finish within this.
pub const DEADLINE: Duration = Duration::from_secs(5);
/// HTML read from the page; metadata lives in `<head>`.
pub const MAX_PAGE_BYTES: usize = 512 * 1024;
/// Image bytes read before decoding.
pub const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;

/// A common desktop browser's, naming neither Pollis nor the platform: a
/// distinctive agent would tell every previewed site this person uses Pollis.
const USER_AGENT: &str =
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Where a fetch may go. Production has exactly one value, [`Reach::PUBLIC`];
/// the loopback variant exists only in this crate's unit tests, so no setting,
/// environment variable or feature can loosen a shipped build.
#[derive(Clone, Copy)]
pub(crate) struct Reach {
    allow_loopback_http: bool,
}

impl Reach {
    pub(crate) const PUBLIC: Reach = Reach { allow_loopback_http: false };

    /// Whether a preview may start at `url` at all.
    pub(crate) fn permits_start(self, url: &str) -> bool {
        self.allow_loopback_http || super::wire::is_previewable_url(url)
    }

    /// A local test server: plain http on 127.0.0.1, any port. Tests only.
    #[cfg(test)]
    pub(crate) const LOOPBACK_TEST: Reach = Reach { allow_loopback_http: true };
}

/// Why a fetch stopped. Never shown to anyone; a preview just doesn't appear.
#[derive(Debug, PartialEq, Eq)]
pub enum Refused {
    BadUrl,
    Scheme,
    Port,
    PrivateAddress,
    Resolve,
    TooManyRedirects,
    Timeout,
    Status(u16),
    ContentType,
    TooLarge,
    Network,
}

/// What came back.
#[derive(Debug)]
pub struct Fetched {
    /// The URL after redirects: relative links in the page resolve against it.
    pub url: url::Url,
    pub body: Vec<u8>,
}

/// What kind of response is acceptable.
#[derive(Clone, Copy)]
pub enum Want {
    Html,
    Image,
}

impl Want {
    fn accept(self) -> &'static str {
        match self {
            Want::Html => "text/html,application/xhtml+xml;q=0.9",
            Want::Image => "image/jpeg,image/png,image/webp,image/gif;q=0.9",
        }
    }

    fn max_bytes(self) -> usize {
        match self {
            Want::Html => MAX_PAGE_BYTES,
            Want::Image => MAX_IMAGE_BYTES,
        }
    }

    fn content_type_ok(self, ct: &str) -> bool {
        let ct = ct.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
        match self {
            Want::Html => ct == "text/html" || ct == "application/xhtml+xml",
            Want::Image => matches!(
                ct.as_str(),
                "image/jpeg" | "image/jpg" | "image/png" | "image/webp" | "image/gif"
            ),
        }
    }

    /// An oversized page still has its `<head>` in the first bytes; an image cut short is useless.
    fn truncate_ok(self) -> bool {
        matches!(self, Want::Html)
    }
}

/// Fetch `start` under every rule above, finishing by `deadline`.
pub(crate) async fn fetch(
    start: &url::Url,
    want: Want,
    reach: Reach,
    deadline: Instant,
) -> Result<Fetched, Refused> {
    let mut current = start.clone();
    for _ in 0..=MAX_REDIRECTS {
        let addr = checked_address(&current, reach, deadline).await?;
        let host = current.host_str().ok_or(Refused::BadUrl)?.to_string();
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .filter(|d| !d.is_zero())
            .ok_or(Refused::Timeout)?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(!reach.allow_loopback_http)
            .resolve(&host, addr)
            .timeout(remaining)
            .user_agent(USER_AGENT)
            .build()
            .map_err(|_| Refused::Network)?;
        let response = client
            .get(current.clone())
            .header(reqwest::header::ACCEPT, want.accept())
            .send()
            .await
            .map_err(|e| if e.is_timeout() { Refused::Timeout } else { Refused::Network })?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or(Refused::Status(status.as_u16()))?;
            current = current.join(location).map_err(|_| Refused::BadUrl)?;
            continue;
        }
        if !status.is_success() {
            return Err(Refused::Status(status.as_u16()));
        }
        let ct = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !want.content_type_ok(ct) {
            return Err(Refused::ContentType);
        }
        let body = read_capped(response, want).await?;
        return Ok(Fetched { url: current, body });
    }
    Err(Refused::TooManyRedirects)
}

async fn read_capped(mut response: reqwest::Response, want: Want) -> Result<Vec<u8>, Refused> {
    let cap = want.max_bytes();
    if response.content_length().is_some_and(|n| n as usize > cap) && !want.truncate_ok() {
        return Err(Refused::TooLarge);
    }
    let mut body = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                let room = cap - body.len();
                if chunk.len() > room {
                    if !want.truncate_ok() {
                        return Err(Refused::TooLarge);
                    }
                    body.extend_from_slice(&chunk[..room]);
                    return Ok(body);
                }
                body.extend_from_slice(&chunk);
            }
            Ok(None) => return Ok(body),
            Err(e) if e.is_timeout() => return Err(Refused::Timeout),
            Err(_) => return Err(Refused::Network),
        }
    }
}

/// Check scheme and port, resolve the host, and return an address to pin the
/// connection to — only if EVERY address the name resolves to is public.
async fn checked_address(url: &url::Url, reach: Reach, deadline: Instant) -> Result<SocketAddr, Refused> {
    match url.scheme() {
        "https" => {}
        "http" if reach.allow_loopback_http => {}
        _ => return Err(Refused::Scheme),
    }
    let port = url.port_or_known_default().ok_or(Refused::Port)?;
    if port != 443 && !reach.allow_loopback_http {
        return Err(Refused::Port);
    }
    let host = url.host().ok_or(Refused::BadUrl)?;
    let addrs: Vec<SocketAddr> = match host {
        url::Host::Ipv4(ip) => vec![SocketAddr::new(IpAddr::V4(ip), port)],
        url::Host::Ipv6(ip) => vec![SocketAddr::new(IpAddr::V6(ip), port)],
        url::Host::Domain(name) => {
            let lookup = tokio::net::lookup_host((name, port));
            match tokio::time::timeout_at(deadline, lookup).await {
                Ok(Ok(found)) => found.collect(),
                Ok(Err(_)) => return Err(Refused::Resolve),
                Err(_) => return Err(Refused::Timeout),
            }
        }
    };
    if addrs.is_empty() {
        return Err(Refused::Resolve);
    }
    let allowed = |ip: IpAddr| is_public(ip) || (reach.allow_loopback_http && ip.is_loopback());
    if !addrs.iter().all(|a| allowed(a.ip())) {
        return Err(Refused::PrivateAddress);
    }
    Ok(addrs[0])
}

/// Whether `ip` is an ordinary address on the public internet. Everything
/// special-purpose — loopback, private, link-local, CGNAT, ULA, multicast,
/// documentation, benchmarking, reserved, and v6 forms embedding any of those
/// — is refused.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || a == 127
        || (a == 100 && (64..=127).contains(&b))
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && c == 0)
        || (a == 192 && b == 0 && c == 2)
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 198 && (18..=19).contains(&b))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let seg = ip.segments();
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    // NAT64 well-known prefix 64:ff9b::/96 carries a v4 address in its low bits.
    if seg[0] == 0x64 && seg[1] == 0xff9b && seg[2..6].iter().all(|s| *s == 0) {
        let o = ip.octets();
        return is_public_v4(Ipv4Addr::new(o[12], o[13], o[14], o[15]));
    }
    // 6to4 2002::/16 embeds a v4 address in bits 16..48.
    if seg[0] == 0x2002 {
        let o = ip.octets();
        return is_public_v4(Ipv4Addr::new(o[2], o[3], o[4], o[5]));
    }
    !(ip.is_unspecified()
        || ip.is_loopback()
        // IPv4-compatible ::/96 (deprecated) and the discard prefix 100::/64.
        || seg[..6].iter().all(|s| *s == 0)
        || (seg[0] == 0x100 && seg[1..4].iter().all(|s| *s == 0))
        // Multicast ff00::/8.
        || seg[0] & 0xff00 == 0xff00
        // Unique local fc00::/7.
        || seg[0] & 0xfe00 == 0xfc00
        // Link-local fe80::/10 and the old site-local fec0::/10.
        || seg[0] & 0xffc0 == 0xfe80
        || seg[0] & 0xffc0 == 0xfec0
        // Teredo 2001::/32, ORCHID 2001:10::/28 and 2001:20::/28, documentation 2001:db8::/32.
        || (seg[0] == 0x2001 && (seg[1] == 0 || seg[1] & 0xffe0 == 0x0010 || seg[1] == 0x0db8 || seg[1] & 0xfff0 == 0x0020)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[test]
    fn private_and_special_addresses_are_not_public() {
        for ip in [
            "0.0.0.0", "10.1.2.3", "127.0.0.1", "100.64.0.1", "100.127.255.255", "169.254.169.254",
            "172.16.0.1", "172.31.255.255", "192.0.0.8", "192.0.2.1", "192.168.1.1", "198.18.0.1",
            "198.51.100.7", "203.0.113.9", "224.0.0.1", "255.255.255.255", "240.0.0.1",
            "::", "::1", "::ffff:127.0.0.1", "::ffff:192.168.0.1", "64:ff9b::a00:1", "2002:c0a8:101::1",
            "fc00::1", "fd12:3456::1", "fe80::1", "fec0::1", "ff02::1", "2001:db8::1", "2001::1",
            "100::1", "::127.0.0.1",
        ] {
            assert!(!is_public(IpAddr::from_str(ip).unwrap()), "{ip} must not be public");
        }
    }

    #[test]
    fn ordinary_addresses_are_public() {
        for ip in [
            "1.1.1.1", "8.8.8.8", "93.184.216.34", "172.32.0.1", "100.128.0.1", "2606:4700::1111",
            "2a00:1450:4001::200e", "::ffff:8.8.8.8", "64:ff9b::808:808",
        ] {
            assert!(is_public(IpAddr::from_str(ip).unwrap()), "{ip} must be public");
        }
    }

    fn soon() -> Instant {
        Instant::now() + Duration::from_secs(5)
    }

    #[tokio::test]
    async fn production_refuses_http_odd_ports_and_private_hosts() {
        for (url, why) in [
            ("http://example.com/", Refused::Scheme),
            ("ftp://example.com/", Refused::Scheme),
            ("https://example.com:8443/", Refused::Port),
            ("https://127.0.0.1/", Refused::PrivateAddress),
            ("https://[::1]/", Refused::PrivateAddress),
            ("https://192.168.0.1/", Refused::PrivateAddress),
            ("https://169.254.169.254/latest/meta-data", Refused::PrivateAddress),
            ("https://localhost/", Refused::PrivateAddress),
        ] {
            let got = fetch(&url::Url::parse(url).unwrap(), Want::Html, Reach::PUBLIC, soon()).await;
            assert_eq!(got.unwrap_err(), why, "{url}");
        }
    }
}
