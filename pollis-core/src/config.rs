use crate::error::{Error, Result};

// Re-exported so consumers needn't depend on `pollis-relay` directly.
pub use pollis_relay::OverlayMode;

#[derive(Debug, Clone)]
/// The client's compile-time configuration.
///
/// Holds no database credential (#987). Every value here is public by
/// construction: `scripts/check-build-recipe.py` requires a new `option_env!`
/// key to be added to `desktop-release.yml` and `rebuild-verify.yml` too, and
/// bounds the build jobs' `$GITHUB_ENV` to these keys, so a credential must
/// never be added here.
pub struct Config {
    /// R2 S3 endpoint, used only to build the display `url` of uploads; R2
    /// credentials live in the DS broker (#393).
    pub r2_endpoint: String,
    pub r2_public_url: String,
    /// LiveKit ws URL; the API key/secret live in the DS broker (#393).
    pub livekit_url: String,
    /// Delivery Service base URL (e.g. `https://api.pollis.com`).
    pub pollis_delivery_url: Option<String>,
    /// Overlay relay mode from `POLLIS_OVERLAY` (`off` | `prefer` | `strict`,
    /// default and fallback `off`). `Off` leaves every network path identical to
    /// a build without the overlay; `Strict` surfaces a degraded error rather
    /// than going direct. LiveKit media is always direct
    /// (`docs/relay-overlay-design.md` §6.4, §10.1, §14).
    pub overlay_mode: pollis_relay::OverlayMode,
    /// Static relay endpoint pool (`POLLIS_OVERLAY_RELAY`, comma-separated).
    /// Absent → no circuit: direct fallback in `Prefer`, a degraded error in
    /// `Strict`.
    pub overlay_relay_url: Option<String>,
    /// Pinned QUIC leaf cert of the relay (`POLLIS_OVERLAY_RELAY_CERT`): a path
    /// to a DER file or base64 DER. Absent → no circuit (fail-closed).
    pub overlay_relay_cert: Option<String>,
    /// Signed relay directory URL (`POLLIS_OVERLAY_DIRECTORY_URL`). With the key
    /// below it makes the pool dynamic, superseding `POLLIS_OVERLAY_RELAY`
    /// (`crate::net::directory`, #616).
    pub overlay_directory_url: Option<String>,
    /// Pinned Ed25519 directory-signing public key
    /// (`POLLIS_OVERLAY_DIRECTORY_KEY`, base64 of 32 bytes). A URL without a key
    /// counts as unconfigured.
    pub overlay_directory_key: Option<String>,
    /// Dev Delivery Service gate key (`POLLIS_DEV_ACCESS_KEY`, #1242), sent as
    /// `X-Pollis-Dev-Key`.
    ///
    /// Runtime env only, never `option_env!`, so a release binary cannot bake it
    /// in. Attach it only through [`Config::dev_access_key_for_ds`], which never
    /// sends it to prod.
    pub dev_access_key: Option<String>,
}

/// The production Delivery Service host. The dev gate key is never sent here,
/// however the client was configured.
pub const PROD_DS_HOST: &str = "api.pollis.com";

impl Config {
    /// True when both the directory URL and pinned key are set. A half-config
    /// falls back to the static list rather than fetch an unverifiable directory.
    pub fn overlay_directory_configured(&self) -> bool {
        self.overlay_directory_url.is_some() && self.overlay_directory_key.is_some()
    }

    /// The configured relay endpoints, in order; empty when unconfigured.
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

    /// The dev gate key for a request to the configured DS, or `None` when the
    /// DS is prod ([`PROD_DS_HOST`]), unparseable or absent, or no key is set.
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
            // option_env! (release builds) first, then the runtime env (dev).
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
            // Runtime only: no option_env! here (see the field).
            dev_access_key: std::env::var("POLLIS_DEV_ACCESS_KEY")
                .ok()
                .filter(|s| !s.trim().is_empty()),
        })
    }
}

/// Parse `POLLIS_OVERLAY`: `prefer` / `strict` (case-insensitive); anything
/// else is `Off`, so a misconfigured value fails safe.
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
    /// Build a Config for the integration-test harness. Reads nothing from the
    /// environment; the flows harness overrides `pollis_delivery_url` with its
    /// in-process DS.
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

    /// The key reaches the dev DS (and a local one) and nothing else.
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

    /// The key is read from the RUNTIME environment only. An `option_env!`
    /// would bake whatever the build machine held into a release binary.
    #[test]
    fn dev_key_is_never_compiled_in() {
        let src = include_str!("config.rs");
        let needle = ["option_env!(\"", "POLLIS_DEV_ACCESS_KEY"].concat();
        assert!(!src.contains(&needle), "POLLIS_DEV_ACCESS_KEY must not be read with option_env!");
        assert!(src.contains("std::env::var(\"POLLIS_DEV_ACCESS_KEY\")"));
    }
}

