//! `pollis` — a full-screen terminal client for Pollis, built directly on
//! `pollis-core` with no Tauri, no IPC and no WebView. See
//! `docs/pollis-tui-spec.md` and `.codesight/wiki/pollis-tui.md`.

mod app;
mod auth;
mod terminal;
mod ui;

use std::sync::Arc;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyEventKind};
use pollis_core::config::Config;
use pollis_core::state::AppState;
use tokio::sync::mpsc;

use crate::app::App;
use crate::terminal::TerminalGuard;

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
async fn run(guard: &mut TerminalGuard, state: Arc<AppState>) -> Result<()> {
    let (tx, mut rx) = mpsc::unbounded_channel::<event::KeyEvent>();
    spawn_input_thread(tx);

    let mut app = App::new(state);
    // First thing after boot: probe for an existing session.
    let mut pending = Some(app.initial_action());

    loop {
        guard
            .terminal
            .draw(|frame| ui::render(frame, &app))
            .context("draw")?;

        if app.should_quit {
            break;
        }

        // A queued async action runs after a "working…" frame is painted.
        if let Some(action) = pending.take() {
            app.busy = true;
            guard
                .terminal
                .draw(|frame| ui::render(frame, &app))
                .context("draw")?;
            app.run(action).await;
            app.busy = false;
            continue;
        }

        // Otherwise block until the next key press.
        match rx.recv().await {
            Some(key) => pending = app.on_key(key),
            // Input thread ended (stdin closed) — exit cleanly.
            None => break,
        }
    }

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
