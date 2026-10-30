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
    /// Catch-ups asked for so far. Each caller takes the next number as its ticket.
    requested: AtomicU64,
    /// Held for a whole pass. Holds the highest ticket whose request a finished pass covered.
    covered: Mutex<u64>,
}

/// Every group's [`CatchUpFlight`], on the `AppState`.
pub type CatchUpFlights = Arc<Mutex<HashMap<String, Arc<CatchUpFlight>>>>;

tokio::task_local! {
    /// The groups whose pass the current task is running, so a pass that re-enters its own
    /// group's catch-up runs it in place instead of waiting on itself.
    static RUNNING: HashSet<String>;
}

/// Run `pass` for `group`, one pass at a time per group.
///
/// A caller that arrives while a pass runs waits for it. It then runs a pass of its own only
/// if no pass has started since it asked, because only a pass that started after its request
/// can have fetched what it was asked to fetch. A burst of callers therefore costs at most one
/// more pass. Nothing polls: callers wait on the group's lock.
pub(crate) async fn coalesced<F, Fut>(
    flights: &CatchUpFlights,
    group: &str,
    pass: F,
) -> crate::error::Result<()>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = crate::error::Result<()>>,
{
    if RUNNING.try_with(|running| running.contains(group)).unwrap_or(false) {
        return pass().await;
    }
    let flight = {
        let mut map = flights.lock().await;
        Arc::clone(map.entry(group.to_string()).or_default())
    };
    let ticket = flight.requested.fetch_add(1, Ordering::SeqCst) + 1;
    let mut covered = flight.covered.lock().await;
    if *covered >= ticket {
        return Ok(());
    }
    // Every request made up to here is covered by the pass that starts now.
    let starting = flight.requested.load(Ordering::SeqCst);
    let mut running = RUNNING.try_with(Clone::clone).unwrap_or_default();
    running.insert(group.to_string());
    let outcome = RUNNING.scope(running, pass()).await;
    if outcome.is_ok() {
        *covered = starting;
    }
    outcome
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
        let pass = |passes: Arc<AtomicUsize>| async move {
            passes.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(())
        };
        let first = {
            let (flights, passes) = (flights.clone(), passes.clone());
            tokio::spawn(async move { coalesced(&flights, "g", || pass(passes)).await })
        };
        // Let the first pass start, then pile up callers behind it.
        tokio::time::sleep(Duration::from_millis(10)).await;
        let waiters: Vec<_> = (0..5)
            .map(|_| {
                let (flights, passes) = (flights.clone(), passes.clone());
                tokio::spawn(async move { coalesced(&flights, "g", || pass(passes)).await })
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
            coalesced(&flights, "g", || async move {
                passes.fetch_add(1, Ordering::SeqCst);
                Ok(())
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
                coalesced(&flights, "g", || async move {
                    passes.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Err(crate::error::Error::Other(anyhow::anyhow!("offline")))
                })
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        let passes2 = passes.clone();
        coalesced(&flights, "g", || async move {
            passes2.fetch_add(1, Ordering::SeqCst);
            Ok(())
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
            coalesced(&flights, "g", || async move {
                coalesced(&f2, "g", || async move {
                    ran.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
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
                coalesced(&flights, "a", || async move {
                    rx.await.ok();
                    Ok(())
                })
                .await
            })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        tokio::time::timeout(
            Duration::from_secs(5),
            coalesced(&flights, "b", || async move { Ok(()) }),
        )
        .await
        .expect("group b waited on group a")
        .unwrap();
        tx.send(()).unwrap();
        a.await.unwrap().unwrap();
    }
}
