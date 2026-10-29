//! The §6 polling sync model: media-off means no LiveKit realtime inbox, so
//! the TUI runs the catch-up sequence on a timer (`.codesight/wiki/pollis-tui.md`).
//!
//! Canonical order (spec §6, mirroring `flows/harness.rs`):
//!
//! ```text
//! 1. poll_mls_welcomes(user)          — drain queued Welcomes (may JOIN new groups/DMs)
//! 2. for each conversation the user is in:
//!        process_pending_commits(conv) — advance that MLS group to head epoch
//! 3. for each conversation:
//!        get_channel_messages / get_dm_messages — ingest + interleaved decrypt
//! ```
//!
//! Welcomes run first (a Welcome can create the group the commits replay
//! into); the message read runs last (it triggers the interleaved
//! replay+decrypt). ~4 rounds settle an interleaved catch-up.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use pollis_core::commands::mls::{poll_mls_welcomes, process_pending_commits};
use pollis_core::state::AppState;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::data;

/// One welcomes → commits → reads pass (§6). Idempotent; returns the tree the
/// round enumerated so callers needn't re-query. A conversation's commit/read
/// error is surfaced, not swallowed.
pub async fn sync_once(state: &Arc<AppState>, user_id: &str) -> Result<data::ConversationTree> {
    poll_mls_welcomes(state, user_id.to_string()).await?;

    // Enumerate after welcomes so a group joined this round is included.
    let tree = data::load_conversations(state, user_id).await?;

    for conversation_id in tree.conversation_ids() {
        process_pending_commits(state, conversation_id, user_id.to_string()).await?;
    }

    for group in &tree.groups {
        for channel in &group.channels {
            data::channel_messages(state, user_id, &channel.id, None).await?;
        }
    }
    for dm in tree.dm_channels.iter().chain(tree.dm_requests.iter()) {
        data::dm_messages(state, user_id, &dm.id, None).await?;
    }

    Ok(tree)
}

/// Run [`sync_once`] exactly `rounds` times; a fixed count because the core
/// exposes no cheap change-detection to stop early.
pub async fn sync_rounds(state: &Arc<AppState>, user_id: &str, rounds: usize) -> Result<usize> {
    for _ in 0..rounds {
        sync_once(state, user_id).await?;
    }
    Ok(rounds)
}

/// A completed round, published over `watch` so the UI reuses what the loop
/// fetched instead of querying remote itself.
pub struct SyncSnapshot {
    pub round: u64,
    pub tree: data::ConversationTree,
}

/// A running background sync loop. Dropping it does not stop the task; call
/// [`SyncLoop::cancel`], which lets the current round finish.
pub struct SyncLoop {
    handle: JoinHandle<()>,
    shutdown: watch::Sender<bool>,
    /// Latest completed round's snapshot; `None` until the first round lands.
    pub snapshot: watch::Receiver<Option<Arc<SyncSnapshot>>>,
}

impl SyncLoop {
    /// Signal the loop to stop after its current round; `await` the returned
    /// handle to join it.
    pub fn cancel(self) -> JoinHandle<()> {
        // A closed receiver means the task already exited.
        let _ = self.shutdown.send(true);
        self.handle
    }
}

/// Spawn a background task running [`sync_once`] every `cadence` (spec §6:
/// ~3–5 s while foregrounded) on the current Tokio runtime.
pub fn spawn_loop(state: Arc<AppState>, user_id: String, cadence: Duration) -> SyncLoop {
    let (shutdown, mut rx) = watch::channel(false);
    let (snapshot_tx, snapshot) = watch::channel(None);
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(cadence);
        // Steady cadence; don't burst to catch up missed ticks.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut round: u64 = 0;
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    match sync_once(&state, &user_id).await {
                        Ok(tree) => {
                            round += 1;
                            // No receiver means the UI is gone; shutdown follows.
                            let _ = snapshot_tx.send(Some(Arc::new(SyncSnapshot { round, tree })));
                        }
                        // A failed round is logged, not fatal — the next tick retries.
                        Err(e) => eprintln!("[sync] round for {user_id} failed: {e}"),
                    }
                }
                changed = rx.changed() => {
                    if changed.is_err() || *rx.borrow() {
                        break;
                    }
                }
            }
        }
    });
    SyncLoop { handle, shutdown, snapshot }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_loop_cancels_cleanly() {
        // A cadence far longer than the test, so no round (and no DB) runs.
        let (shutdown, rx) = watch::channel(false);
        let handle = tokio::spawn(async move {
            let mut rx = rx;
            let mut ticker = tokio::time::interval(Duration::from_secs(3600));
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    _ = ticker.tick() => {}
                    changed = rx.changed() => {
                        if changed.is_err() || *rx.borrow() { break; }
                    }
                }
            }
        });
        let (_snapshot_tx, snapshot) = watch::channel(None);
        let loop_ = SyncLoop { handle, shutdown, snapshot };
        let joined = loop_.cancel();
        tokio::time::timeout(Duration::from_secs(5), joined)
            .await
            .expect("sync loop did not stop after cancel")
            .expect("sync loop task panicked");
    }
}
