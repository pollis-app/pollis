//! `pollis` — a full-screen terminal client for Pollis, built directly on
//! `pollis-core` with no Tauri, no IPC and no WebView. See
//! `docs/pollis-tui-spec.md` and `.codesight/wiki/pollis-tui.md`.

// The UI lives in the `pollis_tui` lib so tests can link it; only `terminal` is binary-only.
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
/// loop wrote; shorter than the sync cadence.
const UI_REFRESH: Duration = Duration::from_millis(750);

/// Arguments, then the terminal check, then the client, so `--help` needs no TTY or env (#1337).
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

    let stdin_tty = std::io::stdin().is_terminal();
    let stdout_tty = std::io::stdout().is_terminal();
    if !stdin_tty || !stdout_tty {
        eprintln!("{}", cli::not_a_terminal_text(stdin_tty, stdout_tty));
        return ExitCode::FAILURE;
    }

    // Multi-thread runtime is mandatory: core paths use spawn_blocking (spec §2).
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
            // Before the log redirect this reaches the terminal; after it, the log.
            eprintln!("pollis: {e:#}");
            ExitCode::FAILURE
        }
    }
}

async fn run_client() -> Result<()> {
    // Built like the desktop app: Config::from_env, the local DB and the device keystore
    // (#882). Everything goes through POLLIS_DELIVERY_URL (#987).
    let config = Config::from_env()
        .context("loading config from env (need POLLIS_DELIVERY_URL, R2_* placeholders)")?;
    let state = Arc::new(
        AppState::new(config)
            .await
            .context("connecting AppState (local DB + keystore)")?,
    );

    // Apply POLLIS_OVERLAY at boot; on failure stay direct.
    let boot_mode = state.config.overlay_mode;
    if let Err(e) = pollis_core::commands::overlay::apply_overlay_mode(&state, boot_mode).await {
        eprintln!("[overlay] boot apply ({boot_mode:?}) failed, staying direct: {e}");
    }

    // Redirect fd 2 to a log: stderr here is the UI's terminal, and any write corrupts it.
    let log_path = redirect_stderr_to_log();

    install_panic_hook();

    let mut guard = TerminalGuard::enter().context("entering raw mode / alt screen")?;
    let result = run(&mut guard, state).await;
    drop(guard);

    if let Some(path) = log_path {
        if let Err(e) = &result {
            println!("pollis: {e:#}");
        }
        println!("logs: {}", path.display());
    }

    result
}

/// Point fd 2 at `pollis-tui.log` in the data dir; `None` leaves stderr alone. Unix only.
#[cfg(not(unix))]
fn redirect_stderr_to_log() -> Option<std::path::PathBuf> {
    None
}

#[cfg(unix)]
fn redirect_stderr_to_log() -> Option<std::path::PathBuf> {
    use std::os::fd::AsRawFd;

    // The data dir, never the shared temp dir (#1337).
    let dir = pollis_core::db::local::dirs_path();
    // First run: the data dir may not exist yet.
    pollis_core::private_fs::create_dir_all(&dir).ok();
    let path = dir.join("pollis-tui.log");
    // Owner-only from creation.
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .mode(pollis_core::private_fs::FILE_MODE)
            .open(&path)
            .ok()?
    };
    // The mode applies only on create, so tighten an older log through the handle.
    let _ = pollis_core::private_fs::restrict_open_handle(&file);
    // SAFETY: dup2 onto fd 2 is the standard daemon-style redirect; the source
    // fd stays open (leaked) so fd 2 never dangles.
    if unsafe { libc::dup2(file.as_raw_fd(), 2) } == -1 {
        return None;
    }
    std::mem::forget(file);
    Some(path)
}

/// The render/input loop: an input thread forwards key presses; a refresh tick surfaces sync
/// writes (spec §6).
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

        // Timer-driven actions skip the busy hint, which would strobe.
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
                None => break,
            },
            _ = refresh.tick() => {
                // Screens re-read on the timer: Home, the enrollment wait and the approvals list.
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

/// Record the message pane's size, then draw one frame.
fn draw(guard: &mut TerminalGuard, app: &mut App) -> Result<()> {
    let (width, height) = ui::message_viewport(guard.terminal.get_frame().area(), app);
    app.set_msg_viewport(width, height);
    guard
        .terminal
        .draw(|frame| ui::render(frame, app))
        .context("draw")?;
    Ok(())
}

/// Forward key presses from a dedicated OS thread; `crossterm::event::read` blocks.
fn spawn_input_thread(tx: mpsc::UnboundedSender<event::KeyEvent>) {
    std::thread::spawn(move || loop {
        match event::read() {
            // Press only: some terminals also emit Release/Repeat.
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
/// loop does not leave the terminal in raw mode.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        TerminalGuard::restore();
        default(info);
    }));
}
