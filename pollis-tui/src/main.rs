//! `pollis` — a full-screen terminal client for Pollis, built directly on
//! `pollis-core` with no Tauri, no IPC and no WebView. See
//! `docs/pollis-tui-spec.md` and `.codesight/wiki/pollis-tui.md`.

// `auth`, `data` and `sync` live in the library crate (`pollis_tui`) so the
// in-box smoke tests can link them; the binary reaches `auth` through the lib.
mod app;
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

    // Restore the terminal even if the render loop panics, so a crash never
    // strands the user in raw mode.
    install_panic_hook();

    let mut guard = TerminalGuard::enter().context("entering raw mode / alt screen")?;
    let result = run(&mut guard, state).await;
    // Explicit restore before printing any error to the (now-normal) terminal.
    drop(guard);

    result
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
        // follow-up (if any) is processed on the next iteration.
        if let Some(action) = pending.take() {
            app.busy = true;
            draw(guard, &mut app)?;
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
                // Only the live Home screen has anything to re-read.
                if app.screen == Screen::Home {
                    pending = Some(Action::Refresh);
                }
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
