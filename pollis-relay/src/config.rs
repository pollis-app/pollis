//! Deployable configuration for the `pollis-relay` binary (design §14 Slice 2a,
//! §7 first-party pool).
//!
//! # File format (TOML)
//!
//! Point the binary at a file with `--config <path>` / `POLLIS_RELAY_CONFIG`.
//! Every field is optional; omitted fields take the documented default. CLI flags
//! and env vars still work and **override** the file (env overrides file, CLI
//! overrides env — resolved in the binary).
//!
//! ```toml
//! # UDP socket the QUIC endpoint binds. Default "0.0.0.0:9444".
//! bind = "0.0.0.0:9444"
//!
//! # Static destination allowlist — the closed-overlay guarantee (§1.2). Each
//! # entry is an exact host, a "*.suffix" glob, or "*" (fully open; NOT v0).
//! # Empty/omitted ⇒ the relay dials nothing (never fails open).
//! allowlist = ["turso.io", "*.pollis.com", "api.pollis.com"]
//!
//! # Path to the persisted QUIC identity keypair. Generated on first start if
//! # absent (the DER cert is written alongside as "<path>.crt"). Default
//! # "relay-identity.key" in the working directory.
//! identity_path = "/var/lib/pollis-relay/identity.key"
//!
//! # Global cap on simultaneously-open QUIC connections. Default 4096.
//! max_concurrent_connections = 4096
//!
//! # Act as a MIDDLE HOP of a multi-hop circuit: honour `Extend` by dialing the
//! # next relay (design §6.2). Default true. Set false to make this node
//! # last-hop-only — it will still serve circuits that terminate here.
//! allow_extend = true
//!
//! # Opt-in HTTP/1.1 health/version endpoint (TCP). Orchestrators/load-balancers
//! # probe this (the relay itself is QUIC/UDP-only). Omitted ⇒ NOT started.
//! health_bind = "0.0.0.0:9445"
//!
//! # Base64 (STANDARD) of the 32-byte Ed25519 key that signs the relay directory
//! # AND the revocation list — the same POLLIS_OVERLAY_DIRECTORY_KEY clients pin.
//! # REQUIRED to act as a middle hop: without it this node cannot evaluate
//! # revocation, and "cannot evaluate" resolves the same way as "revoked", so it
//! # will refuse every `Extend`. Omitting it is safe, never permissive.
//! directory_key_b64 = "…"
//!
//! # Where the signed revocation list is published. Only used when
//! # `directory_key_b64` is set.
//! revocation_url = "https://relays.pollis.com/revocations.json"
//!
//! # Hex of the account-key transparency log's ML-DSA-44 public key (what
//! # https://verify.pollis.com/v1/account-keys/public_key.json serves, and what
//! # PINNED_LOG_PUBLIC_KEYS pins in the client). Omitted ⇒ this node makes no
//! # anchoring claim: it neither requires an anchor nor judges one (#813 E2).
//! transparency_key_hex = "…"
//!
//! # Refuse a client that presents no transparency-log account anchor. Requires
//! # `transparency_key_hex`. Default false — turning it on before clients send
//! # the frame, or before a new account reaches the log's daily publish, locks
//! # those users out. Publish first, enforce second.
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
/// Where the signed relay revocation list is published by default. Matches the
/// client-side `POLLIS_OVERLAY_REVOCATION_URL` default (`infra/relay-hydra`).
pub const DEFAULT_REVOCATION_URL: &str = "https://relays.pollis.com/revocations.json";

/// The relay bin's on-disk config, as parsed from TOML. Optional fields let the
/// binary layer env / CLI overrides on top (env overrides file).
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelayFileConfig {
    pub bind: Option<String>,
    pub allowlist: Option<Vec<String>>,
    pub identity_path: Option<String>,
    pub max_concurrent_connections: Option<u32>,
    /// Whether this node acts as a middle hop (honours `Extend`). `None` ⇒ the
    /// [`crate::server::RelayConfig`] default, which is `true`.
    pub allow_extend: Option<bool>,
    /// TCP bind for the opt-in HTTP health/version endpoint. `None` ⇒ not started.
    pub health_bind: Option<String>,
    /// Base64 of the pinned Ed25519 directory/revocation signing key. `None` ⇒
    /// this node cannot evaluate revocation and therefore cannot extend circuits.
    pub directory_key_b64: Option<String>,
    /// Where the signed revocation list lives. `None` ⇒ [`DEFAULT_REVOCATION_URL`].
    pub revocation_url: Option<String>,
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
