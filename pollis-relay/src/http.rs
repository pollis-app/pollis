//! The shared reqwest client (design §14.2). Every HTTP caller goes through
//! [`http_client`]: with the overlay on it routes via the loopback SOCKS5 shim.
//!
//! Clients are cached, not built per call: a fresh client has an empty pool and
//! pays a full TCP+TLS handshake per request (seconds per DS POST on mobile).

use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};

use crate::shim::OverlayHandle;

/// Set explicitly: reqwest's default is no timeout, so a silent peer would hold
/// a request (and the shared pool) forever.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// An inactivity deadline, not a total-request one: the same client fetches
/// 100 MiB attachments, which a whole-request deadline would have to allow for.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

static DIRECT_CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
// Keyed by socks_addr, which is stable across a live Prefer/Strict flip.
static OVERLAY_CLIENT: Mutex<Option<(SocketAddr, reqwest::Client)>> = Mutex::new(None);

/// A builder wired for the current overlay state, for callers that need custom
/// TLS roots etc.
pub fn http_client_builder(overlay: Option<&OverlayHandle>) -> reqwest::ClientBuilder {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .read_timeout(READ_TIMEOUT);
    if let Some(handle) = overlay {
        // socks5h:// = proxy-side DNS, so allowlisting and SNI see the real name.
        let proxy = reqwest::Proxy::all(format!("socks5h://{}", handle.socks_addr()))
            .expect("valid socks5h proxy URI from a SocketAddr");
        builder = builder.proxy(proxy);
    }
    builder
}

/// The cached client for the current overlay state: `Some` routes through the
/// shim, `None` is a plain direct client.
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

    #[test]
    fn the_deadlines_are_sane() {
        assert!(CONNECT_TIMEOUT >= Duration::from_secs(5));
        assert!(CONNECT_TIMEOUT <= Duration::from_secs(30));
        assert!(READ_TIMEOUT >= Duration::from_secs(10));
        assert!(READ_TIMEOUT <= Duration::from_secs(60));
    }

    /// Without a read timeout this hangs; the outer deadline turns that into a
    /// reported failure.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_peer_that_answers_nothing_is_abandoned_rather_than_waited_on() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind black hole");
        let addr = listener.local_addr().expect("local addr");
        // Hold the sockets so the hang never becomes an immediate EOF.
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

    /// The only files allowed to construct a reqwest client.
    const SEAM_FILES: &[&str] = &[
        "pollis-relay/src/http.rs",
        "pollis-core/src/net/overlay.rs",
        // The DS has no overlay, so it keeps its own `OnceLock` client.
        "pollis-delivery/src/util.rs",
    ];

    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("pollis-relay sits one level under the workspace root")
            .to_path_buf()
    }

    /// Source with every `#[cfg(test)]` module removed, by brace matching.
    /// A copy of `pollis-core/tests/no_client_side_remote_writes.rs`'s: the
    /// crates don't depend on each other.
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
                        // Unbalanced: keep the tail rather than drop source.
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
                // `.claude` holds agent worktrees: other branches' checkouts.
                if matches!(name.as_ref(), "target" | ".git" | "node_modules" | ".claude") {
                    continue;
                }
                rust_sources(&path, out);
            } else if path.extension().is_some_and(|e| e == "rs") {
                out.push(path);
            }
        }
    }

    /// Guards against a caller building its own (cold-pool) client (#875). A
    /// caller that needs bespoke TLS roots adds itself to `SEAM_FILES`.
    ///
    /// Walks the whole workspace, but `relay-image.yml` runs it only on
    /// `pollis-relay/**` / `pollis-device-cert/**` changes, so an offender
    /// elsewhere surfaces late.
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
            // `<crate>/tests/**` never ships.
            if rel.split('/').nth(1) == Some("tests") {
                continue;
            }
            let Ok(src) = std::fs::read_to_string(file) else {
                continue;
            };
            // Tests that drive an HTTP surface legitimately build their own
            // client; the rule is about shipped code.
            let src = without_test_modules(&src);
            for (n, line) in src.lines().enumerate() {
                let t = line.trim_start();
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
