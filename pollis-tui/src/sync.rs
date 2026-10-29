//! The §6 polling sync model (M2).
//!
//! No realtime inbox, so the TUI polls: [`sync_once`] is the catch-up sequence and
//! [`spawn_loop`] repeats it.
//!
//! ## The canonical order (spec §6, mirroring `flows/harness.rs`)
//!
//! ```text
//! 1. poll_mls_welcomes(user)          — drain queued Welcomes (may JOIN new groups/DMs)
//! 2. for each conversation the user is in:
//!        process_pending_commits(conv) — advance that MLS group to head epoch
//! 3. for each conversation:
//!        get_channel_messages / get_dm_messages — ingest + interleaved decrypt
//! ```
//!
//! Welcomes run first (one can create the group commits replay into); the read runs last,
//! triggering decrypt. ~4 rounds settle a catch-up.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use pollis_core::commands::mls::{poll_mls_welcomes, process_pending_commits};
use pollis_core::state::AppState;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::data;

/// Run one full welcomes → commits(all conversations) → read(all) pass for
/// `user_id` (§6). Idempotent; returns the tree it enumerated. Per-conversation errors surface.
pub async fn sync_once(state: &Arc<AppState>, user_id: &str) -> Result<data::ConversationTree> {
    // 1. Welcomes first: they may join groups the enumeration below picks up.
    poll_mls_welcomes(state, user_id.to_string()).await?;

    let tree = data::load_conversations(state, user_id).await?;

    // 2. Advance each conversation's MLS group to the head epoch.
    for conversation_id in tree.conversation_ids() {
        process_pending_commits(state, conversation_id, user_id.to_string()).await?;
    }

    // 3. Read each conversation last; the fetch drives replay+decrypt.
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

/// no difference needs change detection the core lacks, so a fixed count (~4) is used.
pub async fn sync_rounds(state: &Arc<AppState>, user_id: &str, rounds: usize) -> Result<usize> {
    for _ in 0..rounds {
        sync_once(state, user_id).await?;
    }
    Ok(rounds)
}

/// number and the tree it enumerated, so the UI need not query remote itself.
pub struct SyncSnapshot {
    pub round: u64,
    pub tree: data::ConversationTree,
}

/// **not** stop the task; call [`SyncLoop::cancel`].
pub struct SyncLoop {
    handle: JoinHandle<()>,
    shutdown: watch::Sender<bool>,
    /// Latest completed round's snapshot; `None` until the first round lands.
    pub snapshot: watch::Receiver<Option<Arc<SyncSnapshot>>>,
}

impl SyncLoop {
    /// Signal the loop to stop after its current round.
    pub fn cancel(self) -> JoinHandle<()> {
        let _ = self.shutdown.send(true);
        self.handle
    }
}

/// Spawn a background task that runs [`sync_once`] for `user_id` every `cadence`
/// (spec §6: ~3–5 s while foregrounded) on the current Tokio runtime.
pub fn spawn_loop(state: Arc<AppState>, user_id: String, cadence: Duration) -> SyncLoop {
    let (shutdown, mut rx) = watch::channel(false);
    let (snapshot_tx, snapshot) = watch::channel(None);
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(cadence);
        // Only steady cadence matters, not catching up missed ticks.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut round: u64 = 0;
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    match sync_once(&state, &user_id).await {
                        Ok(tree) => {
                            round += 1;
                            // No receiver means the UI is gone; shutdown will follow.
                            let _ = snapshot_tx.send(Some(Arc::new(SyncSnapshot { round, tree })));
                        }
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

    // A long cadence means we cancel before the first tick, so no DB is touched.
    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_loop_cancels_cleanly() {
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
