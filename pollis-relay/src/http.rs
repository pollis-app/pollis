//! The shared reqwest client helper (design §14.2).
//!
//! Use `http_client`, not a bare client: with the overlay on it routes via the `socks5h://` shim.
//!
//! [`http_client`] caches its client: a fresh one has an empty pool and cost ~3-4.5s per mobile DS POST.

use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};

use crate::shim::OverlayHandle;

/// How long to wait for a connection to be established.
///
/// `reqwest` defaults to **no timeout**, and a stall on the shared pool spreads.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// How long a single read may stall before the transfer is abandoned.
///
/// An inactivity deadline, not a total one: the same client fetches 100 MiB attachments.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

static DIRECT_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
// Keyed by socks_addr, which survives a Prefer/Strict flip.
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

/// A pooled reqwest client: `Some` → via the shim; `None` → direct.
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

    /// The deadlines must be neither zero nor ballooned.
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

    /// Every crate's sanctioned client factory.
    const SEAM_FILES: &[&str] = &[
        // this module — the shared builder + cache
        "pollis-relay/src/http.rs",
        // the thin re-export the desktop/mobile core calls
        "pollis-core/src/net/overlay.rs",
        // the DS's own `OnceLock` client (the server has no overlay to route
        // through, so it cannot share `pollis-relay`'s)
        "pollis-delivery/src/util.rs",
        // the client-policy fetch (#1612): one request per launch through a
        // per-call proxy plan, so a pooled client would save no handshake
        "pollis-core/src/client_policy.rs",
    ];

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("pollis-relay sits one level under the workspace root")
            .to_path_buf()
    }

    /// Source with every `#[cfg(test)]` module removed, by brace matching.
    ///
    /// A copy of `pollis-core/tests/no_client_side_remote_writes.rs`'s; the crates share no dependency.
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
                // Not our source; `.claude` holds agent worktrees of other branches.
                if matches!(name.as_ref(), "target" | ".git" | "node_modules" | ".claude") {
                    continue;
                }
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// A per-call client pays a full handshake per request (#875).
    ///
    /// **Scope note.** `relay-image.yml` is path-filtered, so offenders elsewhere trip it late.
    ///
    /// A guard on the shape, not a speed proof; bespoke clients join `SEAM_FILES` deliberately.
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
            // Integration-test crates never ship.
            if rel.split('/').nth(1) == Some("tests") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(file) else {
                continue;
            };
            // `#[cfg(test)]` modules excluded: tests legitimately build their own clients.
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
