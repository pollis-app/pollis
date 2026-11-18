//! The per-host surface the subcommands need from `pollis-core`: pairing,
//! describing an installation to the job loop (whose store it unseals per job),
//! and a separate wake source that only says when to run a job.
//!
//! The subcommands are written against [`BotHost`] so their flow (states
//! printed, limits enforced, records saved) is tested with a fake; `CoreHost`
//! in `core_host.rs` is the real one.

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::core::InstallationCore;
use crate::installations::InstallationRecord;
use crate::secrets::SecretSlot;

/// What a pairing is for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairTarget {
    /// The first host of a new bot (`pollis-bot pair`).
    NewBot,
    /// Another host of an existing bot (`pollis-bot add-host --bot <id>`).
    ExistingBot { bot_user_id: String },
}

/// An open pairing request, as shown to the person at the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingTicket {
    /// The eight-character code the admin types.
    pub code: String,
    /// Unix seconds.
    pub expires_at: i64,
}

/// Where a pairing is, for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingProgress {
    /// The request is filed; the code is ready to show.
    WaitingForApproval(PairingTicket),
    /// Approved; the host is joining the bot's group.
    JoiningGroup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairOutcome {
    /// The installation exists on this host; its record is what to save.
    Paired(InstallationRecord),
    /// Nobody approved the request before it expired.
    Expired,
}

/// Called when an installation should run a catch-up job.
pub type WakeFn = Arc<dyn Fn() + Send + Sync>;

/// What a wake source reports. It never runs a job itself.
#[derive(Clone)]
pub struct Wakes {
    /// Something may have arrived for the group.
    pub wake: WakeFn,
    /// An admin approved another host for this bot.
    pub approval: WakeFn,
}

/// An installation opened for `run`.
pub struct OpenedInstallation {
    pub core: Arc<dyn InstallationCore>,
    pub secrets: Arc<dyn SecretSlot>,
}

#[async_trait]
pub trait BotHost: Send + Sync {
    /// File a pairing request, report progress, and wait (event-driven, bounded
    /// by the request's TTL) for an admin to approve it. The installation's
    /// store lives in `installation_dir(<id>)`.
    async fn pair(
        &self,
        data_dir: &Path,
        target: PairTarget,
        device_name: Option<String>,
        progress: &(dyn Fn(PairingProgress) + Send + Sync),
    ) -> Result<PairOutcome>;

    /// The job loop's handle on one installation. Its store stays sealed until
    /// a job opens it; this only checks the installation can be opened.
    async fn open(&self, record: &InstallationRecord, dir: &Path) -> Result<OpenedInstallation>;

    /// Start the installation's wake source (realtime rooms), reporting to
    /// `wakes` for as long as the process runs. A host without one still works:
    /// it catches up whenever a job runs.
    async fn watch(&self, record: &InstallationRecord, dir: &Path, wakes: Wakes) -> Result<()>;

    /// Remove the installation's store and keystore slots (its webhook secret
    /// included). The caller removes its record afterwards.
    async fn forget(&self, record: &InstallationRecord, dir: &Path) -> Result<()>;

    /// The installation's secret slot alone, for `post` and `rotate-secret`.
    async fn secrets(&self, record: &InstallationRecord, dir: &Path) -> Result<Arc<dyn SecretSlot>>;
}
