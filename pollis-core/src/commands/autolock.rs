//! Idle auto-lock (#851): owns the idle deadline and calls `pin::lock` when it
//! expires.
//!
//! The deadline lives in Rust because a WebView throttles or suspends timers
//! when hidden, exactly when auto-lock must fire. One task sleeps until the
//! current deadline (no fixed cadence); activity just moves the deadline.
//!
//! An active voice session counts as continuous activity: `pin::lock` closes the
//! local DB, which would strand the call UI. The timeout is device-local and
//! pushed in by the renderer (`frontend/src/utils/autoLock.ts`); nothing here is
//! persisted.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tokio::time::Instant;

use crate::error::{Error, Result};
use crate::sink::EventSink;
use crate::state::AppState;

/// Event the renderer listens on to drop to the PIN gate. Must match
/// `AUTO_LOCK_EVENT` in `frontend/src/utils/autoLock.ts`.
pub const AUTO_LOCK_EVENT: &str = "auto-lock";

/// The only timeouts offered, in minutes (`None` = off, the default). Enforced at
/// the IPC chokepoint by [`set_auto_lock_timeout`].
pub const AUTO_LOCK_OPTIONS_MINUTES: &[u32] = &[1, 5, 15, 60];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Disabled,
    Fire,
    /// Not idle yet, or held open by a call: sleep this long, then re-ask.
    Sleep(Duration),
}

/// The pure deadline rule, testable without a clock, runtime or [`AppState`].
pub fn decide(timeout: Option<Duration>, idle_for: Duration, voice_active: bool) -> Decision {
    let Some(timeout) = timeout else {
        return Decision::Disabled;
    };
    if voice_active {
        return Decision::Sleep(timeout);
    }
    match timeout.checked_sub(idle_for) {
        Some(remaining) if !remaining.is_zero() => Decision::Sleep(remaining),
        _ => Decision::Fire,
    }
}

/// What the timer needs from the app. A trait so the loop is testable without a
/// real `AppState`.
#[async_trait::async_trait]
pub trait AutoLockHost: Send + Sync + 'static {
    async fn voice_active(&self) -> bool;
    /// Must be idempotent: the renderer's handler for the event locks again.
    async fn lock(&self);
}

struct Inner {
    timeout: Option<Duration>,
    last_activity: Instant,
}

pub struct AutoLockManager {
    /// `std::sync::Mutex`: no `.await` is ever held, and activity reporting is on the
    /// IPC hot path.
    inner: Mutex<Inner>,
    /// Bumped by every configure; a timer task exits once its generation is stale, so
    /// two timers can never race.
    generation: AtomicU64,
    sink: Mutex<Option<Arc<dyn EventSink<()>>>>,
    host: Mutex<Option<Arc<dyn AutoLockHost>>>,
    me: OnceLock<Weak<AutoLockManager>>,
}

static MANAGER: OnceLock<Arc<AutoLockManager>> = OnceLock::new();

pub fn manager() -> &'static Arc<AutoLockManager> {
    MANAGER.get_or_init(AutoLockManager::new)
}

/// Install the sink [`AUTO_LOCK_EVENT`] goes through. Required: without it the
/// backend locks but the renderer never leaves the unlocked UI.
pub fn set_event_sink(sink: Arc<dyn EventSink<()>>) {
    manager().set_sink(sink);
}

impl AutoLockManager {
    pub fn new() -> Arc<AutoLockManager> {
        let manager = Arc::new(AutoLockManager {
            inner: Mutex::new(Inner {
                timeout: None,
                last_activity: Instant::now(),
            }),
            generation: AtomicU64::new(0),
            sink: Mutex::new(None),
            host: Mutex::new(None),
            me: OnceLock::new(),
        });
        let _ = manager.me.set(Arc::downgrade(&manager));
        manager
    }

    pub fn set_sink(&self, sink: Arc<dyn EventSink<()>>) {
        *self.sink.lock().unwrap() = Some(sink);
    }

    pub fn set_host(&self, host: Arc<dyn AutoLockHost>) {
        *self.host.lock().unwrap() = Some(host);
    }

    pub fn timeout(&self) -> Option<Duration> {
        self.inner.lock().unwrap().timeout
    }

    /// Mark that a human touched the machine. Kept cheap: called from real input
    /// events.
    pub fn report_activity(&self) {
        self.inner.lock().unwrap().last_activity = Instant::now();
    }

    /// Apply a new idle window (`None` = off) and re-arm. Counts as activity, or a
    /// long-idle device would lock the instant someone picked a timeout.
    pub fn configure_duration(&self, timeout: Option<Duration>) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.timeout = timeout;
            inner.last_activity = Instant::now();
        }
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        if timeout.is_none() {
            // Off: the generation bump retires the running task; nothing is spawned.
            return;
        }
        let Some(me) = self.me.get().and_then(Weak::upgrade) else {
            return;
        };
        // No runtime means no timer; the setting is still recorded for a later
        // configure.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            me.run(generation).await;
        });
    }

    pub fn configure_minutes(&self, minutes: Option<u32>) {
        self.configure_duration(minutes.map(|m| Duration::from_secs(u64::from(m) * 60)));
    }

    pub async fn evaluate(&self) -> Decision {
        let (timeout, idle_for) = {
            let inner = self.inner.lock().unwrap();
            (
                inner.timeout,
                Instant::now().saturating_duration_since(inner.last_activity),
            )
        };
        if timeout.is_none() {
            return Decision::Disabled;
        }
        let voice_active = match self.host() {
            Some(host) => host.voice_active().await,
            None => false,
        };
        decide(timeout, idle_for, voice_active)
    }

    fn host(&self) -> Option<Arc<dyn AutoLockHost>> {
        self.host.lock().unwrap().clone()
    }

    fn sink(&self) -> Option<Arc<dyn EventSink<()>>> {
        self.sink.lock().unwrap().clone()
    }

    async fn run(self: Arc<Self>, generation: u64) {
        loop {
            if self.generation.load(Ordering::Acquire) != generation {
                return;
            }
            match self.evaluate().await {
                Decision::Disabled => return,
                Decision::Sleep(remaining) => tokio::time::sleep(remaining).await,
                Decision::Fire => {
                    // A `configure` that landed during the last sleep must win.
                    if self.generation.load(Ordering::Acquire) != generation {
                        return;
                    }
                    self.fire().await;
                    return;
                }
            }
        }
    }

    async fn fire(&self) {
        if let Some(host) = self.host() {
            host.lock().await;
        }
        // Locked first, told second: the keys are out of memory even if the renderer is
        // gone.
        if let Some(sink) = self.sink() {
            let _ = sink.send(());
        }
        // Fresh baseline so the post-unlock configure doesn't start from an elapsed
        // window.
        self.report_activity();
    }
}

struct AppStateHost(Arc<AppState>);

#[async_trait::async_trait]
impl AutoLockHost for AppStateHost {
    async fn voice_active(&self) -> bool {
        // `room` is set between join and release; `joining` covers the in-flight join so
        // a lock can't land in the gap.
        #[cfg(feature = "media")]
        {
            let voice = self.0.voice.lock().await;
            voice.room.is_some() || voice.joining.load(Ordering::Relaxed)
        }
        #[cfg(not(feature = "media"))]
        {
            false
        }
    }

    async fn lock(&self) {
        if let Err(e) = crate::commands::pin::lock(&self.0).await {
            crate::diag!("[autolock] lock failed: {e}");
        }
    }
}

/// Reject any window the UI could never offer. The IPC boundary is the lowest
/// layer for this device-local setting, so this is where 0-minute or 10-year
/// windows are made unrepresentable.
pub fn validate_timeout(minutes: Option<u32>) -> Result<()> {
    match minutes {
        None => Ok(()),
        Some(m) if AUTO_LOCK_OPTIONS_MINUTES.contains(&m) => Ok(()),
        Some(m) => Err(Error::Other(anyhow::anyhow!(
            "unsupported auto-lock timeout: {m} minutes"
        ))),
    }
}

/// Set (or clear, with `None`) this device's idle auto-lock window.
pub async fn set_auto_lock_timeout(state: &Arc<AppState>, minutes: Option<u32>) -> Result<()> {
    validate_timeout(minutes)?;
    let manager = manager();
    manager.set_host(Arc::new(AppStateHost(Arc::clone(state))));
    manager.configure_minutes(minutes);
    Ok(())
}

/// Report user activity. The renderer throttles these.
pub async fn report_user_activity() -> Result<()> {
    manager().report_activity();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    const MIN: Duration = Duration::from_secs(60);

    #[test]
    fn off_is_disabled_however_idle_the_device_is() {
        assert_eq!(
            decide(None, Duration::from_secs(60 * 60 * 24), false),
            Decision::Disabled
        );
    }

    #[test]
    fn fires_once_the_idle_window_has_elapsed() {
        assert_eq!(decide(Some(MIN), MIN, false), Decision::Fire);
        assert_eq!(
            decide(Some(MIN), MIN + Duration::from_secs(1), false),
            Decision::Fire
        );
    }

    #[test]
    fn sleeps_the_remaining_window_when_not_yet_idle() {
        assert_eq!(
            decide(Some(MIN), Duration::from_secs(20), false),
            Decision::Sleep(Duration::from_secs(40))
        );
    }

    #[test]
    fn an_active_call_suppresses_the_fire() {
        assert_eq!(
            decide(Some(MIN), Duration::from_secs(60 * 60), true),
            Decision::Sleep(MIN)
        );
    }

    // Real millisecond sleeps: tokio's paused clock auto-advances when idle, which
    // defeats a "did it wait?" assertion.

    #[derive(Default)]
    struct TestHost {
        voice_active: AtomicBool,
        locks: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl AutoLockHost for TestHost {
        async fn voice_active(&self) -> bool {
            self.voice_active.load(Ordering::SeqCst)
        }
        async fn lock(&self) {
            self.locks.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn armed(timeout: Option<Duration>) -> (Arc<AutoLockManager>, Arc<TestHost>) {
        let manager = AutoLockManager::new();
        let host = Arc::new(TestHost::default());
        manager.set_host(Arc::clone(&host) as Arc<dyn AutoLockHost>);
        manager.configure_duration(timeout);
        (manager, host)
    }

    async fn sleep_ms(ms: u64) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }

    #[tokio::test]
    async fn timer_locks_after_the_idle_window() {
        let (_manager, host) = armed(Some(Duration::from_millis(80)));
        assert_eq!(host.locks.load(Ordering::SeqCst), 0);
        sleep_ms(200).await;
        assert_eq!(host.locks.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn activity_resets_the_deadline() {
        let (manager, host) = armed(Some(Duration::from_millis(200)));
        for _ in 0..6 {
            sleep_ms(80).await;
            manager.report_activity();
        }
        assert_eq!(
            host.locks.load(Ordering::SeqCst),
            0,
            "a device in continuous use must never auto-lock"
        );
        sleep_ms(400).await;
        assert_eq!(host.locks.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn off_never_locks() {
        let (_manager, host) = armed(None);
        sleep_ms(300).await;
        assert_eq!(host.locks.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn switching_to_off_retires_a_running_timer() {
        let (manager, host) = armed(Some(Duration::from_millis(80)));
        manager.configure_duration(None);
        sleep_ms(300).await;
        assert_eq!(host.locks.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn an_active_voice_session_suppresses_the_timer() {
        let (_manager, host) = armed(Some(Duration::from_millis(80)));
        host.voice_active.store(true, Ordering::SeqCst);
        sleep_ms(400).await;
        assert_eq!(
            host.locks.load(Ordering::SeqCst),
            0,
            "locking mid-call closes the DB under the live call UI"
        );
        host.voice_active.store(false, Ordering::SeqCst);
        sleep_ms(200).await;
        assert_eq!(host.locks.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn reconfiguring_never_leaves_two_timers_running() {
        let (manager, host) = armed(Some(Duration::from_millis(80)));
        for _ in 0..5 {
            manager.configure_duration(Some(Duration::from_millis(80)));
        }
        sleep_ms(400).await;
        assert_eq!(
            host.locks.load(Ordering::SeqCst),
            1,
            "each configure must retire the previous timer"
        );
    }

    #[test]
    fn only_the_offered_windows_are_representable() {
        assert!(validate_timeout(None).is_ok());
        for m in AUTO_LOCK_OPTIONS_MINUTES {
            assert!(validate_timeout(Some(*m)).is_ok(), "{m} should be offered");
        }
        for m in [0u32, 2, 7, 1440, u32::MAX] {
            assert!(
                validate_timeout(Some(m)).is_err(),
                "{m} minutes must be rejected"
            );
        }
    }
}
