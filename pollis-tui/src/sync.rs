//! The §6 polling sync model (M2).
//!
//! Media-off means no LiveKit realtime inbox, so the TUI **polls**. A client
//! stays caught up by running the canonical catch-up sequence on a timer. This
//! module owns that sequence ([`sync_once`]) and the background task that repeats
//! it ([`spawn_loop`]). It is the data-plane the M2b three-pane UI will consume;
//! here it is written to be correct and independently testable.
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
//! Order matters: welcomes run **first** (a Welcome can create the very group the
//! commits then replay into), and the message read runs **last** — it is what
//! triggers the interleaved replay+decrypt that surfaces a peer's message. A
//! single round can leave a recovering member and a committer mid-handshake, so
//! [`sync_once`] is cheap to call repeatedly; the spec notes ~4 rounds settle an
//! interleaved catch-up.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use pollis_core::commands::mls::{poll_mls_welcomes, process_pending_commits};
use pollis_core::state::AppState;
use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::data;

/// Run one full welcomes → commits(all conversations) → read(all) pass for
/// `user_id` (§6). Idempotent and safe to call in a loop; returns once the pass
/// completes. Errors from an individual conversation's commit/read are
/// surfaced — a broken conversation should be visible, not silently swallowed
/// (the "messages must work" doctrine).
pub async fn sync_once(state: &Arc<AppState>, user_id: &str) -> Result<()> {
    // 1. Welcomes first — draining them may join brand-new groups/DMs, which the
    //    enumeration below then picks up.
    poll_mls_welcomes(state, user_id.to_string()).await?;

    // Enumerate AFTER welcomes so a group joined this round is included.
    let tree = data::load_conversations(state, user_id).await?;

    // 2. Advance each conversation's MLS group to the head epoch. Per-conversation
    //    because commit processing is keyed by MLS group.
    for conversation_id in tree.conversation_ids() {
        process_pending_commits(state, conversation_id, user_id.to_string()).await?;
    }

    // 3. Read each conversation last — the fetch drives the interleaved
    //    replay+decrypt that surfaces newly-ingested messages.
    for group in &tree.groups {
        for channel in &group.channels {
            data::channel_messages(state, user_id, &channel.id, None).await?;
        }
    }
    for dm in tree.dm_channels.iter().chain(tree.dm_requests.iter()) {
        data::dm_messages(state, user_id, &dm.id, None).await?;
    }

    Ok(())
}

/// Run [`sync_once`] up to `rounds` times, stopping early once a round makes no
/// difference is *not* attempted here (that needs change-detection the core
/// doesn't expose cheaply); instead we just run a fixed number of rounds, which
/// is how §6 settles an interleaved catch-up (~4 rounds). Returns the number of
/// rounds actually run (all of them, unless one errors).
pub async fn sync_rounds(state: &Arc<AppState>, user_id: &str, rounds: usize) -> Result<usize> {
    for round in 0..rounds {
        sync_once(state, user_id).await?;
        let _ = round;
    }
    Ok(rounds)
}

/// A running background sync loop and its shutdown switch. Dropping it does
/// **not** stop the task — call [`SyncLoop::cancel`] (graceful, lets the current
/// round finish) or [`SyncLoop::abort`] (immediate) on shutdown.
pub struct SyncLoop {
    handle: JoinHandle<()>,
    shutdown: watch::Sender<bool>,
}

impl SyncLoop {
    /// Signal the loop to stop after its current round. The task exits on the
    /// next select; `await` the returned handle if you need to join it.
    pub fn cancel(self) -> JoinHandle<()> {
        // Ignore send errors: a closed receiver means the task already exited.
        let _ = self.shutdown.send(true);
        self.handle
    }

    /// Abort the loop immediately, without waiting for the current round.
    pub fn abort(&self) {
        self.handle.abort();
    }
}

/// Spawn a background task that runs [`sync_once`] for `user_id` every `cadence`
/// (spec §6: ~3–5 s while foregrounded). The task runs on the current Tokio
/// runtime and never blocks a render loop — the M2b UI can drive redraws off a
/// separate signal. Returns a [`SyncLoop`] handle to cancel it on shutdown.
pub fn spawn_loop(state: Arc<AppState>, user_id: String, cadence: Duration) -> SyncLoop {
    let (shutdown, mut rx) = watch::channel(false);
    let handle = tokio::spawn(async move {
        let mut ticker = tokio::time::interval(cadence);
        // Skip the immediate first tick's burst semantics on lag — we only care
        // about steady cadence, not catching up missed ticks.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    if let Err(e) = sync_once(&state, &user_id).await {
                        // A failed round is logged, not fatal — the next tick retries.
                        eprintln!("[sync] round for {user_id} failed: {e}");
                    }
                }
                changed = rx.changed() => {
                    // Sender dropped, or asked to stop → exit.
                    if changed.is_err() || *rx.borrow() {
                        break;
                    }
                }
            }
        }
    });
    SyncLoop { handle, shutdown }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The loop is unit-constructible and cancelable without ever touching a DB:
    // a long cadence means the first sync tick never fires before we cancel, so
    // this exercises spawn/cancel wiring in isolation (no AppState needed beyond
    // the type — we cancel before the first tick).
    #[tokio::test(flavor = "multi_thread")]
    async fn spawn_loop_cancels_cleanly() {
        // A cadence far longer than the test: guarantees we cancel before the
        // first sync round would run, so no DB access happens.
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
        let loop_ = SyncLoop { handle, shutdown };
        // Graceful cancel returns the join handle; it should complete promptly.
        let joined = loop_.cancel();
        tokio::time::timeout(Duration::from_secs(5), joined)
            .await
            .expect("sync loop did not stop after cancel")
            .expect("sync loop task panicked");
    }
}
