//! Chat action timings (#2141): how long a chat command ran on a bridge worker and how many
//! DS requests it waited on, as one `[perf]` diagnostics line per command.
//!
//! Off unless the host turns it on (the mobile bridge's `perf_marks` init flag, set only by a
//! build made with `EXPO_PUBLIC_PERF_MARKS=1`). The line names the command, never an id or
//! any content. Requests a command hands to a background task are not its own and are not
//! counted: the count is the command's critical path.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

static ENABLED: AtomicBool = AtomicBool::new(false);

/// The commands a `[perf]` line is written for.
const CHAT_COMMANDS: &[&str] = &[
    "send_message",
    "retry_message",
    "edit_message",
    "delete_message",
    "discard_unsent_message",
    "pin_message",
    "unpin_message",
    "list_pinned_messages",
    "add_reaction",
    "remove_reaction",
    "get_reactions_for_messages",
];

tokio::task_local! {
    static DS_REQUESTS: Arc<AtomicU32>;
}

/// Turn the `[perf]` lines on for this process.
pub fn enable() {
    ENABLED.store(true, Ordering::Relaxed);
}

/// One DS request made by the current command, if it is being measured.
pub(crate) fn note_ds_request() {
    let _ = DS_REQUESTS.try_with(|n| n.fetch_add(1, Ordering::Relaxed));
}

/// Run command `name`, writing its `[perf]` line when measuring is on.
pub async fn measured<F: Future>(name: &str, command: F) -> F::Output {
    if !ENABLED.load(Ordering::Relaxed) || !CHAT_COMMANDS.contains(&name) {
        return command.await;
    }
    let requests = Arc::new(AtomicU32::new(0));
    let started = std::time::Instant::now();
    let out = DS_REQUESTS.scope(Arc::clone(&requests), command).await;
    crate::diag!(
        "[perf] {name}: ran {}ms, {} DS requests",
        started.elapsed().as_millis(),
        requests.load(Ordering::Relaxed)
    );
    out
}

/// Run `command`, returning what it returned and the DS requests it waited on (#2141): those
/// made in its own task, not by work it handed to the background. The flows suite asserts
/// these per chat command.
pub async fn counted<F: Future>(command: F) -> (F::Output, u32) {
    let requests = Arc::new(AtomicU32::new(0));
    let out = DS_REQUESTS.scope(Arc::clone(&requests), command).await;
    (out, requests.load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn counts_only_the_measured_command() {
        enable();
        let requests = Arc::new(AtomicU32::new(0));
        DS_REQUESTS
            .scope(Arc::clone(&requests), async {
                note_ds_request();
                note_ds_request();
                // A spawned task is outside the scope: not the command's own.
                tokio::spawn(async { note_ds_request() }).await.unwrap();
            })
            .await;
        assert_eq!(requests.load(Ordering::Relaxed), 2);
        // Outside any scope nothing is counted, and nothing panics.
        note_ds_request();
    }
}
