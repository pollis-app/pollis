//! On-device diagnostics log (#1305).
//!
//! Every [`crate::diag!`] line goes to stderr and, after [`init`], to owner-only `logs/pollis.log` (rotated at [`MAX_FILE_BYTES`]); nothing is sent anywhere.
//!
//! Every line passes [`redact`] first; that is the backstop, and `tests/diag_discipline.rs` enforces call-site discipline.
//!
//! Only a host calls [`init`]; tests, CLI and TUI write stderr only.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Size at which `pollis.log` rotates to `pollis.log.1`.
pub const MAX_FILE_BYTES: u64 = 512 * 1024;

const FILE_NAME: &str = "pollis.log";
const PREV_FILE_NAME: &str = "pollis.log.1";

struct Sink {
    dir: PathBuf,
    file: File,
    len: u64,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);

/// Open (or reopen) the log under `dir`, owner-only, and install a panic hook that records panics.
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

/// Backing function of [`crate::diag!`]. Never fails or panics.
pub fn emit(args: std::fmt::Arguments<'_>) {
    let line = redact(&args.to_string());
    eprintln!("{line}");
    #[cfg(feature = "test-harness")]
    loss_tap::observe(&line);
    write_line(&mut lock(), &line);
}

/// Test-harness only (#1524): `[loss]` lines since the last [`loss_tap::take`].
#[cfg(feature = "test-harness")]
pub mod loss_tap {
    use std::sync::Mutex;

    static LINES: Mutex<Vec<String>> = Mutex::new(Vec::new());

    /// Accepted loss (1): sealed before this device joined; logged but not avoidable.
    const PRE_JOIN: &str = "sealed before this device joined";

    pub(super) fn observe(line: &str) {
        if line.starts_with("[loss]") && !line.contains(PRE_JOIN) {
            LINES
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(line.to_string());
        }
    }

    pub fn take() -> Vec<String> {
        std::mem::take(&mut *LINES.lock().unwrap_or_else(|e| e.into_inner()))
    }
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

/// The whole log (rotated file first). Empty when no host has called [`init`].
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

/// Strip the home directory, then everything [`pollis_api::redact::scrub`] removes.
pub fn redact(line: &str) -> String {
    pollis_api::redact::scrub(&redact_home(line))
}

fn lock() -> std::sync::MutexGuard<'static, Option<Sink>> {
    // Every write is a single `write_all`, so a poisoned lock is still safe.
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
            // `try_lock`: a panic while this thread holds the sink would deadlock here.
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

/// Write a line to stderr and, once [`diag::init`] ran, to the diagnostics log.
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
