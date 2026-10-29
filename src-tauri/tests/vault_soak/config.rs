//! Soak configuration, all from the environment (`scripts/vault-soak.sh` sets it).

use std::path::PathBuf;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    /// In-process DS + a local S3 (SeaweedFS) — every hop observable.
    Local,
    /// api-dev + dev R2; only the DS hop is tapped (via a local proxy).
    Dev,
}

#[derive(Clone, Debug)]
pub struct S3 {
    pub endpoint: String,
    pub public_url: String,
    pub bucket: String,
    pub region: String,
    pub access_key: String,
    pub secret_key: String,
}

#[derive(Clone, Debug)]
pub struct Dev {
    pub ds_url: String,
    pub access_key: Option<String>,
    pub otp: String,
    /// Read-only Turso access for the DS-row proofs; optional.
    pub turso: Option<(String, String)>,
}

#[derive(Clone, Debug)]
pub struct SoakConfig {
    pub target: Target,
    pub sizes: Vec<u64>,
    pub media_dir: Option<PathBuf>,
    pub workdir: PathBuf,
    pub report: PathBuf,
    pub seed: u64,
    pub s3: S3,
    pub dev: Option<Dev>,
}

fn var(k: &str) -> Option<String> {
    std::env::var(k).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

fn need(k: &str) -> String {
    var(k).unwrap_or_else(|| panic!("vault soak: {k} must be set (scripts/vault-soak.sh sets it)"))
}

/// `512`, `64K`, `5M`, `1.5G` — binary units.
pub fn parse_size(s: &str) -> u64 {
    let s = s.trim();
    let (num, mult) = match s.chars().last().map(|c| c.to_ascii_uppercase()) {
        Some('K') => (&s[..s.len() - 1], 1024.0),
        Some('M') => (&s[..s.len() - 1], 1024.0 * 1024.0),
        Some('G') => (&s[..s.len() - 1], 1024.0 * 1024.0 * 1024.0),
        _ => (s, 1.0),
    };
    let n: f64 = num.parse().unwrap_or_else(|_| panic!("vault soak: bad size {s:?}"));
    (n * mult).round() as u64
}

pub fn human(n: u64) -> String {
    const U: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

/// `count` log-spaced sizes from `min` to `max` inclusive.
pub fn log_spaced(count: usize, min: u64, max: u64) -> Vec<u64> {
    if count == 0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![max];
    }
    let (lmin, lmax) = ((min.max(1) as f64).ln(), (max.max(1) as f64).ln());
    (0..count)
        .map(|i| (lmin + (lmax - lmin) * i as f64 / (count - 1) as f64).exp().round() as u64)
        .collect()
}

impl SoakConfig {
    pub fn from_env() -> Self {
        let target = match var("POLLIS_SOAK_TARGET").as_deref().unwrap_or("local") {
            "local" => Target::Local,
            "dev" => Target::Dev,
            other => panic!("vault soak: POLLIS_SOAK_TARGET must be local or dev, not {other:?}"),
        };
        let sizes = match var("POLLIS_SOAK_SIZES") {
            Some(list) => list.split(',').filter(|s| !s.trim().is_empty()).map(parse_size).collect(),
            None => {
                let count: usize = var("POLLIS_SOAK_COUNT").map(|c| c.parse().expect("POLLIS_SOAK_COUNT")).unwrap_or(20);
                let min = var("POLLIS_SOAK_MIN_SIZE").map(|s| parse_size(&s)).unwrap_or(1024 * 1024);
                let max = var("POLLIS_SOAK_MAX_SIZE").map(|s| parse_size(&s)).unwrap_or(1024 * 1024 * 1024);
                log_spaced(count, min, max)
            }
        };
        let workdir = var("POLLIS_SOAK_WORKDIR").map(PathBuf::from).unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../target/vault-soak")
                .join(chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string())
        });
        let report = var("POLLIS_SOAK_REPORT").map(PathBuf::from).unwrap_or_else(|| workdir.join("report.md"));
        let seed = var("POLLIS_SOAK_SEED")
            .map(|s| s.parse().expect("POLLIS_SOAK_SEED"))
            .unwrap_or_else(rand::random);
        let media_dir = var("POLLIS_SOAK_MEDIA_DIR").map(PathBuf::from);

        let (s3, dev) = match target {
            Target::Local => (
                S3 {
                    endpoint: need("POLLIS_SOAK_S3_ENDPOINT"),
                    public_url: need("POLLIS_SOAK_S3_ENDPOINT"),
                    bucket: need("POLLIS_SOAK_S3_BUCKET"),
                    region: var("POLLIS_SOAK_S3_REGION").unwrap_or_else(|| "us-east-1".to_string()),
                    access_key: need("POLLIS_SOAK_S3_ACCESS_KEY"),
                    secret_key: need("POLLIS_SOAK_S3_SECRET_KEY"),
                },
                None,
            ),
            Target::Dev => {
                let ds_url = need("POLLIS_DELIVERY_URL");
                let host = ds_url.trim().trim_end_matches('/').to_ascii_lowercase();
                assert!(
                    !(host.ends_with("://api.pollis.com") || host.ends_with("://api.pollis.com.")),
                    "vault soak: refusing to run against the PRODUCTION DS"
                );
                (
                    S3 {
                        endpoint: need("R2_S3_ENDPOINT"),
                        public_url: need("R2_PUBLIC_URL"),
                        bucket: need("R2_BUCKET"),
                        region: var("R2_REGION").unwrap_or_else(|| "auto".to_string()),
                        access_key: need("R2_ACCESS_KEY_ID"),
                        secret_key: var("R2_SECRET_ACCESS_KEY").unwrap_or_else(|| need("R2_SECRET_KEY")),
                    },
                    Some(Dev {
                        ds_url,
                        access_key: var("POLLIS_DEV_ACCESS_KEY"),
                        otp: need("DEV_OTP"),
                        turso: match (var("TURSO_URL"), var("TURSO_TOKEN")) {
                            (Some(u), Some(t)) => Some((u, t)),
                            _ => None,
                        },
                    }),
                )
            }
        };

        Self { target, sizes, media_dir, workdir, report, seed, s3, dev }
    }
}
