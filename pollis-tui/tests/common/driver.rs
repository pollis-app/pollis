//! A headless, in-process e2e driver for the REAL ratatui UI state machine.
//!
//! Where the `*_smoke.rs` tests link only the library core (`auth`/`data`/`send`/
//! `sync`/`enroll`) and never touch `app.rs`/`ui.rs`, this driver owns an [`App`]
//! exactly the way `main.rs::run` does — keystrokes → [`App::on_key`] →
//! [`App::run`] → [`ui::render`] — but pumps it deterministically against a
//! ratatui [`TestBackend`] instead of a pseudo-terminal. No crossterm input
//! thread, no `tokio::select`, no Tauri/WebKit, no Xvfb.
//!
//! It reuses the in-process Delivery Service rig from [`super`] (do NOT stand up a
//! new DS): each driver builds its own [`AppState`] + [`InMemoryKeystore`] +
//! read-only main view against the shared world, so every write still routes
//! through the DS and a stray direct write fails loudly.
//!
//! Determinism: the background sync loop is spawned at a short cadence (via
//! [`App::with_sync_cadence`]) and surfacing is asserted with the bounded
//! poll-until-visible [`Driver::wait_for`] — never a fixed unconditional sleep.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use pollis_core::commands::dm;
use pollis_core::keystore::{InMemoryKeystore, Keystore};
use pollis_core::state::AppState;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

use pollis_tui::app::{Action, App, Screen};
use pollis_tui::ui;

use super::{World, DEV_OTP, TEST_PIN};

/// The fixed TestBackend size. Wide + tall enough that the three-pane Home fits
/// without truncating the message pane text we assert on.
const COLS: u16 = 120;
const ROWS: u16 = 40;

/// A short background-sync cadence so a synced message surfaces within a couple
/// of poll iterations rather than the production 4s.
const TEST_SYNC_CADENCE: Duration = Duration::from_millis(40);

/// One headless UI client: its own `App` + `AppState` + `TestBackend`, pinned to
/// its own `POLLIS_DATA_DIR` so two drivers in one process never collide on disk.
pub struct Driver {
    app: App,
    terminal: Terminal<TestBackend>,
    state: Arc<AppState>,
    data_dir: std::path::PathBuf,
}

impl Driver {
    /// Build a fresh, signed-out driver against the shared world. `device_name`
    /// scopes this client's on-disk state (local SQLCipher DB + accounts index).
    pub fn new(world: &World, device_name: &str) -> Self {
        let data_dir = world.device_dir(device_name);
        // Point POLLIS_DATA_DIR at this client's dir before building anything that
        // reads it (the keystore is in-memory, but accounts.json / local DB derive
        // their path from it during signup).
        std::env::set_var("POLLIS_DATA_DIR", &data_dir);

        let keystore: Arc<dyn Keystore> = Arc::new(InMemoryKeystore::new());
        let state = Arc::new(AppState::new_with_parts(
            world.config.clone(),
            // Read-only main view: a stray client-side write (one that should have
            // gone through the DS) fails loudly, exactly like the smokes.
            Arc::new(world.main.query_only_view()),
            world.log.clone(),
            keystore,
        ));

        let app = App::with_sync_cadence(state.clone(), TEST_SYNC_CADENCE);
        let terminal = Terminal::new(TestBackend::new(COLS, ROWS)).expect("build TestBackend");

        Self {
            app,
            terminal,
            state,
            data_dir,
        }
    }

    /// This client's `AppState` — the SAME handle the `App` drives, so DM
    /// membership established through the core layer (below) manipulates the very
    /// MLS/local state the UI's send/receive path then uses.
    pub fn state(&self) -> Arc<AppState> {
        self.state.clone()
    }

    /// This client's signed-in user id (panics if called before signup).
    pub fn user_id(&self) -> String {
        self.app
            .signed_in_user_id()
            .expect("driver not signed in yet")
            .to_string()
    }

    /// Re-point the process-global `POLLIS_DATA_DIR` at THIS driver's dir. Called
    /// before any keystore/local-DB/accounts touch. Safe because a single test
    /// drives its clients sequentially and each integration-test file is its own
    /// process (mirrors `TestClient::use_dir`). Post-signup send/sync/read paths
    /// use the already-cached local-DB + in-memory keystore handles, so they
    /// don't depend on the env var — only boot/signup do.
    fn activate(&self) {
        std::env::set_var("POLLIS_DATA_DIR", &self.data_dir);
    }

    // ── Pumping the UI ────────────────────────────────────────────────────────

    /// Mirror `main.rs::draw`: record the message-pane height (so page-scroll keys
    /// jump correctly) then render one frame into the TestBackend.
    fn draw(&mut self) {
        let area = self.terminal.get_frame().area();
        let msg_height = ui::message_viewport_height(area);
        self.app.set_msg_height(msg_height);
        let app = &self.app;
        self.terminal
            .draw(|frame| ui::render(frame, app))
            .expect("draw to TestBackend");
    }

    /// Run an `Action` and follow its returned `Option<Action>` chain to
    /// completion, redrawing after each step — the deterministic equivalent of
    /// `main.rs::run`'s pump, minus the crossterm input thread / `tokio::select`.
    async fn pump(&mut self, action: Action) {
        self.activate();
        let mut pending = Some(action);
        while let Some(action) = pending.take() {
            pending = self.app.run(action).await;
            self.draw();
        }
    }

    /// Feed one key press through `App::on_key` (kind = Press) and pump any
    /// resulting Action chain.
    pub async fn press(&mut self, code: KeyCode, modifiers: KeyModifiers) {
        self.activate();
        let event = KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };
        let follow = self.app.on_key(event);
        self.draw();
        if let Some(action) = follow {
            self.pump(action).await;
        }
    }

    /// Type a run of characters, one Press each. A `\n` maps to Enter (so a caller
    /// can submit inline). No modifiers.
    pub async fn send_keys(&mut self, keys: &str) {
        for ch in keys.chars() {
            if ch == '\n' {
                self.press(KeyCode::Enter, KeyModifiers::NONE).await;
            } else {
                self.press(KeyCode::Char(ch), KeyModifiers::NONE).await;
            }
        }
    }

    /// Convenience for pressing Enter.
    pub async fn enter(&mut self) {
        self.press(KeyCode::Enter, KeyModifiers::NONE).await;
    }

    // ── Reading the rendered screen ───────────────────────────────────────────

    /// Flatten the TestBackend buffer's cells into a newline-joined string for
    /// substring assertions — this is the RENDERED screen, the thing that makes
    /// these UI e2e tests rather than core smokes.
    pub fn buffer_text(&self) -> String {
        let buffer = self.terminal.backend().buffer();
        let width = buffer.area.width as usize;
        let mut out = String::new();
        for (i, cell) in buffer.content.iter().enumerate() {
            if i > 0 && i % width == 0 {
                out.push('\n');
            }
            out.push_str(cell.symbol());
        }
        out
    }

    /// Drive ONE deterministic sync round for this client outside the UI loop —
    /// the same primitive the `*_smoke.rs` tests use (`sync::sync_once`). The
    /// free-running background loop converges MLS state *eventually*, but the DM
    /// accept handshake (external-join → GroupInfo → the peer processing the join
    /// commit) needs a controlled A/B ordering to settle without racing; a few
    /// ordered `sync_now` rounds provide exactly that before the loop + UI take
    /// over message surfacing. Errors are swallowed: a round can legitimately
    /// no-op (nothing new on the DS yet) and the caller re-drives.
    pub async fn sync_now(&self) {
        self.activate();
        if let Some(user_id) = self.app.signed_in_user_id() {
            let _ = pollis_tui::sync::sync_once(&self.state, user_id).await;
        }
    }

    /// Poll-until-visible: repeatedly consume the background sync loop's latest
    /// snapshot (via the real [`Action::Refresh`] path) and redraw, yielding
    /// briefly between tries so the fast sync loop advances, until `needle`
    /// appears in the rendered buffer or the bounded `timeout` elapses. On
    /// timeout, panics with the current buffer dumped for debuggability.
    ///
    /// This is the mechanism of correctness — there is deliberately no fixed
    /// unconditional sleep that "should" be long enough.
    pub async fn wait_for(&mut self, needle: &str, timeout: Duration) {
        let start = Instant::now();
        loop {
            // The real data-plane surfacing path: Refresh consumes whatever the
            // background sync loop has published since the last tick.
            self.pump(Action::Refresh).await;
            if self.buffer_text().contains(needle) {
                return;
            }
            if start.elapsed() >= timeout {
                panic!(
                    "wait_for({needle:?}) timed out after {timeout:?}.\n--- rendered buffer ---\n{}",
                    self.buffer_text()
                );
            }
            // Let the background sync loop (and the peer's) make progress.
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    // ── High-level flows ──────────────────────────────────────────────────────

    /// Drive the real Email → OTP → PIN signup screens via keystrokes and assert
    /// it lands on [`Screen::Home`]. Every step goes through the real
    /// `App::on_key` / `App::run` path against the in-process DS.
    pub async fn signup(&mut self, email: &str, otp: &str, pin: &str) {
        // Boot probes for a session; a fresh device lands on the Email screen.
        let boot = self.app.initial_action();
        self.pump(boot).await;
        assert_eq!(
            &self.app.screen,
            &Screen::Email,
            "fresh device should boot to the Email screen, buffer:\n{}",
            self.buffer_text()
        );

        // Email → request OTP.
        self.send_keys(email).await;
        self.enter().await;
        assert_eq!(
            &self.app.screen,
            &Screen::Otp,
            "entering an email should advance to the OTP screen, buffer:\n{}",
            self.buffer_text()
        );

        // OTP (6 digits) → verify.
        self.send_keys(otp).await;
        self.enter().await;
        assert_eq!(
            &self.app.screen,
            &Screen::SetPin,
            "a valid OTP should advance to the SetPin screen, buffer:\n{}",
            self.buffer_text()
        );

        // PIN (4 digits) → set-pin + initialize identity → Home (+ EnterHome).
        self.send_keys(pin).await;
        self.enter().await;
        assert_eq!(
            &self.app.screen,
            &Screen::Home,
            "setting a PIN should land on Home, buffer:\n{}",
            self.buffer_text()
        );
    }

    /// The default dev signup: fixed dev OTP + PIN (the same the smokes use).
    pub async fn signup_dev(&mut self, email: &str) {
        self.signup(email, DEV_OTP, TEST_PIN).await;
    }

    // ── DM membership via the core layer (see the test's boundary comment) ─────

    /// Create a 1:1 DM to `other_user_id` through the core command layer (the DS
    /// path), returning the DM channel id. Establishing MEMBERSHIP this way — like
    /// the smokes — keeps the first e2e's focus on send + receive-surfacing
    /// through the real UI; the accept handshake is intentionally not wired
    /// through the UI here.
    pub async fn create_dm(&self, other_user_id: &str) -> String {
        self.activate();
        let me = self.user_id();
        dm::create_dm_channel(me.clone(), vec![me, other_user_id.to_string()], &self.state)
            .await
            .expect("create_dm_channel")
            .id
    }

    /// Accept a pending DM request through the core command layer (the DS path).
    pub async fn accept_dm(&self, dm_channel_id: &str) {
        self.activate();
        let me = self.user_id();
        dm::accept_dm_request(dm_channel_id.to_string(), me, &self.state)
            .await
            .expect("accept_dm_request");
    }
}
