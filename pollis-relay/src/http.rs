//! The shared reqwest client helper (design §14.2).
//!
//! Every HTTP caller in the app should go through `http_client` instead of
//! `reqwest::Client::new()`: when the overlay is on, it points the client at the
//! loopback SOCKS5 shim via `socks5h://` (proxy-side DNS, so the real hostname
//! reaches the relay and the inner TLS still terminates at the real service);
//! when off, it is a plain client identical to `reqwest::Client::new()`.

use crate::shim::OverlayHandle;

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

/// Build a reqwest client for the current overlay state. `Some` → routed through
/// the shim; `None` → a plain direct client (the overlay is genuinely inert).
pub fn http_client(overlay: Option<&OverlayHandle>) -> reqwest::Client {
    http_client_builder(overlay)
        .build()
        .expect("reqwest client builds with default TLS")
}
