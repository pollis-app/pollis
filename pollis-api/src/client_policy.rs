//! The minimum-client-version policy (#1611): the manifest's format, its parser and the one rule.
//!
//! The published file is `https://cdn.pollis.com/client-policy.json` (dev:
//! `https://cdn.pollis.com/dev/client-policy.json`), written only by `scripts/client-policy.mjs`:
//!
//! ```json
//! { "v": 1,
//!   "minimum": { "ios": "1.0.21", "android-play": "1.0.21", "android-sideload": "1.0.21",
//!                "macos": "1.17.0", "windows": "1.17.0", "linux": "1.17.0", "cli": "1.17.0" },
//!   "message": null }
//! ```
//!
//! **The rule** ([`ClientPolicy::evaluate`]): a build is blocked iff `version < minimum[platform]`
//! under semver precedence. A missing key means no minimum. A version that does not parse is never
//! blocked (fail open). Clients (through `pollis-core`), the DS and the publishing script all use
//! this one implementation; the script runs it as `cargo run -p pollis-api --example client-policy`.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Header naming the client's build version (e.g. `1.17.0`); the ops log already reads it.
pub const CLIENT_VERSION_HEADER: &str = "x-pollis-client-version";
/// Header naming the client's [`ClientPlatform`] (e.g. `android-play`).
pub const CLIENT_PLATFORM_HEADER: &str = "x-pollis-client-platform";
/// The DS's read-only mirror of the published policy.
pub const CLIENT_POLICY_PATH: &str = "/v1/client-policy";
/// The `error` field of a 426 body.
pub const UPGRADE_REQUIRED_ERROR: &str = "upgrade_required";
/// The only manifest format version this build understands.
pub const POLICY_FORMAT_VERSION: u32 = 1;
/// Upper bound on the optional operator message, in characters.
pub const MESSAGE_MAX_CHARS: usize = 500;

/// Where the prod policy is published.
pub const PROD_POLICY_URL: &str = "https://cdn.pollis.com/client-policy.json";
/// Where the dev policy is published (builds pointed at a non-prod DS read this one).
pub const DEV_POLICY_URL: &str = "https://cdn.pollis.com/dev/client-policy.json";

/// Every distribution that can be held to its own minimum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClientPlatform {
    Ios,
    AndroidPlay,
    AndroidSideload,
    Macos,
    Windows,
    Linux,
    Cli,
}

impl ClientPlatform {
    pub const ALL: [ClientPlatform; 7] = [
        ClientPlatform::Ios,
        ClientPlatform::AndroidPlay,
        ClientPlatform::AndroidSideload,
        ClientPlatform::Macos,
        ClientPlatform::Windows,
        ClientPlatform::Linux,
        ClientPlatform::Cli,
    ];

    /// The manifest key and header value.
    pub fn as_str(self) -> &'static str {
        match self {
            ClientPlatform::Ios => "ios",
            ClientPlatform::AndroidPlay => "android-play",
            ClientPlatform::AndroidSideload => "android-sideload",
            ClientPlatform::Macos => "macos",
            ClientPlatform::Windows => "windows",
            ClientPlatform::Linux => "linux",
            ClientPlatform::Cli => "cli",
        }
    }

    /// Inverse of [`Self::as_str`]; `None` for anything else.
    pub fn parse(s: &str) -> Option<ClientPlatform> {
        ClientPlatform::ALL.into_iter().find(|p| p.as_str() == s)
    }
}

impl fmt::Display for ClientPlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A strict semver `MAJOR.MINOR.PATCH[-pre][+build]`, compared by semver precedence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
    /// Dot-separated pre-release identifiers; empty for a release.
    pre: Vec<String>,
}

impl Version {
    /// Parse a strict semver string. Leading zeros, empty parts and `v` prefixes are refused.
    pub fn parse(s: &str) -> Option<Version> {
        let s = s.trim();
        // Build metadata never affects precedence.
        let s = s.split_once('+').map(|(core, _)| core).unwrap_or(s);
        let (core, pre) = match s.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (s, None),
        };
        let mut parts = core.split('.');
        let major = numeric(parts.next()?)?;
        let minor = numeric(parts.next()?)?;
        let patch = numeric(parts.next()?)?;
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => {
                let ids: Vec<String> = pre.split('.').map(str::to_string).collect();
                let valid = ids.iter().all(|id| {
                    !id.is_empty()
                        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                        && !(id.len() > 1 && id.starts_with('0') && id.chars().all(|c| c.is_ascii_digit()))
                });
                if !valid {
                    return None;
                }
                ids
            }
        };
        Some(Version { major, minor, patch, pre })
    }
}

fn numeric(s: &str) -> Option<u64> {
    if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0')) {
        return None;
    }
    s.parse().ok()
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                // A pre-release sorts before its release.
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => compare_pre(&self.pre, &other.pre),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

fn compare_pre(a: &[String], b: &[String]) -> Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(x), Ok(y)) => x.cmp(&y),
            // Numeric identifiers sort before alphanumeric ones.
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => x.cmp(y),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

/// The published manifest, as clients and the DS read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientPolicy {
    /// Format version; only [`POLICY_FORMAT_VERSION`] is understood.
    pub v: u32,
    /// Minimum version per platform key. A missing key means no minimum. Keys a build does not
    /// know (a platform added later) are kept, so the cache round-trips them, and ignored.
    #[serde(default)]
    pub minimum: BTreeMap<String, String>,
    /// Shown on the blocked screen in place of the built-in text; `null` for the built-in text.
    #[serde(default)]
    pub message: Option<String>,
}

/// Why a manifest was refused. Clients treat every one of these as "no policy" (fail open).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    Json(String),
    UnsupportedFormat(u32),
    BadVersion { platform: String, value: String },
    UnknownPlatform(String),
    UnknownField(String),
    MessageTooLong(usize),
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PolicyError::Json(e) => write!(f, "not a policy object: {e}"),
            PolicyError::UnsupportedFormat(v) => {
                write!(f, "format v{v} is not supported (this build reads v{POLICY_FORMAT_VERSION})")
            }
            PolicyError::BadVersion { platform, value } => {
                write!(f, "minimum.{platform} = {value:?} is not a MAJOR.MINOR.PATCH version")
            }
            PolicyError::UnknownPlatform(p) => write!(
                f,
                "minimum.{p}: unknown platform (known: {})",
                ClientPlatform::ALL.map(ClientPlatform::as_str).join(", ")
            ),
            PolicyError::UnknownField(k) => write!(f, "unknown top-level field {k:?}"),
            PolicyError::MessageTooLong(n) => {
                write!(f, "message is {n} characters (max {MESSAGE_MAX_CHARS})")
            }
        }
    }
}

impl std::error::Error for PolicyError {}

impl ClientPolicy {
    /// A policy with no minimums: nothing is blocked.
    pub fn empty() -> ClientPolicy {
        ClientPolicy { v: POLICY_FORMAT_VERSION, minimum: BTreeMap::new(), message: None }
    }

    /// The client/DS parser: the format version, every minimum and the message are checked;
    /// unknown platform keys and top-level fields are tolerated (forward compatibility).
    pub fn parse(json: &str) -> Result<ClientPolicy, PolicyError> {
        let policy: ClientPolicy =
            serde_json::from_str(json).map_err(|e| PolicyError::Json(e.to_string()))?;
        policy.validate()?;
        Ok(policy)
    }

    /// The publisher's parser: [`Self::parse`] plus no unknown platform keys or fields, so a
    /// typo (`"andriod-play"`) is refused before it silently sets no minimum.
    pub fn parse_strict(json: &str) -> Result<ClientPolicy, PolicyError> {
        let raw: serde_json::Value =
            serde_json::from_str(json).map_err(|e| PolicyError::Json(e.to_string()))?;
        if let Some(obj) = raw.as_object() {
            if let Some(k) = obj.keys().find(|k| !matches!(k.as_str(), "v" | "minimum" | "message")) {
                return Err(PolicyError::UnknownField(k.clone()));
            }
        }
        let policy = ClientPolicy::parse(json)?;
        if let Some(k) = policy.minimum.keys().find(|k| ClientPlatform::parse(k).is_none()) {
            return Err(PolicyError::UnknownPlatform(k.clone()));
        }
        Ok(policy)
    }

    fn validate(&self) -> Result<(), PolicyError> {
        if self.v != POLICY_FORMAT_VERSION {
            return Err(PolicyError::UnsupportedFormat(self.v));
        }
        for (platform, value) in &self.minimum {
            if Version::parse(value).is_none() {
                return Err(PolicyError::BadVersion { platform: platform.clone(), value: value.clone() });
            }
        }
        if let Some(m) = &self.message {
            let n = m.chars().count();
            if n > MESSAGE_MAX_CHARS {
                return Err(PolicyError::MessageTooLong(n));
            }
        }
        Ok(())
    }

    /// The minimum for `platform`, if one is set.
    pub fn minimum_for(&self, platform: ClientPlatform) -> Option<&str> {
        self.minimum.get(platform.as_str()).map(String::as_str)
    }

    /// THE rule: blocked iff `version < minimum[platform]`. A missing key, or a version or
    /// minimum that does not parse, is allowed (fail open).
    pub fn evaluate(&self, platform: ClientPlatform, version: &str) -> Verdict {
        let Some(minimum) = self.minimum_for(platform) else {
            return Verdict::Allowed;
        };
        match (Version::parse(version), Version::parse(minimum)) {
            (Some(have), Some(need)) if have < need => Verdict::Blocked { minimum: minimum.to_string() },
            _ => Verdict::Allowed,
        }
    }

    /// The operator message, if it is set and not blank.
    pub fn message(&self) -> Option<&str> {
        self.message.as_deref().map(str::trim).filter(|m| !m.is_empty())
    }
}

/// The outcome of [`ClientPolicy::evaluate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    Blocked { minimum: String },
}

impl Verdict {
    pub fn is_blocked(&self) -> bool {
        matches!(self, Verdict::Blocked { .. })
    }
}

/// The typed body of a DS `426 Upgrade Required` (#1611).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpgradeRequiredBody {
    /// Always [`UPGRADE_REQUIRED_ERROR`].
    pub error: String,
    pub platform: ClientPlatform,
    /// The version the request declared.
    pub version: String,
    /// The platform's current minimum.
    pub minimum: String,
    /// The policy's operator message, if any.
    #[serde(default)]
    pub message: Option<String>,
    /// The whole policy the DS enforced, so the client can cache it and keep blocking offline.
    pub policy: ClientPolicy,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s} should parse"))
    }

    #[test]
    fn versions_parse_strictly() {
        for ok in ["0.0.0", "1.17.0", "10.20.30", "1.0.0-rc.1", "1.0.0-alpha-1", "1.0.0+build.5", " 1.2.3 "] {
            assert!(Version::parse(ok).is_some(), "{ok}");
        }
        for bad in ["", "1", "1.2", "1.2.3.4", "v1.2.3", "01.2.3", "1.02.3", "1.2.x", "1.2.3-", "1.2.3-01", "1.2.3-a..b", "-1.2.3", "unknown"] {
            assert!(Version::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn versions_compare_by_semver_precedence() {
        let ordered = [
            "1.0.0-alpha", "1.0.0-alpha.1", "1.0.0-alpha.beta", "1.0.0-beta", "1.0.0-beta.2",
            "1.0.0-beta.11", "1.0.0-rc.1", "1.0.0", "1.0.1", "1.1.0", "1.9.0", "1.10.0", "2.0.0",
        ];
        for pair in ordered.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
        // Numeric, not lexical.
        assert!(v("1.17.0") > v("1.9.0"));
        // Build metadata is ignored.
        assert_eq!(v("1.2.3+a").cmp(&v("1.2.3+b")), Ordering::Equal);
    }

    #[test]
    fn platform_keys_round_trip() {
        for p in ClientPlatform::ALL {
            assert_eq!(ClientPlatform::parse(p.as_str()), Some(p));
            assert_eq!(serde_json::to_string(&p).unwrap(), format!("\"{}\"", p.as_str()));
        }
        assert_eq!(ClientPlatform::parse("android"), None);
    }

    fn policy(minimum: &[(&str, &str)]) -> ClientPolicy {
        ClientPolicy {
            v: 1,
            minimum: minimum.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            message: None,
        }
    }

    #[test]
    fn the_rule() {
        let p = policy(&[("macos", "1.17.0"), ("ios", "1.0.21")]);
        // Up to date.
        assert_eq!(p.evaluate(ClientPlatform::Macos, "1.18.2"), Verdict::Allowed);
        // Exactly at the minimum is allowed.
        assert_eq!(p.evaluate(ClientPlatform::Macos, "1.17.0"), Verdict::Allowed);
        // Below it is blocked.
        assert_eq!(
            p.evaluate(ClientPlatform::Macos, "1.16.9"),
            Verdict::Blocked { minimum: "1.17.0".into() }
        );
        // A pre-release of the minimum is below it.
        assert!(p.evaluate(ClientPlatform::Macos, "1.17.0-rc.1").is_blocked());
        // Each platform has its own floor; a missing key is no minimum.
        assert!(p.evaluate(ClientPlatform::Ios, "1.0.20").is_blocked());
        assert_eq!(p.evaluate(ClientPlatform::Windows, "0.0.1"), Verdict::Allowed);
        assert_eq!(p.evaluate(ClientPlatform::AndroidPlay, "0.0.1"), Verdict::Allowed);
        // An unparseable own version fails open.
        assert_eq!(p.evaluate(ClientPlatform::Macos, "unknown"), Verdict::Allowed);
        assert_eq!(p.evaluate(ClientPlatform::Macos, ""), Verdict::Allowed);
    }

    #[test]
    fn an_empty_policy_blocks_nothing() {
        let p = ClientPolicy::parse(r#"{"v":1,"minimum":{},"message":null}"#).unwrap();
        for platform in ClientPlatform::ALL {
            assert_eq!(p.evaluate(platform, "0.0.0"), Verdict::Allowed);
        }
        // `minimum` and `message` may be omitted.
        assert_eq!(ClientPolicy::parse(r#"{"v":1}"#).unwrap(), ClientPolicy::empty());
    }

    #[test]
    fn the_issue_example_parses() {
        let json = r#"{ "v": 1,
          "minimum": { "ios": "1.0.21", "android-play": "1.0.21", "android-sideload": "1.0.21",
                       "macos": "1.17.0", "windows": "1.17.0", "linux": "1.17.0", "cli": "1.17.0" },
          "message": null }"#;
        let p = ClientPolicy::parse_strict(json).unwrap();
        assert_eq!(p.minimum.len(), 7);
        assert!(p.evaluate(ClientPlatform::Cli, "1.16.0").is_blocked());
    }

    #[test]
    fn malformed_manifests_are_refused() {
        for (json, what) in [
            ("", "empty"),
            ("not json", "garbage"),
            ("[]", "array"),
            (r#"{"minimum":{}}"#, "no format version"),
            (r#"{"v":2,"minimum":{}}"#, "future format"),
            (r#"{"v":"1"}"#, "string format"),
            (r#"{"v":1,"minimum":{"macos":"1.17"}}"#, "short version"),
            (r#"{"v":1,"minimum":{"macos":17}}"#, "number version"),
            (r#"{"v":1,"minimum":[]}"#, "minimum not an object"),
            (r#"{"v":1,"message":5}"#, "message not a string"),
        ] {
            assert!(ClientPolicy::parse(json).is_err(), "{what}: {json}");
        }
        let long = format!(r#"{{"v":1,"message":"{}"}}"#, "x".repeat(MESSAGE_MAX_CHARS + 1));
        assert_eq!(
            ClientPolicy::parse(&long),
            Err(PolicyError::MessageTooLong(MESSAGE_MAX_CHARS + 1))
        );
    }

    #[test]
    fn clients_tolerate_what_the_publisher_refuses() {
        let json = r#"{"v":1,"minimum":{"visionos":"1.0.0","macos":"1.17.0"},"extra":true}"#;
        let p = ClientPolicy::parse(json).unwrap();
        assert!(p.evaluate(ClientPlatform::Macos, "1.0.0").is_blocked());
        assert_eq!(ClientPolicy::parse_strict(json), Err(PolicyError::UnknownField("extra".into())));
        let typo = r#"{"v":1,"minimum":{"andriod-play":"1.0.0"}}"#;
        assert_eq!(
            ClientPolicy::parse_strict(typo),
            Err(PolicyError::UnknownPlatform("andriod-play".into()))
        );
    }

    #[test]
    fn blank_messages_mean_the_built_in_text() {
        let mut p = ClientPolicy::empty();
        p.message = Some("   ".into());
        assert_eq!(p.message(), None);
        p.message = Some(" Please update. ".into());
        assert_eq!(p.message(), Some("Please update."));
    }

    #[test]
    fn the_426_body_round_trips() {
        let body = UpgradeRequiredBody {
            error: UPGRADE_REQUIRED_ERROR.into(),
            platform: ClientPlatform::AndroidSideload,
            version: "1.0.3".into(),
            minimum: "1.0.21".into(),
            message: None,
            policy: policy(&[("android-sideload", "1.0.21")]),
        };
        let json = serde_json::to_string(&body).unwrap();
        assert!(json.contains(r#""platform":"android-sideload""#), "{json}");
        assert_eq!(serde_json::from_str::<UpgradeRequiredBody>(&json).unwrap(), body);
    }
}
