//! `pollis` — a full-screen terminal client for Pollis, built directly on
//! `pollis-core` with no Tauri, no IPC and no WebView. See
//! `docs/pollis-tui-spec.md` and `.codesight/wiki/pollis-tui.md`.

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

/// How often the UI re-reads local state the sync loop wrote; shorter than the
/// sync cadence so a synced message appears promptly.
const UI_REFRESH: Duration = Duration::from_millis(750);

/// Arguments, then the terminal check, then the client: `--help` / `--version`
/// need no TTY and no env, and a missing terminal is reported on stderr (#1337).
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
            // After the log redirect this lands in the log; `run_client` has
            // already printed it to stdout.
            eprintln!("pollis: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run_client() -> Result<()> {
    // The keystore falls back to the machine-bound encrypted file where there is
    // no OS keychain (#882), so this works over SSH. POLLIS_DATA_DIR makes the
    // TUI enroll as its own device.
    let config = Config::from_env()
        .context("loading config from env (need POLLIS_DELIVERY_URL, R2_* placeholders)")?;
    let state = Arc::new(
        AppState::new(config)
            .await
            .context("connecting AppState (local DB + keystore)")?,
    );

    // Honor POLLIS_OVERLAY at boot; on failure stay direct.
    let boot_mode = state.config.overlay_mode;
    if let Err(e) = pollis_core::commands::overlay::apply_overlay_mode(&state, boot_mode).await {
        eprintln!("[overlay] boot apply ({boot_mode:?}) failed, staying direct: {e}");
    }

    // pollis-core logs with bare `eprintln!`, and here stderr is the UI's
    // terminal (any write corrupts the ratatui layout), so fd 2 goes to a log
    // file; that also catches native-library output.
    let log_path = redirect_stderr_to_log();

    install_panic_hook();

    let mut guard = TerminalGuard::enter().context("entering raw mode / alt screen")?;
    let result = run(&mut guard, state).await;
    // Restore before printing any error to the terminal.
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

/// Point fd 2 at `pollis-tui.log` in the device's data dir for the process
/// lifetime. `None` if the file couldn't be opened (stderr is then left alone).
/// A no-op stub off Unix.
#[cfg(not(unix))]
fn redirect_stderr_to_log() -> Option<std::path::PathBuf> {
    None
}

#[cfg(unix)]
fn redirect_stderr_to_log() -> Option<std::path::PathBuf> {
    use std::os::fd::AsRawFd;

    // The data dir, never the shared world-writable temp dir (#1337).
    let dir = pollis_core::db::local::dirs_path();
    // On first run the data dir doesn't exist yet; without this the redirect
    // would silently no-op.
    pollis_core::private_fs::create_dir_all(&dir).ok();
    let path = dir.join("pollis-tui.log");
    // Created owner-only rather than chmod-ed afterwards.
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(pollis_core::private_fs::FILE_MODE)
            .open(&path)
            .ok()?
    };
    // The mode applies only on create; tighten a pre-existing file too.
    let _ = pollis_core::private_fs::restrict_open_handle(&file);
    // SAFETY: dup2 onto fd 2 is the standard daemon-style redirect; the source
    // fd stays open (leaked) so fd 2 never dangles.
    if unsafe { libc::dup2(file.as_raw_fd(), 2) } == -1 {
        return None;
    }
    std::mem::forget(file);
    Some(path)
}

/// The render/input loop: selects over key input and a UI-refresh tick, so a
/// slow sync round never freezes input (spec §6).
async fn run(guard: &mut TerminalGuard, state: Arc<AppState>) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<event::KeyEvent>();
    spawn_input_thread(tx);

    let mut app = App::new(state);
    let mut pending = Some(app.initial_action());

    let mut refresh = tokio::time::interval(UI_REFRESH);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        draw(guard, &mut app)?;

        if app.should_quit {
            break;
        }

        // A queued action runs after a "working…" frame is painted. Timer-driven
        // actions skip it: painting it every refresh tick strobes the status line.
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

        tokio::select! {
            key = rx.recv() => match key {
                Some(key) => pending = app.on_key(key),
                // stdin closed.
                None => break,
            },
            _ = refresh.tick() => {
                pending = match app.screen {
                    Screen::Home => Some(Action::Refresh),
                    Screen::EnrollWaiting => app.enrollment_poll_action(),
                    Screen::PendingEnrollments => Some(Action::LoadPendingEnrollments),
                    _ => None,
                };
            }
        }
    }

    app.shutdown().await;

    Ok(())
}

/// Records the message pane's size first so the key handler can page-scroll
/// correctly.
fn draw(guard: &mut TerminalGuard, app: &mut App) -> Result<()> {
    let (width, height) = ui::message_viewport(guard.terminal.get_frame().area(), app);
    app.set_msg_viewport(width, height);
    guard
        .terminal
        .draw(|frame| ui::render(frame, app))
        .context("draw")?;
    Ok(())
}

/// `crossterm::event::read` blocks, so it runs on its own OS thread; the thread
/// exits when the receiver is dropped.
fn spawn_input_thread(tx: mpsc::UnboundedSender<event::KeyEvent>) {
    std::thread::spawn(move || loop {
        match event::read() {
            // Some terminals also emit Release/Repeat, doubling every keystroke.
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

/// Chain a terminal restore onto the default panic hook.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        TerminalGuard::restore();
        default(info);
    }));
}
