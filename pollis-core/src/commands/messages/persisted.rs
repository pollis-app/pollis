//! Which conversations' local rows changed, for a host with no event channel (#1610).
//!
//! The ingest notes a conversation the moment it writes rows there — mid-replay, before the
//! commit fetch, the backstops, the watermark and the receipts that still follow. Mobile parks
//! one `wait_persisted` bridge call on [`wait`] and re-reads those conversations from the local
//! store when it returns, so a message shows as soon as it is stored, whichever path stored it:
//! the conversation's own ingest, a catch-up sweep, or Android's in-process push fetch.
//!
//! Nothing is drained: each conversation keeps the position of its latest note, and every waiter
//! brings its own cursor (`after`) and gets the conversations noted past it. So two waiters (an
//! old feed loop still parked across a sign-out and the new one) each see every note, and neither
//! can swallow the other's. Process-wide, like the store it describes, and bounded by the
//! account's conversations; past [`MAX_TRACKED`] the map is reset and a waiter whose cursor
//! predates that is told to re-read everything.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use tokio::sync::Notify;

/// Conversations remembered before the map is reset.
const MAX_TRACKED: usize = 4096;

struct Feed {
    state: Mutex<FeedState>,
    notify: Notify,
}

#[derive(Default)]
struct FeedState {
    /// Bumped on every note.
    seq: u64,
    /// Each noted conversation and the position of its latest note.
    noted: HashMap<String, u64>,
    /// Notes at or before this position were forgotten by a reset.
    floor: u64,
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
        let at = st.seq + 1;
        for id in conversation_ids {
            if st.noted.len() >= MAX_TRACKED {
                st.noted.clear();
                st.floor = st.seq;
            }
            st.noted.insert(id.into(), at);
            any = true;
        }
        if any {
            st.seq = at;
        }
    }
    if any {
        FEED.notify.notify_waiters();
    }
}

/// What a wait returns: the cursor to wait after next, and the conversations noted since.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Persisted {
    pub seq: u64,
    pub conversation_ids: Vec<String>,
    /// The waiter's cursor predates a reset: re-read every conversation.
    pub everything: bool,
}

/// The current position, without waiting.
pub fn position() -> u64 {
    lock().seq
}

/// Wait until something is noted after cursor `after`, then return what was. Returns at once
/// if that already happened. Any number of waiters may wait, each with its own cursor.
pub async fn wait(after: u64) -> Persisted {
    loop {
        // Registered before the check, so a note between the check and the await still wakes us.
        let notified = FEED.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        {
            let st = lock();
            if st.seq > after {
                let mut conversation_ids: Vec<String> = st
                    .noted
                    .iter()
                    .filter(|(_, &at)| at > after)
                    .map(|(id, _)| id.clone())
                    .collect();
                conversation_ids.sort();
                return Persisted {
                    seq: st.seq,
                    conversation_ids,
                    everything: after < st.floor,
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

    // One process-wide feed, and other tests in this binary may note too, so only what this
    // test noted is asserted.
    #[tokio::test]
    async fn every_waiter_sees_every_note_past_its_own_cursor() {
        let start = position();
        note(["c-old"]);
        let got = wait(start).await;
        assert!(got.seq > start);
        assert!(got.conversation_ids.contains(&"c-old".to_string()));

        // Two waiters parked at the same cursor: a stale feed loop and a new one.
        let after = got.seq;
        let stale = tokio::spawn(async move { wait(after).await });
        let fresh = tokio::spawn(async move { wait(after).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!stale.is_finished() && !fresh.is_finished(), "returned with nothing noted");
        note(["c-1", "c-2", "c-1"]);
        for waiter in [stale, fresh] {
            let got = tokio::time::timeout(Duration::from_secs(5), waiter)
                .await
                .expect("a note must wake every waiter")
                .expect("join");
            let ids = got.conversation_ids;
            assert!(ids.contains(&"c-1".to_string()) && ids.contains(&"c-2".to_string()));
            assert_eq!(ids.iter().filter(|i| *i == "c-1").count(), 1, "deduplicated");
            assert!(!ids.contains(&"c-old".to_string()), "nothing from before the cursor");
        }

        // An empty note is not a change.
        let at = position();
        note(Vec::<String>::new());
        assert_eq!(position(), at);
    }
}
