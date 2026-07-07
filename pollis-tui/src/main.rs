//! `pollis` — a full-screen terminal client for Pollis, built directly on
//! `pollis-core` with no Tauri, no IPC and no WebView. See
//! `docs/pollis-tui-spec.md` and `.codesight/wiki/pollis-tui.md`.

// `auth`, `data` and `sync` live in the library crate (`pollis_tui`) so the
// in-box smoke tests can link them; the binary reaches `auth` through the lib.
mod app;
mod enroll_flow;
mod home;
mod terminal;
mod ui;

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyEventKind};
use pollis_core::config::Config;
use pollis_core::state::AppState;
use tokio::sync::mpsc;

use crate::app::{App, Action, Screen};
use crate::terminal::TerminalGuard;

/// How often the UI re-reads local state to surface what the background sync
/// loop wrote. Deliberately shorter than the sync cadence so a synced message
/// appears within a frame or so of landing in the local DB.
const UI_REFRESH: Duration = Duration::from_millis(750);

// Multi-thread runtime is mandatory: pollis-core's DB/keystore paths use
// spawn_blocking, so a current-thread runtime deadlocks (spec §2).
#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    // Build the client the same way AppState::new does for the desktop app:
    // Config::from_env + connect both DBs + file-backed keystore (os-keystore
    // off → JSON store). POLLIS_DELIVERY_URL is required for writes; TURSO_*
    // for reads. Set POLLIS_DATA_DIR so the TUI enrolls as its own device.
    let config = Config::from_env().context(
        "loading config from env (need TURSO_URL, TURSO_TOKEN, POLLIS_DELIVERY_URL, R2_* placeholders)",
    )?;
    let state = Arc::new(
        AppState::new(config)
            .await
            .context("connecting AppState (Turso + keystore)")?,
    );

    // pollis-core logs with bare `eprintln!` (fine under the desktop shell,
    // where stderr is a dev terminal). Here stderr IS the UI's terminal — any
    // write scrolls the screen under ratatui and corrupts the layout — so
    // redirect fd 2 to a log file for the whole session. This also catches
    // native-library chatter (e.g. libsql) that no Rust-level capture could.
    let log_path = redirect_stderr_to_log();

    // Restore the terminal even if the render loop panics, so a crash never
    // strands the user in raw mode.
    install_panic_hook();

    let mut guard = TerminalGuard::enter().context("entering raw mode / alt screen")?;
    let result = run(&mut guard, state).await;
    // Explicit restore before printing any error to the (now-normal) terminal.
    drop(guard);

    if let Some(path) = log_path {
        println!("logs: {}", path.display());
    }

    result
}

/// Point fd 2 at `$POLLIS_DATA_DIR/pollis-tui.log` (fallback: the OS temp dir)
/// for the lifetime of the process. Returns the log path, or `None` if the file
/// couldn't be opened — in that case stderr is left alone rather than lost.
fn redirect_stderr_to_log() -> Option<std::path::PathBuf> {
    use std::os::fd::AsRawFd;

    let dir = std::env::var_os("POLLIS_DATA_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    // First run: the data dir may not exist yet (pollis-core creates it later,
    // during identity setup) — without this the redirect would silently no-op
    // and the whole first session would render over log spam.
    std::fs::create_dir_all(&dir).ok();
    let path = dir.join("pollis-tui.log");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .ok()?;
    // SAFETY: dup2 onto fd 2 is the standard daemon-style redirect; the source
    // fd stays open (leaked) so fd 2 never dangles.
    if unsafe { libc::dup2(file.as_raw_fd(), 2) } == -1 {
        return None;
    }
    std::mem::forget(file);
    Some(path)
}

/// The render/input loop. Owns an input thread that forwards key presses over an
/// mpsc channel, keeping `crossterm`'s blocking `read` off the async runtime.
/// The loop selects over {key input, a periodic UI-refresh tick}: the refresh
/// tick re-reads local state so the background sync loop's writes surface without
/// blocking input (spec §6 — "a slow sync round must not freeze input").
async fn run(guard: &mut TerminalGuard, state: Arc<AppState>) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<event::KeyEvent>();
    spawn_input_thread(tx);

    let mut app = App::new(state);
    // First thing after boot: probe for an existing session.
    let mut pending = Some(app.initial_action());

    let mut refresh = tokio::time::interval(UI_REFRESH);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        draw(guard, &mut app)?;

        if app.should_quit {
            break;
        }

        // A queued async action runs after a "working…" frame is painted. Its
        // follow-up (if any) is processed on the next iteration. Timer-driven
        // actions skip the busy hint: painting "working…" every refresh tick
        // strobes the status line.
        if let Some(action) = pending.take() {
            let background = action.is_background();
            if !background {
                app.busy = true;
                draw(guard, &mut app)?;
            }
            pending = app.run(action).await;
            app.busy = false;
            continue;
        }

        // Otherwise wait for either a key press or the next refresh tick.
        tokio::select! {
            key = rx.recv() => match key {
                Some(key) => pending = app.on_key(key),
                // Input thread ended (stdin closed) — exit cleanly.
                None => break,
            },
            _ = refresh.tick() => {
                // Screens with something to re-read on the timer: the live Home
                // screen (background-synced data), the new device's enrollment
                // wait (poll `enrollment_status`), and the existing device's
                // pending-approvals list (surface newly-arrived requests).
                pending = match app.screen {
                    Screen::Home => Some(Action::Refresh),
                    Screen::EnrollWaiting => app.enrollment_poll_action(),
                    Screen::PendingEnrollments => Some(Action::LoadPendingEnrollments),
                    _ => None,
                };
            }
        }
    }

    // Stop the background poll loop before the terminal is restored.
    app.shutdown().await;

    Ok(())
}

/// Draw one frame, first recording the message-pane height so the key handler can
/// page-scroll by the right amount.
fn draw(guard: &mut TerminalGuard, app: &mut App) -> Result<()> {
    let msg_height = ui::message_viewport_height(guard.terminal.get_frame().area());
    app.set_msg_height(msg_height);
    guard
        .terminal
        .draw(|frame| ui::render(frame, app))
        .context("draw")?;
    Ok(())
}

/// Read terminal events on a dedicated OS thread and forward key *presses* to the
/// async loop. `crossterm::event::read` is blocking; isolating it here keeps the
/// tokio runtime free. The thread exits when the receiver is dropped.
fn spawn_input_thread(tx: mpsc::UnboundedSender<event::KeyEvent>) {
    std::thread::spawn(move || loop {
        match event::read() {
            // Filter to Press: some terminals also emit Release/Repeat, which
            // would double every keystroke.
            Ok(Event::Key(key)) if key.kind == KeyEventKind::Press => {
                if tx.send(key).is_err() {
                    break;
                }
            }
            Ok(_) => {}
            Err(_) => break,
        }
    });
}

/// Chain a terminal-restore onto the default panic hook so a panic in the render
/// loop doesn't leave the user in raw mode with a garbled screen.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        TerminalGuard::restore();
        default(info);
    }));
}
