//! In-app terminal pane backend.
//!
//! Spawns the user's `$SHELL` behind a Unix PTY using `nix::pty::openpty` +
//! `std::process::Command::pre_exec`. Each session gets a dedicated OS
//! reader thread plus an aggregator thread that coalesce the master fd into
//! a [`RawSink`] of raw bytes (binary IPC, no serde — see issue #282). The
//! frontend (xterm.js) writes those bytes to the screen and forwards
//! keystrokes back through [`terminal_write`].
//!
//! Sessions are spawned on first activation and kept alive for the app's
//! lifetime; toggling the view away and back reattaches to the same PTY.
//! Dropping a [`PtySession`] kills its child, so process exit / `logout`
//! cleanup leaves no zombies.
//!
//! Why not `portable-pty`: its `posix::spawn_command` does a post-fork
//! `close_random_fds` sweep that walks `/proc/self/fd` and `close(2)`s every
//! file descriptor it doesn't recognise. Under Electron the renderer
//! process has thousands of FDs from the surrounding host, and Rust's I/O
//! safety guard treats any close of an FD it owns (stdio, OwnedFds in
//! tokio's reactor, etc.) as undefined behaviour and aborts with "Crashing
//! due to FD ownership violation". Building our own spawn path with
//! `pre_exec` skips the sweep entirely — same approach Tauri used before
//! their portable_pty wrapper.
//!
//! Backpressure: bytes handed to xterm are credited via [`terminal_ack`]
//! (fired from xterm's write callback when a chunk is actually rendered).
//! The aggregator parks while `in_flight` exceeds [`HIGH_WATERMARK`] so a
//! runaway producer (`cat bigfile`, chatty build) can't balloon xterm's
//! unbounded internal write buffer on weak/software-rendered devices.

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

/// Largest coalesced message handed to one sink send. The reader fills the
/// mpsc faster than the aggregator drains under bulk output, so try_recv
/// naturally batches many 64 KiB reads up to this bound.
const MAX_BATCH: usize = 256 * 1024;

/// Aggregator parks once this many un-acked bytes are outstanding.
const HIGH_WATERMARK: usize = 1024 * 1024;

/// ...and resumes once outstanding drops back below this.
const LOW_WATERMARK: usize = 256 * 1024;

/// Shared credit counter + parking primitive for one session's flow
/// control. `terminal_ack` decrements and notifies; the aggregator parks
/// on the condvar while over the high watermark. `closed` lets
/// `terminal_close` / drop wake a parked aggregator so its thread exits
/// instead of leaking until EOF.
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

    /// Park the aggregator until outstanding bytes fall below the low
    /// watermark (or the session is closed), then reserve `n` bytes.
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

    /// Credit `n` acked bytes back and wake a parked aggregator.
    fn ack(&self, n: usize) {
        self.in_flight.fetch_sub(n, Ordering::AcqRel);
        let _g = self.lock.lock().unwrap();
        self.cv.notify_all();
    }

    /// Mark closed and wake any parked aggregator so its thread exits.
    fn close(&self) {
        self.closed.store(1, Ordering::Release);
        let _g = self.lock.lock().unwrap();
        self.cv.notify_all();
    }
}

pub struct PtySession {
    /// Parent-side master fd, owned. Kept so [`terminal_resize`] can
    /// `ioctl(TIOCSWINSZ)` against the right pty without racing the reader
    /// thread's clone.
    master: OwnedFd,
    writer: Box<dyn Write + Send>,
    child: Child,
    flow: Arc<FlowControl>,
}

impl Drop for PtySession {
    fn drop(&mut self) {
        // Wake a parked aggregator so its thread isn't stuck until EOF.
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

/// Spawn a shell behind a PTY. Bytes from the shell are pushed to
/// `on_output` until EOF (shell exit) or the sink detaches. Returns the
/// session id the frontend passes back to `terminal_write` / `_resize` /
/// `_close`.
pub async fn terminal_open(
    rows: u16,
    cols: u16,
    on_output: Arc<dyn RawSink>,
    state: &Arc<AppState>,
) -> Result<String> {
    let winsize = Winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // openpty(3) gives us an owned master + slave pair. Both are OwnedFd
    // so Drop will close them — the master stays with us, the slave is
    // dup2'd into the child's stdio in pre_exec then dropped in the parent
    // after spawn(). We never hit portable_pty's close_random_fds sweep.
    let pty = openpty(Some(&winsize), None).map_err(map_pty)?;
    let master_fd = pty.master;
    let slave_fd = pty.slave;

    // Build the shell command. login-shell `-l` so `.zprofile` /
    // `.bash_profile` runs and the user's full PATH (Homebrew
    // `eval "$(brew shellenv)"`, /usr/local/bin, nvm, fnm, asdf, etc.) is
    // available — not just whatever launchd inherits for GUI apps on
    // macOS. Terminal.app, iTerm2, and Warp all do this for the same
    // reason. zsh and bash both honor `-l`.
    let mut cmd = Command::new(default_shell());
    cmd.arg("-l");
    cmd.env("TERM", "xterm-256color");
    if let Ok(dir) = std::env::var("HOME") {
        if !dir.is_empty() {
            cmd.current_dir(dir);
        }
    }
    // The child must NOT inherit our (parent) stdio fds — those belong to
    // the host process (Electron renderer / Tauri shell). Stub them with
    // /dev/null-style Stdio::null() so std doesn't try to wire them up;
    // pre_exec then overwrites 0/1/2 with the pty slave anyway.
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // pre_exec runs in the forked child between fork() and execvp(). Only
    // async-signal-safe libc calls are allowed here — no allocations, no
    // Rust I/O. Sequence (post-fork, pre-exec):
    //   1. setsid()                              — new session for the shell
    //   2. dup2(slave, 0/1/2)                    — wire stdio to the pty
    //   3. ioctl(slave, TIOCSCTTY, 0)            — make slave the controlling tty
    //   4. close(slave)                          — still open as 0/1/2
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
            // Slave fd is still open at its original number on top of the
            // 0/1/2 dups; close it so only stdio references the pty.
            libc::close(slave_raw);
            Ok(())
        });
    }

    let child = cmd.spawn().map_err(map_pty)?;
    // Slave fd in the parent process can go away now — the child holds
    // its own dup'd copies in 0/1/2. Dropping makes the shell see EOF /
    // SIGHUP after it exits, which lets the reader loop terminate.
    drop(slave_fd);

    // Reader needs its own OwnedFd so it can do blocking reads while the
    // parent thread keeps writing through its own File handle. `try_clone`
    // is dup(2) under the hood — both fds reference the same pty master.
    let reader_fd = master_fd.try_clone().map_err(map_pty)?;
    let mut reader = File::from(reader_fd);
    // Writer is the original master fd wrapped as a File so
    // `Box<dyn Write + Send>` works unchanged downstream. Cloned into the
    // session as a second handle (we need master_fd for terminal_resize
    // too — File takes ownership otherwise).
    let writer_fd = master_fd.try_clone().map_err(map_pty)?;
    let writer: Box<dyn Write + Send> = Box::new(File::from(writer_fd));

    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed).to_string();

    let flow = Arc::new(FlowControl::new());

    // PTY reads are blocking, so the reader lives on a dedicated OS thread.
    // It does no sinking — just forwards 64 KiB chunks over an mpsc to the
    // aggregator. Dropping `tx` on EOF/err signals Disconnected downstream.
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

    // Aggregator: coalesces queued reads into one ≤MAX_BATCH message with
    // no timer (try_recv drains only what's already queued, so interactive
    // keystrokes add zero latency), gates on flow control, then sinks.
    let flow_agg = Arc::clone(&flow);
    std::thread::Builder::new()
        .name(format!("pty-agg-{id}"))
        .spawn(move || {
            loop {
                // Blocks; returns immediately for interactive output.
                let first = match rx.recv() {
                    Ok(c) => c,
                    Err(_) => break,
                };
                let mut buf = first;
                let mut disconnected = false;
                while buf.len() < MAX_BATCH {
                    match rx.try_recv() {
                        Ok(c) => buf.extend(c),
                        // Nothing queued -> send now (low latency).
                        Err(TryRecvError::Empty) => break,
                        // EOF: flush tail below, then exit after send.
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

/// Credit acked bytes back toward the per-session flow-control window so
/// the aggregator can resume producing. Fired from xterm.js's `write`
/// callback (true end-to-end render signal). Unknown ids are a no-op —
/// the PTY may have closed between render and this call.
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

/// Forward user keystrokes (UTF-8 bytes) to the shell. Borrows the id +
/// payload so the Tauri shim can hand straight through the IPC raw body
/// (no clone, no JSON number-array per keystroke — see issue #282 for the
/// matching output path). Unknown ids are a no-op — the PTY may have
/// exited between a keypress and this call.
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

/// Propagate a window/grid resize to the PTY (drives SIGWINCH).
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
        // TIOCSWINSZ on the master fd resizes the pty and delivers
        // SIGWINCH to the foreground process group in the slave's session.
        // libc::TIOCSWINSZ is c_ulong on Linux, c_int on BSD/macOS — the
        // `as _` cast handles both without a per-OS branch.
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
    // Removing drops PtySession, whose Drop kills + reaps the child.
    state.terminals.lock().await.remove(&terminal_id);
    Ok(())
}
