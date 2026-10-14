//! On-device diagnostics log (#1305).
//!
//! **Why:** a first-device signup failed for a tester after the DS had already
//! accepted the code, and nothing anywhere could say which step broke — the
//! core's progress lines went to stderr, which nobody sees in a packaged app.
//!
//! **What:** every [`crate::diag!`] line goes to stderr (unchanged from the
//! `eprintln!` it replaces) and, once a host calls [`init`], to a small
//! owner-only file under the data directory: `logs/pollis.log`, rotated to
//! `logs/pollis.log.1` at [`MAX_FILE_BYTES`], so the whole log never exceeds
//! twice that. Nothing is sent anywhere — the user copies it out through
//! `get_diagnostics` ("Copy diagnostics" in the app) if and when they choose.
//!
//! **Not PII:** every line passes [`redact`] before it is written: the home
//! directory becomes `~`, email addresses keep only their domain, IP addresses
//! go, and any long token-shaped run (session tokens, keys, signatures,
//! loopback media URLs' secrets) is replaced by its length — the same
//! [`pollis_api::redact::scrub`] the DS applies to its failed-request log. That is the backstop, not the
//! policy: call sites must not log message content, file names, display names,
//! keys or tokens in the first place, and `tests/diag_discipline.rs` fails the
//! build on the obvious ways to do so.
//!
//! **Only a host opens the file.** Tests, the CLI and the TUI never call [`init`],
//! so they write stderr only — a test run must never append to a real data
//! directory's log.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Size at which `pollis.log` rotates to `pollis.log.1`. The log on disk is
/// therefore at most twice this.
pub const MAX_FILE_BYTES: u64 = 512 * 1024;

const FILE_NAME: &str = "pollis.log";
const PREV_FILE_NAME: &str = "pollis.log.1";

struct Sink {
    dir: PathBuf,
    file: File,
    len: u64,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);

/// Open (or reopen) the log under `dir`, creating it owner-only, and install a
/// panic hook that records the panic before the default hook runs. Idempotent
/// in effect: a second call re-points the sink at `dir`.
pub fn init(dir: &Path) -> std::io::Result<()> {
    crate::private_fs::create_dir_all(dir)?;
    let path = dir.join(FILE_NAME);
    let file = open_append(&path)?;
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    *lock() = Some(Sink { dir: dir.to_path_buf(), file, len });
    install_panic_hook();
    crate::diag!(
        "[diag] log opened — core {} on {}/{}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    Ok(())
}

/// Where [`init`] puts the log for a data directory.
pub fn log_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}

/// The backing function of [`crate::diag!`]. Never fails and never panics: a
/// log that cannot be written must not take the operation it describes down
/// with it.
pub fn emit(args: std::fmt::Arguments<'_>) {
    let line = redact(&args.to_string());
    eprintln!("{line}");
    write_line(&mut lock(), &line);
}

fn write_line(guard: &mut Option<Sink>, line: &str) {
    let Some(sink) = guard.as_mut() else {
        return;
    };
    let stamped = format!(
        "{} {line}\n",
        chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ")
    );
    if sink.len + stamped.len() as u64 > MAX_FILE_BYTES {
        rotate(sink);
    }
    if sink.file.write_all(stamped.as_bytes()).is_ok() {
        sink.len += stamped.len() as u64;
    }
}

/// The whole log — the rotated file, then the current one — for the user to
/// copy out. Empty when no host has called [`init`].
pub fn read_log() -> String {
    let dir = match lock().as_ref() {
        Some(sink) => sink.dir.clone(),
        None => return String::new(),
    };
    let mut out = String::new();
    for name in [PREV_FILE_NAME, FILE_NAME] {
        if let Ok(bytes) = std::fs::read(dir.join(name)) {
            out.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    out
}

/// Strip what must never sit in a log the user may hand to someone: the home
/// directory (it usually carries the account name), then everything
/// [`pollis_api::redact::scrub`] removes — email local parts, IP addresses,
/// long token-shaped runs.
pub fn redact(line: &str) -> String {
    pollis_api::redact::scrub(&redact_home(line))
}

fn lock() -> std::sync::MutexGuard<'static, Option<Sink>> {
    // A panic while holding the lock leaves the sink intact (every write is a
    // single `write_all`), so a poisoned lock is still safe to use.
    SINK.lock().unwrap_or_else(|e| e.into_inner())
}

fn open_append(path: &Path) -> std::io::Result<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let file = opts.open(path)?;
    crate::private_fs::restrict_open_handle(&file)?;
    Ok(file)
}

fn rotate(sink: &mut Sink) {
    let current = sink.dir.join(FILE_NAME);
    let _ = std::fs::rename(&current, sink.dir.join(PREV_FILE_NAME));
    if let Ok(file) = open_append(&current) {
        sink.file = file;
        sink.len = 0;
    }
}

fn install_panic_hook() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            // `try_lock`, never `lock`: a panic raised while this thread holds
            // the sink would otherwise deadlock inside its own hook.
            let line = redact(&format!("[panic] {info}"));
            match SINK.try_lock() {
                Ok(mut guard) => write_line(&mut guard, &line),
                Err(std::sync::TryLockError::Poisoned(e)) => {
                    write_line(&mut e.into_inner(), &line)
                }
                Err(std::sync::TryLockError::WouldBlock) => {}
            }
            previous(info);
        }));
    });
}

fn redact_home(line: &str) -> String {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    // A one-character "home" (`/`) would rewrite every path.
    if home.len() > 1 && line.contains(&home) {
        return line.replace(&home, "~");
    }
    line.to_string()
}

/// Write a line to stderr and, once a host has called [`diag::init`], to the
/// on-device diagnostics log. Same arguments as `eprintln!`. See [`crate::diag`]
/// for what may and may not be logged.
#[macro_export]
macro_rules! diag {
    () => {
        $crate::diag::emit(::std::format_args!(""))
    };
    ($($arg:tt)*) => {
        $crate::diag::emit(::std::format_args!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_scrubbed_before_it_is_written() {
        let out = redact("verify for alice@example.com from 203.0.113.7 failed");
        assert_eq!(out, "verify for ***@example.com from [redacted] failed");
    }

    #[test]
    fn the_file_rotates_and_reads_back_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let mut sink = Sink {
            dir: dir.path().to_path_buf(),
            file: open_append(&dir.path().join(FILE_NAME)).unwrap(),
            len: 0,
        };
        sink.file.write_all(b"old\n").unwrap();
        rotate(&mut sink);
        sink.file.write_all(b"new\n").unwrap();
        let prev = std::fs::read_to_string(dir.path().join(PREV_FILE_NAME)).unwrap();
        let cur = std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
        assert_eq!((prev.as_str(), cur.as_str()), ("old\n", "new\n"));
    }
}
