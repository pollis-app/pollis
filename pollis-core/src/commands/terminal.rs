//! In-app terminal pane backend.
//!
//! Spawns the user's `$SHELL` behind a PTY via `portable-pty`. Each session
//! gets a dedicated OS thread pumping the master fd into an [`EventSink`] of
//! raw bytes (≤64 KB chunks). The frontend (xterm.js) writes those bytes to
//! the screen and forwards keystrokes back through [`terminal_write`].
//!
//! Sessions are spawned on first activation and kept alive for the app's
//! lifetime; toggling the view away and back reattaches to the same PTY.
//! Dropping a [`PtySession`] kills its child, so process exit / `logout`
//! cleanup leaves no zombies.

use std::io::{Read, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use portable_pty::{native_pty_system, CommandBuilder, MasterPty, PtySize};

use crate::error::{Error, Result};
use crate::sink::EventSink;
use crate::state::AppState;

fn map_pty<E: std::fmt::Display>(e: E) -> Error {
    Error::Other(anyhow::anyhow!("PTY error: {e}"))
}

pub struct PtySession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl Drop for PtySession {
    fn drop(&mut self) {
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
    on_output: Arc<dyn EventSink<Vec<u8>>>,
    state: &Arc<AppState>,
) -> Result<String> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(map_pty)?;

    let mut cmd = CommandBuilder::new(default_shell());
    cmd.env("TERM", "xterm-256color");
    let home = if cfg!(windows) {
        std::env::var("USERPROFILE")
    } else {
        std::env::var("HOME")
    };
    if let Ok(dir) = home {
        if !dir.is_empty() {
            cmd.cwd(dir);
        }
    }

    let child = pair.slave.spawn_command(cmd).map_err(map_pty)?;
    // Slave fd must be dropped in the parent so the shell sees EOF /
    // SIGHUP when it exits and the reader loop can terminate.
    drop(pair.slave);

    let mut reader = pair.master.try_clone_reader().map_err(map_pty)?;
    let writer = pair.master.take_writer().map_err(map_pty)?;

    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed).to_string();

    // PTY reads are blocking, so this lives on a dedicated OS thread
    // rather than a tokio task. 64 KB chunks bound per-message size; if
    // the sink detaches (view closed / app shutting down) we stop.
    std::thread::Builder::new()
        .name(format!("pty-{id}"))
        .spawn(move || {
            let mut buf = [0u8; 64 * 1024];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if on_output.send(buf[..n].to_vec()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        })
        .map_err(map_pty)?;

    let session = PtySession {
        master: pair.master,
        writer,
        child,
    };
    state.terminals.lock().await.insert(id.clone(), session);
    Ok(id)
}

/// Forward user keystrokes (UTF-8 bytes) to the shell. Unknown ids are a
/// no-op — the PTY may have exited between a keypress and this call.
pub async fn terminal_write(
    terminal_id: String,
    data: Vec<u8>,
    state: &Arc<AppState>,
) -> Result<()> {
    let mut map = state.terminals.lock().await;
    if let Some(session) = map.get_mut(&terminal_id) {
        session.writer.write_all(&data).map_err(map_pty)?;
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
        session
            .master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(map_pty)?;
    }
    Ok(())
}

/// Kill the PTY and drop the session. Idempotent.
pub async fn terminal_close(terminal_id: String, state: &Arc<AppState>) -> Result<()> {
    // Removing drops PtySession, whose Drop kills + reaps the child.
    state.terminals.lock().await.remove(&terminal_id);
    Ok(())
}
