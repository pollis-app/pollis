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
