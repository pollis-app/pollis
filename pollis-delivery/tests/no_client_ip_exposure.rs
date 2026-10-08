//! No plain client IP may be readable anywhere the DS controls — not in a log
//! line, a trace span, an error, a response, or the rate limiter's memory.
//!
//! The limiter hashes the IP under a per-process key before it is stored
//! (`ratelimit.rs`, unit-tested there). This file guards everything AROUND it by
//! reading the source, because the ways an IP leaks are one careless line each:
//! a `tracing::info!("{headers:?}")`, a `TraceLayer` that records request
//! headers, `ConnectInfo<SocketAddr>` threaded into a handler, a `console.log`
//! in the Worker, or Workers Logs switched on in a wrangler config.
//!
//! Each rule below fails with the offending file and line. If a rule fires on
//! something legitimate, the fix is to route it through `ratelimit.rs` (the one
//! place allowed to read the IP, and it hashes in the same expression), not to
//! loosen the rule.

use std::fs;
use std::path::{Path, PathBuf};

/// Headers that carry the client IP. Only `ratelimit.rs` may name them.
const IP_HEADERS: &[&str] = &[
    "cf-connecting-ip",
    "x-forwarded-for",
    "x-real-ip",
    "true-client-ip",
    "forwarded",
];

/// APIs that hand a handler the socket peer address, or log whole requests.
const FORBIDDEN_APIS: &[&str] = &[
    "ConnectInfo",
    "peer_addr",
    "into_make_service_with_connect_info",
    "TraceLayer",
    "tower_http",
    "remote_addr",
];

/// Macros whose arguments end up in a log, a panic message or stdout.
const LOGGING_MACROS: &[&str] = &[
    "trace!", "debug!", "info!", "warn!", "error!", "event!", "span!",
    "info_span!", "debug_span!", "trace_span!", "warn_span!", "error_span!",
    "println!", "eprintln!", "print!", "eprint!", "dbg!", "panic!",
];

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Code lines only: `//` comments may DISCUSS these headers and APIs freely.
fn code_lines(src: &str) -> impl Iterator<Item = (usize, &str)> {
    src.lines()
        .enumerate()
        .map(|(i, l)| (i + 1, l))
        .filter(|(_, l)| !l.trim_start().starts_with("//"))
}

/// The text of every logging-macro invocation, from the macro name to the
/// statement's closing `;` (so multi-line arguments are included).
fn logging_invocations(src: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for mac in LOGGING_MACROS {
        let mut from = 0;
        while let Some(off) = src[from..].find(mac) {
            let start = from + off;
            // Skip `foo_info!` style lookalikes and comment-only lines.
            let preceded_by_ident = src[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
            let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
            let in_comment = src[line_start..start].contains("//");
            let end = src[start..].find(';').map_or(src.len(), |i| start + i);
            if !preceded_by_ident && !in_comment {
                let line = src[..start].matches('\n').count() + 1;
                found.push((line, src[start..end].to_string()));
            }
            from = start + mac.len();
        }
    }
    found
}

#[test]
fn only_the_rate_limiter_reads_ip_headers() {
    let mut files = Vec::new();
    rust_sources(&crate_dir().join("src"), &mut files);
    let mut violations = Vec::new();
    for file in &files {
        if file.file_name().is_some_and(|n| n == "ratelimit.rs") {
            continue;
        }
        let src = fs::read_to_string(file).expect("read source");
        for (n, line) in code_lines(&src) {
            let lower = line.to_ascii_lowercase();
            for h in IP_HEADERS {
                // `forwarded` alone is too common a word; match it only as a
                // quoted header name.
                let needle = if *h == "forwarded" { "\"forwarded\"" } else { h };
                if lower.contains(needle) {
                    violations.push(format!("{}:{n}: names IP header {h}", file.display()));
                }
            }
            if line.contains("client_ip(") {
                violations.push(format!("{}:{n}: calls client_ip outside ratelimit.rs", file.display()));
            }
        }
    }
    assert!(violations.is_empty(), "client IP read outside ratelimit.rs:\n{}", violations.join("\n"));
}

#[test]
fn no_peer_address_or_request_tracing_apis() {
    let mut files = Vec::new();
    rust_sources(&crate_dir().join("src"), &mut files);
    let mut violations = Vec::new();
    for file in &files {
        let src = fs::read_to_string(file).expect("read source");
        for (n, line) in code_lines(&src) {
            for api in FORBIDDEN_APIS {
                if line.contains(api) {
                    violations.push(format!("{}:{n}: uses {api}", file.display()));
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "an API that exposes the client address or logs whole requests:\n{}",
        violations.join("\n")
    );
}

#[test]
fn nothing_logs_request_headers() {
    let mut files = Vec::new();
    rust_sources(&crate_dir().join("src"), &mut files);
    let mut violations = Vec::new();
    for file in &files {
        let src = fs::read_to_string(file).expect("read source");
        for (n, call) in logging_invocations(&src) {
            let lower = call.to_ascii_lowercase();
            let leaks = lower.contains("header")
                || lower.contains("client_ip")
                || lower.contains("socketaddr")
                || IP_HEADERS.iter().any(|h| lower.contains(h));
            if leaks {
                violations.push(format!("{}:{n}: {}", file.display(), call.trim()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "a log/panic/print that may carry request headers or the client IP:\n{}",
        violations.join("\n")
    );
}

/// The Worker is the one place a client IP legitimately arrives, so it must
/// strip it: every IP-bearing header in `CLIENT_IP_HEADERS` (deleted before the
/// request reaches the container) and nothing logged on the way.
#[test]
fn the_worker_strips_every_ip_header_and_logs_nothing() {
    let src = fs::read_to_string(crate_dir().join("worker/index.ts")).expect("read worker/index.ts");
    let list = src
        .split("const CLIENT_IP_HEADERS = [")
        .nth(1)
        .and_then(|rest| rest.split("] as const").next())
        .expect("worker/index.ts must declare `const CLIENT_IP_HEADERS = [ ... ] as const`");
    let required = IP_HEADERS.iter().copied().chain(["cf-connecting-ipv6", "cf-pseudo-ipv4"]);
    for h in required {
        assert!(
            list.contains(&format!("\"{h}\"")),
            "CLIENT_IP_HEADERS in worker/index.ts does not strip {h}"
        );
    }
    assert!(
        src.contains("for (const name of CLIENT_IP_HEADERS) {") && src.contains("headers.delete(name);"),
        "worker/index.ts must delete every CLIENT_IP_HEADERS entry before forwarding"
    );
    assert!(
        src.contains("headers.delete(CLIENT_BUCKET_HEADER);"),
        "worker/index.ts must drop an inbound bucket header so a client cannot pick its own bucket"
    );
    let mut violations = Vec::new();
    for (n, line) in code_lines(&src) {
        if line.contains("console.") {
            violations.push(format!("worker/index.ts:{n}: console output"));
        }
    }
    assert!(violations.is_empty(), "the Worker logs:\n{}", violations.join("\n"));
}

/// Workers Logs (`observability`) records each invocation's request — headers
/// and the `cf` object, i.e. the client IP — readable by any account member in
/// the dashboard; Logpush exports the same to a sink. Both must be off,
/// explicitly, in every environment, so a dashboard toggle or a changed
/// wrangler default cannot turn them on behind the config's back.
#[test]
fn workers_logs_and_logpush_are_off_in_every_environment() {
    for env in ["dev", "prod"] {
        let name = format!("wrangler.{env}.jsonc");
        let raw = fs::read_to_string(crate_dir().join(&name)).expect("read wrangler config");
        let json: String = raw
            .lines()
            .filter(|l| !l.trim_start().starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        let cfg: serde_json::Value = serde_json::from_str(&json).expect("parse wrangler config");
        assert_eq!(
            cfg.pointer("/observability/enabled"),
            Some(&serde_json::Value::Bool(false)),
            "{name}: `observability.enabled` must be explicitly false (Workers Logs records request headers, including the client IP)"
        );
        assert_eq!(
            cfg.get("logpush"),
            Some(&serde_json::Value::Bool(false)),
            "{name}: `logpush` must be explicitly false"
        );
        assert!(cfg.get("tail_consumers").is_none(), "{name}: a tail consumer receives every request's headers");
    }
}
