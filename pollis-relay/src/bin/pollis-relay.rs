//! The `pollis-relay` server binary (deployable first-party pool node, §7).
//!
//! Config precedence: **CLI flag > env var > config file > built-in default.**
//!
//! - `--config <path>`   / `POLLIS_RELAY_CONFIG`    — TOML file (see
//!   [`pollis_relay::config`] for the format).
//! - `--bind <addr>`     / `POLLIS_RELAY_BIND`       — UDP bind (default `0.0.0.0:9444`).
//! - `--allow <a,b,...>` / `POLLIS_RELAY_ALLOWLIST`  — comma-separated host patterns.
//! - `--identity <path>` / `POLLIS_RELAY_IDENTITY`   — persisted QUIC identity key
//!   (generated on first start; cert written to `<path>.crt`).
//! - `--health-bind <addr>` / `POLLIS_RELAY_HEALTH_BIND` — TCP bind for the opt-in
//!   HTTP `/health` + `/version` endpoint (unset ⇒ not started).
//!
//! Authentication is the OFFLINE device-cert chain verified per handshake — the
//! relay holds NO Turso credentials and makes NO metadata-plane query (design
//! §11.1; `docs/relay-operations.md`). There is no devices file anymore: trust
//! flows from the cert the client presents, not an operator-maintained table.
//!
//! On SIGTERM/SIGINT the relay shuts down gracefully: it stops accepting, drains
//! in-flight pipes for up to [`DRAIN_TIMEOUT`], then exits 0.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use pollis_relay::config::{RelayFileConfig, DEFAULT_BIND, DEFAULT_IDENTITY_PATH};
use pollis_relay::health;
use pollis_relay::server::{Allowlist, RelayConfig, RelayServer};
use pollis_relay::tls;

/// How long in-flight pipes may keep draining after a shutdown signal.
const DRAIN_TIMEOUT: Duration = Duration::from_secs(20);

fn arg_or_env(args: &HashMap<String, String>, flag: &str, env: &str) -> Option<String> {
    args.get(flag).cloned().or_else(|| std::env::var(env).ok())
}

/// Parse `--flag value` pairs into a map. Unknown/positional args are ignored.
fn parse_args() -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if let Some(flag) = a.strip_prefix("--") {
            if let Some(val) = it.next() {
                map.insert(flag.to_string(), val);
            }
        }
    }
    map
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber_init();

    let args = parse_args();

    // Config file first (lowest precedence after built-in defaults), then env/CLI
    // override each field.
    let file = match arg_or_env(&args, "config", "POLLIS_RELAY_CONFIG") {
        Some(path) => RelayFileConfig::from_path(&path)?,
        None => RelayFileConfig::default(),
    };

    let bind: SocketAddr = arg_or_env(&args, "bind", "POLLIS_RELAY_BIND")
        .or_else(|| file.bind.clone())
        .unwrap_or_else(|| DEFAULT_BIND.to_string())
        .parse()?;

    let allowlist = match arg_or_env(&args, "allow", "POLLIS_RELAY_ALLOWLIST") {
        Some(s) => Allowlist::from_patterns(s.split(',').map(|p| p.trim().to_string())),
        None => match &file.allowlist {
            Some(patterns) => Allowlist::from_patterns(patterns.iter().cloned()),
            None => {
                tracing::warn!("no destination allowlist configured — relay will dial nothing");
                Allowlist::default()
            }
        },
    };

    let identity_path = arg_or_env(&args, "identity", "POLLIS_RELAY_IDENTITY")
        .or_else(|| file.identity_path.clone())
        .unwrap_or_else(|| DEFAULT_IDENTITY_PATH.to_string());
    let identity = tls::load_or_generate_identity(&identity_path)?;

    let mut config = RelayConfig::with_identity(bind, allowlist, identity);
    config.rate_limits = file.rate_limits();
    if let Some(max) = file.max_concurrent_connections {
        config.max_concurrent_connections = max;
    }

    // One OS shutdown signal fans out to both the QUIC relay and the auxiliary
    // health endpoint via a watch channel, so a single SIGTERM/SIGINT stops both.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = shutdown_tx.send(true);
    });
    let relay_shutdown = wait_for_shutdown(shutdown_rx.clone());

    // The health endpoint is opt-in: only started when a bind is configured. A
    // bind failure is logged inside `health::spawn` and does NOT abort the relay.
    let health_bind = arg_or_env(&args, "health-bind", "POLLIS_RELAY_HEALTH_BIND")
        .or_else(|| file.health_bind.clone());
    let mut health_handle = None;
    if let Some(addr_str) = health_bind {
        let health_addr: SocketAddr = addr_str.parse()?;
        if let Some((handle, bound)) = health::spawn(health_addr, wait_for_shutdown(shutdown_rx.clone())).await? {
            tracing::info!("pollis-relay health endpoint on {bound}");
            health_handle = Some(handle);
        }
    }

    let (handle, addr) = RelayServer::spawn_with_shutdown(config, relay_shutdown, DRAIN_TIMEOUT)?;
    tracing::info!("pollis-relay listening on {addr} (identity: {identity_path})");

    // The spawn task returns once shutdown has fired and draining completes.
    handle.await?;
    if let Some(h) = health_handle {
        let _ = h.await;
    }
    tracing::info!("pollis-relay shut down cleanly");
    Ok(())
}

/// Resolve once the shared shutdown flag flips to `true` (or the sender drops).
async fn wait_for_shutdown(mut rx: tokio::sync::watch::Receiver<bool>) {
    let _ = rx.wait_for(|fired| *fired).await;
}

/// Resolve when the process receives SIGINT (Ctrl-C) or SIGTERM.
async fn shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
        tracing::info!("shutdown signal received — draining");
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        tracing::info!("shutdown signal received — draining");
    }
}

/// Minimal tracing init; no-op if a subscriber is already set.
fn tracing_subscriber_init() {
    // Keep the binary dependency-light: the lib uses `tracing` macros, and a
    // missing subscriber just drops them. Nothing to do without pulling in
    // tracing-subscriber, which we intentionally omit from this crate.
}
