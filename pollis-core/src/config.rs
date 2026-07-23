use crate::error::{Error, Result};

// Re-exported so Config consumers can name the overlay mode without taking a
// direct dependency on `pollis-relay` (the field type below lives there).
pub use pollis_relay::OverlayMode;

#[derive(Debug, Clone)]
pub struct Config {
    pub turso_url: String,
    pub turso_token: String,
    /// Optional read-only connection to the commit-log Turso DB (the future
    /// home of `mls_commit_log` / `mls_welcome` / `mls_group_info`). When both
    /// are set, `AppState.log_db` connects here; otherwise it falls back to
    /// `remote_db` so behavior is unchanged pre-cutover. See `docs/goal-a-commit-log-sole-writer.md`.
    pub log_db_url: Option<String>,
    pub log_db_token: Option<String>,
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
    /// Sealed sender (issue #331, `docs/metadata-minimization-design.md` §2).
    /// When true, outbound `message_envelope` rows are written with `sealed = 1`
    /// and a non-identifying sentinel `sender_id` instead of the real sender — so
    /// the stored envelope no longer reveals sender-per-message (at-rest / breach
    /// / subpoena defense). Attribution is unaffected: recipients always take the
    /// sender from the MLS credential inside the ciphertext (release N, already
    /// shipped). Defaults OFF: landing the sending half == release N (reader on,
    /// sealing off); flipping `POLLIS_SEAL_SENDER` on later == release N+1. This
    /// is the additive two-release dance CLAUDE.md prescribes.
    pub seal_sender: bool,
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
}

impl Config {
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
            turso_url:            require_env("TURSO_URL",        option_env!("TURSO_URL"))?,
            turso_token:          require_env("TURSO_TOKEN",      option_env!("TURSO_TOKEN"))?,
            // Optional: absent → log_db falls back to remote_db (tests / pre-cutover).
            log_db_url: option_env!("LOG_DB_URL")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("LOG_DB_URL").ok())
                .filter(|s| !s.is_empty()),
            log_db_token: option_env!("LOG_DB_TOKEN")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("LOG_DB_TOKEN").ok())
                .filter(|s| !s.is_empty()),
            r2_endpoint:          require_env("R2_S3_ENDPOINT",   option_env!("R2_S3_ENDPOINT"))?,
            r2_public_url:        require_env("R2_PUBLIC_URL",    option_env!("R2_PUBLIC_URL"))?,
            livekit_url: option_env!("LIVEKIT_URL")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("LIVEKIT_URL").ok())
                .unwrap_or_default(),
            // Optional: absent → direct Turso writes; present → route through the DS.
            pollis_delivery_url: option_env!("POLLIS_DELIVERY_URL")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("POLLIS_DELIVERY_URL").ok())
                .filter(|s| !s.is_empty()),
            // Optional boolean, default OFF (release N: reader on, sealing off).
            seal_sender: option_env!("POLLIS_SEAL_SENDER")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("POLLIS_SEAL_SENDER").ok())
                .map(|s| parse_env_bool(&s))
                .unwrap_or(false),
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

/// Parse a boolean-ish env var: `1` / `true` / `yes` / `on` (case-insensitive)
/// are true; everything else (including unset, handled by the caller) is false.
fn parse_env_bool(s: &str) -> bool {
    matches!(
        s.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn require_env(key: &str, compiled: Option<&'static str>) -> Result<String> {
    compiled
        .map(|s| s.to_string())
        .or_else(|| std::env::var(key).ok())
        .ok_or_else(|| Error::Config(format!("missing env var: {key}")))
}

#[cfg(any(test, feature = "test-harness"))]
impl Config {
    /// Build a Config for the integration-test harness. Loads `.env.test`
    /// (searching up from the workspace) with override semantics so any
    /// ambient `.env.development` values never leak into tests. R2 /
    /// LiveKit fields are filled with placeholders — the harness does not
    /// touch R2 or real-time media, and OTP delivery is routed through the
    /// configured Delivery Service.
    pub fn for_test() -> Result<Self> {
        let env_path = find_env_test_file()?;
        dotenvy::from_filename_override(&env_path)
            .map_err(|e| Error::Config(format!("load {}: {e}", env_path.display())))?;

        let turso_url = std::env::var("TURSO_URL")
            .map_err(|_| Error::Config("TURSO_URL missing from .env.test".into()))?;
        let turso_token = std::env::var("TURSO_TOKEN")
            .map_err(|_| Error::Config("TURSO_TOKEN missing from .env.test".into()))?;

        Ok(Self {
            turso_url,
            turso_token,
            // Tests use a single Turso instance; log_db falls back to remote_db.
            log_db_url: None,
            log_db_token: None,
            r2_endpoint: String::new(),
            r2_public_url: String::new(),
            livekit_url: String::new(),
            // Default None; the flows harness overrides this to its in-process
            // DS URL, so integration tests exercise the real (signed) DS write
            // path. There is no remaining direct-write path to exercise.
            pollis_delivery_url: None,
            // Default OFF; the sealed-sender flows test flips this per-client to
            // exercise the release-N+1 sealing path (see `TestClient::new_sealed`).
            seal_sender: false,
            // Overlay off in the integration harness — it exercises the direct
            // control-plane path. Overlay wiring has its own unit tests
            // (`net::overlay`) that spin an in-process relay.
            overlay_mode: pollis_relay::OverlayMode::Off,
            overlay_relay_url: None,
            overlay_relay_cert: None,
        })
    }
}

#[cfg(any(test, feature = "test-harness"))]
fn find_env_test_file() -> Result<std::path::PathBuf> {
    let start = std::env::current_dir()
        .map_err(|e| Error::Config(format!("current_dir: {e}")))?;
    let mut dir = start.as_path();
    loop {
        let candidate = dir.join(".env.test");
        if candidate.exists() {
            return Ok(candidate);
        }
        dir = match dir.parent() {
            Some(p) => p,
            None => {
                return Err(Error::Config(format!(
                    ".env.test not found walking up from {}",
                    start.display()
                )))
            }
        };
    }
}
