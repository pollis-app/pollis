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
        let deadline = Instant::now() + wait;
        let mut logged = false;
        loop {
            {
                let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
                if inner.holders > 0 {
                    inner.holders += 1;
                    return Ok(SharedStateGuard {
                        owner: Some(self.clone()),
                    });
                }
                if let Some(lock) = StateLock::try_acquire_in(data_dir)? {
                    inner.lock = Some(lock);
                    inner.holders = 1;
                    return Ok(SharedStateGuard {
                        owner: Some(self.clone()),
                    });
                }
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

    fn release(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|p| p.into_inner());
        inner.holders = inner.holders.saturating_sub(1);
        if inner.holders == 0 {
            inner.lock = None;
        }
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

    #[test]
    fn a_second_descriptor_is_refused_while_the_first_holds_it() {
        let d = dir();
        let first = StateLock::try_acquire_in(d.path()).unwrap().expect("free");
        assert!(StateLock::try_acquire_in(d.path()).unwrap().is_none());
        drop(first);
        assert!(StateLock::try_acquire_in(d.path()).unwrap().is_some());
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
        assert!(StateLock::try_acquire_in(d.path()).unwrap().is_some(), "released with the last share");
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
