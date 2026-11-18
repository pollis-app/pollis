//! The narrow surface the job loop needs from `pollis-core`, per installation.
//!
//! The loop, lock, claim and grant logic are written against this trait so they
//! are tested with a fake; the real implementation is a thin adapter over one
//! installation's `AppState` (its own identity, keystore slot and encrypted
//! store). Nothing is shared between two installations' cores.
//!
//! The store is open only during a job: [`InstallationCore::open`] unseals it
//! at the start (step 3) and [`InstallationCore::close`] seals it again at the
//! end, whatever happened in between. Every other method is called only while
//! it is open. Nothing assumes a host stays up between jobs.

use async_trait::async_trait;

use crate::action::{ChannelInfo, CheckedAction};
use crate::trigger::{IncomingMessage, TriggerKey};

/// A failed call into the core, with the step it failed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreError(pub String);

impl std::fmt::Display for CoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<anyhow::Error> for CoreError {
    fn from(e: anyhow::Error) -> Self {
        CoreError(format!("{e:#}"))
    }
}

#[async_trait]
pub trait InstallationCore: Send + Sync {
    /// The installation id the DS claims are keyed by (the bot's user id while
    /// a bot has one installation).
    fn installation_id(&self) -> &str;

    /// The bot's own user id. Its own messages are never triggers.
    fn bot_user_id(&self) -> &str;

    /// Job loop step 3, first half: unseal the installation's store and keys.
    async fn open(&self) -> Result<(), CoreError>;

    /// Seal the store again (end of every job, success or not).
    async fn close(&self);

    /// Verify and act on approved add-host requests for this bot. With
    /// `wrap_now`, a host about to go back to sleep wraps at once instead of
    /// deferring to a peer that ranks first (a duplicate wrap is refused by the
    /// DS, so this costs at most one redundant request). Returns the earliest
    /// `retry_at` of any request it deferred.
    async fn host_approvals(&self, wrap_now: bool) -> Result<Option<i64>, CoreError>;

    /// Job loop step 3: process Welcomes and pending commits through the
    /// normal ingest path, then return the decrypted messages of the bot's
    /// group newer than the stored cursor, oldest first.
    async fn catch_up(&self) -> Result<Vec<IncomingMessage>, CoreError>;

    /// Advance the stored cursor past `message`; it will not be returned by
    /// [`InstallationCore::catch_up`] again, even after a restart.
    async fn mark_handled(&self, message: &IncomingMessage) -> Result<(), CoreError>;

    /// The bot's group's channels, for the grant check.
    async fn channels(&self) -> Result<Vec<ChannelInfo>, CoreError>;

    /// Job loop step 5: claim `(installation_id, key)` on the DS. `true` when
    /// this host won (a retry by the winner answers `true` again).
    async fn claim(&self, key: &TriggerKey) -> Result<bool, CoreError>;

    /// Post through the normal (durable) send path under `message_id`, so a
    /// retried job never posts twice.
    async fn post(&self, channel_id: &str, text: &str, message_id: &str) -> Result<(), CoreError>;

    /// Apply one checked action. The default handles `post` only; the real
    /// core applies every kind (#2004).
    async fn act(&self, action: &CheckedAction, message_id: &str) -> Result<(), CoreError> {
        match action {
            CheckedAction::Post { channel_id, text } => self.post(channel_id, text, message_id).await,
            other => Err(CoreError(format!("this core cannot apply {other:?}"))),
        }
    }

    /// #2004: one App state key as the DS holds it (version 0 when absent),
    /// and the installation's usage in plaintext bytes.
    async fn state_read(&self, _key: &str) -> Result<(StateValue, u64), CoreError> {
        Err(CoreError("this installation has no App state".into()))
    }

    /// #2004: write a job's App state under the claim this host won on `key`.
    async fn state_commit(&self, _key: &TriggerKey, _writes: &[StateWrite]) -> Result<StateCommit, CoreError> {
        Err(CoreError("this installation has no App state".into()))
    }

    /// #2004: the bot's group's members, for an App's message data (author
    /// name, is-bot) and for telling a mention of the bot. Empty by default.
    async fn members(&self) -> Result<Vec<Member>, CoreError> {
        Ok(Vec::new())
    }

    /// #2004: the channel and author of a message this host has seen.
    async fn message_meta(&self, _message_id: &str) -> Result<Option<(String, String)>, CoreError> {
        Ok(None)
    }
}

/// A member of the bot's group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub user_id: String,
    pub username: Option<String>,
    pub is_bot: bool,
}

/// One App state key as read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateValue {
    pub version: i64,
    pub value: Option<Vec<u8>>,
}

/// One write of a job's App state, at the version the job read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateWrite {
    pub key: String,
    pub expected_version: i64,
    pub value: Option<Vec<u8>>,
}

/// How a state commit went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateCommit {
    Committed,
    /// A retried job: this claim's state was written before.
    AlreadyCommitted,
    /// Another host moved a key since this job read it: re-run the job.
    Conflict,
    /// The DS's per-installation ceiling.
    OverQuota { max_bytes: u64 },
}
