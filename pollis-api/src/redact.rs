//! Scrubbing for diagnostic text (#1305), shared by both ends: the client's
//! on-device log (`pollis_core::diag`) and the DS's failed-request log
//! (`pollis_delivery::error_log`). One rule, so the two logs can never disagree
//! about what counts as identifying.
//!
//! [`scrub`] removes:
//!   - email local parts — `alice@example.com` → `***@example.com`;
//!   - IP addresses — `[redacted]`, by [`redact_ip_literals`] (the DS's rule
//!     for device names and security-log metadata since migration 000034);
//!   - long secret-shaped runs — [`REDACT_RUN_LEN`]+ characters of base64/hex
//!     that mix letters and digits become `[redacted:N]` (session tokens, keys,
//!     signatures). A ULID (26) stays readable.
//!
//! It is a backstop. The callers' rule is still "do not log content, names,
//! keys or tokens" — this only catches what slips past it.

/// Header carrying a fresh random id on every DS request. It names one request
/// and nothing else — never reused, never derived from the user or device — so
/// a line in a user's diagnostics log can be matched to the DS's failed-request
/// log without either side recording who sent it.
pub const REQUEST_ID_HEADER: &str = "X-Pollis-Request-Id";

/// Longest request id the DS will record; anything longer (or not plain
/// alphanumeric) is dropped rather than stored. The client sends 16 hex chars.
pub const REQUEST_ID_MAX_LEN: usize = 32;

/// Whether `id` is shaped like a request id the DS may store.
pub fn is_valid_request_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= REQUEST_ID_MAX_LEN && id.chars().all(|c| c.is_ascii_alphanumeric())
}

/// Runs of token-shaped characters at least this long are redacted. A ULID
/// (26) and a UUID's hex groups stay readable; a 32-byte value in base64 (44)
/// or hex (64) does not.
pub const REDACT_RUN_LEN: usize = 40;

/// Apply every rule in this module to `text`.
pub fn scrub(text: &str) -> String {
    let text = scrub_emails(text);
    let text = redact_ip_literals(&text);
    scrub_long_runs(&text)
}

fn is_local_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '%' | '+' | '-')
}

fn is_domain_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '-')
}

fn scrub_emails(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] != '@' {
            out.push(chars[i]);
            i += 1;
            continue;
        }
        // The local part is what was just copied to `out`; walk it back.
        let mut start = i;
        while start > 0 && is_local_char(chars[start - 1]) {
            start -= 1;
        }
        let mut end = i + 1;
        while end < chars.len() && is_domain_char(chars[end]) {
            end += 1;
        }
        let domain: String = chars[i + 1..end].iter().collect();
        let domain = domain.trim_end_matches('.');
        let local_len = i - start;
        if local_len > 0 && domain.contains('.') {
            for _ in 0..local_len {
                out.pop();
            }
            out.push_str("***@");
            out.push_str(domain);
            i += 1 + domain.chars().count();
        } else {
            out.push('@');
            i += 1;
        }
    }
    out
}

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
/// Replace every IP-address-looking token in client-supplied free text (a
/// device name, a security-event note) with `[redacted]`.
///
/// No client IP may be readable anywhere the DS stores, admins included, and
/// these fields are text the client chooses. Shipped desktop builds send a
/// device name of `"{hostname} ({os})"`, and a cloud, corporate or ISP hostname
/// routinely embeds the machine's address (`ip-10-0-0-12`,
/// `c-73-162-1-2.hsd1.ca.comcast.net`). Current clients send an OS label
/// instead, but old builds stay in the field, so the DS REDACTS rather than
/// rejects: their writes must keep succeeding.
///
/// Tokens are split on whitespace and `= , ; ( ) < > { } " '`, so `name=…`
/// keeps its key. A token is redacted when it parses as an IPv4/IPv6 address
/// or socket address (brackets and `%zone` allowed), or contains four
/// consecutive `0..=255` decimal parts separated by `.`, `-` or `_`.
pub fn redact_ip_literals(text: &str) -> String {
    const SEPARATORS: &str = "=,;()<>{}\"'";
    let mut out = String::with_capacity(text.len());
    let mut token = String::new();
    let flush = |token: &mut String, out: &mut String| {
        if !token.is_empty() {
            if looks_like_ip(token) {
                out.push_str("[redacted]");
            } else {
                out.push_str(token);
            }
            token.clear();
        }
    };
    for c in text.chars() {
        if c.is_whitespace() || SEPARATORS.contains(c) {
            flush(&mut token, &mut out);
            out.push(c);
        } else {
            token.push(c);
        }
    }
    flush(&mut token, &mut out);
    out
}

fn looks_like_ip(token: &str) -> bool {
    let bare = token.trim_start_matches('[').trim_end_matches(']');
    if bare.parse::<std::net::IpAddr>().is_ok() || token.parse::<std::net::SocketAddr>().is_ok() {
        return true;
    }
    // `fe80::1%en0` — a scoped IPv6 literal.
    if let Some((addr, _zone)) = bare.split_once('%') {
        if addr.parse::<std::net::Ipv6Addr>().is_ok() {
            return true;
        }
    }
    // Four consecutive octets inside a longer token: `ip-10-0-0-12`,
    // `c-73-162-1-2.hsd1…`, `192_168_1_5.lan`.
    let parts: Vec<&str> = bare.split(['.', '-', '_', ':', '[', ']']).collect();
    parts.windows(4).any(|w| {
        w.iter().all(|p| {
            !p.is_empty()
                && p.len() <= 3
                && p.bytes().all(|b| b.is_ascii_digit())
                && p.parse::<u16>().is_ok_and(|n| n <= 255)
        })
    })
}

fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '_' | '-')
}

/// A run of [`REDACT_RUN_LEN`]+ token characters that mixes letters and digits
/// is a secret-shaped value until proven otherwise. A long path made only of
/// letters and slashes is left alone.
fn scrub_long_runs(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        let n = run.chars().count();
        let mixed = run.chars().any(|c| c.is_ascii_digit())
            && run.chars().any(|c| c.is_ascii_alphabetic());
        if n >= REDACT_RUN_LEN && mixed {
            out.push_str(&format!("[redacted:{n}]"));
        } else {
            out.push_str(run);
        }
        run.clear();
    };
    for c in text.chars() {
        if is_token_char(c) {
            run.push(c);
        } else {
            flush(&mut run, &mut out);
            out.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_email_keeps_only_its_domain() {
        assert_eq!(
            scrub("verify for alice.b+x@mail.example.com failed"),
            "verify for ***@mail.example.com failed"
        );
        assert_eq!(scrub("a@b.co, c@d.org."), "***@b.co, ***@d.org.");
    }

    #[test]
    fn an_at_sign_that_is_not_an_email_is_left_alone() {
        assert_eq!(scrub("@here and x@y and @"), "@here and x@y and @");
    }

    #[test]
    fn ip_addresses_are_removed() {
        assert_eq!(scrub("from 203.0.113.7 ok"), "from [redacted] ok");
        assert_eq!(scrub("connect 10.0.0.1:443 refused"), "connect [redacted] refused");
        assert_eq!(scrub("peer [2001:db8::1] gone"), "peer [redacted] gone");
        assert_eq!(scrub("host ip-10-0-0-12 down"), "host [redacted] down");
    }

    #[test]
    fn times_versions_and_ids_survive() {
        let line = "16:21:03 v1.16.2 user_id=01J9Z3K8F1QW2E3R4T5Y6U7I8O status=401 establish-identity";
        assert_eq!(scrub(line), line);
    }

    #[test]
    fn a_secret_shaped_run_is_replaced_by_its_length() {
        let token = "Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5MGFiY2RlZmdoaWprbG1u";
        let out = scrub(&format!("session={token} ok"));
        assert_eq!(out, format!("session=[redacted:{}] ok", token.len()));
    }

    #[test]
    fn request_ids_are_short_and_alphanumeric() {
        assert!(is_valid_request_id("0123456789abcdef"));
        assert!(!is_valid_request_id(""));
        assert!(!is_valid_request_id("a b"));
        assert!(!is_valid_request_id("alice@example.com"));
        assert!(!is_valid_request_id(&"a".repeat(REQUEST_ID_MAX_LEN + 1)));
    }
}
