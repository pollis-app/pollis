//! Idle auto-lock (#851).
//!
//! The PIN is cryptographically load-bearing and `pin::lock` already exists,
//! but nothing ever called it on a timer — walk away from an unlocked laptop
//! and the whole decrypted history stays on screen. This module owns the idle
//! deadline and drives `pin::lock` when it expires.
//!
//! **Why the deadline lives here and not in the renderer.** A WebView throttles
//! (and on some platforms suspends) its timers once the window is hidden or
//! minimized — exactly the states in which an auto-lock most needs to fire. A
//! `setTimeout` in JS would therefore be reliable only while the user is
//! looking at the app, which is the case that does not matter. Rust holds
//! `last_activity` and a **single** sleeping task that wakes at the deadline;
//! the renderer's only job is to report that a human touched the machine.
//!
//! That single resettable timer is deliberately not polling (CLAUDE.md bans
//! `setInterval`-style keepalives): the task sleeps exactly until the current
//! deadline, and a reported activity simply moves the deadline so the next wake
//! re-evaluates and sleeps again. Nothing runs on a fixed cadence.
//!
//! **Voice suppresses it.** `pin::lock` closes the local DB, so locking mid-call
//! would leave the call UI unable to read anything while audio kept flowing —
//! a confusing half-state, not a security win. An active session is therefore
//! treated as continuous activity: the deadline keeps sliding while the call is
//! up, and the idle clock resumes the moment it ends.
//!
//! The chosen timeout is **device-local** and owned by the renderer (see
//! `frontend/src/utils/autoLock.ts`) — it describes where a machine physically
//! sits, so syncing it would force one answer onto every device. The renderer
//! pushes it in via [`set_auto_lock_timeout`] at startup and on every change;
//! nothing here is persisted.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::Duration;

use tokio::time::Instant;

use crate::error::{Error, Result};
use crate::sink::EventSink;
use crate::state::AppState;

/// The global event name the renderer listens on to drop to the PIN gate. Must
/// match `AUTO_LOCK_EVENT` in `frontend/src/utils/autoLock.ts`.
pub const AUTO_LOCK_EVENT: &str = "auto-lock";

/// The only timeouts the UI offers, in minutes. `None` (Off) is the default and
/// is not in this list. Kept here rather than only in the renderer so
/// [`set_auto_lock_timeout`] can reject anything else at the IPC chokepoint —
/// a 1-second auto-lock is an invalid state, not a user preference.
pub const AUTO_LOCK_OPTIONS_MINUTES: &[u32] = &[1, 5, 15, 60];

/// What the timer task should do the instant it asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Auto-lock is off. No timer should exist at all.
    Disabled,
    /// The idle window has elapsed — lock now.
    Fire,
    /// Not idle yet (or held open by a call). Sleep this long, then re-ask.
    Sleep(Duration),
}

/// The pure deadline rule, factored out of the timer task so it can be tested
/// without a clock, a runtime, or an [`AppState`].
///
/// * `timeout` — the configured idle window, or `None` when auto-lock is off.
/// * `idle_for` — how long since the last reported user activity.
/// * `voice_active` — whether a voice session is up right now.
pub fn decide(timeout: Option<Duration>, idle_for: Duration, voice_active: bool) -> Decision {
    let Some(timeout) = timeout else {
        return Decision::Disabled;
    };
    // A live call counts as continuous activity, so the deadline is pushed a
    // full window out rather than being allowed to expire underneath it.
    if voice_active {
        return Decision::Sleep(timeout);
    }
    match timeout.checked_sub(idle_for) {
        Some(remaining) if !remaining.is_zero() => Decision::Sleep(remaining),
        _ => Decision::Fire,
    }
}

/// The two things the timer needs from the rest of the app. A trait rather than
/// a bare `Arc<AppState>` so the loop can be unit-tested: building a real
/// `AppState` requires a live remote DB connection.
#[async_trait::async_trait]
pub trait AutoLockHost: Send + Sync + 'static {
    /// Whether a voice session is up right now.
    async fn voice_active(&self) -> bool;
    /// Perform the lock. Must be idempotent — the renderer routes the resulting
    /// event through the same handler Cmd/Ctrl+L uses, which locks again.
    async fn lock(&self);
}

struct Inner {
    timeout: Option<Duration>,
    last_activity: Instant,
}

/// Owns the idle deadline and the one task that waits on it.
pub struct AutoLockManager {
    /// A `std::sync::Mutex`, not tokio's: every path that touches it does a
    /// couple of field writes with no `.await` held. Activity reporting is on
    /// the IPC hot path and must never park a task.
    inner: Mutex<Inner>,
    /// Bumped by every [`AutoLockManager::configure_duration`]. A timer task
    /// carries the generation it was spawned under and exits as soon as it no
    /// longer matches, so a settings change can never leave two timers racing.
    generation: AtomicU64,
    sink: Mutex<Option<Arc<dyn EventSink<()>>>>,
    host: Mutex<Option<Arc<dyn AutoLockHost>>>,
    /// A handle back to the `Arc` this manager lives in, so `configure` can
    /// hand an owned reference to the task it spawns.
    me: OnceLock<Weak<AutoLockManager>>,
}

static MANAGER: OnceLock<Arc<AutoLockManager>> = OnceLock::new();

/// The process-wide manager.
pub fn manager() -> &'static Arc<AutoLockManager> {
    MANAGER.get_or_init(AutoLockManager::new)
}

/// Install the sink the [`AUTO_LOCK_EVENT`] event is pushed through. The
/// desktop shell wires this to `AppHandle::emit` at setup. Without a sink the
/// backend still locks — the renderer just wouldn't know to leave the unlocked
/// UI, so this is required, not decorative.
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

    /// The configured idle window, or `None` when auto-lock is off.
    pub fn timeout(&self) -> Option<Duration> {
        self.inner.lock().unwrap().timeout
    }

    /// Mark "a human touched this machine just now". Cheap by construction —
    /// one uncontended lock and one field write — because the renderer calls it
    /// from real input events.
    pub fn report_activity(&self) {
        self.inner.lock().unwrap().last_activity = Instant::now();
    }

    /// Apply a new idle window (`None` = off) and re-arm. Also counts as
    /// activity: changing the setting is itself a user action, and without the
    /// reset a device that had been idle for an hour would lock the instant
    /// someone picked "60 minutes".
    pub fn configure_duration(&self, timeout: Option<Duration>) {
        {
            let mut inner = self.inner.lock().unwrap();
            inner.timeout = timeout;
            inner.last_activity = Instant::now();
        }
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        if timeout.is_none() {
            // Off: the generation bump alone retires the running task. Spawning
            // nothing is what makes "Off" cost exactly zero.
            return;
        }
        let Some(me) = self.me.get().and_then(Weak::upgrade) else {
            return;
        };
        // No runtime (a sync test, a host that hasn't started tokio) means no
        // timer. The setting is still recorded, so a later `configure` on a
        // runtime arms it.
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        handle.spawn(async move {
            me.run(generation).await;
        });
    }

    /// Convenience wrapper over [`AutoLockManager::configure_duration`] for the
    /// minute-granularity values the UI deals in.
    pub fn configure_minutes(&self, minutes: Option<u32>) {
        self.configure_duration(minutes.map(|m| Duration::from_secs(u64::from(m) * 60)));
    }

    /// What the timer should do right now.
    pub async fn evaluate(&self) -> Decision {
        let (timeout, idle_for) = {
            let inner = self.inner.lock().unwrap();
            (
                inner.timeout,
                Instant::now().saturating_duration_since(inner.last_activity),
            )
        };
        // Skip the (async) voice read entirely when auto-lock is off, so a
        // disabled timer never touches the voice mutex.
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

    /// The whole timer. One task, one sleep at a time, always sleeping until a
    /// real deadline rather than on a fixed cadence.
    async fn run(self: Arc<Self>, generation: u64) {
        loop {
            if self.generation.load(Ordering::Acquire) != generation {
                return;
            }
            match self.evaluate().await {
                Decision::Disabled => return,
                Decision::Sleep(remaining) => tokio::time::sleep(remaining).await,
                Decision::Fire => {
                    // Re-check after the last sleep: a `configure` that landed
                    // while we were asleep must win over a stale decision.
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
        // Locked first, told second: if the sink is missing or the renderer is
        // gone, the keys are already out of memory.
        if let Some(sink) = self.sink() {
            let _ = sink.send(());
        }
        // A fresh baseline so the next `configure` (which the renderer issues
        // after a successful unlock) starts from now rather than from an idle
        // window that has already elapsed.
        self.report_activity();
    }
}

/// Bridges the process-wide manager to the running app.
struct AppStateHost(Arc<AppState>);

#[async_trait::async_trait]
impl AutoLockHost for AppStateHost {
    async fn voice_active(&self) -> bool {
        // `VoiceState.room` is `Some` between a successful join and
        // `release_voice_resources` — the authoritative "in a call" signal.
        // `joining` covers the in-flight join so a lock can't land in the gap
        // between "user clicked join" and "room is up".
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
            eprintln!("[autolock] lock failed: {e}");
        }
    }
}

// ── Commands ─────────────────────────────────────────────────────────

/// Reject any window the UI could never have produced. `None` (Off) is always
/// valid; everything else must be one of [`AUTO_LOCK_OPTIONS_MINUTES`].
///
/// This is the chokepoint that keeps a 0-minute (lock instantly, unusable) or
/// 10-year (indistinguishable from off, but pretending otherwise) window
/// unrepresentable — the IPC boundary is the lowest layer available, since the
/// setting is device-local and never touches a schema.
pub fn validate_timeout(minutes: Option<u32>) -> Result<()> {
    match minutes {
        None => Ok(()),
        Some(m) if AUTO_LOCK_OPTIONS_MINUTES.contains(&m) => Ok(()),
        Some(m) => Err(Error::Other(anyhow::anyhow!(
            "unsupported auto-lock timeout: {m} minutes"
        ))),
    }
}

/// Set (or clear) the idle auto-lock window for this device.
///
/// `None` turns auto-lock off. See [`validate_timeout`] for what else is
/// accepted.
pub async fn set_auto_lock_timeout(state: &Arc<AppState>, minutes: Option<u32>) -> Result<()> {
    validate_timeout(minutes)?;
    let manager = manager();
    manager.set_host(Arc::new(AppStateHost(Arc::clone(state))));
    manager.configure_minutes(minutes);
    Ok(())
}

/// Report that a human interacted with this device. The renderer throttles
/// these, so this is a handful of calls per minute at worst.
pub async fn report_user_activity() -> Result<()> {
    manager().report_activity();
    Ok(())
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize};

    const MIN: Duration = Duration::from_secs(60);

    // ── The pure rule ────────────────────────────────────────────────

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
        // Idle far past the window, but a call is up: hold, do not lock.
        assert_eq!(
            decide(Some(MIN), Duration::from_secs(60 * 60), true),
            Decision::Sleep(MIN)
        );
    }

    // ── The timer loop ───────────────────────────────────────────────
    //
    // Real (millisecond) sleeps rather than a paused clock: tokio's paused
    // clock auto-advances whenever the runtime goes idle, which is precisely
    // what a "did it wait?" assertion needs it not to do.

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
        // Keep reporting activity across more than two full windows.
        for _ in 0..6 {
            sleep_ms(80).await;
            manager.report_activity();
        }
        assert_eq!(
            host.locks.load(Ordering::SeqCst),
            0,
            "a device in continuous use must never auto-lock"
        );
        // Stop touching it and it locks.
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
        // Ending the call hands the device straight back to the idle clock —
        // it has been idle far longer than the window, so the next wake fires.
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
        // A zero window would lock the instant the app opened; the rest are
        // simply not offered, and a caller inventing one is a bug, not a
        // preference.
        for m in [0u32, 2, 7, 1440, u32::MAX] {
            assert!(
                validate_timeout(Some(m)).is_err(),
                "{m} minutes must be rejected"
            );
        }
    }
}
