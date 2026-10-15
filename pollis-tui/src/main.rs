//! `pollis` — a full-screen terminal client for Pollis, built directly on
//! `pollis-core` with no Tauri, no IPC and no WebView. See
//! `docs/pollis-tui-spec.md` and `.codesight/wiki/pollis-tui.md`.

// The UI state machine (`app`/`ui`/`home`/`enroll_flow`) and the data/sync/auth
// core all live in the library crate (`pollis_tui`) so the in-box smoke + e2e
// tests can link them; the binary reaches them through the lib. Only `terminal`
// (the real crossterm raw-mode / alt-screen guard) stays binary-only here.
mod terminal;

use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyEventKind};
use pollis_core::config::Config;
use pollis_core::state::AppState;
use tokio::sync::mpsc;

use crate::terminal::TerminalGuard;
use pollis_tui::app::{Action, App, Screen};
use pollis_tui::{cli, ui};

/// How often the UI re-reads local state to surface what the background sync
/// loop wrote. Deliberately shorter than the sync cadence so a synced message
/// appears within a frame or so of landing in the local DB.
const UI_REFRESH: Duration = Duration::from_millis(750);

/// Arguments first, then the terminal check, then the client — in that order,
/// so `--help` / `--version` work with no TTY and no environment at all (a dev
/// build without its env used to die on `missing env var: R2_S3_ENDPOINT`
/// before it could say anything), and running without a terminal says so on
/// stderr instead of exiting 1 with the reason only in the log (#1337).
fn main() -> ExitCode {
    match cli::parse(std::env::args().skip(1)) {
        cli::Command::Help => {
            println!("{}", cli::help_text());
            return ExitCode::SUCCESS;
        }
        cli::Command::Version => {
            println!("{}", cli::version_text());
            return ExitCode::SUCCESS;
        }
        cli::Command::Invalid(msg) => {
            eprintln!("pollis: {msg}");
            return ExitCode::from(2);
        }
        cli::Command::Run => {}
    }

    // Checked before stderr is redirected into the log, so the user sees it.
    let stdin_tty = std::io::stdin().is_terminal();
    let stdout_tty = std::io::stdout().is_terminal();
    if !stdin_tty || !stdout_tty {
        eprintln!("{}", cli::not_a_terminal_text(stdin_tty, stdout_tty));
        return ExitCode::FAILURE;
    }

    // Multi-thread runtime is mandatory: pollis-core's DB/keystore paths use
    // spawn_blocking, so a current-thread runtime deadlocks (spec §2).
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("pollis: starting the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run_client()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            // Lands on the terminal for a failure before the log redirect (bad
            // config), in the log after it — `run_client` has then already put
            // it on stdout too.
            eprintln!("pollis: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run_client() -> Result<()> {
    // Build the client the same way AppState::new does for the desktop app:
    // Config::from_env + the local DB + the device keystore, which picks the OS
    // keychain or the machine-bound encrypted file at runtime (#882) — so this
    // works over SSH with no secret-service. POLLIS_DELIVERY_URL is required for
    // everything: since #987 the TUI holds no database credential either, so
    // reads and writes alike are signed POSTs to the DS. Set POLLIS_DATA_DIR so
    // the TUI enrolls as its own device.
    let config = Config::from_env()
        .context("loading config from env (need POLLIS_DELIVERY_URL, R2_* placeholders)")?;
    let state = Arc::new(
        AppState::new(config)
            .await
            .context("connecting AppState (local DB + keystore)")?,
    );

    // Honor POLLIS_OVERLAY at boot via the runtime apply path (build the client
    // direct, then apply the mode). Non-fatal: on failure stay direct.
    let boot_mode = state.config.overlay_mode;
    if let Err(e) = pollis_core::commands::overlay::apply_overlay_mode(&state, boot_mode).await {
        eprintln!("[overlay] boot apply ({boot_mode:?}) failed, staying direct: {e}");
    }

    // pollis-core logs with bare `eprintln!` (fine under the desktop shell,
    // where stderr is a dev terminal). Here stderr IS the UI's terminal — any
    // write scrolls the screen under ratatui and corrupts the layout — so
    // redirect fd 2 to a log file for the whole session. This also catches
    // native-library chatter that no Rust-level capture could.
    let log_path = redirect_stderr_to_log();

    // Restore the terminal even if the render loop panics, so a crash never
    // strands the user in raw mode.
    install_panic_hook();

    let mut guard = TerminalGuard::enter().context("entering raw mode / alt screen")?;
    let result = run(&mut guard, state).await;
    // Explicit restore before printing any error to the (now-normal) terminal.
    drop(guard);

    if let Some(path) = log_path {
        // stderr is the log now, so a failure would otherwise be invisible.
        if let Err(e) = &result {
            println!("pollis: {e:#}");
        }
        println!("logs: {}", path.display());
    }

    result
}

/// Point fd 2 at `pollis-tui.log` in this device's data dir — `POLLIS_DATA_DIR`,
/// else the platform default `pollis_core` keeps the database and keystore in
/// (`db::local::dirs_path`) — for the lifetime of the process. Returns the log
/// path, or `None` if the file couldn't be opened — in that case stderr is left
/// alone rather than lost.
///
/// Unix only: it dup2's onto fd 2. On Windows this is a no-op stub (stderr left
/// as-is) until a ConPTY-based redirect is wired, mirroring `terminal_windows.rs`.
#[cfg(not(unix))]
fn redirect_stderr_to_log() -> Option<std::path::PathBuf> {
    None
}

#[cfg(unix)]
fn redirect_stderr_to_log() -> Option<std::path::PathBuf> {
    use std::os::fd::AsRawFd;

    // The same directory the local DB and keystore live in. It used to fall
    // back to the shared OS temp dir when `POLLIS_DATA_DIR` was unset, which
    // put the log somewhere world-writable and nowhere near the device it
    // belongs to (#1337).
    let dir = pollis_core::db::local::dirs_path();
    // First run: the data dir may not exist yet (pollis-core creates it later,
    // during identity setup) — without this the redirect would silently no-op
    // and the whole first session would render over log spam.
    pollis_core::private_fs::create_dir_all(&dir).ok();
    let path = dir.join("pollis-tui.log");
    // Owner-only, and created that way rather than chmod-ed afterwards. Every
    // line this file receives is one some code path wrote to stderr.
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(pollis_core::private_fs::FILE_MODE)
            .open(&path)
            .ok()?
    };
    // The mode above applies only when the open CREATED the file, so a log left
    // by an older build keeps whatever it had. Tighten it through the handle.
    let _ = pollis_core::private_fs::restrict_open_handle(&file);
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

/// Draw one frame, first recording the message pane's size so the key handler
/// can page-scroll by the right amount and keep the selected message on screen.
fn draw(guard: &mut TerminalGuard, app: &mut App) -> Result<()> {
    let (width, height) = ui::message_viewport(guard.terminal.get_frame().area(), app);
    app.set_msg_viewport(width, height);
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
