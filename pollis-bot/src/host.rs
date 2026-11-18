//! The per-host surface the subcommands need from `pollis-core`: pairing, and
//! opening an installation (its own state, keystore slot and store) with its
//! realtime wake-ups wired to the job loop.
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

    /// Open one installation unattended (no PIN) from its keystore slot, and
    /// wire its realtime wake-ups to `wake`.
    async fn open(&self, record: &InstallationRecord, dir: &Path, wake: WakeFn) -> Result<OpenedInstallation>;

    /// The installation's secret slot alone, for `post` and `rotate-secret`.
    async fn secrets(&self, record: &InstallationRecord, dir: &Path) -> Result<Arc<dyn SecretSlot>>;
}
