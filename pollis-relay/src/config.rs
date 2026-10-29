//! TOML config for the `pollis-relay` binary (`--config` / `POLLIS_RELAY_CONFIG`).
//! Every field is optional; env overrides file, CLI overrides env.
//!
//! ```toml
//! # UDP socket the QUIC endpoint binds. Default "0.0.0.0:9444".
//! bind = "0.0.0.0:9444"
//!
//! # Destination allowlist, the closed-overlay guarantee (§1.2). `host[:port]`:
//! # host is exact, "*.suffix" or "*"; port is a number, "*", or omitted (443
//! # only). Empty ⇒ dials nothing. A malformed entry aborts startup.
//! allowlist = ["*.turso.io", "api.pollis.com", "api.pollis.com:8443"]
//!
//! # Persisted QUIC identity (cert at "<path>.crt"). Default "relay-identity.key".
//! identity_path = "/var/lib/pollis-relay/identity.key"
//!
//! # Cap on established QUIC connections. Handshakes in flight are bounded
//! # separately by the same number, so half-open ones never evict established.
//! max_concurrent_connections = 4096
//!
//! # Per-source-IP connection cap, after QUIC Retry. Each circuit rides its own
//! # connection, so size for one address's fan-in (a busy guard, a large NAT).
//! max_connections_per_ip = 32
//!
//! # Seconds a new stream may take to finish negotiating before it is refused.
//! first_frame_timeout_secs = 10
//!
//! # Honour `Extend` as a middle hop (§6.2). Also needs `directory_key_b64` and
//! # a reachable `directory_url`. False ⇒ last-hop-only.
//! allow_extend = true
//!
//! # Seconds the next leg (dial, handshake, `Layer` ack) may take.
//! extend_dial_timeout_secs = 5
//!
//! # Allow `Extend` to private/loopback addresses. LAB POOLS ONLY: off is what
//! # stops a mis-signed directory turning this node into an intranet dialer.
//! allow_private_next_hops = false
//!
//! # Opt-in HTTP health endpoint (TCP). Omitted ⇒ not started.
//! health_bind = "0.0.0.0:9445"
//!
//! # Base64 Ed25519 key signing the directory and revocation list (clients pin
//! # the same key). Omitted ⇒ every `Extend` is refused: safe, never permissive.
//! directory_key_b64 = "…"
//!
//! revocation_url = "https://relays.pollis.com/revocations.json"
//!
//! # The signed directory: the only statement of where `Extend` may dial.
//! directory_url = "https://relays.pollis.com/v1/directory.json"
//!
//! # Hex ML-DSA-44 key of the account-key transparency log. Omitted ⇒ no
//! # anchoring claim (#813).
//! transparency_key_hex = "…"
//!
//! # Refuse clients with no account anchor. Enabling it before new accounts
//! # reach the log's daily publish locks them out: publish first, enforce second.
//! require_account_anchor = false
//!
//! # Max age of a presented signed tree head. Default 7 days.
//! anchor_max_age_secs = 604800
//!
//! [rate_limit]
//! new_circuits_per_min_per_ip = 600
//! new_circuits_per_min_per_account = 600
//! max_concurrent_per_ip = 256
//! max_concurrent_per_account = 128
//! ```

use crate::ratelimit::RateLimitConfig;

pub const DEFAULT_BIND: &str = "0.0.0.0:9444";
pub const DEFAULT_IDENTITY_PATH: &str = "relay-identity.key";
pub const DEFAULT_MAX_CONCURRENT_CONNECTIONS: u32 = 4096;
pub const DEFAULT_MAX_CONNECTIONS_PER_IP: u32 = 32;
pub const DEFAULT_FIRST_FRAME_TIMEOUT_SECS: u64 = 10;
/// Matches the client's `POLLIS_OVERLAY_REVOCATION_URL` default.
pub const DEFAULT_REVOCATION_URL: &str = "https://relays.pollis.com/revocations.json";
/// Matches the client's `POLLIS_OVERLAY_DIRECTORY_URL` default.
pub const DEFAULT_DIRECTORY_URL: &str = "https://relays.pollis.com/v1/directory.json";
pub const DEFAULT_EXTEND_DIAL_TIMEOUT_SECS: u64 = 5;

/// The on-disk config; see the module docs for each field.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayFileConfig {
    pub bind: Option<String>,
    pub allowlist: Option<Vec<String>>,
    pub identity_path: Option<String>,
    pub max_concurrent_connections: Option<u32>,
    pub max_connections_per_ip: Option<u32>,
    pub first_frame_timeout_secs: Option<u64>,
    pub allow_extend: Option<bool>,
    pub extend_dial_timeout_secs: Option<u64>,
    pub allow_private_next_hops: Option<bool>,
    pub health_bind: Option<String>,
    pub directory_key_b64: Option<String>,
    pub revocation_url: Option<String>,
    pub directory_url: Option<String>,
    pub transparency_key_hex: Option<String>,
    pub require_account_anchor: Option<bool>,
    pub anchor_max_age_secs: Option<u64>,
    pub rate_limit: Option<RateLimitFileConfig>,
}

/// The `[rate_limit]` sub-table; omitted fields take [`RateLimitConfig`]'s defaults.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimitFileConfig {
    pub new_circuits_per_min_per_ip: Option<u32>,
    pub new_circuits_per_min_per_account: Option<u32>,
    pub max_concurrent_per_ip: Option<u32>,
    pub max_concurrent_per_account: Option<u32>,
}

impl RelayFileConfig {
    pub fn from_toml(s: &str) -> anyhow::Result<RelayFileConfig> {
        toml::from_str(s).map_err(|e| anyhow::anyhow!("parse relay config: {e}"))
    }

    pub fn from_path(path: &str) -> anyhow::Result<RelayFileConfig> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("read relay config {path}: {e}"))?;
        Self::from_toml(&contents)
    }

    pub fn revocation_url(&self) -> String {
        self.revocation_url
            .clone()
            .unwrap_or_else(|| DEFAULT_REVOCATION_URL.to_string())
    }

    pub fn directory_url(&self) -> String {
        self.directory_url
            .clone()
            .unwrap_or_else(|| DEFAULT_DIRECTORY_URL.to_string())
    }

    pub fn anchor_max_age_secs(&self) -> u64 {
        self.anchor_max_age_secs
            .unwrap_or(crate::anchor::DEFAULT_ANCHOR_MAX_AGE_SECS)
    }

    pub fn rate_limits(&self) -> RateLimitConfig {
        let d = RateLimitConfig::default();
        let Some(r) = &self.rate_limit else {
            return d;
        };
        RateLimitConfig {
            new_circuits_per_min_per_ip: r.new_circuits_per_min_per_ip.unwrap_or(d.new_circuits_per_min_per_ip),
            new_circuits_per_min_per_account: r
                .new_circuits_per_min_per_account
                .unwrap_or(d.new_circuits_per_min_per_account),
            max_concurrent_per_ip: r.max_concurrent_per_ip.unwrap_or(d.max_concurrent_per_ip),
            max_concurrent_per_account: r.max_concurrent_per_account.unwrap_or(d.max_concurrent_per_account),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_a_full_config() {
        let toml = r#"
            bind = "127.0.0.1:7000"
            allowlist = ["turso.io", "*.pollis.com"]
            identity_path = "/tmp/id.key"
            max_concurrent_connections = 100
            max_connections_per_ip = 7
            first_frame_timeout_secs = 3
            allow_extend = false
            health_bind = "0.0.0.0:9445"

            [rate_limit]
            new_circuits_per_min_per_ip = 10
            max_concurrent_per_account = 3
        "#;
        let cfg = RelayFileConfig::from_toml(toml).unwrap();
        assert_eq!(cfg.bind.as_deref(), Some("127.0.0.1:7000"));
        assert_eq!(
            cfg.allowlist.as_deref(),
            Some(&["turso.io".to_string(), "*.pollis.com".to_string()][..])
        );
        assert_eq!(cfg.identity_path.as_deref(), Some("/tmp/id.key"));
        assert_eq!(cfg.max_concurrent_connections, Some(100));
        assert_eq!(cfg.max_connections_per_ip, Some(7));
        assert_eq!(cfg.first_frame_timeout_secs, Some(3));
        assert_eq!(cfg.allow_extend, Some(false));
        assert_eq!(cfg.health_bind.as_deref(), Some("0.0.0.0:9445"));

        let rl = cfg.rate_limits();
        assert_eq!(rl.new_circuits_per_min_per_ip, 10);
        assert_eq!(rl.max_concurrent_per_account, 3);
        let d = RateLimitConfig::default();
        assert_eq!(rl.max_concurrent_per_ip, d.max_concurrent_per_ip);
        assert_eq!(
            rl.new_circuits_per_min_per_account,
            d.new_circuits_per_min_per_account
        );
    }

    #[test]
    fn empty_config_is_all_defaults() {
        let cfg = RelayFileConfig::from_toml("").unwrap();
        assert_eq!(cfg, RelayFileConfig::default());
        assert_eq!(cfg.rate_limits(), RateLimitConfig::default());
        assert_eq!(cfg.allow_extend, None);
        assert!(
            crate::server::RelayConfig::new(
                "127.0.0.1:0".parse().unwrap(),
                crate::server::Allowlist::default()
            )
            .unwrap()
            .allow_extend
        );
    }

    #[test]
    fn unknown_field_is_rejected() {
        assert!(RelayFileConfig::from_toml("bogus = 1").is_err());
    }
}
