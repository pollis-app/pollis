//! The Pollis App manifest and grants (#2004).
//!
//! An App ships a component plus `pollis-app.json`, the manifest: what it wants
//! to be woken by, what it wants to read, what it wants to do and which hosts it
//! wants to reach. An admin narrows that when approving it (#2005), never
//! widens it. [`app_hash`] is the one App hash the runtime, the CLI and the
//! apps compute; [`diff`] is the update diff and the widening rule. The runtime enforces only the **effective grants**: the manifest
//! intersected with the admin's grants ([`effective_grants`]).
//!
//! The JSON schema published beside the WIT (`pollis-app-wit/pollis-app.schema.json`)
//! describes the same shape; `tests/schema.rs` keeps the two in step.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

pub mod app_hash;
pub mod diff;

pub use app_hash::{app_hash, hex, manifest_hash, parse_hex_hash, Hash};
pub use diff::{diff, widenings_between, ChangeField, ChangeKind, ManifestChange};

/// The file name a manifest is shipped under, beside the component.
pub const MANIFEST_FILE_NAME: &str = "pollis-app.json";

/// What can wake an App. Each maps to one exported handler in the WIT world.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    /// Every new message in a granted channel (`on-message`).
    Messages,
    /// Messages that mention the bot (`on-mention`).
    Mentions,
    /// Slash commands addressed to the App (`on-command`).
    Commands,
    /// Scheduled firings (`on-schedule`).
    Schedule,
    /// Requests from members (`on-request`).
    Requests,
    /// Inbound webhook deliveries (`on-webhook`).
    Webhook,
}

impl TriggerKind {
    pub const ALL: [TriggerKind; 6] = [
        TriggerKind::Messages,
        TriggerKind::Mentions,
        TriggerKind::Commands,
        TriggerKind::Schedule,
        TriggerKind::Requests,
        TriggerKind::Webhook,
    ];

    /// The manifest spelling, for refusal messages.
    pub fn as_str(self) -> &'static str {
        match self {
            TriggerKind::Messages => "messages",
            TriggerKind::Mentions => "mentions",
            TriggerKind::Commands => "commands",
            TriggerKind::Schedule => "schedule",
            TriggerKind::Requests => "requests",
            TriggerKind::Webhook => "webhook",
        }
    }
}

/// Which kinds of message event an App may see, as a trigger or in a query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    /// A message as first sent.
    Created,
    /// A message that has been edited since it was sent.
    Edited,
}

impl EventKind {
    pub const ALL: [EventKind; 2] = [EventKind::Created, EventKind::Edited];

    pub fn as_str(self) -> &'static str {
        match self {
            EventKind::Created => "created",
            EventKind::Edited => "edited",
        }
    }
}

/// Things an App may ask the runtime to do in the group. There is no
/// moderation action (deleting others' messages, removing members, changing
/// roles), and none may be added: bots never moderate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    Post,
    Reply,
    React,
    EditOwn,
    DeleteOwn,
    /// Declared here so manifests can name it; the capability is #2008.
    TransferHistory,
}

impl ActionKind {
    pub const ALL: [ActionKind; 6] = [
        ActionKind::Post,
        ActionKind::Reply,
        ActionKind::React,
        ActionKind::EditOwn,
        ActionKind::DeleteOwn,
        ActionKind::TransferHistory,
    ];

    /// The manifest spelling, for refusal messages.
    pub fn as_str(self) -> &'static str {
        match self {
            ActionKind::Post => "post",
            ActionKind::Reply => "reply",
            ActionKind::React => "react",
            ActionKind::EditOwn => "edit_own",
            ActionKind::DeleteOwn => "delete_own",
            ActionKind::TransferHistory => "transfer_history",
        }
    }
}

/// The manifest's read scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadScope {
    /// Channel selectors: `"#name"` for one channel by name, or `"*"` for
    /// whichever channels the admin grants. Empty: no channel.
    #[serde(default)]
    pub channels: Vec<String>,
    /// History window in days that `query-messages` may reach back. 0: none.
    #[serde(default)]
    pub history: u32,
    /// Message event kinds the App may see. Defaults to new messages only.
    #[serde(default = "default_events")]
    pub events: BTreeSet<EventKind>,
}

fn default_events() -> BTreeSet<EventKind> {
    BTreeSet::from([EventKind::Created])
}

impl Default for ReadScope {
    fn default() -> Self {
        ReadScope {
            channels: Vec::new(),
            history: 0,
            events: default_events(),
        }
    }
}

/// Sealed per-installation key-value state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateSpec {
    /// Bytes of keys plus values the installation may hold. 0: no state.
    #[serde(default)]
    pub quota: u64,
}

/// `pollis-app.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// Editors put the schema URL here; it carries no meaning.
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    pub name: String,
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub triggers: BTreeSet<TriggerKind>,
    #[serde(default)]
    pub read: ReadScope,
    #[serde(default)]
    pub actions: BTreeSet<ActionKind>,
    #[serde(default)]
    pub state: StateSpec,
    /// Exact host names the App may reach over HTTPS. No wildcards, no IP
    /// literals, no ports.
    #[serde(default)]
    pub egress: BTreeSet<String>,
    /// When the `schedule` trigger fires. Required with it, refused without it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub schedules: Vec<ScheduleSpec>,
}

/// Shortest and longest schedule period, in seconds.
pub const MIN_SCHEDULE_SECS: u64 = 60;
pub const MAX_SCHEDULE_SECS: u64 = 7 * 24 * 3600;

/// One schedule: it fires at every Unix time `k * every + offset` (seconds,
/// UTC), so every host of a bot computes the same slots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScheduleSpec {
    /// Lowercase letters, digits and '-', 1 to 32 characters; passed to `on-schedule`.
    pub name: String,
    /// Period in seconds, [`MIN_SCHEDULE_SECS`] to [`MAX_SCHEDULE_SECS`].
    pub every: u64,
    /// Seconds after each period boundary, less than `every`.
    #[serde(default)]
    pub offset: u64,
}

impl ScheduleSpec {
    /// The latest slot at or before `now` (Unix seconds).
    pub fn slot_at_or_before(&self, now: u64) -> Option<u64> {
        if now < self.offset {
            return None;
        }
        Some((now - self.offset) / self.every * self.every + self.offset)
    }

    /// The first slot strictly after `now`.
    pub fn next_after(&self, now: u64) -> u64 {
        match self.slot_at_or_before(now) {
            Some(s) => s + self.every,
            None => self.offset,
        }
    }
}

/// Why a manifest or a grant set was refused.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ManifestError {
    #[error("`{MANIFEST_FILE_NAME}` is not valid: {0}")]
    Parse(String),
    #[error("`name` {0:?} is not valid: lowercase letters, digits and '-', 1 to 64 characters, starting with a letter or digit")]
    Name(String),
    #[error("`version` must not be empty")]
    Version,
    #[error("`read.channels` entry {0:?} is not valid: use \"#name\" or \"*\"")]
    Channel(String),
    #[error("`egress` entry {0:?} is not valid: {1}")]
    Egress(String, &'static str),
    #[error("grants are wider than the manifest: {0}")]
    Widening(String),
    #[error("`schedules` is not valid: {0}")]
    Schedule(String),
}

impl Manifest {
    /// Parse and validate a manifest.
    pub fn from_json(bytes: &[u8]) -> Result<Manifest, ManifestError> {
        let manifest: Manifest =
            serde_json::from_slice(bytes).map_err(|e| ManifestError::Parse(e.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Check every field the types alone cannot.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if !valid_name(&self.name) {
            return Err(ManifestError::Name(self.name.clone()));
        }
        if self.version.trim().is_empty() {
            return Err(ManifestError::Version);
        }
        for selector in &self.read.channels {
            if selector != "*" && channel_name(selector).is_none() {
                return Err(ManifestError::Channel(selector.clone()));
            }
        }
        for host in &self.egress {
            validate_egress_host(host).map_err(|why| ManifestError::Egress(host.clone(), why))?;
        }
        self.validate_schedules()
    }

    fn validate_schedules(&self) -> Result<(), ManifestError> {
        let bad = |why: String| Err(ManifestError::Schedule(why));
        let declared = self.triggers.contains(&TriggerKind::Schedule);
        if declared && self.schedules.is_empty() {
            return bad("the `schedule` trigger needs at least one entry in `schedules`".into());
        }
        if !declared && !self.schedules.is_empty() {
            return bad("`schedules` needs the `schedule` trigger in `triggers`".into());
        }
        let mut names = BTreeSet::new();
        for s in &self.schedules {
            let ok_name = !s.name.is_empty()
                && s.name.len() <= 32
                && s.name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
            if !ok_name {
                return bad(format!("name {:?}: lowercase letters, digits and '-', 1 to 32 characters", s.name));
            }
            if !names.insert(&s.name) {
                return bad(format!("name {:?} appears twice", s.name));
            }
            if !(MIN_SCHEDULE_SECS..=MAX_SCHEDULE_SECS).contains(&s.every) {
                return bad(format!(
                    "{:?}: `every` is {MIN_SCHEDULE_SECS} to {MAX_SCHEDULE_SECS} seconds",
                    s.name
                ));
            }
            if s.offset >= s.every {
                return bad(format!("{:?}: `offset` must be less than `every`", s.name));
            }
        }
        Ok(())
    }

    /// Whether a channel with this name is requested by `read.channels`.
    pub fn requests_channel(&self, name: &str) -> bool {
        let Some(name) = channel_name(name) else {
            return false;
        };
        self.read.channels.iter().any(|selector| {
            selector == "*" || channel_name(selector).is_some_and(|wanted| wanted == name)
        })
    }
}

fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 || bytes[0] == b'-' {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// `"#ops"` or `"ops"` to `"ops"`, lowercased; `None` for an empty name or one
/// containing whitespace.
fn channel_name(raw: &str) -> Option<String> {
    let name = raw.strip_prefix('#').unwrap_or(raw);
    if name.is_empty() || name.chars().any(char::is_whitespace) || name.contains('*') {
        return None;
    }
    Some(name.to_lowercase())
}

/// An egress entry is a bare DNS host name: lowercase, at least two labels, no
/// scheme, port, path, wildcard or IP literal. `http-fetch` matches it exactly.
pub fn validate_egress_host(host: &str) -> Result<(), &'static str> {
    if host.is_empty() || host.len() > 253 {
        return Err("a host name is 1 to 253 characters");
    }
    if host.contains("://") || host.contains('/') {
        return Err("give the bare host name, without scheme or path");
    }
    if host.contains('*') {
        return Err("wildcards are not allowed; list each host exactly");
    }
    if host.contains(':') || host.starts_with('[') {
        return Err("ports and IP literals are not allowed");
    }
    if host != host.to_ascii_lowercase() {
        return Err("host names are lowercase");
    }
    let labels: Vec<&str> = host.split('.').collect();
    if labels.len() < 2 {
        return Err("a host name has at least two labels");
    }
    for label in &labels {
        let ok = !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-');
        if !ok {
            return Err("not a valid DNS host name");
        }
    }
    // URL parsers read any host whose last label is numeric (10.0.0.1,
    // 0x7f.0.0.1, a.1) as an IPv4 address, so such a name is never a DNS host.
    if labels
        .last()
        .is_some_and(|l| l.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err("ports and IP literals are not allowed");
    }
    Ok(())
}

/// A channel an admin granted, by id, with its name at grant time (the
/// manifest selects channels by name; the runtime enforces by id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelGrant {
    pub id: String,
    pub name: String,
}

/// The admin's narrowing of a manifest, signed into the App approval record
/// (#2005). The runtime takes grants from a verified approval only.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdminGrants {
    #[serde(default)]
    pub channels: Vec<ChannelGrant>,
    /// History window in days.
    #[serde(default)]
    pub history: u32,
    #[serde(default)]
    pub events: BTreeSet<EventKind>,
    #[serde(default)]
    pub actions: BTreeSet<ActionKind>,
    #[serde(default)]
    pub egress: BTreeSet<String>,
}

impl AdminGrants {
    /// Everything in these grants that the manifest did not request. The
    /// approval flow refuses to produce, and the runtime refuses to accept, a
    /// grant set for which this is non-empty.
    pub fn widenings(&self, manifest: &Manifest) -> Vec<String> {
        let mut out = Vec::new();
        for channel in &self.channels {
            if !manifest.requests_channel(&channel.name) {
                out.push(format!(
                    "channel #{} is not in `read.channels`",
                    channel.name
                ));
            }
        }
        if self.history > manifest.read.history {
            out.push(format!(
                "history of {} days exceeds `read.history` ({} days)",
                self.history, manifest.read.history
            ));
        }
        for event in self.events.difference(&manifest.read.events) {
            out.push(format!(
                "event `{}` is not in `read.events`",
                event.as_str()
            ));
        }
        for action in self.actions.difference(&manifest.actions) {
            out.push(format!("action `{}` is not in `actions`", action.as_str()));
        }
        for host in self.egress.difference(&manifest.egress) {
            out.push(format!("host `{host}` is not in `egress`"));
        }
        out
    }

    /// Like [`AdminGrants::widenings`], as a refusal.
    pub fn check_not_wider(&self, manifest: &Manifest) -> Result<(), ManifestError> {
        let widenings = self.widenings(manifest);
        if widenings.is_empty() {
            Ok(())
        } else {
            Err(ManifestError::Widening(widenings.join("; ")))
        }
    }
}

/// What the runtime enforces for one installation: the manifest intersected
/// with the admin's grants. Nothing here is wider than either side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveGrants {
    pub triggers: BTreeSet<TriggerKind>,
    /// Granted channel ids, to the name each was granted under.
    pub channels: BTreeMap<String, String>,
    pub history_days: u32,
    pub events: BTreeSet<EventKind>,
    pub actions: BTreeSet<ActionKind>,
    pub egress: BTreeSet<String>,
    pub state_quota: u64,
}

impl EffectiveGrants {
    /// Grants that allow nothing at all.
    pub fn none() -> EffectiveGrants {
        EffectiveGrants {
            triggers: BTreeSet::new(),
            channels: BTreeMap::new(),
            history_days: 0,
            events: BTreeSet::new(),
            actions: BTreeSet::new(),
            egress: BTreeSet::new(),
            state_quota: 0,
        }
    }
}

/// effective grants = manifest ∩ admin grants.
///
/// Triggers and the state quota are not narrowed by the admin in v1, so they
/// come from the manifest alone; every other dimension is an intersection.
/// Egress hosts are compared exactly (both sides are validated lowercase host
/// names).
pub fn effective_grants(manifest: &Manifest, admin: &AdminGrants) -> EffectiveGrants {
    let channels = admin
        .channels
        .iter()
        .filter(|c| manifest.requests_channel(&c.name))
        .map(|c| (c.id.clone(), c.name.clone()))
        .collect();
    EffectiveGrants {
        triggers: manifest.triggers.clone(),
        channels,
        history_days: manifest.read.history.min(admin.history),
        events: manifest
            .read
            .events
            .intersection(&admin.events)
            .copied()
            .collect(),
        actions: manifest
            .actions
            .intersection(&admin.actions)
            .copied()
            .collect(),
        egress: manifest
            .egress
            .intersection(&admin.egress)
            .cloned()
            .collect(),
        state_quota: manifest.state.quota,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> Manifest {
        Manifest::from_json(
            br##"{
                "name": "triage",
                "version": "1.0.0",
                "triggers": ["commands", "messages"],
                "read": { "channels": ["#support", "#Ops"], "history": 30, "events": ["created", "edited"] },
                "actions": ["reply", "react"],
                "state": { "quota": 4096 },
                "egress": ["api.linear.app"]
            }"##,
        )
        .unwrap()
    }

    fn admin() -> AdminGrants {
        AdminGrants {
            channels: vec![
                ChannelGrant {
                    id: "c1".into(),
                    name: "support".into(),
                },
                ChannelGrant {
                    id: "c2".into(),
                    name: "random".into(),
                },
            ],
            history: 90,
            events: BTreeSet::from([EventKind::Created]),
            actions: BTreeSet::from([ActionKind::Reply, ActionKind::Post]),
            egress: BTreeSet::from(["api.linear.app".to_string(), "evil.example".to_string()]),
        }
    }

    #[test]
    fn effective_grants_never_exceed_the_manifest() {
        let eff = effective_grants(&manifest(), &admin());
        assert_eq!(
            eff.channels,
            BTreeMap::from([("c1".to_string(), "support".to_string())])
        );
        assert_eq!(eff.history_days, 30);
        assert_eq!(eff.events, BTreeSet::from([EventKind::Created]));
        assert_eq!(eff.actions, BTreeSet::from([ActionKind::Reply]));
        assert_eq!(eff.egress, BTreeSet::from(["api.linear.app".to_string()]));
        assert_eq!(eff.state_quota, 4096);
    }

    #[test]
    fn effective_grants_never_exceed_the_admin_grants() {
        let mut narrow = admin();
        narrow.history = 7;
        narrow.egress.clear();
        narrow.actions = BTreeSet::from([ActionKind::React]);
        let eff = effective_grants(&manifest(), &narrow);
        assert_eq!(eff.history_days, 7);
        assert!(eff.egress.is_empty());
        assert_eq!(eff.actions, BTreeSet::from([ActionKind::React]));
    }

    #[test]
    fn wildcard_selector_takes_whatever_the_admin_grants() {
        let mut m = manifest();
        m.read.channels = vec!["*".into()];
        let eff = effective_grants(&m, &admin());
        assert_eq!(eff.channels.len(), 2);
    }

    #[test]
    fn widenings_name_the_manifest_field() {
        let w = admin().widenings(&manifest());
        assert!(
            w.contains(&"channel #random is not in `read.channels`".to_string()),
            "{w:?}"
        );
        assert!(w.contains(&"history of 90 days exceeds `read.history` (30 days)".to_string()));
        assert!(w.contains(&"action `post` is not in `actions`".to_string()));
        assert!(w.contains(&"host `evil.example` is not in `egress`".to_string()));
        assert!(admin().check_not_wider(&manifest()).is_err());
    }

    #[test]
    fn egress_hosts_must_be_exact_dns_names() {
        for bad in [
            "*.example.com",
            "https://api.x.com",
            "api.x.com/path",
            "api.x.com:8443",
            "10.0.0.1",
            "[::1]",
            "localhost",
            "API.X.COM",
            "a..b",
        ] {
            assert!(validate_egress_host(bad).is_err(), "{bad} accepted");
        }
        assert!(validate_egress_host("api.x.com").is_ok());
    }

    #[test]
    fn schedules_go_with_the_schedule_trigger_and_compute_the_same_slots_everywhere() {
        let ok = br#"{"name":"a","version":"1","triggers":["schedule"],"schedules":[{"name":"daily","every":86400,"offset":3600}]}"#;
        let m = Manifest::from_json(ok).unwrap();
        let s = &m.schedules[0];
        assert_eq!(s.slot_at_or_before(86_400 * 10 + 3_599), Some(86_400 * 9 + 3_600));
        assert_eq!(s.slot_at_or_before(86_400 * 10 + 3_600), Some(86_400 * 10 + 3_600));
        assert_eq!(s.next_after(86_400 * 10 + 3_600), 86_400 * 11 + 3_600);
        for bad in [
            &br#"{"name":"a","version":"1","triggers":["schedule"]}"#[..],
            br#"{"name":"a","version":"1","schedules":[{"name":"x","every":60}]}"#,
            br#"{"name":"a","version":"1","triggers":["schedule"],"schedules":[{"name":"x","every":59}]}"#,
            br#"{"name":"a","version":"1","triggers":["schedule"],"schedules":[{"name":"x","every":60,"offset":60}]}"#,
            br#"{"name":"a","version":"1","triggers":["schedule"],"schedules":[{"name":"X","every":60}]}"#,
            br#"{"name":"a","version":"1","triggers":["schedule"],"schedules":[{"name":"x","every":60},{"name":"x","every":120}]}"#,
        ] {
            assert!(matches!(Manifest::from_json(bad), Err(ManifestError::Schedule(_))), "{}", String::from_utf8_lossy(bad));
        }
    }

    #[test]
    fn unknown_fields_are_refused() {
        let err =
            Manifest::from_json(br#"{"name":"a","version":"1","network":"any"}"#).unwrap_err();
        assert!(matches!(err, ManifestError::Parse(_)));
    }
}
