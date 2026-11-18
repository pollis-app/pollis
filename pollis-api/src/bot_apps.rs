//! App approvals (#2005): the signed records that say which App an
//! installation may run, with which grants, and what the runtime reports back.
//!
//! The DS stores an approval as an opaque, signed record keyed
//! `(installation_id, seq)`; it holds no admin key, so it cannot forge one, and
//! the runtime verifies every record from its own state before it loads an App.
//! The manifest travels beside the record as text; the record pins its hash.
//!
//! Wire types only; see `pollis-delivery/src/bot_apps.rs`.

use serde::{Deserialize, Serialize};

/// Which record an approval row holds. A production approval pins one App
/// hash; a development approval names a developer signing key instead. They
/// are separate record types with separate signature domains, so one can never
/// be read as the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppApprovalKind {
    Production,
    Development,
}

impl AppApprovalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            AppApprovalKind::Production => "production",
            AppApprovalKind::Development => "development",
        }
    }
}

/// The longest signed record the DS stores, base64 characters.
pub const APP_APPROVAL_MAX_RECORD_LEN: usize = 32 * 1024;
/// The longest manifest the DS stores, bytes of JSON text.
pub const APP_MANIFEST_MAX_LEN: usize = 64 * 1024;

/// `POST /v1/bots/app/approve` — device-signed by a current admin of the bot's
/// group: store the next approval for the installation. `seq` must be exactly
/// one more than the latest stored; otherwise the DS answers `stale` with the
/// latest, and the admin's app reloads and asks again.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApproveBotAppBody {
    pub bot_user_id: String,
    pub seq: i64,
    pub kind: AppApprovalKind,
    /// base64 (STANDARD) of the signed record, opaque to the DS.
    pub record: String,
    /// The manifest the record's `manifest_hash` names, as JSON text.
    pub manifest: String,
    /// Self-scope: when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ApproveBotAppResponse {
    Approved,
    /// Another approval landed first; nothing stored. `latest` is the seq now stored.
    Stale { latest: i64 },
}

/// `POST /v1/read/bot-apps` — device-signed by a member of `group_id` (a
/// person, or one of the group's bots' hosts): the latest approval of each
/// bot in the group, or of `bot_user_id` only. Admins also get each
/// installation's latest runtime report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotAppsReadBody {
    pub group_id: String,
    #[serde(default)]
    pub bot_user_id: Option<String>,
    /// Self-scope: when signed it must equal the authenticated user.
    #[serde(default)]
    pub user_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotAppsReadResponse {
    pub apps: Vec<BotAppWire>,
}

/// One installation: its latest approval and, for admins, its latest report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotAppWire {
    pub bot_user_id: String,
    #[serde(default)]
    pub approval: Option<AppApprovalWire>,
    #[serde(default)]
    pub report: Option<BotAppReportWire>,
}

/// A stored approval, as the DS holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppApprovalWire {
    pub seq: i64,
    pub kind: AppApprovalKind,
    /// base64 (STANDARD) of the signed record.
    pub record: String,
    /// JSON text.
    pub manifest: String,
    /// Who the DS saw store it (the record names its own signer; verifiers use that).
    pub approver_user_id: String,
    pub created_at: String,
}

/// What a bot's runtime last said about its App.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BotAppRunState {
    /// The approval verified and the artifact matched it.
    Running,
    /// The artifact the runtime has is not approved yet (a first install or an update).
    AwaitingApproval,
    /// The approval did not verify; the installation does not run.
    Refused,
}

/// `POST /v1/bots/app/report` — device-signed by one of the bot's hosts: what
/// the runtime found on its last check. Shown to the group's admins on the
/// bot's page. Untrusted display text: an admin's app re-derives what it can
/// (the manifest hash) and shows the rest as reported.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BotAppReportBody {
    pub installation_id: String,
    pub state: BotAppRunState,
    /// A short machine code (`hash_mismatch`, `bad_signature`, ...), empty when running.
    #[serde(default)]
    pub reason: String,
    /// One line for people.
    #[serde(default)]
    pub detail: String,
    /// The artifact the runtime has: its App hash, lowercase hex.
    #[serde(default)]
    pub app_hash: Option<String>,
    /// The artifact's manifest, JSON text.
    #[serde(default)]
    pub manifest: Option<String>,
    /// Where the runtime loads the artifact from, as given at setup.
    #[serde(default)]
    pub source: Option<String>,
    /// For a development build: the developer key that signed it, base64 (STANDARD).
    #[serde(default)]
    pub developer_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BotAppReportWire {
    pub state: BotAppRunState,
    pub reason: String,
    pub detail: String,
    #[serde(default)]
    pub app_hash: Option<String>,
    #[serde(default)]
    pub manifest: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub developer_key: Option<String>,
    pub device_id: String,
    pub updated_at: String,
}
