//! The cross-process state lock (#1599): an exclusive `flock` on `<data_dir>/state.lock`.
//!
//! Decrypting consumes MLS secret-tree keys and encrypting advances this device's own ratchet,
//! and both are read-modify-writes of `mls_kv`. Two processes doing that to the same group at
//! once (the app and a notification extension) would each write back a state the other had
//! already advanced past: a rollback, the #1571 class of bug, and on the send side a reused
//! nonce. The in-process MLS mutexes cannot see another process, so every MLS-mutating step also
//! holds this lock.
//!
//! The lock lives on an open file description, so the OS drops it when the holder exits or is
//! killed; a crashed extension can never wedge the app. `flock` locks two descriptors of one
//! process against each other too, so an in-process headless fetch (Android) is excluded the
//! same way a second process is.
//!
//! Two shapes:
//! - [`StateLock`]: one exclusive hold, for the headless fetch, which holds it for its whole run.
//! - [`SharedStateLock`]: the app's. Many concurrent tasks of one `AppState` share a single
//!   hold (a count), so nested MLS sections in one process never wait on themselves; the hold
//!   is released when the last section ends.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The lock file's name inside the data dir.
pub const LOCK_FILE: &str = "state.lock";

/// How often a waiter retries a held lock. `flock` has no timed wait, and a blocking wait could
/// not be abandoned at a deadline.
const RETRY: Duration = Duration::from_millis(25);

/// How long the app waits for a notification extension to finish before an MLS step gives up.
///
/// Longer than any extension may run (iOS stops a Notification Service Extension at ~30 s), so in
/// practice this only fires if something is badly wrong; the step then fails and is retried
/// later rather than racing the other process.
pub const APP_WAIT: Duration = Duration::from_secs(45);

#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// Another holder kept the lock past the deadline.
    #[error("the state lock is held by another process")]
    Busy,
    #[error("state lock: {0}")]
    Io(#[from] std::io::Error),
}

impl From<LockError> for crate::error::Error {
    fn from(e: LockError) -> Self {
        crate::error::Error::Other(anyhow::anyhow!("{e}"))
    }
}

/// An exclusive hold on `<data_dir>/state.lock`. Released on drop (the descriptor closes).
#[derive(Debug)]
pub struct StateLock {
    _file: File,
}

impl StateLock {
    /// One non-blocking attempt. `Ok(None)` when someone else holds it.
    pub fn try_acquire_in(data_dir: &Path) -> Result<Option<Self>, LockError> {
        crate::private_fs::create_dir_all(data_dir)?;
        let path = data_dir.join(LOCK_FILE);
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        if sys::try_lock_exclusive(&file)? {
            Ok(Some(Self { _file: file }))
        } else {
            Ok(None)
        }
    }

    /// Retry until `deadline`, sleeping the thread between attempts. For non-async hosts.
    pub fn acquire_blocking_in(data_dir: &Path, timeout: Duration) -> Result<Self, LockError> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(lock) = Self::try_acquire_in(data_dir)? {
                return Ok(lock);
            }
            if Instant::now() >= deadline {
                return Err(LockError::Busy);
            }
            std::thread::sleep(RETRY);
        }
    }

    /// Retry until `deadline` without blocking the runtime.
    pub async fn try_acquire_until_in(data_dir: &Path, deadline: Instant) -> Result<Self, LockError> {
        loop {
            if let Some(lock) = Self::try_acquire_in(data_dir)? {
                return Ok(lock);
            }
            if Instant::now() >= deadline {
                return Err(LockError::Busy);
            }
            tokio::time::sleep(RETRY).await;
        }
    }

    /// [`Self::acquire_blocking_in`] on this process's data dir.
    pub fn acquire_blocking(timeout: Duration) -> Result<Self, LockError> {
        Self::acquire_blocking_in(&crate::db::local::dirs_path(), timeout)
    }

    /// [`Self::try_acquire_until_in`] on this process's data dir.
    pub async fn try_acquire_until(deadline: Instant) -> Result<Self, LockError> {
        Self::try_acquire_until_in(&crate::db::local::dirs_path(), deadline).await
    }
}

#[cfg(unix)]
mod sys {
    use std::fs::File;
    use std::os::fd::AsRawFd;

    /// `flock(LOCK_EX | LOCK_NB)`: `Ok(false)` when another descriptor holds it.
    pub fn try_lock_exclusive(file: &File) -> std::io::Result<bool> {
        loop {
            // SAFETY: a valid, open descriptor owned by `file` for the duration of the call.
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Ok(true);
            }
            let err = std::io::Error::last_os_error();
            match err.raw_os_error() {
                Some(libc::EWOULDBLOCK) => return Ok(false),
                Some(libc::EINTR) => continue,
                _ => return Err(err),
            }
        }
    }
}

// Windows runs one Pollis process per data dir and has no notification extension; the lock is
// only ever contended on mobile. Always granted there.
#[cfg(not(unix))]
mod sys {
    pub fn try_lock_exclusive(_file: &std::fs::File) -> std::io::Result<bool> {
        Ok(true)
    }
}

/// Who takes the cross-process lock for an `AppState`'s MLS sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockPolicy {
    /// The app: every MLS section takes (a share of) the lock.
    PerSection,
    /// The headless fetch: it already holds a [`StateLock`] for its whole run, and a second
    /// descriptor would wait on that first one forever.
    HeldByCaller,
}

#[derive(Default)]
struct Shared {
    holders: usize,
    lock: Option<StateLock>,
}

/// The app's refcounted hold. One per `AppState` (so one descriptor per `AppState`).
pub struct SharedStateLock {
    policy: LockPolicy,
    inner: Mutex<Shared>,
}

impl SharedStateLock {
    pub fn new(policy: LockPolicy) -> Arc<Self> {
        Arc::new(Self {
            policy,
            inner: Mutex::new(Shared::default()),
        })
    }

    pub fn policy(&self) -> LockPolicy {
        self.policy
    }

    /// Join the hold, taking the file lock if nobody in this `AppState` has it yet. Waits up to
    /// `wait` for another process.
    ///
    /// Never holds the inner mutex across an await, so a waiter never blocks a runtime thread
    /// another section of this process needs.
    pub async fn acquire(
        self: &Arc<Self>,
        data_dir: &Path,
        wait: Duration,
    ) -> Result<SharedStateGuard, LockError> {
        if self.policy == LockPolicy::HeldByCaller {
            return Ok(SharedStateGuard { owner: None });
        }
        let mut deadline = Instant::now() + wait;
        let mut logged = false;
        loop {
            {
                let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
                // Joining an existing hold is always allowed, even while suspending: the joiner
                // may be nested inside the very section the suspend is waiting to drain. A hold
                // whose file lock was force-released (#1599, `force_release`) is not joined: it
                // no longer excludes anyone, so the file lock is taken again first.
                if inner.holders > 0 && inner.lock.is_some() {
                    inner.holders += 1;
                    return Ok(SharedStateGuard {
                        owner: Some(self.clone()),
                    });
                }
                if !suspend::is_suspending() {
                    if let Some(lock) = StateLock::try_acquire_in(data_dir)? {
                        inner.lock = Some(lock);
                        inner.holders += 1;
                        suspend::hold_started();
                        return Ok(SharedStateGuard {
                            owner: Some(self.clone()),
                        });
                    }
                }
            }
            if suspend::is_suspending() {
                // Going to the background: start no new hold until the host says we are back.
                // Time spent here is not time another process kept the lock.
                deadline = Instant::now() + wait;
                tokio::time::sleep(RETRY).await;
                continue;
            }
            if !logged {
                crate::diag!("[state-lock] another process holds {LOCK_FILE}; waiting");
                logged = true;
            }
            if Instant::now() >= deadline {
                crate::diag!("[state-lock] gave up after {}s", wait.as_secs());
                return Err(LockError::Busy);
            }
            tokio::time::sleep(RETRY).await;
        }
    }

    /// How many sections currently share the hold. Tests only.
    #[cfg(test)]
    pub fn holders(&self) -> usize {
        self.inner.lock().unwrap().holders
    }

    /// Drop the file lock now, whoever still shares the hold (#1599: the app's background time
    /// is expiring and it must not be suspended holding it). Sections still running lose their
    /// exclusion; the next acquisition takes the file lock again. Never blocks on the file.
    pub fn force_release(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        if inner.lock.take().is_some() {
            suspend::hold_ended();
        }
    }

    fn release(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.holders = inner.holders.saturating_sub(1);
        if inner.holders == 0 && inner.lock.take().is_some() {
            suspend::hold_ended();
        }
    }
}

/// The app going to the background (#1599, iOS `0xdead10cc`).
///
/// iOS kills a suspended app that still holds a file lock in an App Group container. The host
/// takes a [`ticket`](suspend::ticket) the moment the app enters the background, then (off the
/// main thread, inside a background-task assertion) calls [`begin`](suspend::begin) with it; it
/// calls [`end`](suspend::end) when the app returns. Between `begin` and `end` no NEW hold
/// starts, and `begin` waits for the holds in flight to finish.
///
/// The ticket closes the race between a slow `begin` and a quick return to the foreground:
/// every `end` advances the resume count, and a `begin` whose ticket is older than the current
/// count does nothing, so a stale `begin` can never leave the app gated after it came back.
///
/// A hold in flight may be doing network I/O (each DS request is bounded by the shared
/// client's 10 s connect and 30 s read-stall timeouts), so `begin` waits with a deadline.
pub mod suspend {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    struct Gate {
        /// Advanced by every [`end`].
        resumes: u64,
        /// Closed by a current [`begin`], opened by [`end`].
        closed: bool,
    }

    static GATE: Mutex<Gate> = Mutex::new(Gate {
        resumes: 0,
        closed: false,
    });
    /// App-side holds of `state.lock` currently open, across every `AppState` in the process.
    static OPEN_HOLDS: AtomicUsize = AtomicUsize::new(0);

    fn gate() -> std::sync::MutexGuard<'static, Gate> {
        GATE.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(super) fn is_suspending() -> bool {
        gate().closed
    }

    pub(super) fn hold_started() {
        OPEN_HOLDS.fetch_add(1, Ordering::SeqCst);
    }

    pub(super) fn hold_ended() {
        OPEN_HOLDS.fetch_sub(1, Ordering::SeqCst);
    }

    /// Taken when the app enters the background, before anything asynchronous.
    pub fn ticket() -> u64 {
        gate().resumes
    }

    /// Whether `ticket` is still the suspension in force (no `end` since it was taken).
    pub fn is_current(ticket: u64) -> bool {
        let g = gate();
        g.closed && g.resumes == ticket
    }

    /// Close the gate for `ticket`, then block up to `timeout` for the open holds to end.
    /// `true` when the gate is closed for this ticket and no hold is open.
    pub fn begin(ticket: u64, timeout: Duration) -> bool {
        {
            let mut g = gate();
            if g.resumes != ticket {
                // The app came back before this ran.
                return false;
            }
            g.closed = true;
        }
        let deadline = Instant::now() + timeout;
        loop {
            if !is_current(ticket) {
                return false;
            }
            if OPEN_HOLDS.load(Ordering::SeqCst) == 0 {
                return true;
            }
            if Instant::now() >= deadline {
                crate::diag!("[state-lock] still held as the app goes to the background");
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Back in the foreground: holds may start again, and any `begin` not yet run is void.
    pub fn end() {
        let mut g = gate();
        g.resumes = g.resumes.wrapping_add(1);
        g.closed = false;
    }

    /// Open app-side holds. Diagnostics and tests.
    pub fn open_holds() -> usize {
        OPEN_HOLDS.load(Ordering::SeqCst)
    }
}

/// One section's share of a [`SharedStateLock`]. The file lock is released with the last share.
pub struct SharedStateGuard {
    owner: Option<Arc<SharedStateLock>>,
}

impl Drop for SharedStateGuard {
    fn drop(&mut self) {
        if let Some(owner) = self.owner.take() {
            owner.release();
        }
    }
}

/// The lock file for `data_dir`. Diagnostics and tests.
pub fn lock_path(data_dir: &Path) -> PathBuf {
    data_dir.join(LOCK_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    /// Whether the lock can be taken once its holders drop. Retries briefly: a sibling test
    /// that spawns a process forks this one, and until that child execs it shares every open
    /// descriptor, so a just-dropped `flock` can stay held for a moment.
    fn released(data_dir: &Path) -> bool {
        StateLock::acquire_blocking_in(data_dir, Duration::from_secs(5)).is_ok()
    }

    #[test]
    fn a_second_descriptor_is_refused_while_the_first_holds_it() {
        let d = dir();
        let first = StateLock::try_acquire_in(d.path()).unwrap().expect("free");
        assert!(StateLock::try_acquire_in(d.path()).unwrap().is_none());
        drop(first);
        assert!(released(d.path()));
    }

    #[test]
    fn a_blocking_acquire_times_out_as_busy() {
        let d = dir();
        let _held = StateLock::try_acquire_in(d.path()).unwrap().unwrap();
        let started = Instant::now();
        let err = StateLock::acquire_blocking_in(d.path(), Duration::from_millis(120)).unwrap_err();
        assert!(matches!(err, LockError::Busy));
        assert!(started.elapsed() >= Duration::from_millis(120));
    }

    #[test]
    fn a_killed_holder_releases_the_lock() {
        // A child process takes the lock and is killed; the OS must release it.
        let d = dir();
        let path = lock_path(d.path());
        std::fs::write(&path, b"").unwrap();
        let mut child = std::process::Command::new("perl")
            .arg("-e")
            .arg("use Fcntl ':flock'; open(my $f, '<', $ARGV[0]) or die; flock($f, LOCK_EX) or die; print \"locked\\n\"; $|=1; sleep 60;")
            .arg(&path)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("perl is available on every dev and CI host");
        let mut out = child.stdout.take().unwrap();
        let mut buf = [0u8; 7];
        std::io::Read::read_exact(&mut out, &mut buf).unwrap();
        assert_eq!(&buf, b"locked\n");
        assert!(StateLock::try_acquire_in(d.path()).unwrap().is_none(), "the child holds it");
        child.kill().unwrap();
        child.wait().unwrap();
        StateLock::acquire_blocking_in(d.path(), Duration::from_secs(5))
            .expect("a killed holder's lock is released by the OS");
    }

    #[tokio::test]
    async fn sections_of_one_state_share_a_single_hold() {
        let d = dir();
        let shared = SharedStateLock::new(LockPolicy::PerSection);
        let a = shared.acquire(d.path(), Duration::from_millis(50)).await.unwrap();
        let b = shared.acquire(d.path(), Duration::from_millis(50)).await.unwrap();
        assert_eq!(shared.holders(), 2);
        assert!(StateLock::try_acquire_in(d.path()).unwrap().is_none());
        drop(a);
        assert!(StateLock::try_acquire_in(d.path()).unwrap().is_none(), "b still holds");
        drop(b);
        assert!(released(d.path()), "released with the last share");
    }

    #[tokio::test]
    async fn the_app_waits_for_a_headless_holder_then_proceeds() {
        let d = dir();
        let headless = StateLock::try_acquire_in(d.path()).unwrap().unwrap();
        let shared = SharedStateLock::new(LockPolicy::PerSection);
        let path = d.path().to_path_buf();
        let s2 = shared.clone();
        let waiter = tokio::spawn(async move { s2.acquire(&path, Duration::from_secs(5)).await.map(|_| ()) });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!waiter.is_finished(), "the app waits while the extension holds the lock");
        drop(headless);
        waiter.await.unwrap().expect("acquired once the extension let go");
    }

    /// One test for the whole suspend gate: it is process-global, so its cases must not run in
    /// parallel with each other.
    #[tokio::test(flavor = "multi_thread")]
    async fn going_to_the_background_drains_holds_and_survives_a_quick_return() {
        let d = dir();
        let shared = SharedStateLock::new(LockPolicy::PerSection);

        // An in-flight hold is drained.
        let held = shared.acquire(d.path(), Duration::from_secs(1)).await.unwrap();
        assert!(suspend::open_holds() >= 1);
        let releaser = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(held);
        });
        let t = suspend::ticket();
        let drained = tokio::task::spawn_blocking(move || suspend::begin(t, Duration::from_secs(5)))
            .await
            .unwrap();
        releaser.await.unwrap();
        assert!(drained, "the in-flight hold ended");
        assert!(released(d.path()), "nothing holds the file");

        // While suspended a new hold waits, without timing out, until the app is back.
        let s2 = shared.clone();
        let path = d.path().to_path_buf();
        let waiter = tokio::spawn(async move {
            s2.acquire(&path, Duration::from_millis(50)).await.map(|_| ())
        });
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(!waiter.is_finished(), "no new hold while suspended");
        suspend::end();
        waiter.await.unwrap().expect("acquired after resume");

        // A quick background -> foreground: the background's `begin` runs only after the
        // return. It must do nothing, or the app would stay gated for good.
        let stale = suspend::ticket();
        suspend::end();
        assert!(!suspend::begin(stale, Duration::from_millis(10)));
        assert!(!suspend::is_suspending(), "a stale begin never closes the gate");
        shared
            .acquire(d.path(), Duration::from_millis(200))
            .await
            .expect("holds still start");

        // Two backgrounds in a row: the older ticket cannot undo the newer suspension.
        let old = suspend::ticket();
        suspend::end();
        let new = suspend::ticket();
        assert!(suspend::begin(new, Duration::from_millis(10)));
        assert!(!suspend::begin(old, Duration::from_millis(10)));
        assert!(suspend::is_current(new), "the newer suspension stands");
        suspend::end();
        assert!(!suspend::is_suspending());
    }

    #[tokio::test]
    async fn a_force_release_frees_the_file_and_the_next_section_retakes_it() {
        let d = dir();
        let shared = SharedStateLock::new(LockPolicy::PerSection);
        let in_flight = shared.acquire(d.path(), Duration::from_millis(50)).await.unwrap();
        shared.force_release();
        let other = StateLock::acquire_blocking_in(d.path(), Duration::from_secs(5))
            .expect("the file lock is gone although a section still runs");
        // A new section does not join the released hold; it waits for the file lock.
        let s2 = shared.clone();
        let path = d.path().to_path_buf();
        let next = tokio::spawn(async move {
            s2.acquire(&path, Duration::from_secs(5)).await.map(drop)
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!next.is_finished(), "it must not run unexcluded");
        drop(other);
        next.await.unwrap().expect("retook the file lock");
        drop(in_flight);
        assert!(released(d.path()), "all released");
    }

    #[tokio::test]
    async fn a_caller_held_policy_never_touches_the_file() {
        let d = dir();
        let _outer = StateLock::try_acquire_in(d.path()).unwrap().unwrap();
        let shared = SharedStateLock::new(LockPolicy::HeldByCaller);
        shared
            .acquire(d.path(), Duration::from_millis(10))
            .await
            .expect("does not wait on the caller's own hold");
    }
}
