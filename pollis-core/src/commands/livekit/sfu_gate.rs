//! One process-wide "SFU unreachable" gate shared by every LiveKit reconnect loop.
//!
//! A user has a dozen realtime rooms plus a voice session on the same SFU. When it
//! goes down, each loop used to re-mint a DS token and dial on its own schedule —
//! a dozen times the DS load for the whole outage. Here the first loop to fail
//! becomes the single prober; the rest park on a notify and follow it back.

use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

/// What a loop that just failed to reach the SFU should do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// Keep retrying on your own backoff; you are the one probe.
    Prober,
    /// Park on [`SfuGate::wait_up`]; the prober will wake you.
    Waiter,
}

#[derive(Default)]
struct Inner {
    down_since: Option<Instant>,
    prober: Option<String>,
}

#[derive(Default)]
pub struct SfuGate {
    inner: Mutex<Inner>,
    up: Notify,
}

impl SfuGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a failed dial from `who`; the first reporter of an outage becomes the prober.
    pub fn report_down(&self, who: &str) -> Role {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner.down_since.is_none() {
            inner.down_since = Some(Instant::now());
        }
        match inner.prober.as_deref() {
            None => {
                inner.prober = Some(who.to_string());
                Role::Prober
            }
            Some(p) if p == who => Role::Prober,
            Some(_) => Role::Waiter,
        }
    }

    /// A dial succeeded: the outage is over; every waiter is released.
    pub fn report_up(&self) {
        let was_down = {
            let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
            let was_down = inner.down_since.take().is_some();
            inner.prober = None;
            was_down
        };
        if was_down {
            self.up.notify_waiters();
        }
    }

    /// The prober is going away (room removed, session left): the next failure elects a new one.
    pub fn release_prober(&self, who: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner.prober.as_deref() == Some(who) {
            inner.prober = None;
        }
    }

    pub fn is_down(&self) -> bool {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .down_since
            .is_some()
    }

    /// How long the SFU has been unreachable, if it is.
    pub fn down_for(&self) -> Option<Duration> {
        self.inner
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .down_since
            .map(|t| t.elapsed())
    }

    /// Park until the prober reports the SFU back, or `max` passes. `true` = it is back.
    pub async fn wait_up(&self, max: Duration) -> bool {
        let notified = self.up.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if !self.is_down() {
            return true;
        }
        tokio::select! {
            _ = &mut notified => true,
            _ = tokio::time::sleep(max) => !self.is_down(),
        }
    }
}

static GATE: LazyLock<SfuGate> = LazyLock::new(SfuGate::new);

/// The process-wide gate; one SFU per build, so one gate.
pub fn sfu_gate() -> &'static SfuGate {
    &GATE
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn the_first_to_fail_probes_and_everyone_else_waits() {
        let gate = SfuGate::new();
        assert!(!gate.is_down());
        assert_eq!(gate.report_down("room-a"), Role::Prober);
        assert_eq!(gate.report_down("room-b"), Role::Waiter);
        assert_eq!(gate.report_down("voice"), Role::Waiter);
        // The prober failing again stays the prober.
        assert_eq!(gate.report_down("room-a"), Role::Prober);
        assert!(gate.is_down());
    }

    #[test]
    fn a_departed_prober_hands_over_to_the_next_failure() {
        let gate = SfuGate::new();
        assert_eq!(gate.report_down("room-a"), Role::Prober);
        gate.release_prober("room-a");
        assert_eq!(gate.report_down("room-b"), Role::Prober);
        assert_eq!(gate.report_down("room-a"), Role::Waiter);
    }

    #[tokio::test]
    async fn waiters_wake_when_the_prober_reports_up() {
        let gate = Arc::new(SfuGate::new());
        assert_eq!(gate.report_down("room-a"), Role::Prober);
        assert_eq!(gate.report_down("room-b"), Role::Waiter);
        let waiter = {
            let gate = Arc::clone(&gate);
            tokio::spawn(async move { gate.wait_up(Duration::from_secs(30)).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        gate.report_up();
        assert!(
            waiter.await.unwrap(),
            "the waiter must be released by report_up"
        );
        assert!(!gate.is_down());
        // A new outage starts a new election.
        assert_eq!(gate.report_down("room-b"), Role::Prober);
    }

    #[tokio::test]
    async fn a_waiter_is_not_parked_once_the_sfu_is_already_up() {
        let gate = SfuGate::new();
        assert!(gate.wait_up(Duration::from_secs(30)).await);
        gate.report_down("x");
        assert!(
            !gate.wait_up(Duration::from_millis(20)).await,
            "still down after the wait"
        );
    }
}
