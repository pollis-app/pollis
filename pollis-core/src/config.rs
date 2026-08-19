use crate::error::{Error, Result};

// Re-exported so Config consumers can name the overlay mode without taking a
// direct dependency on `pollis-relay` (the field type below lives there).
pub use pollis_relay::OverlayMode;

#[derive(Debug, Clone)]
/// The client's compile-time configuration.
///
/// **No database credential appears here (#987).** `TURSO_URL`, `TURSO_TOKEN`,
/// `LOG_DB_URL` and `LOG_DB_TOKEN` were baked in with `option_env!` and reached
/// every shipped binary; the client now speaks only to the Delivery Service, so
/// there is nothing to bake. Removing the FIELDS is what makes that permanent —
/// a future read cannot quietly reintroduce a connection when no URL and no
/// token exist to make one with, and `pollis-core` no longer depends on
/// `libsql` either.
///
/// Note for anyone adding a compile-time input here: `scripts/check-build-recipe.py`
/// enforces `desktop-release.yml` ⊆ `rebuild-verify.yml` ⊆ the `option_env!`
/// keys below, so a new value must be added to the release workflow AND the
/// reproducer's recipe, or independent Linux reproduction breaks.
pub struct Config {
    /// R2 S3 endpoint. Non-secret — retained only to build the display `url`
    /// returned from uploads. All R2 access credentials moved server-side to the
    /// DS secrets broker (`/v1/r2/presign`); the client holds none. See #393.
    pub r2_endpoint: String,
    pub r2_public_url: String,
    /// LiveKit ws URL. Non-secret — the client SDK dials it and the DS also
    /// returns it with each minted token. The LiveKit API key/secret moved
    /// server-side to the DS broker (#393); the client holds neither.
    pub livekit_url: String,
    /// Delivery Service base URL (e.g. `https://api.pollis.com`). When set, MLS
    /// commit submission routes through the DS (serialized, race/gap-free);
    /// when `None`, commits write direct to Turso. See `commands::mls::delivery`.
    pub pollis_delivery_url: Option<String>,
    /// Closed-overlay relay mode (design `docs/relay-overlay-design.md` §10.1,
    /// §14). Parsed from `POLLIS_OVERLAY` (`off` | `prefer` | `strict`, default
    /// **off**; unknown/empty → off). When `Off` the overlay is inert and every
    /// network path is byte-for-byte identical to a build without it. `Prefer`
    /// routes the control plane through the overlay with direct fallback; `Strict`
    /// requires it and surfaces a degraded error rather than silently going direct
    /// (messages-must-work). Media (LiveKit) stays direct in every mode (§6.4).
    pub overlay_mode: pollis_relay::OverlayMode,
    /// The v0 first-party relay endpoint(s) (`POLLIS_OVERLAY_RELAY`, e.g.
    /// `relay.pollis.com:443`). Comma-separated for a POOL: `RealRelayFactory`
    /// tries them in health order and fails over to the first success (see
    /// [`overlay_relay_endpoints`](Config::overlay_relay_endpoints)). Absent → the
    /// overlay cannot build a circuit: in `Prefer` that means direct fallback,
    /// in `Strict` a surfaced
    /// degraded error — never a silent drop. The shim still starts whenever the
    /// mode is non-off so `Strict` degrades instead of silently going direct.
    pub overlay_relay_url: Option<String>,
    /// The pinned QUIC server identity of the relay (`POLLIS_OVERLAY_RELAY_CERT`):
    /// a filesystem path to a DER cert, or the base64 (STANDARD) of the DER bytes.
    /// The client pins this exact leaf (the relay's identity *is* its cert, see
    /// `pollis_relay::tls::PinnedServerCertVerifier`) so it verifies which relay
    /// it dials. Absent → no circuit can be built (fail-closed, same as an absent
    /// endpoint). Kept separate from the endpoint so a future pool can pin one
    /// cert while listing several addresses.
    pub overlay_relay_cert: Option<String>,
    /// URL of the signed relay **directory** (`POLLIS_OVERLAY_DIRECTORY_URL`, e.g.
    /// `https://relays.pollis.com/directory.json`). When set (with the key below),
    /// the overlay pool is DYNAMIC: the client fetches this directory, verifies it
    /// (§3), and refreshes it as membership changes — superseding the static
    /// `POLLIS_OVERLAY_RELAY` list. Absent → the static list is used (v0). See
    /// `crate::net::directory` and issue #616.
    pub overlay_directory_url: Option<String>,
    /// The pinned Ed25519 directory-signing PUBLIC key
    /// (`POLLIS_OVERLAY_DIRECTORY_KEY`): base64 (STANDARD) of the raw 32 bytes. The
    /// client verifies every fetched directory against exactly this key, so a
    /// rolled-back or forged directory fails closed. Required alongside the URL —
    /// a URL without a key is treated as "directory not configured" (fail-safe).
    pub overlay_directory_key: Option<String>,
}

impl Config {
    /// True when BOTH the directory URL and pinned key are set — the DYNAMIC pool
    /// path. A URL without a key (or vice versa) is deliberately NOT configured:
    /// verifying against a key is non-negotiable, so a half-config fails safe to
    /// the static list rather than fetching an unverifiable directory.
    pub fn overlay_directory_configured(&self) -> bool {
        self.overlay_directory_url.is_some() && self.overlay_directory_key.is_some()
    }

    /// The configured relay endpoints, in order. `RealRelayFactory` treats them
    /// as a pool — tries them in health order and fails over to the first
    /// success. Empty when unconfigured.
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
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            // option_env! embeds the value at compile time (e.g. from GH Actions secrets).
            // Falls back to std::env::var for dev builds loaded via dotenvy.
            r2_endpoint:          require_env("R2_S3_ENDPOINT",   option_env!("R2_S3_ENDPOINT"))?,
            r2_public_url:        require_env("R2_PUBLIC_URL",    option_env!("R2_PUBLIC_URL"))?,
            livekit_url: option_env!("LIVEKIT_URL")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("LIVEKIT_URL").ok())
                .unwrap_or_default(),
            // Absent → no remote backend at all (see the field's doc).
            pollis_delivery_url: option_env!("POLLIS_DELIVERY_URL")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("POLLIS_DELIVERY_URL").ok())
                .filter(|s| !s.is_empty()),
            // Optional overlay mode, default OFF (§14: overlay inert unless a
            // non-off mode is selected at runtime).
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
        })
    }
}

/// Parse `POLLIS_OVERLAY`: `prefer` / `strict` (case-insensitive) select those
/// modes; everything else — including `off`, unknown values, and empty — is
/// `Off`, so a misconfigured value fails safe to today's direct path.
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
    ///
    /// Reads nothing from the environment (#987). It used to load `.env.test`
    /// and REQUIRE `TURSO_URL` / `TURSO_TOKEN` from it — CI provisioned both as
    /// literal placeholders, because the harness always backed "remote Turso"
    /// with a local libsql file and never dialled Turso at all. With the fields
    /// gone there is nothing left to provision, so the requirement goes with
    /// them: the suite now builds and runs with every `TURSO_*` and `LOG_DB_*`
    /// variable unset, which is the property #987 is actually claiming.
    ///
    /// R2 / LiveKit fields are placeholders — the harness touches neither — and
    /// the flows harness overrides `pollis_delivery_url` with its in-process DS.
    pub fn for_test() -> Result<Self> {
        Ok(Self {
            r2_endpoint: String::new(),
            r2_public_url: String::new(),
            livekit_url: String::new(),
            // Default None; the flows harness overrides this to its in-process
            // DS URL, so integration tests exercise the real (signed) DS path.
            // There is no other path left to exercise.
            pollis_delivery_url: None,
            // Overlay off in the integration harness — it exercises the direct
            // control-plane path. Overlay wiring has its own unit tests
            // (`net::overlay`) that spin an in-process relay.
            overlay_mode: pollis_relay::OverlayMode::Off,
            overlay_relay_url: None,
            overlay_relay_cert: None,
            overlay_directory_url: None,
            overlay_directory_key: None,
        })
    }
}

