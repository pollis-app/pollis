//! What a realtime wake may announce (#1628).
//!
//! A `new_message` wake names no sender and is not always a message: a delivered or read
//! receipt rides the same wake, and every device launch emits receipts for its catch-up. So a
//! wake alerts only for what was actually stored by the catch-up it caused.
//!
//! Catch-ups of one group are single-flight, so the caller whose wake caused a pass may not be
//! the caller that ran it: it may have joined a pass already running. The answer therefore
//! cannot be the caller's own return value. Every pass records what it stored here, numbered in
//! order; a wake takes a [`mark`] before its ingest and, after it, [`take`]s what any pass of its
//! group stored since. Taking removes the entries, so two wakes that shared one pass announce a
//! message once. Entries stored before a wake began (a launch catch-up) are never its news.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

/// How many stored-message entries are kept for wakes to take. Older ones were either taken
/// already or belong to no wake (a launch or resume catch-up); dropping them loses no alert a
/// running wake is still waiting for unless that many messages arrive inside one ingest.
const CAP: usize = 4096;

/// One message a catch-up stored for the first time, authored by someone else.
#[derive(Debug, Clone)]
struct Entry {
    seq: u64,
    group: String,
    conversation_id: String,
    message_id: String,
}

/// The per-`AppState` record of newly stored inbound messages.
#[derive(Debug, Default)]
pub struct Ledger {
    next: u64,
    entries: VecDeque<Entry>,
}

impl Ledger {
    fn mark(&self) -> u64 {
        self.next
    }

    fn record<'a>(&mut self, group: &str, stored: impl Iterator<Item = (&'a str, &'a [String])>) {
        for (conversation_id, ids) in stored {
            for id in ids {
                self.entries.push_back(Entry {
                    seq: self.next,
                    group: group.to_string(),
                    conversation_id: conversation_id.to_string(),
                    message_id: id.clone(),
                });
                self.next += 1;
            }
        }
        while self.entries.len() > CAP {
            self.entries.pop_front();
        }
    }

    /// Remove and return, per conversation, what `group`'s passes stored at or after `mark`.
    fn take(&mut self, group: &str, mark: u64) -> Vec<(String, Vec<String>)> {
        let mut out: Vec<(String, Vec<String>)> = Vec::new();
        self.entries.retain(|e| {
            if e.seq < mark || e.group != group {
                return true;
            }
            match out.iter_mut().find(|(c, _)| *c == e.conversation_id) {
                Some((_, ids)) => ids.push(e.message_id.clone()),
                None => out.push((e.conversation_id.clone(), vec![e.message_id.clone()])),
            }
            false
        });
        out
    }
}

pub type SharedLedger = Arc<std::sync::Mutex<Ledger>>;

fn with<T>(state: &AppState, f: impl FnOnce(&mut Ledger) -> T) -> T {
    let mut guard = state.inbound_news.lock().unwrap_or_else(|p| p.into_inner());
    f(&mut guard)
}

/// Where the ledger stands now; a wake takes this before its ingest.
pub(crate) fn mark(state: &AppState) -> u64 {
    with(state, |l| l.mark())
}

/// Record what one catch-up pass of `group` stored, per conversation.
pub(crate) fn record<'a>(
    state: &AppState,
    group: &str,
    stored: impl Iterator<Item = (&'a str, &'a [String])>,
) {
    with(state, |l| l.record(group, stored));
}

/// The wake's news: what `group`'s passes stored since `mark`, narrowed to the newest message per
/// conversation that is someone else's, not deleted and past the read cursor, and hydrated.
pub(crate) async fn take(
    state: &Arc<AppState>,
    user_id: &str,
    group: &str,
    mark: u64,
) -> Result<Vec<super::types::ChannelMessage>> {
    let stored = with(state, |l| l.take(group, mark));
    super::read::fresh_unread_messages(state, user_id, &stored).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_wake_gets_what_a_pass_it_joined_stored() {
        let mut l = Ledger::default();
        let mark = l.mark();
        // Another caller ran the pass; this one only waited for it.
        l.record("g", [("c1", ids(&["m1"]).as_slice())].into_iter());
        assert_eq!(l.take("g", mark), vec![("c1".to_string(), ids(&["m1"]))]);
    }

    #[test]
    fn two_wakes_sharing_one_pass_announce_it_once() {
        let mut l = Ledger::default();
        let (a, b) = (l.mark(), l.mark());
        l.record("g", [("c1", ids(&["m1"]).as_slice())].into_iter());
        assert_eq!(l.take("g", a).len(), 1);
        assert!(l.take("g", b).is_empty());
    }

    #[test]
    fn what_was_stored_before_the_wake_began_is_not_its_news() {
        let mut l = Ledger::default();
        // A launch catch-up stored it.
        l.record("g", [("c1", ids(&["old"]).as_slice())].into_iter());
        let mark = l.mark();
        assert!(l.take("g", mark).is_empty(), "a receipt wake re-announced an old message");
    }

    #[test]
    fn a_wake_takes_only_its_own_group() {
        let mut l = Ledger::default();
        let mark = l.mark();
        l.record("other", [("c9", ids(&["x"]).as_slice())].into_iter());
        l.record("g", [("c1", ids(&["m1"]).as_slice()), ("c2", ids(&["n1"]).as_slice())].into_iter());
        let got = l.take("g", mark);
        assert_eq!(got, vec![("c1".to_string(), ids(&["m1"])), ("c2".to_string(), ids(&["n1"]))]);
        assert_eq!(l.take("other", mark).len(), 1);
    }

    #[test]
    fn the_ledger_stays_bounded() {
        let mut l = Ledger::default();
        let many: Vec<String> = (0..CAP + 10).map(|i| format!("m{i}")).collect();
        l.record("g", [("c1", many.as_slice())].into_iter());
        assert_eq!(l.entries.len(), CAP);
    }
}
