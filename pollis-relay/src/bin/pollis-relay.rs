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
//!   HTTP `/health` + `/version` + `/peers` endpoint (unset ⇒ not started).
//!   `/peers` is what the hydra reconciler reads to put this node's parked
//!   peer-hosted relays into the signed directory (#813 Phase P1).
//! - `--directory-key <b64>` / `POLLIS_RELAY_DIRECTORY_KEY` — the pinned Ed25519
//!   key that signs the relay directory and the revocation list. **Required to be
//!   a middle hop:** without it this node cannot evaluate revocation, and
//!   "cannot evaluate" resolves the same way as "revoked", so every `Extend` is
//!   refused (#813 Phase C).
//! - `--revocation-url <url>` / `POLLIS_RELAY_REVOCATION_URL` — where the signed
//!   revocation list is published.
//! - `--transparency-key <hex>` / `POLLIS_RELAY_TRANSPARENCY_KEY` — the pinned
//!   account-key transparency log key. Unset ⇒ this node makes no anchoring
//!   claim (#813 Phase E2).
//! - `--require-anchor` (`true`/`false`) / `POLLIS_RELAY_REQUIRE_ANCHOR` — refuse
//!   clients that present no account anchor. Requires the key above; startup
//!   **aborts** rather than coming up silently unenforcing.
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

use pollis_relay::anchor::AnchorPolicy;
use pollis_relay::config::{RelayFileConfig, DEFAULT_BIND, DEFAULT_IDENTITY_PATH};
use pollis_relay::health;
use pollis_relay::policy::RevocationStore;
use pollis_relay::revocation_sync;
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
    if let Some(allow_extend) = file.allow_extend {
        config.allow_extend = allow_extend;
    }

    // Live relay revocation (#813 Phase C). No key ⇒ the store stays
    // `unconfigured`, which admits NOTHING — this node still serves circuits that
    // terminate here, it just will not hand one to a next hop it cannot vouch
    // for. That is the intended strict state, not a misconfiguration to paper
    // over, so it is a warning and the node comes up.
    let directory_key = arg_or_env(&args, "directory-key", "POLLIS_RELAY_DIRECTORY_KEY")
        .or_else(|| file.directory_key_b64.clone());
    let revocation_url = arg_or_env(&args, "revocation-url", "POLLIS_RELAY_REVOCATION_URL")
        .unwrap_or_else(|| file.revocation_url());
    match &directory_key {
        Some(key) => {
            config.revocations = RevocationStore::enforcing(key.trim());
        }
        None => {
            tracing::warn!(
                "no directory key configured — this node cannot evaluate relay revocation and will refuse to extend circuits"
            );
        }
    }

    // Transparency-log account anchoring (#813 Phase E2).
    let transparency_key = arg_or_env(&args, "transparency-key", "POLLIS_RELAY_TRANSPARENCY_KEY")
        .or_else(|| file.transparency_key_hex.clone());
    let require_anchor = arg_or_env(&args, "require-anchor", "POLLIS_RELAY_REQUIRE_ANCHOR")
        .map(|v| matches!(v.trim(), "1" | "true" | "yes"))
        .or(file.require_account_anchor)
        .unwrap_or(false);
    config.anchor = match &transparency_key {
        Some(key) => AnchorPolicy::configured(key, file.anchor_max_age_secs(), require_anchor)?,
        None if require_anchor => {
            // Refusing to start beats coming up "requiring" something this node
            // has no key to check — that would be enforcement in name only.
            anyhow::bail!(
                "require_account_anchor is set but no transparency log key is configured"
            );
        }
        None => AnchorPolicy::Ignore,
    };

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
        // The health endpoint publishes the parked peers (#813 Phase P1) from the
        // SAME registry the relay splices into, so `/peers` cannot advertise a
        // peer this node would not actually extend to.
        let parked = config.parked.clone();
        if let Some((handle, bound)) =
            health::spawn(health_addr, wait_for_shutdown(shutdown_rx.clone()), parked).await?
        {
            tracing::info!("pollis-relay health endpoint on {bound}");
            health_handle = Some(handle);
        }
    }

    // Something has to keep the (pure) revocation store loaded. A missed refresh
    // is safe: `admit` re-checks expiry at USE time, so the worst case is a node
    // that stops being a middle hop, never one that trusts a stale list.
    let revocation_task = revocation_sync::spawn(
        config.revocations.clone(),
        revocation_url,
        wait_for_shutdown(shutdown_rx.clone()),
    );

    let (handle, addr) = RelayServer::spawn_with_shutdown(config, relay_shutdown, DRAIN_TIMEOUT)?;
    tracing::info!("pollis-relay listening on {addr} (identity: {identity_path})");

    // The spawn task returns once shutdown has fired and draining completes.
    handle.await?;
    if let Some(h) = health_handle {
        let _ = h.await;
    }
    if let Some(h) = revocation_task {
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
