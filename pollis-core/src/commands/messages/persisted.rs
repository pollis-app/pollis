//! Which conversations' local rows changed, for a host with no event channel (#1610).
//!
//! The ingest notes a conversation the moment it writes rows there — mid-replay, before the
//! commit fetch, the backstops, the watermark and the receipts that still follow. Mobile parks
//! one `wait_persisted` bridge call on [`wait`] and re-reads those conversations from the local
//! store when it returns, so a message shows as soon as it is stored, whichever path stored it:
//! the conversation's own ingest, a catch-up sweep, or Android's in-process push fetch.
//!
//! Process-wide, like the store it describes. Notes with no waiter just accumulate (a set of
//! conversation ids, so bounded by the account's conversations) until the next wait drains them.

use std::collections::BTreeSet;
use std::sync::{LazyLock, Mutex};

use tokio::sync::Notify;

struct Feed {
    state: Mutex<FeedState>,
    notify: Notify,
}

#[derive(Default)]
struct FeedState {
    /// Bumped on every note; a waiter returns once it passes the `after` it was given.
    seq: u64,
    pending: BTreeSet<String>,
}

static FEED: LazyLock<Feed> = LazyLock::new(|| Feed {
    state: Mutex::new(FeedState::default()),
    notify: Notify::new(),
});

fn lock() -> std::sync::MutexGuard<'static, FeedState> {
    // Plain data; a panic elsewhere cannot leave it inconsistent.
    FEED.state.lock().unwrap_or_else(|p| p.into_inner())
}

/// Record that rows of these conversations were written. Never blocks on a waiter.
pub fn note<I, S>(conversation_ids: I)
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let mut any = false;
    {
        let mut st = lock();
        for id in conversation_ids {
            st.pending.insert(id.into());
            any = true;
        }
        if any {
            st.seq += 1;
        }
    }
    if any {
        FEED.notify.notify_waiters();
    }
}

/// What a wait returns: the position to wait after next, and the conversations noted since.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Persisted {
    pub seq: u64,
    pub conversation_ids: Vec<String>,
}

/// The current position, without waiting or draining.
pub fn position() -> u64 {
    lock().seq
}

/// Wait until something is noted after position `after`, then drain and return it.
/// Returns at once if that already happened. Meant for one waiter at a time.
pub async fn wait(after: u64) -> Persisted {
    loop {
        // Registered before the check, so a note between the check and the await still wakes us.
        let notified = FEED.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        {
            let mut st = lock();
            if st.seq > after {
                return Persisted {
                    seq: st.seq,
                    conversation_ids: std::mem::take(&mut st.pending).into_iter().collect(),
                };
            }
        }
        notified.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    // One process-wide feed, so the cases run in one test, in order.
    #[tokio::test]
    async fn a_waiter_wakes_on_a_note_and_drains_it() {
        let start = position();
        note(["c-old"]);
        // Already past `start`: returns at once with what is pending.
        let got = wait(start).await;
        assert!(got.seq > start);
        assert!(got.conversation_ids.contains(&"c-old".to_string()));

        // Nothing new: a wait parks until the next note.
        let after = got.seq;
        let waiter = tokio::spawn(async move { wait(after).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished(), "returned with nothing noted");
        note(["c-1", "c-2", "c-1"]);
        let got = tokio::time::timeout(Duration::from_secs(5), waiter)
            .await
            .expect("a note must wake the waiter")
            .expect("join");
        // Other tests in this binary may note too, so only what we noted is asserted.
        let ids = got.conversation_ids;
        assert!(ids.contains(&"c-1".to_string()) && ids.contains(&"c-2".to_string()));
        assert_eq!(ids.iter().filter(|i| *i == "c-1").count(), 1, "deduplicated");
    }
}
