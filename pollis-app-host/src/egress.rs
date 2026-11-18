//! `http-fetch` egress checks: HTTPS only, default port, exact host match
//! against the granted list, no IP literals, no credentials in the URL, no
//! header that could redirect the request to another virtual host.

use std::collections::BTreeSet;

use url::{Host, Url};

const ALLOWED_METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

/// Headers an App may not set: they would let a request on a granted
/// connection address another host, or tamper with the connection itself.
const FORBIDDEN_HEADERS: [&str; 9] = [
    "host",
    "connection",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
    "keep-alive",
];

/// A URL that passed the checks, and the host it names.
#[derive(Debug)]
pub struct CheckedUrl {
    pub url: Url,
    pub host: String,
}

/// Check a URL against the egress grant. The error names the manifest field.
pub fn check_url(raw: &str, egress: &BTreeSet<String>) -> Result<CheckedUrl, String> {
    let url = Url::parse(raw)
        .map_err(|_| format!("`http-fetch` to {raw:?} refused: not a valid absolute URL"))?;
    let host = match url.host() {
        Some(Host::Domain(d)) => d.to_string(),
        Some(Host::Ipv4(ip)) => {
            return Err(format!("`http-fetch` to `{ip}` refused: IP literals are not allowed; list a host name in `egress`"));
        }
        Some(Host::Ipv6(ip)) => {
            return Err(format!("`http-fetch` to `[{ip}]` refused: IP literals are not allowed; list a host name in `egress`"));
        }
        None => return Err(format!("`http-fetch` to {raw:?} refused: no host")),
    };
    if url.scheme() != "https" {
        return Err(format!(
            "`http-fetch` to `{host}` refused: only https is allowed"
        ));
    }
    if url.port().is_some() {
        return Err(format!(
            "`http-fetch` to `{host}` refused: only the default https port is allowed"
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!(
            "`http-fetch` to `{host}` refused: credentials in the URL are not allowed"
        ));
    }
    // A trailing dot names the same DNS host; compare without it, but never
    // let it smuggle a different spelling past the list.
    let bare = host.strip_suffix('.').unwrap_or(&host);
    if !egress.contains(bare) {
        return Err(format!("`http-fetch` to `{bare}` refused: not in `egress`"));
    }
    Ok(CheckedUrl {
        url,
        host: bare.to_string(),
    })
}

pub fn check_method(method: &str) -> Result<String, String> {
    let upper = method.to_ascii_uppercase();
    if ALLOWED_METHODS.contains(&upper.as_str()) {
        Ok(upper)
    } else {
        Err(format!(
            "`http-fetch` refused: method {method:?} is not allowed"
        ))
    }
}

pub fn check_headers(headers: &[(String, String)]) -> Result<(), String> {
    for (name, value) in headers {
        let lower = name.to_ascii_lowercase();
        if FORBIDDEN_HEADERS.contains(&lower.as_str()) {
            return Err(format!(
                "`http-fetch` refused: header `{name}` may not be set by an App"
            ));
        }
        let bad = |s: &str| s.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0);
        if name.is_empty() || bad(name) || bad(value) {
            return Err(format!(
                "`http-fetch` refused: header {name:?} is malformed"
            ));
        }
    }
    Ok(())
}

/// Where a redirect response points, resolved against the request URL.
pub fn redirect_target(
    base: &Url,
    status: u16,
    headers: &[(String, String)],
) -> Option<Result<String, String>> {
    if !matches!(status, 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    let location = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("location"))
        .map(|(_, v)| v.as_str())?;
    Some(
        base.join(location)
            .map(|u| u.to_string())
            .map_err(|_| "`http-fetch` refused: redirect with an invalid Location".to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn egress() -> BTreeSet<String> {
        BTreeSet::from(["api.x.com".to_string()])
    }

    #[test]
    fn exact_https_host_passes() {
        let ok = check_url("https://API.x.com/v1?q=1", &egress()).unwrap();
        assert_eq!(ok.host, "api.x.com");
        assert!(check_url("https://api.x.com:443/", &egress()).is_ok());
    }

    #[test]
    fn refusals_name_the_rule() {
        let cases = [
            (
                "https://evil.x.com/",
                "`http-fetch` to `evil.x.com` refused: not in `egress`",
            ),
            (
                "https://x.com/",
                "`http-fetch` to `x.com` refused: not in `egress`",
            ),
            (
                "https://api.x.com.evil.com/",
                "`http-fetch` to `api.x.com.evil.com` refused: not in `egress`",
            ),
            (
                "http://api.x.com/",
                "`http-fetch` to `api.x.com` refused: only https is allowed",
            ),
            (
                "https://api.x.com:8443/",
                "`http-fetch` to `api.x.com` refused: only the default https port is allowed",
            ),
            (
                "https://u:p@api.x.com/",
                "`http-fetch` to `api.x.com` refused: credentials in the URL are not allowed",
            ),
        ];
        for (url, want) in cases {
            assert_eq!(check_url(url, &egress()).unwrap_err(), want, "{url}");
        }
    }

    #[test]
    fn ip_literals_in_every_spelling_are_refused() {
        for url in [
            "https://127.0.0.1/",
            "https://2130706433/",
            "https://0x7f.1/",
            "https://[::1]/",
            "https://[::ffff:7f00:1]/",
        ] {
            let err = check_url(url, &egress()).unwrap_err();
            assert!(err.contains("IP literals are not allowed"), "{url}: {err}");
        }
    }

    #[test]
    fn host_header_cannot_retarget_the_request() {
        assert!(check_headers(&[("Host".into(), "evil.com".into())]).is_err());
        assert!(check_headers(&[("x-a".into(), "b\r\nHost: evil".into())]).is_err());
        assert!(check_headers(&[("authorization".into(), "Bearer t".into())]).is_ok());
    }
}
