//! #1305 — what may reach the on-device diagnostics log (`src/diag.rs`).
//!
//! The log is a file a user may hand to someone, so it must not carry message
//! content, file names, display names, emails, keys or tokens. `diag::redact`
//! scrubs emails, IPs and token-shaped runs as a backstop; this file guards the
//! call sites, by reading the source, against the obvious ways around it:
//!
//!   1. Every log line goes through `diag!`. A stray `eprintln!` bypasses the
//!      file AND the scrubber, and would be invisible in a packaged app anyway.
//!   2. No `diag!` interpolates a value whose name says it is content or a
//!      secret. A rule that fires on something legitimate is fixed by logging a
//!      length, a count or an id instead — not by renaming the variable.

use std::fs;
use std::path::{Path, PathBuf};

/// Names that must never be interpolated into a log line.
const FORBIDDEN_NAMES: &[&str] = &[
    "plaintext",
    "content",
    "body",
    "text",
    "filename",
    "file_name",
    "display_name",
    "preferred_name",
    "username",
    "email",
    "password",
    "secret",
    "secret_key",
    "session_token",
    "token",
    "pin",
    "private_key",
    "dev_name",
];

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).expect("read src dir") {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            // The CLI binaries print to their own terminal; they never open the log.
            if path.file_name().is_some_and(|n| n == "bin") {
                continue;
            }
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn sources() -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    rust_sources(&Path::new(env!("CARGO_MANIFEST_DIR")).join("src"), &mut files);
    files
        .into_iter()
        .map(|p| {
            let src = fs::read_to_string(&p).expect("read source");
            (p, src)
        })
        .collect()
}

/// Each `diag!( … )` invocation's full argument text, with its line number.
fn diag_invocations(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut search = 0;
    while let Some(found) = src[search..].find("diag!(") {
        let start = search + found;
        let line_start = src[..start].rfind('\n').map_or(0, |i| i + 1);
        let in_comment = src[line_start..start].contains("//");
        let mut depth = 0usize;
        let mut end = start;
        for (i, c) in src[start..].char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end = start + i + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        if !in_comment {
            let line = src[..start].matches('\n').count() + 1;
            out.push((line, src[start..end].to_string()));
        }
        search = end.max(start + 1);
    }
    out
}

/// Identifiers interpolated into a format string (`{name}`, `{name:?}`) or
/// passed as an argument after it.
fn interpolated_names(call: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = call;
    while let Some(open) = rest.find('{') {
        rest = &rest[open + 1..];
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.')
            .collect();
        if !name.is_empty() {
            names.push(name);
        }
    }
    // Positional arguments: everything after the closing quote of the format
    // string, split on commas.
    if let Some(close) = call.rfind('"') {
        for arg in call[close + 1..].split(',') {
            let ident: String = arg
                .trim()
                .trim_end_matches(')')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '.' || *c == '&')
                .collect();
            if !ident.is_empty() {
                names.push(ident.trim_start_matches('&').to_string());
            }
        }
    }
    names
}

fn is_forbidden(name: &str) -> bool {
    // `profile.username`, `att.content` — judge the last segment.
    let last = name.rsplit('.').next().unwrap_or(name).to_ascii_lowercase();
    FORBIDDEN_NAMES.contains(&last.as_str())
}

#[test]
fn every_log_line_goes_through_diag() {
    let mut violations = Vec::new();
    for (path, src) in sources() {
        if path.ends_with("diag.rs") {
            continue;
        }
        for (n, line) in src.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains("eprintln!(") || code.contains("eprint!(") {
                violations.push(format!("{}:{}: {}", path.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "use crate::diag! — eprintln! skips the diagnostics log and its scrubber:\n{}",
        violations.join("\n")
    );
}

#[test]
fn no_log_line_interpolates_content_or_secrets() {
    let mut violations = Vec::new();
    let mut scanned = 0;
    for (path, src) in sources() {
        for (n, call) in diag_invocations(&src) {
            scanned += 1;
            for name in interpolated_names(&call) {
                if is_forbidden(&name) {
                    violations.push(format!("{}:{n}: `{name}` in {}", path.display(), call.trim()));
                }
            }
        }
    }
    // A scanner that finds nothing passes everything; there are ~490 call sites.
    assert!(scanned > 300, "only {scanned} diag! invocations found — the scanner is broken");
    assert!(
        violations.is_empty(),
        "a diagnostics log line interpolates content or a secret (log a length, count or id instead):\n{}",
        violations.join("\n")
    );
}

#[test]
fn the_name_check_catches_what_it_should() {
    let names = interpolated_names(r#"diag!("x {filename} {n:?}", profile.username, count)"#);
    assert_eq!(names, vec!["filename", "n", "profile.username", "count"]);
    assert!(is_forbidden("filename"));
    assert!(is_forbidden("profile.username"));
    assert!(!is_forbidden("count"));
    assert!(!is_forbidden("conversation_id"));
}
