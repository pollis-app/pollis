//! Deployable configuration for the `pollis-relay` binary (design §14 Slice 2a,
//! §7 first-party pool).
//!
//! # File format (TOML)
//!
//! `--config <path>` / `POLLIS_RELAY_CONFIG`; every field optional; env and CLI override the file.
//!
//! ```toml
//! # UDP socket the QUIC endpoint binds. Default "0.0.0.0:9444".
//! bind = "0.0.0.0:9444"
//!
//! # Destination allowlist (§1.2): `host[:port]`; no port ⇒ 443 only. Empty ⇒ dials nothing.
//! allowlist = ["*.turso.io", "api.pollis.com", "api.pollis.com:8443"]
//!
//! # Persisted QUIC identity; generated if absent (cert at "<path>.crt").
//! identity_path = "/var/lib/pollis-relay/identity.key"
//!
//! # Cap on handshake-completed QUIC connections (in-flight handshakes capped separately).
//! max_concurrent_connections = 4096
//!
//! # Per-source-IP QUIC connection cap; size for a busy guard's or NAT's fan-in.
//! max_connections_per_ip = 32
//!
//! # Seconds a new stream may take to negotiate before it is refused.
//! first_frame_timeout_secs = 10
//!
//! # Honour `Extend` as a middle hop; also needs `directory_key_b64` + `directory_url`.
//! allow_extend = true
//!
//! # Seconds the next leg of a circuit (dial, QUIC handshake, `Layer` ack) may
//! # take before the `Extend` is refused. Default 5.
//! extend_dial_timeout_secs = 5
//!
//! # Let `Extend` name directory-listed private addresses. TEST RIGS AND LAB POOLS ONLY.
//! allow_private_next_hops = false
//!
//! # Opt-in HTTP health/version endpoint (TCP). Omitted ⇒ NOT started.
//! health_bind = "0.0.0.0:9445"
//!
//! # Base64 Ed25519 key signing the directory and revocation list. Required to extend.
//! directory_key_b64 = "…"
//!
//! # Where the signed revocation list is published. Only used when
//! # `directory_key_b64` is set.
//! revocation_url = "https://relays.pollis.com/revocations.json"
//!
//! # Where the signed relay directory is published; without a fresh one, no `Extend`.
//! directory_url = "https://relays.pollis.com/v1/directory.json"
//!
//! # Hex ML-DSA-44 account-key log public key. Omitted ⇒ no anchoring claim (#813 E2).
//! transparency_key_hex = "…"
//!
//! # Refuse clients with no account anchor; needs `transparency_key_hex`. Publish first.
//! require_account_anchor = false
//!
//! # How stale a presented signed tree head may be, in seconds. Default 7 days.
//! anchor_max_age_secs = 604800
//!
//! [rate_limit]
//! new_circuits_per_min_per_ip = 600
//! new_circuits_per_min_per_account = 600
//! max_concurrent_per_ip = 256
//! max_concurrent_per_account = 128
//! ```

use crate::ratelimit::RateLimitConfig;

/// The default UDP bind address.
pub const DEFAULT_BIND: &str = "0.0.0.0:9444";
/// The default QUIC identity key path (cert goes to `<path>.crt`).
pub const DEFAULT_IDENTITY_PATH: &str = "relay-identity.key";
/// The default global concurrent-connection cap.
pub const DEFAULT_MAX_CONCURRENT_CONNECTIONS: u32 = 4096;
/// The default per-source-IP concurrent-connection cap.
pub const DEFAULT_MAX_CONNECTIONS_PER_IP: u32 = 32;
/// How long a stream may take to negotiate before it is refused, by default.
pub const DEFAULT_FIRST_FRAME_TIMEOUT_SECS: u64 = 10;
/// Default revocation list URL (matches the client's `POLLIS_OVERLAY_REVOCATION_URL`).
pub const DEFAULT_REVOCATION_URL: &str = "https://relays.pollis.com/revocations.json";
/// Where the signed relay directory is published by default — the same artifact
/// the client's `POLLIS_OVERLAY_DIRECTORY_URL` points at.
pub const DEFAULT_DIRECTORY_URL: &str = "https://relays.pollis.com/v1/directory.json";
/// How long opening a circuit's next leg may take before the `Extend` is
/// refused, by default.
pub const DEFAULT_EXTEND_DIAL_TIMEOUT_SECS: u64 = 5;

/// The relay bin's on-disk config; env / CLI override it.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayFileConfig {
    pub bind: Option<String>,
    pub allowlist: Option<Vec<String>>,
    pub identity_path: Option<String>,
    pub max_concurrent_connections: Option<u32>,
    /// Per-source-IP cap on open QUIC connections. `None` ⇒
    /// [`DEFAULT_MAX_CONNECTIONS_PER_IP`].
    pub max_connections_per_ip: Option<u32>,
    /// Seconds a stream may take to negotiate before it is refused. `None` ⇒
    /// [`DEFAULT_FIRST_FRAME_TIMEOUT_SECS`].
    pub first_frame_timeout_secs: Option<u64>,
    /// Whether this node acts as a middle hop (honours `Extend`). `None` ⇒ the
    /// [`crate::server::RelayConfig`] default, which is `true`.
    pub allow_extend: Option<bool>,
    /// Seconds the next leg may take to open before the `Extend` is refused.
    /// `None` ⇒ [`DEFAULT_EXTEND_DIAL_TIMEOUT_SECS`].
    pub extend_dial_timeout_secs: Option<u64>,
    /// Permit an `Extend` to a directory-listed loopback/private address. `None`
    /// ⇒ `false`. Test rigs and lab pools only.
    pub allow_private_next_hops: Option<bool>,
    /// TCP bind for the opt-in HTTP health/version endpoint. `None` ⇒ not started.
    pub health_bind: Option<String>,
    /// Base64 of the pinned Ed25519 directory/revocation signing key. `None` ⇒
    /// this node cannot evaluate revocation and therefore cannot extend circuits.
    pub directory_key_b64: Option<String>,
    /// Where the signed revocation list lives. `None` ⇒ [`DEFAULT_REVOCATION_URL`].
    pub revocation_url: Option<String>,
    /// Where the signed relay directory lives. `None` ⇒ [`DEFAULT_DIRECTORY_URL`].
    pub directory_url: Option<String>,
    /// Hex of the account-key transparency log's ML-DSA-44 public key. `None` ⇒
    /// this node makes no anchoring claim (#813 Phase E2).
    pub transparency_key_hex: Option<String>,
    /// Refuse clients that present no account anchor. Meaningless — and rejected
    /// at startup — without `transparency_key_hex`.
    pub require_account_anchor: Option<bool>,
    /// Bound on a presented tree head's age, seconds. `None` ⇒
    /// [`crate::anchor::DEFAULT_ANCHOR_MAX_AGE_SECS`].
    pub anchor_max_age_secs: Option<u64>,
    pub rate_limit: Option<RateLimitFileConfig>,
}

/// The `[rate_limit]` sub-table. Each field defaults to [`RateLimitConfig`]'s
/// default when omitted.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RateLimitFileConfig {
    pub new_circuits_per_min_per_ip: Option<u32>,
    pub new_circuits_per_min_per_account: Option<u32>,
    pub max_concurrent_per_ip: Option<u32>,
    pub max_concurrent_per_account: Option<u32>,
}

impl RelayFileConfig {
    /// Parse a config from a TOML string.
    pub fn from_toml(s: &str) -> anyhow::Result<RelayFileConfig> {
        toml::from_str(s).map_err(|e| anyhow::anyhow!("parse relay config: {e}"))
    }

    /// Load and parse a config file from disk.
    pub fn from_path(path: &str) -> anyhow::Result<RelayFileConfig> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("read relay config {path}: {e}"))?;
        Self::from_toml(&contents)
    }

    /// Where to fetch the signed revocation list from.
    pub fn revocation_url(&self) -> String {
        self.revocation_url
            .clone()
            .unwrap_or_else(|| DEFAULT_REVOCATION_URL.to_string())
    }

    /// Where to fetch the signed relay directory from.
    pub fn directory_url(&self) -> String {
        self.directory_url
            .clone()
            .unwrap_or_else(|| DEFAULT_DIRECTORY_URL.to_string())
    }

    /// Bound on a presented tree head's age.
    pub fn anchor_max_age_secs(&self) -> u64 {
        self.anchor_max_age_secs
            .unwrap_or(crate::anchor::DEFAULT_ANCHOR_MAX_AGE_SECS)
    }

    /// Resolve the effective [`RateLimitConfig`], filling omitted fields from the
    /// defaults.
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

        // Filled fields override; omitted ones fall to the default.
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
        // Omitted `allow_extend` leaves the server default in force, which is
        // "yes, act as a middle hop".
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
