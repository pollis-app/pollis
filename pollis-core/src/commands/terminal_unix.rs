//! In-app terminal pane backend: the user's `$SHELL` behind a Unix PTY, with a
//! reader thread and an aggregator thread coalescing output into a [`RawSink`]
//! (raw bytes, #282). Sessions live for the app's lifetime; dropping a
//! [`PtySession`] kills its child.
//!
//! Not `portable-pty`: its post-fork `close_random_fds` sweep closes fds Rust
//! owns, which aborts with "FD ownership violation". Our own `pre_exec` spawn
//! skips it.
//!
//! Backpressure: xterm acks rendered bytes via [`terminal_ack`]; the aggregator
//! parks above [`HIGH_WATERMARK`] so a runaway producer can't balloon xterm's
//! unbounded write buffer.

use std::fs::File;
use std::io::{Read, Write};
use std::mem::take;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Condvar, Mutex};

use nix::pty::{openpty, Winsize};

use crate::error::{Error, Result};
use crate::sink::RawSink;
use crate::state::AppState;

fn map_pty<E: std::fmt::Display>(e: E) -> Error {
    Error::Other(anyhow::anyhow!("PTY error: {e}"))
}

/// Largest coalesced message per sink send.
const MAX_BATCH: usize = 256 * 1024;

/// Aggregator parks once this many un-acked bytes are outstanding...
const HIGH_WATERMARK: usize = 1024 * 1024;

/// ...and resumes once outstanding drops back below this.
const LOW_WATERMARK: usize = 256 * 1024;

/// One session's flow-control credit. `closed` lets close/drop wake a parked
/// aggregator so its thread exits instead of leaking until EOF.
struct FlowControl {
    in_flight: AtomicUsize,
    closed: AtomicU64,
    lock: Mutex<()>,
    cv: Condvar,
}

impl FlowControl {
    fn new() -> Self {
        Self {
            in_flight: AtomicUsize::new(0),
            closed: AtomicU64::new(0),
            lock: Mutex::new(()),
            cv: Condvar::new(),
        }
    }

    /// Park until outstanding bytes fall below the low watermark (or the session
    /// closes), then reserve `n`.
    fn gate(&self, n: usize) {
        if self.in_flight.load(Ordering::Acquire) > HIGH_WATERMARK {
            let mut guard = self.lock.lock().unwrap();
            while self.in_flight.load(Ordering::Acquire) > LOW_WATERMARK
                && self.closed.load(Ordering::Acquire) == 0
            {
                guard = self.cv.wait(guard).unwrap();
            }
        }
        self.in_flight.fetch_add(n, Ordering::AcqRel);
    }

    fn ack(&self, n: usize) {
        self.in_flight.fetch_sub(n, Ordering::AcqRel);
        let _g = self.lock.lock().unwrap();
        self.cv.notify_all();
    }

    fn close(&self) {
        self.closed.store(1, Ordering::Release);
        let _g = self.lock.lock().unwrap();
        self.cv.notify_all();
    }
}

pub struct PtySession {
    /// Kept for [`terminal_resize`]'s `TIOCSWINSZ` ioctl.
    master: OwnedFd,
    writer: Box<dyn Write + Send>,
    child: Child,
    flow: Arc<FlowControl>,
}

impl Drop for PtySession {
    fn drop(&mut self) {
        self.flow.close();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn default_shell() -> String {
    if let Ok(s) = std::env::var("SHELL") {
        if !s.is_empty() {
            return s;
        }
    }
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    }
    #[cfg(not(windows))]
    {
        "/bin/bash".to_string()
    }
}

/// Spawn a shell behind a PTY, streaming its output to `on_output` until EOF.
/// Returns the session id.
pub async fn terminal_open(
    rows: u16,
    cols: u16,
    on_output: Arc<dyn RawSink>,
    state: &Arc<AppState>,
) -> Result<String> {
    // Must come first: off is the default and the fail-safe.
    crate::commands::terminal_gate::require_terminal_enabled()?;

    let winsize = Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // The master stays with us; the slave is dup2'd into the child's stdio in
    // `pre_exec`, then dropped in the parent after spawn.
    let pty = openpty(Some(&winsize), None).map_err(map_pty)?;
    let master_fd = pty.master;
    let slave_fd = pty.slave;

    // Login shell so `.zprofile`/`.bash_profile` run and the user's full PATH is
    // available, not just what launchd gives GUI apps on macOS.
    let mut cmd = Command::new(default_shell());
    cmd.arg("-l");
    cmd.env("TERM", "xterm-256color");
    if let Ok(dir) = std::env::var("HOME") {
        if !dir.is_empty() {
            cmd.current_dir(dir);
        }
    }
    // The child must not inherit the host process's stdio; `pre_exec` replaces
    // 0/1/2 with the slave anyway.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // Runs between fork and exec: async-signal-safe libc calls only, no allocation
    // or Rust I/O. setsid, dup2 the slave onto 0/1/2, make it the controlling tty,
    // close the original slave fd.
    let slave_raw = slave_fd.as_raw_fd();
    unsafe {
        cmd.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            for target in 0..3 {
                if libc::dup2(slave_raw, target) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            if libc::ioctl(slave_raw, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            libc::close(slave_raw);
            Ok(())
        });
    }

    let child = cmd.spawn().map_err(map_pty)?;
    // Drop the parent's slave fd so the reader sees EOF once the shell exits.
    drop(slave_fd);

    // Separate dup'd fds for the blocking reader, the writer, and the resize ioctl.
    let reader_fd = master_fd.try_clone().map_err(map_pty)?;
    let mut reader = File::from(reader_fd);
    let writer_fd = master_fd.try_clone().map_err(map_pty)?;
    let writer: Box<dyn Write + Send> = Box::new(File::from(writer_fd));

    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed).to_string();

    let flow = Arc::new(FlowControl::new());

    // PTY reads block, so the reader has its own OS thread and only forwards
    // chunks; dropping `tx` signals Disconnected.
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::Builder::new()
        .name(format!("pty-rd-{id}"))
        .spawn(move || {
            let mut buf = [0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .map_err(map_pty)?;

    // Aggregator: coalesces what is already queued (no timer, so keystrokes add no
    // latency), gates on flow control, then sinks.
    let flow_agg = Arc::clone(&flow);
    std::thread::Builder::new()
        .name(format!("pty-agg-{id}"))
        .spawn(move || {
            while let Ok(first) = rx.recv() {
                let mut buf = first;
                let mut disconnected = false;
                while buf.len() < MAX_BATCH {
                    match rx.try_recv() {
                        Ok(c) => buf.extend(c),
                        Err(TryRecvError::Empty) => break,
                        Err(TryRecvError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }
                let n = buf.len();
                flow_agg.gate(n);
                if on_output.send(take(&mut buf)).is_err() {
                    break;
                }
                if disconnected {
                    break;
                }
            }
        })
        .map_err(map_pty)?;

    let session = PtySession {
        master: master_fd,
        writer,
        child,
        flow,
    };
    state.terminals.lock().await.insert(id.clone(), session);
    Ok(id)
}

/// Credit rendered bytes back to the session's flow-control window (from
/// xterm.js's `write` callback). Unknown ids are a no-op.
pub async fn terminal_ack(
    terminal_id: String,
    bytes: usize,
    state: &Arc<AppState>,
) -> Result<()> {
    let map = state.terminals.lock().await;
    if let Some(session) = map.get(&terminal_id) {
        session.flow.ack(bytes);
    }
    Ok(())
}

/// Forward keystrokes to the shell. Borrowed so the shim can pass the IPC raw
/// body straight through (#282). Unknown ids are a no-op.
pub async fn terminal_write(
    terminal_id: &str,
    data: &[u8],
    state: &Arc<AppState>,
) -> Result<()> {
    let mut map = state.terminals.lock().await;
    if let Some(session) = map.get_mut(terminal_id) {
        session.writer.write_all(data).map_err(map_pty)?;
        let _ = session.writer.flush();
    }
    Ok(())
}

/// Propagate a resize to the PTY (delivers SIGWINCH).
pub async fn terminal_resize(
    terminal_id: String,
    rows: u16,
    cols: u16,
    state: &Arc<AppState>,
) -> Result<()> {
    let map = state.terminals.lock().await;
    if let Some(session) = map.get(&terminal_id) {
        let ws = Winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // `TIOCSWINSZ` is c_ulong on Linux and c_int on BSD/macOS; `as _` covers both.
        let rc = unsafe {
            libc::ioctl(
                session.master.as_raw_fd(),
                libc::TIOCSWINSZ as _,
                &ws as *const Winsize,
            )
        };
        if rc == -1 {
            return Err(map_pty(std::io::Error::last_os_error()));
        }
    }
    Ok(())
}

/// Kill the PTY and drop the session. Idempotent.
pub async fn terminal_close(terminal_id: String, state: &Arc<AppState>) -> Result<()> {
    state.terminals.lock().await.remove(&terminal_id);
    Ok(())
}
