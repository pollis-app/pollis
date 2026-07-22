use crate::error::{Error, Result};

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
        })
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
