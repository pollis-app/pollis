//! The runtime's configuration: `<data dir>/config.json`, every field optional.
//!
//! ```json
//! {
//!   "max_installations": 8,
//!   "webhook": { "bind": "127.0.0.1:8787", "per_minute": 30, "burst": 10 }
//! }
//! ```
//!
//! The webhook listener is off until `webhook` is present (or `run --webhook`
//! is given). Command-line flags override the file for that run.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::ratelimit::{DEFAULT_BURST, DEFAULT_PER_MINUTE};

/// How many installations one runtime hosts unless configured otherwise.
pub const DEFAULT_MAX_INSTALLATIONS: usize = 8;
/// Where the webhook listener binds when enabled without an address: loopback.
pub const DEFAULT_WEBHOOK_BIND: &str = "127.0.0.1:8787";

pub const CONFIG_FILE: &str = "config.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeConfig {
    /// The runtime refuses to start (and `pair`/`add-host` refuse to add) past this.
    pub max_installations: usize,
    /// `None`: the webhook listener is off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub webhook: Option<WebhookConfig>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self { max_installations: DEFAULT_MAX_INSTALLATIONS, webhook: None }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WebhookConfig {
    pub bind: SocketAddr,
    /// Per-installation rate limit: sustained calls a minute.
    pub per_minute: u32,
    /// Per-installation rate limit: calls allowed at once.
    pub burst: u32,
}

impl Default for WebhookConfig {
    fn default() -> Self {
        Self {
            bind: DEFAULT_WEBHOOK_BIND.parse().expect("valid default address"),
            per_minute: DEFAULT_PER_MINUTE,
            burst: DEFAULT_BURST,
        }
    }
}

impl RuntimeConfig {
    pub fn load(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join(CONFIG_FILE);
        let config: RuntimeConfig = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("{} is not valid config (see pollis-bot --help)", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => RuntimeConfig::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        if config.max_installations == 0 {
            bail!("{}: max_installations must be at least 1", path.display());
        }
        Ok(config)
    }
}

/// The runtime's data directory: `--data-dir`, else `POLLIS_BOT_DATA_DIR`, else
/// the platform's per-user data directory.
pub fn data_dir(flag: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = flag {
        return dir;
    }
    if let Some(dir) = std::env::var_os("POLLIS_BOT_DATA_DIR").filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    platform_data_dir()
}

#[cfg(target_os = "macos")]
fn platform_data_dir() -> PathBuf {
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join("Library/Application Support/com.pollis.bot")
}

#[cfg(target_os = "windows")]
fn platform_data_dir() -> PathBuf {
    let appdata = std::env::var_os("APPDATA").unwrap_or_default();
    PathBuf::from(appdata).join("pollis-bot")
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn platform_data_dir() -> PathBuf {
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return PathBuf::from(xdg).join("pollis-bot");
    }
    let home = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(home).join(".local/share/pollis-bot")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_file_means_defaults_with_the_webhook_off() {
        let dir = tempfile::tempdir().unwrap();
        let c = RuntimeConfig::load(dir.path()).unwrap();
        assert_eq!(c.max_installations, DEFAULT_MAX_INSTALLATIONS);
        assert_eq!(c.webhook, None);
    }

    #[test]
    fn a_partial_webhook_section_fills_in_loopback_and_the_default_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), r#"{"webhook":{}}"#).unwrap();
        let c = RuntimeConfig::load(dir.path()).unwrap();
        let w = c.webhook.unwrap();
        assert!(w.bind.ip().is_loopback());
        assert_eq!((w.per_minute, w.burst), (DEFAULT_PER_MINUTE, DEFAULT_BURST));
    }

    #[test]
    fn zero_installations_and_unknown_fields_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(CONFIG_FILE), r#"{"max_installations":0}"#).unwrap();
        assert!(RuntimeConfig::load(dir.path()).is_err());
        std::fs::write(dir.path().join(CONFIG_FILE), r#"{"max_instalations":3}"#).unwrap();
        assert!(RuntimeConfig::load(dir.path()).is_err());
    }
}
