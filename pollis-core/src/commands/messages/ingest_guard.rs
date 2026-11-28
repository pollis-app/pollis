//! Opening each fetched envelope once (#1629).
//!
//! Two things used to put a second decrypt on an envelope, each logging an error for nothing:
//!
//! - **This device's own envelopes come back.** A fetch returns everything posted to the
//!   conversation, this device's receipts and edits included. openmls cannot open its own
//!   sender's ciphertext, and the failure read as "not an application message".
//! - **Overlapping catch-ups of one group.** A catch-up fetches past the watermark first and
//!   posts the new watermark last, so a second catch-up started in between fetched the same
//!   envelopes and failed on every one the first had opened ("secret reuse").
//!
//! [`already_held`] skips both before any decrypt. [`coalesced`] makes catch-ups of one group
//! single-flight. A failure on an envelope this device never opened is still reported: a
//! "secret reuse" there means a sender reused a ratchet generation.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::sync::Mutex;

/// Whether envelope `id` is already accounted for on this device: one it sealed itself, one it
/// opened (a control frame or an edit, which leave no row of their own), or a stored message.
/// A placeholder for a lost message does not count, so that envelope is still tried.
pub(crate) fn already_held(conn: &rusqlite::Connection, id: &str) -> bool {
    super::loss::is_known_envelope(conn, id) || super::loss::has_real_row(conn, id)
}

/// One group's catch-up bookkeeping for [`coalesced`].
#[derive(Default)]
pub struct CatchUpFlight {
    /// Held for a whole pass. Holds the highest ticket a finished pass's read answered for.
    covered: Mutex<u64>,
}

/// Every group's [`CatchUpFlight`], and the request count they share.
#[derive(Default)]
pub struct FlightTable {
    /// Catch-ups asked for so far, all groups together. Each caller takes the next number as
    /// its ticket, and a read is marked with the count when it is sent ([`FlightTable::mark`]).
    requested: AtomicU64,
    groups: Mutex<HashMap<String, Arc<CatchUpFlight>>>,
}

/// The `AppState`'s [`FlightTable`].
pub type CatchUpFlights = Arc<FlightTable>;

/// The catch-up requests one read answers for: every request made before it was sent, and
/// none made after. A pass covers exactly what the read it applied answers for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReadMark(u64);

impl FlightTable {
    /// The mark for a read about to be sent. Take it before the request goes out: a request
    /// made while the read is in flight must not count as answered by it.
    pub(crate) fn mark(&self) -> ReadMark {
        ReadMark(self.requested.load(Ordering::SeqCst))
    }

    /// Test only: the highest ticket `group`'s finished passes answered for.
    #[cfg(test)]
    async fn covered(&self, group: &str) -> u64 {
        let flight = self.groups.lock().await.get(group).cloned();
        match flight {
            Some(flight) => *flight.covered.lock().await,
            None => 0,
        }
    }
}

tokio::task_local! {
    /// The groups whose pass the current task is running, so a pass that re-enters its own
    /// group's catch-up runs it in place instead of waiting on itself.
    static RUNNING: HashSet<String>;
}

/// Run `pass` for `group`, one pass at a time per group.
///
/// A caller that arrives while a pass runs waits for it. It then runs a pass of its own unless
/// a finished pass already answered its request: one whose read was sent after the request was
/// made. A burst of callers therefore costs at most one more pass. Nothing polls: callers wait
/// on the group's lock.
///
/// `pass` gets the [`ReadMark`] of a read it sends itself and returns the mark of the read it
/// applied. Usually that is the one it was given; a read made before the pass began (the
/// pre-seal read, #2141, or a push fetch's prefetch, #1804) carries its own, older mark, so a
/// request made after that read gets a pass of its own instead of being answered by it.
pub(crate) async fn coalesced<F, Fut>(
    flights: &CatchUpFlights,
    group: &str,
    pass: F,
) -> crate::error::Result<()>
where
    F: FnOnce(ReadMark) -> Fut,
    Fut: Future<Output = crate::error::Result<ReadMark>>,
{
    if RUNNING.try_with(|running| running.contains(group)).unwrap_or(false) {
        return pass(flights.mark()).await.map(|_| ());
    }
    let flight = {
        let mut map = flights.groups.lock().await;
        Arc::clone(map.entry(group.to_string()).or_default())
    };
    let ticket = flights.requested.fetch_add(1, Ordering::SeqCst) + 1;
    // Test-harness only: a caller holding a ticket that must wait for a running pass parks
    // first, so a test decides which waiter goes next.
    #[cfg(feature = "test-harness")]
    let mut covered = match flight.covered.try_lock() {
        Ok(covered) => covered,
        Err(_) => {
            crate::commands::mls::rendezvous::park(
                crate::commands::mls::rendezvous::Point::CatchUpWaiting,
            )
            .await;
            flight.covered.lock().await
        }
    };
    #[cfg(not(feature = "test-harness"))]
    let mut covered = flight.covered.lock().await;
    if *covered >= ticket {
        return Ok(());
    }
    // A read the pass sends from here on answers every request made up to now.
    let starting = flights.mark();
    let mut running = RUNNING.try_with(Clone::clone).unwrap_or_default();
    running.insert(group.to_string());
    let applied = RUNNING.scope(running, pass(starting)).await?;
    // Never more than the read the pass applied answers for, and never less than earlier
    // passes already did.
    *covered = (*covered).max(applied.min(starting).0);
    Ok(())
}

/// Test-harness only: how many times each envelope reached a decrypt in this process.
#[cfg(feature = "test-harness")]
static ATTEMPTS: std::sync::Mutex<Option<HashMap<String, usize>>> = std::sync::Mutex::new(None);

#[cfg(feature = "test-harness")]
pub(crate) fn note_attempt(id: &str) {
    if let Ok(mut guard) = ATTEMPTS.lock() {
        *guard.get_or_insert_with(HashMap::new).entry(id.to_string()).or_default() += 1;
    }
}

/// Test-harness only (#1629): decrypt attempts per envelope id since the last [`forget_decrypt_attempts`].
#[cfg(feature = "test-harness")]
pub fn decrypt_attempts() -> HashMap<String, usize> {
    ATTEMPTS.lock().ok().and_then(|g| g.clone()).unwrap_or_default()
}

#[cfg(feature = "test-harness")]
pub fn forget_decrypt_attempts() {
    if let Ok(mut guard) = ATTEMPTS.lock() {
        *guard = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    use std::time::Duration;

    fn db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::local::apply_local_schema(&conn).unwrap();
        conn
    }

    #[test]
    fn own_opened_and_stored_envelopes_are_held_and_a_placeholder_is_not() {
        let conn = db();
        crate::commands::messages::loss::record_own_envelope(&conn, "own");
        crate::commands::messages::loss::record_opened_envelope(&conn, "opened");
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at)
             VALUES ('stored', 'c', 'u', X'00', 'hi', '2026-01-01T00:00:00Z')",
            [],
        )
        .unwrap();
        crate::commands::messages::loss::write_placeholder(
            &conn,
            "c",
            "lost",
            "u",
            "2026-01-01T00:00:01Z",
            crate::commands::messages::loss::Reason::Failed,
        );
        assert!(already_held(&conn, "own"));
        assert!(already_held(&conn, "opened"));
        assert!(already_held(&conn, "stored"));
        assert!(!already_held(&conn, "lost"), "a placeholder must still be tried");
        assert!(!already_held(&conn, "never-seen"));
    }

    /// Callers that overlap one running pass share a single follow-up pass, never one each.
    #[tokio::test(flavor = "multi_thread")]
    async fn overlapping_catch_ups_run_at_most_one_more_pass() {
        let flights: CatchUpFlights = Arc::default();
        let passes = Arc::new(AtomicUsize::new(0));
        let pass = |passes: Arc<AtomicUsize>, mark: ReadMark| async move {
            passes.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(mark)
        };
        let first = {
            let (flights, passes) = (flights.clone(), passes.clone());
            tokio::spawn(async move { coalesced(&flights, "g", |m| pass(passes, m)).await })
        };
        // Let the first pass start, then pile up callers behind it.
        tokio::time::sleep(Duration::from_millis(10)).await;
        let waiters: Vec<_> = (0..5)
            .map(|_| {
                let (flights, passes) = (flights.clone(), passes.clone());
                tokio::spawn(async move { coalesced(&flights, "g", |m| pass(passes, m)).await })
            })
            .collect();
        first.await.unwrap().unwrap();
        for w in waiters {
            w.await.unwrap().unwrap();
        }
        assert_eq!(passes.load(Ordering::SeqCst), 2, "one pass, then one for everyone who waited");
    }

    /// A caller that asks after a pass has finished gets a pass of its own.
    #[tokio::test]
    async fn a_later_request_is_never_answered_by_an_earlier_pass() {
        let flights: CatchUpFlights = Arc::default();
        let passes = Arc::new(AtomicUsize::new(0));
        for _ in 0..3 {
            let passes = passes.clone();
            coalesced(&flights, "g", |mark| async move {
                passes.fetch_add(1, Ordering::SeqCst);
                Ok(mark)
            })
            .await
            .unwrap();
        }
        assert_eq!(passes.load(Ordering::SeqCst), 3);
    }

    /// A failed pass covers nobody: whoever waited on it runs again.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_pass_leaves_its_waiters_to_try_again() {
        let flights: CatchUpFlights = Arc::default();
        let passes = Arc::new(AtomicUsize::new(0));
        let first = {
            let (flights, passes) = (flights.clone(), passes.clone());
            tokio::spawn(async move {
                coalesced(&flights, "g", |_| async move {
                    passes.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Err(crate::error::Error::Other(anyhow::anyhow!("offline")))
                })
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        let passes2 = passes.clone();
        coalesced(&flights, "g", |mark| async move {
            passes2.fetch_add(1, Ordering::SeqCst);
            Ok(mark)
        })
        .await
        .unwrap();
        assert!(first.await.unwrap().is_err());
        assert_eq!(passes.load(Ordering::SeqCst), 2);
    }

    /// A pass that re-enters its own group's catch-up runs it in place; it must not wait on itself.
    #[tokio::test]
    async fn a_reentrant_catch_up_does_not_deadlock() {
        let flights: CatchUpFlights = Arc::default();
        let inner_ran = Arc::new(AtomicUsize::new(0));
        let (f2, ran) = (flights.clone(), inner_ran.clone());
        tokio::time::timeout(
            Duration::from_secs(5),
            coalesced(&flights, "g", |mark| async move {
                coalesced(&f2, "g", |inner| async move {
                    ran.fetch_add(1, Ordering::SeqCst);
                    Ok(inner)
                })
                .await
                .map(|()| mark)
            }),
        )
        .await
        .expect("a re-entrant catch-up deadlocked")
        .unwrap();
        assert_eq!(inner_ran.load(Ordering::SeqCst), 1);
    }

    /// Groups never wait on each other.
    #[tokio::test(flavor = "multi_thread")]
    async fn different_groups_run_side_by_side() {
        let flights: CatchUpFlights = Arc::default();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let a = {
            let flights = flights.clone();
            tokio::spawn(async move {
                coalesced(&flights, "a", |mark| async move {
                    rx.await.ok();
                    Ok(mark)
                })
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        tokio::time::timeout(
            Duration::from_secs(5),
            coalesced(&flights, "b", |mark| async move { Ok(mark) }),
        )
        .await
        .expect("group b waited on group a")
        .unwrap();
        tx.send(()).unwrap();
        a.await.unwrap().unwrap();
    }

    /// A pass that applies a read made before some request answers for nothing after that
    /// read: the flight records the read's mark, not the count when the pass began.
    #[tokio::test]
    async fn a_pass_covers_only_what_its_read_answers_for() {
        let flights: CatchUpFlights = Arc::default();
        // A read sent now, before anyone asks.
        let early = flights.mark();
        coalesced(&flights, "g", |_| async move { Ok(early) }).await.unwrap();
        assert_eq!(flights.covered("g").await, early.0, "only what the early read answers for");
        // A pass reading for itself covers every request made before it began.
        coalesced(&flights, "g", |mark| async move { Ok(mark) }).await.unwrap();
        assert_eq!(flights.covered("g").await, 2);
        // An older read never takes coverage back.
        coalesced(&flights, "g", |_| async move { Ok(early) }).await.unwrap();
        assert_eq!(flights.covered("g").await, 2);
    }
}

/// Counts the passes that change this device's MLS state (#2141): every catch-up pass, and a
/// drain that applied Welcomes. A read made before a seal records [`PassClock::now`]; it is
/// used in place of a fresh read only if no such pass started, ran or finished since.
#[derive(Default)]
pub struct PassClock {
    /// Starts and ends of passes, and applied Welcome drains.
    events: AtomicU64,
    /// Passes running now.
    running: AtomicU64,
}

impl PassClock {
    /// The clock's reading, or `None` while a pass runs (its result would race a read).
    pub(crate) fn now(&self) -> Option<u64> {
        if self.running.load(Ordering::SeqCst) > 0 {
            return None;
        }
        Some(self.events.load(Ordering::SeqCst))
    }

    /// Whether nothing changed since `reading`.
    pub(crate) fn unchanged_since(&self, reading: Option<u64>) -> bool {
        reading.is_some() && self.now() == reading
    }

    /// Something outside a pass changed the MLS state (Welcomes applied).
    pub(crate) fn touch(&self) {
        self.events.fetch_add(1, Ordering::SeqCst);
    }

    /// Start a pass; it ends when the returned guard drops.
    pub(crate) fn pass(self: &Arc<Self>) -> PassGuard {
        self.running.fetch_add(1, Ordering::SeqCst);
        self.events.fetch_add(1, Ordering::SeqCst);
        PassGuard(Arc::clone(self))
    }
}

/// A running pass on a [`PassClock`].
pub(crate) struct PassGuard(Arc<PassClock>);

impl Drop for PassGuard {
    fn drop(&mut self) {
        self.0.events.fetch_add(1, Ordering::SeqCst);
        self.0.running.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod pass_clock_tests {
    use super::*;

    #[test]
    fn a_read_is_current_only_while_no_pass_touched_the_state() {
        let clock = Arc::new(PassClock::default());
        let read = clock.now();
        assert!(clock.unchanged_since(read));
        drop(clock.pass());
        assert!(!clock.unchanged_since(read), "a pass ran since");

        let read = clock.now();
        let running = clock.pass();
        assert_eq!(clock.now(), None, "no reading while a pass runs");
        assert!(!clock.unchanged_since(read));
        drop(running);

        let read = clock.now();
        clock.touch();
        assert!(!clock.unchanged_since(read), "Welcomes applied since");
        assert!(!clock.unchanged_since(None));
    }
}
