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
    let mut builder = reqwest::Client::builder();
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
                if matches!(name.as_ref(), "target" | ".git" | "node_modules") {
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
