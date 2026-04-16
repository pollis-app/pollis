use crate::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct Config {
    pub turso_url: String,
    pub turso_token: String,
    pub r2_endpoint: String,
    pub r2_access_key_id: String,
    pub r2_secret_access_key: String,
    pub r2_region: String,
    pub r2_public_url: String,
    pub livekit_url: String,
    pub livekit_api_key: String,
    pub livekit_api_secret: String,
    pub resend_api_key: String,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            // option_env! embeds the value at compile time (e.g. from GH Actions secrets).
            // Falls back to std::env::var for dev builds loaded via dotenvy.
            turso_url:            require_env("TURSO_URL",        option_env!("TURSO_URL"))?,
            turso_token:          require_env("TURSO_TOKEN",      option_env!("TURSO_TOKEN"))?,
            r2_endpoint:          require_env("R2_S3_ENDPOINT",   option_env!("R2_S3_ENDPOINT"))?,
            r2_access_key_id:     require_env("R2_ACCESS_KEY_ID", option_env!("R2_ACCESS_KEY_ID"))?,
            r2_secret_access_key: require_env("R2_SECRET_KEY",    option_env!("R2_SECRET_KEY"))?,
            r2_public_url:        require_env("R2_PUBLIC_URL",    option_env!("R2_PUBLIC_URL"))?,
            resend_api_key:       require_env("RESEND_API_KEY",   option_env!("RESEND_API_KEY"))?,
            // Cloudflare R2 uses "auto" as its S3-compatible region
            r2_region: option_env!("R2_REGION")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("R2_REGION").ok())
                .unwrap_or_else(|| "auto".to_string()),
            livekit_url: option_env!("LIVEKIT_URL")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("LIVEKIT_URL").ok())
                .unwrap_or_default(),
            livekit_api_key: option_env!("LIVEKIT_API_KEY")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("LIVEKIT_API_KEY").ok())
                .unwrap_or_default(),
            livekit_api_secret: option_env!("LIVEKIT_API_SECRET")
                .map(|s| s.to_string())
                .or_else(|| std::env::var("LIVEKIT_API_SECRET").ok())
                .unwrap_or_default(),
        })
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
    /// Build a Config for the integration-test harness. Loads `.env.test`
    /// (searching up from the workspace) with override semantics so any
    /// ambient `.env.development` values never leak into tests. R2 /
    /// LiveKit / Resend fields are filled with placeholders — the harness
    /// does not touch R2 or real-time media, and OTP delivery is bypassed
    /// by `DEV_OTP`.
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
            r2_endpoint: String::new(),
            r2_access_key_id: String::new(),
            r2_secret_access_key: String::new(),
            r2_region: "auto".into(),
            r2_public_url: String::new(),
            livekit_url: String::new(),
            livekit_api_key: String::new(),
            livekit_api_secret: String::new(),
            resend_api_key: String::new(),
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
