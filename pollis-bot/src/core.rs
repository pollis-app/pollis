//! The narrow surface the job loop needs from `pollis-core`, per installation.
//!
//! The loop, lock, claim and grant logic are written against this trait so they
//! are tested with a fake; the real implementation is a thin adapter over one
//! installation's `AppState` (its own identity, keystore slot and encrypted
//! store). Nothing is shared between two installations' cores.

use async_trait::async_trait;

use crate::action::ChannelInfo;
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
}
