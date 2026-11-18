//! The reference `http-fetch` client a runtime should use to implement
//! [`HostServices::http_fetch`](crate::HostServices::http_fetch).
//!
//! The host has already checked the URL against the egress grant (HTTPS,
//! default port, exact host, no IP literal). What only a client can check is
//! where that host name *resolves*: a granted name must never be a way into
//! the runtime's own network. [`PublicHttpClient`]:
//!
//! - resolves the host once and keeps only public addresses, refusing the
//!   request when none remain (loopback, private, link-local including cloud
//!   metadata, CGNAT, unique-local, multicast, reserved, documentation, and
//!   IPv4 embedded in IPv6 in any of those ranges);
//! - connects only to the addresses it checked, so a second DNS answer (DNS
//!   rebinding) is never consulted;
//! - follows no redirects (the host follows same-host ones itself, through
//!   this client again), uses no proxy from the environment, and stops
//!   reading the body one byte past the cap.

use std::io::{self, Read};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use crate::types::{HttpResponse, OutboundRequest, ServiceError};

/// Whether `ip` is an address an App's request may reach.
pub fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    let blocked = ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_documentation()
        // 0.0.0.0/8 "this network"
        || o[0] == 0
        // 100.64.0.0/10 shared address space (CGNAT)
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        // 192.0.0.0/24 IETF protocol assignments
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        // 198.18.0.0/15 benchmarking
        || (o[0] == 198 && (o[1] & 0xfe) == 18)
        // 240.0.0.0/4 reserved
        || o[0] >= 240;
    !blocked
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let s = ip.segments();
    // 64:ff9b::/96 NAT64 and 2002::/16 6to4 carry an IPv4 address.
    if s[0] == 0x64 && s[1] == 0xff9b && s[2..6] == [0, 0, 0, 0] {
        return is_public_v4(Ipv4Addr::new(
            (s[6] >> 8) as u8,
            s[6] as u8,
            (s[7] >> 8) as u8,
            s[7] as u8,
        ));
    }
    if s[0] == 0x2002 {
        return is_public_v4(Ipv4Addr::new(
            (s[1] >> 8) as u8,
            s[1] as u8,
            (s[2] >> 8) as u8,
            s[2] as u8,
        ));
    }
    let blocked = ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        // ::/96 IPv4-compatible (deprecated)
        || s[..6] == [0, 0, 0, 0, 0, 0]
        // fc00::/7 unique local (includes fd00:ec2::254, the AWS metadata endpoint)
        || (s[0] & 0xfe00) == 0xfc00
        // fe80::/10 link-local
        || (s[0] & 0xffc0) == 0xfe80
        // fec0::/10 site-local (deprecated)
        || (s[0] & 0xffc0) == 0xfec0
        // 2001:db8::/32 documentation
        || (s[0] == 0x2001 && s[1] == 0x0db8)
        // 2001::/32 Teredo
        || (s[0] == 0x2001 && s[1] == 0)
        // 100::/64 discard-only
        || (s[0] == 0x0100 && s[1..4] == [0, 0, 0]);
    !blocked
}

type Lookup = dyn Fn(&str) -> io::Result<Vec<SocketAddr>> + Send + Sync;

/// Resolves a `host:port` and keeps only public addresses.
#[derive(Clone)]
pub struct PublicResolver {
    lookup: Arc<Lookup>,
}

impl PublicResolver {
    /// The system resolver.
    pub fn system() -> PublicResolver {
        PublicResolver::with_lookup(|netloc| netloc.to_socket_addrs().map(|a| a.collect()))
    }

    /// A custom lookup (tests, or a runtime with its own DNS client).
    pub fn with_lookup(
        lookup: impl Fn(&str) -> io::Result<Vec<SocketAddr>> + Send + Sync + 'static,
    ) -> PublicResolver {
        PublicResolver {
            lookup: Arc::new(lookup),
        }
    }

    /// The public addresses `netloc` resolves to, or an error when it
    /// resolves to none. Looks up exactly once.
    pub fn resolve_public(&self, netloc: &str) -> io::Result<Vec<SocketAddr>> {
        let all = (self.lookup)(netloc)?;
        let public: Vec<SocketAddr> = all.into_iter().filter(|a| is_public_ip(a.ip())).collect();
        if public.is_empty() {
            let host = netloc.rsplit_once(':').map_or(netloc, |(h, _)| h);
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("`{host}` resolves only to non-public addresses (loopback, private, link-local or metadata)"),
            ));
        }
        Ok(public)
    }
}

impl ureq::Resolver for PublicResolver {
    fn resolve(&self, netloc: &str) -> io::Result<Vec<SocketAddr>> {
        self.resolve_public(netloc)
    }
}

/// The reference client. Cheap to clone; share one per runtime.
#[derive(Clone)]
pub struct PublicHttpClient {
    agent: ureq::Agent,
}

impl PublicHttpClient {
    pub fn new() -> PublicHttpClient {
        PublicHttpClient::with_resolver(PublicResolver::system())
    }

    pub fn with_resolver(resolver: PublicResolver) -> PublicHttpClient {
        let agent = ureq::AgentBuilder::new()
            .resolver(resolver)
            .redirects(0)
            .try_proxy_from_env(false)
            .user_agent("pollis-app")
            .build();
        PublicHttpClient { agent }
    }

    /// One request, no redirects, at most `max_body_bytes + 1` bytes read.
    pub fn fetch(
        &self,
        request: &OutboundRequest,
        max_body_bytes: usize,
        timeout: Duration,
    ) -> Result<HttpResponse, ServiceError> {
        let mut req = self
            .agent
            .request(&request.method, &request.url)
            .timeout(timeout);
        for (name, value) in &request.headers {
            req = req.set(name, value);
        }
        let result = match &request.body {
            Some(body) => req.send_bytes(body),
            None => req.call(),
        };
        let response = match result {
            Ok(r) => r,
            // 4xx/5xx are answers, not failures: hand them to the App.
            Err(ureq::Error::Status(_, r)) => r,
            Err(ureq::Error::Transport(t)) => return Err(ServiceError(t.to_string())),
        };
        let status = response.status();
        let mut headers = Vec::new();
        for name in response.headers_names() {
            for value in response.all(&name) {
                headers.push((name.clone(), value.to_string()));
            }
        }
        let mut body = Vec::new();
        response
            .into_reader()
            .take(max_body_bytes as u64 + 1)
            .read_to_end(&mut body)
            .map_err(|e| ServiceError(e.to_string()))?;
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

impl Default for PublicHttpClient {
    fn default() -> Self {
        PublicHttpClient::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn non_public_addresses_are_refused() {
        for ip in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "0.1.2.3",
            "255.255.255.255",
            "224.0.0.1",
            "192.0.0.170",
            "198.18.0.1",
            "240.0.0.1",
            "192.0.2.1",
            "::1",
            "::",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::127.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "2002:c0a8:0101::1",
            "fd00:ec2::254",
            "fc00::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "2001:db8::1",
            "2001::1",
            "100::1",
        ] {
            assert!(!is_public_ip(ip.parse().unwrap()), "{ip} counted as public");
        }
    }

    #[test]
    fn public_addresses_pass() {
        for ip in [
            "8.8.8.8",
            "93.184.216.34",
            "2606:4700:4700::1111",
            "::ffff:8.8.8.8",
            "64:ff9b::808:808",
        ] {
            assert!(is_public_ip(ip.parse().unwrap()), "{ip} refused");
        }
    }

    #[test]
    fn a_mixed_answer_keeps_only_public_addresses_and_looks_up_once() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c = calls.clone();
        let resolver = PublicResolver::with_lookup(move |_| {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(vec![
                "127.0.0.1:443".parse().unwrap(),
                "8.8.8.8:443".parse().unwrap(),
            ])
        });
        let got = resolver.resolve_public("api.x.com:443").unwrap();
        assert_eq!(got, vec!["8.8.8.8:443".parse::<SocketAddr>().unwrap()]);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_granted_name_that_resolves_inside_is_refused_without_connecting() {
        // A listener on loopback stands in for an internal service. The
        // granted host name "rebinds" to it.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let target = listener.local_addr().unwrap();
        let client =
            PublicHttpClient::with_resolver(PublicResolver::with_lookup(move |_| Ok(vec![target])));
        let request = OutboundRequest {
            method: "GET".into(),
            url: "https://api.x.com/".into(),
            headers: vec![],
            body: None,
        };
        let err = client
            .fetch(&request, 1024, Duration::from_secs(2))
            .unwrap_err();
        assert!(
            err.0.contains("resolves only to non-public addresses"),
            "{}",
            err.0
        );
        assert_eq!(
            listener.accept().map(|_| ()).unwrap_err().kind(),
            io::ErrorKind::WouldBlock,
            "the client connected to a non-public address"
        );
    }
}
