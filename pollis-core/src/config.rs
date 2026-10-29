use crate::error::{Error, Result};

pub use pollis_relay::OverlayMode;

#[derive(Debug, Clone)]
/// The client's compile-time configuration.
///
/// **No database credential appears here (#987)**; the client speaks only to the Delivery Service.
///
/// A new compile-time input must also go in `desktop-release.yml` and `rebuild-verify.yml` (`scripts/check-build-recipe.py`); never a credential.
pub struct Config {
    /// R2 S3 endpoint. Non-secret, only for the display `url`; credentials are server-side (#393).
    pub r2_endpoint: String,
    pub r2_public_url: String,
    /// LiveKit ws URL. Non-secret; the API key/secret are server-side (#393).
    pub livekit_url: String,
    /// Delivery Service base URL (e.g. `https://api.pollis.com`).
    pub pollis_delivery_url: Option<String>,
    /// Closed-overlay relay mode from `POLLIS_OVERLAY` (default off). Media stays direct in every mode.
    pub overlay_mode: pollis_relay::OverlayMode,
    /// v0 relay endpoint(s), comma-separated for a pool. Absent: `Prefer` goes direct, `Strict` surfaces an error.
    pub overlay_relay_url: Option<String>,
    /// Pinned relay QUIC cert (`POLLIS_OVERLAY_RELAY_CERT`): DER path or base64. Absent fails closed.
    pub overlay_relay_cert: Option<String>,
    /// Signed relay directory URL (#616); when set with the key, supersedes `POLLIS_OVERLAY_RELAY`.
    pub overlay_directory_url: Option<String>,
    /// Pinned Ed25519 directory key (base64). Required alongside the URL, else not configured.
    pub overlay_directory_key: Option<String>,
    /// Dev DS gate key (`POLLIS_DEV_ACCESS_KEY`, #1242), sent as `X-Pollis-Dev-Key`.
    ///
    /// **Runtime env only, never `option_env!`.** Attach only via [`Config::dev_access_key_for_ds`].
    pub dev_access_key: Option<String>,
}

/// The production DS host. The dev gate key is never sent here.
pub const PROD_DS_HOST: &str = "api.pollis.com";

impl Config {
    /// True only when BOTH the directory URL and key are set; a half-config fails safe to the static list.
    pub fn overlay_directory_configured(&self) -> bool {
        self.overlay_directory_url.is_some() && self.overlay_directory_key.is_some()
    }

    /// Configured relay endpoints, in order. Empty when unconfigured.
    pub fn overlay_relay_endpoints(&self) -> Vec<String> {
        self.overlay_relay_url
            .as_deref()
            .map(|s| {
                s.split(',')
                    .map(|e| e.trim().to_string())
                    .filter(|e| !e.is_empty())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The dev gate key for the configured DS, or `None` for prod, unparseable or unset. Only `ds_client.rs` calls this.
    pub fn dev_access_key_for_ds(&self) -> Option<&str> {
        let key = self.dev_access_key.as_deref().map(str::trim).filter(|k| !k.is_empty())?;
        let ds = self.pollis_delivery_url.as_deref()?;
        let url = reqwest::Url::parse(ds.trim()).ok()?;
        let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
        if host == PROD_DS_HOST {
            return None;
        }
        Some(key)
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            // option_env! at compile time, else std::env::var for dev builds.
            r2_endpoint:          require_env("R2_S3_ENDPOINT",   option_env!("R2_S3_ENDPOINT"))?,
            r2_public_url:        require_env("R2_PUBLIC_URL",    option_env!("R2_PUBLIC_URL"))?,
            livekit_url: option_env!("LIVEKIT_URL")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("LIVEKIT_URL").ok())
                .unwrap_or_default(),
            pollis_delivery_url: option_env!("POLLIS_DELIVERY_URL")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("POLLIS_DELIVERY_URL").ok())
                .filter(|s| !s.is_empty()),
            overlay_mode: option_env!("POLLIS_OVERLAY")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("POLLIS_OVERLAY").ok())
                .map(|s| parse_overlay_mode(&s))
                .unwrap_or(pollis_relay::OverlayMode::Off),
            overlay_relay_url: option_env!("POLLIS_OVERLAY_RELAY")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("POLLIS_OVERLAY_RELAY").ok())
                .filter(|s| !s.is_empty()),
            overlay_relay_cert: option_env!("POLLIS_OVERLAY_RELAY_CERT")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("POLLIS_OVERLAY_RELAY_CERT").ok())
                .filter(|s| !s.is_empty()),
            overlay_directory_url: option_env!("POLLIS_OVERLAY_DIRECTORY_URL")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("POLLIS_OVERLAY_DIRECTORY_URL").ok())
                .filter(|s| !s.is_empty()),
            overlay_directory_key: option_env!("POLLIS_OVERLAY_DIRECTORY_KEY")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("POLLIS_OVERLAY_DIRECTORY_KEY").ok())
                .filter(|s| !s.is_empty()),
            dev_access_key: std::env::var("POLLIS_DEV_ACCESS_KEY")
                .ok()
                .filter(|s| !s.trim().is_empty()),
        })
    }
}

/// `prefer` / `strict` (any case); everything else is `Off`.
pub(crate) fn parse_overlay_mode(s: &str) -> pollis_relay::OverlayMode {
    match s.trim().to_ascii_lowercase().as_str() {
        "prefer" => pollis_relay::OverlayMode::Prefer,
        "strict" => pollis_relay::OverlayMode::Strict,
        _ => pollis_relay::OverlayMode::Off,
    }
}

fn require_env(key: &str, compiled: Option<&'static str>) -> Result<String> {
    compiled
        .map(|s| s.to_string())
        .or_else(|| std::env::var(key).ok())
        .ok_or_else(|| Error::Config(format!("missing env var: {key}")))
}

#[cfg(any(test, feature = "test-harness"))]
impl Config {
    /// Build a Config for the integration-test harness.
    pub fn for_test() -> Result<Self> {
        Ok(Self {
            r2_endpoint: String::new(),
            r2_public_url: String::new(),
            livekit_url: String::new(),
            pollis_delivery_url: None,
            overlay_mode: pollis_relay::OverlayMode::Off,
            overlay_relay_url: None,
            overlay_relay_cert: None,
            overlay_directory_url: None,
            overlay_directory_key: None,
            dev_access_key: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(ds: Option<&str>, key: Option<&str>) -> Config {
        let mut c = Config::for_test().unwrap();
        c.pollis_delivery_url = ds.map(str::to_string);
        c.dev_access_key = key.map(str::to_string);
        c
    }

    #[test]
    fn dev_key_is_sent_to_a_non_prod_ds_only() {
        assert_eq!(with(Some("https://api-dev.pollis.com"), Some("k")).dev_access_key_for_ds(), Some("k"));
        assert_eq!(with(Some("http://127.0.0.1:8788/"), Some("k")).dev_access_key_for_ds(), Some("k"));
        for prod in [
            "https://api.pollis.com",
            "https://api.pollis.com/",
            "https://API.Pollis.com",
            "https://api.pollis.com.:443",
            " https://api.pollis.com ",
        ] {
            assert_eq!(with(Some(prod), Some("k")).dev_access_key_for_ds(), None, "{prod}");
        }
        assert_eq!(with(None, Some("k")).dev_access_key_for_ds(), None);
        assert_eq!(with(Some("not a url"), Some("k")).dev_access_key_for_ds(), None);
        assert_eq!(with(Some("https://api-dev.pollis.com"), None).dev_access_key_for_ds(), None);
        assert_eq!(with(Some("https://api-dev.pollis.com"), Some("  ")).dev_access_key_for_ds(), None);
    }

    /// A runtime env read only; `option_env!` would bake it into a release binary.
    #[test]
    fn dev_key_is_never_compiled_in() {
        let src = include_str!("config.rs");
        let needle = ["option_env!(\"", "POLLIS_DEV_ACCESS_KEY"].concat();
        assert!(!src.contains(&needle), "POLLIS_DEV_ACCESS_KEY must not be read with option_env!");
        assert!(src.contains("std::env::var(\"POLLIS_DEV_ACCESS_KEY\")"));
    }
}

