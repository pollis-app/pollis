//! The `pollis-relay` server binary (deployable first-party pool node, §7).
//!
//! Config precedence: **CLI flag > env var > config file > built-in default.**
//!
//! - `--config <path>`   / `POLLIS_RELAY_CONFIG`    — TOML file (see
//!   [`pollis_relay::config`] for the format).
//! - `--bind <addr>`     / `POLLIS_RELAY_BIND`       — UDP bind (default `0.0.0.0:9444`).
//! - `--allow <a,b,...>` / `POLLIS_RELAY_ALLOWLIST`  — `host[:port]` patterns (no
//!   port ⇒ 443 only; `:*` ⇒ any port). A malformed entry aborts startup.
//! - `--identity <path>` / `POLLIS_RELAY_IDENTITY`   — persisted QUIC identity key
//!   (cert at `<path>.crt`).
//! - `--health-bind <addr>` / `POLLIS_RELAY_HEALTH_BIND` — opt-in HTTP `/health`,
//!   `/version`, `/peers`.
//! - `--directory-key <b64>` / `POLLIS_RELAY_DIRECTORY_KEY` — pinned Ed25519 key
//!   for the directory and revocation list. Without it every `Extend` is refused.
//! - `--revocation-url <url>` / `POLLIS_RELAY_REVOCATION_URL`.
//! - `--directory-url <url>` / `POLLIS_RELAY_DIRECTORY_URL` — also required to
//!   be a middle hop (`pollis_relay::nexthop`).
//! - `--transparency-key <hex>` / `POLLIS_RELAY_TRANSPARENCY_KEY` — account-key
//!   log key; unset ⇒ no anchoring.
//! - `--require-anchor` / `POLLIS_RELAY_REQUIRE_ANCHOR` — refuse clients with no
//!   account anchor; startup aborts without the key above.
//!
//! Auth is the offline device-cert chain; the relay holds no Turso credential
//! (§11.1, `docs/relay-operations.md`). SIGTERM/SIGINT drains for up to
//! [`DRAIN_TIMEOUT`], then exits 0.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::Duration;

use pollis_relay::anchor::AnchorPolicy;
use pollis_relay::config::{RelayFileConfig, DEFAULT_BIND, DEFAULT_IDENTITY_PATH};
use pollis_relay::health;
use pollis_relay::nexthop::{self, NextHopDirectory};
use pollis_relay::policy::RevocationStore;
use pollis_relay::revocation_sync;
use pollis_relay::server::{Allowlist, RelayConfig, RelayServer};
use pollis_relay::tls;

const DRAIN_TIMEOUT: Duration = Duration::from_secs(20);

fn arg_or_env(args: &HashMap<String, String>, flag: &str, env: &str) -> Option<String> {
    args.get(flag).cloned().or_else(|| std::env::var(env).ok())
}

/// Parse `--flag value` pairs; positional args are ignored.
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

    let file = match arg_or_env(&args, "config", "POLLIS_RELAY_CONFIG") {
        Some(path) => RelayFileConfig::from_path(&path)?,
        None => RelayFileConfig::default(),
    };

    let bind: SocketAddr = arg_or_env(&args, "bind", "POLLIS_RELAY_BIND")
        .or_else(|| file.bind.clone())
        .unwrap_or_else(|| DEFAULT_BIND.to_string())
        .parse()?;

    let allowlist = match arg_or_env(&args, "allow", "POLLIS_RELAY_ALLOWLIST") {
        Some(s) => Allowlist::try_from_patterns(s.split(',').map(|p| p.trim().to_string()))?,
        None => match &file.allowlist {
            Some(patterns) => Allowlist::try_from_patterns(patterns.iter().cloned())?,
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
    if let Some(max) = file.max_connections_per_ip {
        config.max_connections_per_ip = max;
    }
    if let Some(secs) = file.first_frame_timeout_secs {
        config.first_frame_timeout = Duration::from_secs(secs.max(1));
    }
    if let Some(allow_extend) = file.allow_extend {
        config.allow_extend = allow_extend;
    }
    if let Some(secs) = file.extend_dial_timeout_secs {
        config.extend_dial_timeout = Duration::from_secs(secs.max(1));
    }
    if let Some(allow_private) = file.allow_private_next_hops {
        config.allow_private_next_hops = allow_private;
        if allow_private {
            tracing::warn!(
                "allow_private_next_hops is on — this node will extend circuits to private and \
                 loopback addresses the directory lists. Lab pools only."
            );
        }
    }

    // No key ⇒ the store stays `unconfigured` and admits nothing: the node still
    // serves circuits ending here but never extends. Intended, so only a warning.
    let directory_key = arg_or_env(&args, "directory-key", "POLLIS_RELAY_DIRECTORY_KEY")
        .or_else(|| file.directory_key_b64.clone());
    let revocation_url = arg_or_env(&args, "revocation-url", "POLLIS_RELAY_REVOCATION_URL")
        .unwrap_or_else(|| file.revocation_url());
    let directory_url = arg_or_env(&args, "directory-url", "POLLIS_RELAY_DIRECTORY_URL")
        .unwrap_or_else(|| file.directory_url());
    match &directory_key {
        Some(key) => {
            config.revocations = RevocationStore::enforcing(key.trim());
            config.next_hops = NextHopDirectory::enforcing(key.trim());
        }
        None => {
            tracing::warn!(
                "no directory key configured — this node cannot evaluate relay revocation or the \
                 signed directory, and will refuse to extend circuits"
            );
        }
    }

    let transparency_key = arg_or_env(&args, "transparency-key", "POLLIS_RELAY_TRANSPARENCY_KEY")
        .or_else(|| file.transparency_key_hex.clone());
    let require_anchor = arg_or_env(&args, "require-anchor", "POLLIS_RELAY_REQUIRE_ANCHOR")
        .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
        .or(file.require_account_anchor)
        .unwrap_or(false);
    config.anchor = match &transparency_key {
        Some(key) => AnchorPolicy::configured(key, file.anchor_max_age_secs(), require_anchor)?,
        None if require_anchor => {
            anyhow::bail!(
                "require_account_anchor is set but no transparency log key is configured"
            );
        }
        None => AnchorPolicy::Ignore,
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = shutdown_tx.send(true);
    });
    let relay_shutdown = wait_for_shutdown(shutdown_rx.clone());

    // A health bind failure is logged and does not abort the relay.
    let health_bind = arg_or_env(&args, "health-bind", "POLLIS_RELAY_HEALTH_BIND")
        .or_else(|| file.health_bind.clone());
    let mut health_handle = None;
    if let Some(addr_str) = health_bind {
        let health_addr: SocketAddr = addr_str.parse()?;
        // The same registry the relay splices into, so `/peers` cannot advertise
        // a peer this node would not extend to.
        let parked = config.parked.clone();
        if let Some((handle, bound)) =
            health::spawn(health_addr, wait_for_shutdown(shutdown_rx.clone()), parked).await?
        {
            tracing::info!("pollis-relay health endpoint on {bound}");
            health_handle = Some(handle);
        }
    }

    let revocation_task = revocation_sync::spawn(
        config.revocations.clone(),
        revocation_url,
        wait_for_shutdown(shutdown_rx.clone()),
    );

    let directory_task = nexthop::spawn(
        config.next_hops.clone(),
        directory_url,
        wait_for_shutdown(shutdown_rx.clone()),
    );

    let (handle, addr) = RelayServer::spawn_with_shutdown(config, relay_shutdown, DRAIN_TIMEOUT)?;
    tracing::info!("pollis-relay listening on {addr} (identity: {identity_path})");

    handle.await?;
    if let Some(h) = health_handle {
        let _ = h.await;
    }
    if let Some(h) = revocation_task {
        let _ = h.await;
    }
    if let Some(h) = directory_task {
        let _ = h.await;
    }
    tracing::info!("pollis-relay shut down cleanly");
    Ok(())
}

async fn wait_for_shutdown(mut rx: tokio::sync::watch::Receiver<bool>) {
    let _ = rx.wait_for(|fired| *fired).await;
}

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

/// Deliberately a no-op: tracing-subscriber is omitted to keep the binary light.
fn tracing_subscriber_init() {
}
