//! The shared reqwest client helper (design §14.2).
//!
//! Every HTTP caller in the app should go through `http_client` instead of
//! `reqwest::Client::new()`: when the overlay is on, it points the client at the
//! loopback SOCKS5 shim via `socks5h://` (proxy-side DNS, so the real hostname
//! reaches the relay and the inner TLS still terminates at the real service);
//! when off, it is a plain client identical to `reqwest::Client::new()`.
//!
//! [`http_client`] caches and clones rather than building fresh each call —
//! `reqwest::Client` is `Arc`-backed internally (cloning is cheap and shares
//! the connection pool), but a *freshly built* client starts with an empty
//! pool, so a per-call `.build()` silently pays a full TCP+TLS handshake on
//! every single request. Benchmarked on the mobile dev build: ~3-4.5s per DS
//! POST with a fresh client each time, on a control plane that chains a dozen
//! of them sequentially through onboarding/group-setup — the dominant cost
//! wasn't the DS's own work, it was reconnecting from scratch every time.

use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};

use crate::shim::OverlayHandle;

/// How long to wait for a connection to be established.
///
/// `reqwest`'s default for both of these is **no timeout at all**, which is why
/// they are set here rather than left out: a host that completes the TCP
/// handshake and then never answers — a black hole, a hung SFU-adjacent proxy, a
/// relay that stopped forwarding — held a DS post or a media fetch open forever,
/// and with the shared connection pool that stall is not confined to the one
/// unlucky request.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a single read may stall before the transfer is abandoned.
///
/// Deliberately an INACTIVITY deadline, not a total-request one
/// (`ClientBuilder::timeout`): the same client fetches 100 MiB attachments, and
/// a whole-request deadline generous enough for those on a slow link is too long
/// to be a deadline at all. This bounds "the peer stopped sending", which is the
/// failure being defended against, while a slow-but-progressing download runs to
/// completion.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

static DIRECT_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
// Overlay mode is off-by-default and rare relative to the direct path, but
// still needs to reuse its connection pool across calls when it IS on. Keyed
// by socks_addr (stable across a live Prefer/Strict mode flip — same shim,
// same port — so a cached client stays valid through that).
static OVERLAY_CLIENT: Mutex<Option<(SocketAddr, reqwest::Client)>> = Mutex::new(None);

/// A reqwest client builder wired for the current overlay state. Prefer this
/// over building the client directly when you need to customize TLS roots etc.;
/// [`http_client`] is the zero-config entry point.
pub fn http_client_builder(overlay: Option<&OverlayHandle>) -> reqwest::ClientBuilder {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT);
    if let Some(handle) = overlay {
        // socks5h:// = proxy-side DNS: the hostname travels to the relay, not a
        // pre-resolved IP, so allowlisting and inner-TLS SNI both see the real
        // name.
        let proxy = reqwest::Proxy::all(format!("socks5h://{}", handle.socks_addr()))
            .expect("valid socks5h proxy URI from a SocketAddr");
        builder = builder.proxy(proxy);
    }
    builder
}

/// A reqwest client for the current overlay state, reused across calls so
/// callers share one connection pool instead of reconnecting from scratch
/// every request. `Some` → routed through the shim; `None` → a plain direct
/// client (the overlay is genuinely inert).
pub fn http_client(overlay: Option<&OverlayHandle>) -> reqwest::Client {
    match overlay {
        None => DIRECT_CLIENT
            .get_or_init(|| {
                http_client_builder(None)
                    .build()
                    .expect("reqwest client builds with default TLS")
            })
            .clone(),
        Some(handle) => {
            let addr = handle.socks_addr();
            let mut cached = OVERLAY_CLIENT.lock().unwrap();
            if let Some((cached_addr, client)) = cached.as_ref() {
                if *cached_addr == addr {
                    return client.clone();
                }
            }
            let client = http_client_builder(Some(handle))
                .build()
                .expect("reqwest client builds with default TLS");
            *cached = Some((addr, client.clone()));
            client
        }
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    use std::time::{Duration, Instant};

    /// The deadlines have to be long enough not to fire on a working peer and
    /// short enough that a dead one is noticed. A future edit that zeroes or
    /// balloons either is caught here rather than in production.
    #[test]
    fn the_deadlines_are_sane() {
        assert!(CONNECT_TIMEOUT >= Duration::from_secs(5));
        assert!(CONNECT_TIMEOUT <= Duration::from_secs(30));
        assert!(READ_TIMEOUT >= Duration::from_secs(10));
        assert!(READ_TIMEOUT <= Duration::from_secs(60));
    }

    /// The defect: `reqwest`'s default is NO timeout, so a peer that completes
    /// the handshake and then never sends a byte held the caller forever — and,
    /// because the client is shared, sat on the connection pool while it did.
    ///
    /// Against the pre-fix builder this test does not fail with a wrong error;
    /// it never returns. The outer deadline is what turns that hang into a
    /// reported failure.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_peer_that_answers_nothing_is_abandoned_rather_than_waited_on() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind black hole");
        let addr = listener.local_addr().expect("local addr");
        // Sockets are parked so nothing closes them and turns the hang into an
        // EOF the client would report immediately.
        tokio::spawn(async move {
            let mut held = Vec::new();
            while let Ok((sock, _)) = listener.accept().await {
                held.push(sock);
            }
        });

        let started = Instant::now();
        let result = tokio::time::timeout(
            READ_TIMEOUT + Duration::from_secs(30),
            http_client(None).get(format!("http://{addr}/")).send(),
        )
        .await
        .expect("the request must give up on a silent peer, not wait forever");
        let elapsed = started.elapsed();

        let err = result.expect_err("a peer that never answers cannot produce a response");
        assert!(err.is_timeout(), "the request must end as a timeout; got {err}");
        assert!(
            elapsed >= READ_TIMEOUT,
            "gave up after {elapsed:?}, before the configured deadline — a slow but \
             working peer would be cut off too"
        );
    }
}

#[cfg(test)]
mod seam_tests {
    use std::path::{Path, PathBuf};

    /// Every crate's sanctioned client factory. A construction inside one of
    /// these files IS the seam; anywhere else is the anti-pattern this module
    /// exists to retire.
    const SEAM_FILES: &[&str] = &[
        // this module — the shared builder + cache
        "pollis-relay/src/http.rs",
        // the thin re-export the desktop/mobile core calls
        "pollis-core/src/net/overlay.rs",
        // the DS's own `OnceLock` client (the server has no overlay to route
        // through, so it cannot share `pollis-relay`'s)
        "pollis-delivery/src/util.rs",
    ];

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("pollis-relay sits one level under the workspace root")
            .to_path_buf()
    }

    /// Source with every `#[cfg(test)]` module removed, by brace matching.
    ///
    /// Mirrors `pollis-core/tests/no_client_side_remote_writes.rs`, which faced
    /// the same question and answered it the same way. Kept as a copy rather
    /// than shared: the two crates do not depend on each other, and a guard that
    /// needs a dependency to run is a guard that gets deleted.
    fn without_test_modules(src: &str) -> String {
        let mut out = String::with_capacity(src.len());
        let mut rest = src;
        while let Some(i) = rest.find("#[cfg(test)]") {
            out.push_str(&rest[..i]);
            let after = &rest[i..];
            match after.find('{') {
                Some(open) => {
                    let mut depth = 0usize;
                    let mut end = None;
                    for (off, ch) in after[open..].char_indices() {
                        match ch {
                            '{' => depth += 1,
                            '}' => {
                                depth -= 1;
                                if depth == 0 {
                                    end = Some(open + off + 1);
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    match end {
                        Some(e) => rest = &after[e..],
                        // Unbalanced braces: keep the tail rather than silently
                        // dropping source the guard is supposed to read.
                        None => {
                            out.push_str(after);
                            return out;
                        }
                    }
                }
                None => {
                    out.push_str(after);
                    return out;
                }
            }
        }
        out.push_str(rest);
        out
    }

    fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                // Build output, VCS metadata and node deps are not our source.
                // `.claude` holds agent git worktrees — full checkouts of this
                // same repo, so walking it reports every offender once per
                // worktree and, worse, reports offenders from OTHER BRANCHES as
                // if they were on this one.
                if matches!(name.as_ref(), "target" | ".git" | "node_modules" | ".claude") {
                    continue;
                }
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// A fresh `reqwest::Client` starts with an EMPTY connection pool, so
    /// building one per call pays a full DNS + TCP + TLS handshake every
    /// request — the cost this module's docs put at 3-4.5s per DS POST on a
    /// mobile dev build. #875 found four such sites still live in the DS and
    /// three in `pollis-core`, all on request paths.
    ///
    /// **Scope note.** This guard walks the WHOLE workspace, but the workflow
    /// that runs it (`relay-image.yml`) is path-filtered to `pollis-relay/**`
    /// and `pollis-device-cert/**`. A rogue client added in `pollis-core` or
    /// `pollis-delivery` therefore does not trip it until something in the relay
    /// crate changes — which is how it came to be red on `main` unnoticed.
    ///
    /// This is a GUARD, not a proof that any particular call got faster: it
    /// cannot tell a warm pool from a cold one. What it makes impossible is the
    /// *shape* coming back — a new outbound caller that quietly builds its own
    /// client. If a caller genuinely needs bespoke TLS roots or a proxy, it adds
    /// itself to `SEAM_FILES` deliberately, which is a review conversation
    /// rather than a silent regression.
    #[test]
    fn no_crate_builds_its_own_reqwest_client_outside_the_seam() {
        let root = workspace_root();
        let mut files = Vec::new();
        rust_sources(&root, &mut files);
        assert!(
            files.len() > 100,
            "walked only {} files from {} — the workspace walk is broken, not clean",
            files.len(),
            root.display()
        );

        let mut offenders: Vec<String> = Vec::new();
        for file in &files {
            let rel = file
                .strip_prefix(&root)
                .unwrap_or(file)
                .to_string_lossy()
                .replace('\\', "/");
            if SEAM_FILES.contains(&rel.as_str()) {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(file) else {
                continue;
            };
            // `#[cfg(test)]` modules are excluded, for the same reason
            // `pollis-core/tests/no_client_side_remote_writes.rs` excludes them:
            // a test that drives an HTTP surface legitimately builds its own
            // client — `media_server.rs`'s `decrypted_bytes_are_never_storable_
            // by_the_webview` fetches from the loopback server it just started,
            // and routing that through the shared seam would test the seam
            // instead of the server. The rule is about what SHIPPED code does.
            let src = without_test_modules(&src);
            for (n, line) in src.lines().enumerate() {
                let t = line.trim_start();
                // Comments and doc comments name the anti-pattern constantly;
                // only real code counts.
                if t.starts_with("//") {
                    continue;
                }
                if line.contains("reqwest::Client::new()")
                    || line.contains("reqwest::Client::builder()")
                {
                    offenders.push(format!("{rel}:{}", n + 1));
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "these build a fresh reqwest::Client instead of going through the shared seam \
             ({SEAM_FILES:?}); a fresh client has an empty connection pool, so each call \
             re-handshakes:\n  {}",
            offenders.join("\n  ")
        );
    }
}
