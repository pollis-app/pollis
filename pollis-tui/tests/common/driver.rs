//! A headless, in-process e2e driver for the REAL ratatui UI state machine.
//!
//! Owns an [`App`] the way `main.rs::run` does, pumped deterministically against a [`TestBackend`].
//!
//! Reuses the in-process DS rig from [`super`].
//!
//! Surfacing is asserted with the bounded [`Driver::wait_for`], never a fixed sleep.

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

/// The fixed TestBackend size, wide enough for the asserted message text.
const COLS: u16 = 120;
const ROWS: u16 = 40;

/// A short sync cadence instead of the production 4s.
const TEST_SYNC_CADENCE: Duration = Duration::from_millis(40);

/// One headless UI client: its own `App` + `AppState` + `TestBackend`, pinned to
/// its own data dir so two drivers in one process never collide on disk.
pub struct Driver {
    app: App,
    terminal: Terminal<TestBackend>,
    state: Arc<AppState>,
    data_dir: std::path::PathBuf,
}

impl Driver {
    /// Build a fresh, signed-out driver against the shared world. `device_name`
    /// scopes this client's on-disk state.
    pub fn new(world: &World, device_name: &str) -> Self {
        let data_dir = world.device_dir(device_name);
        // Point the data dir at this client's before anything reads it.
        pollis_core::db::local::set_data_dir(&data_dir);

        let keystore: Arc<dyn Keystore> = Arc::new(InMemoryKeystore::new());
        let state = Arc::new(AppState::new_with_parts(world.config.clone(), keystore));

        let app = App::with_sync_cadence(state.clone(), TEST_SYNC_CADENCE);
        let terminal = Terminal::new(TestBackend::new(COLS, ROWS)).expect("build TestBackend");

        Self {
            app,
            terminal,
            state,
            data_dir,
        }
    }

    /// This client's `AppState`, the same handle the `App` drives.
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

    /// Re-point the data dir at THIS driver's, via the locked `set_data_dir` (#923).
    fn activate(&self) {
        pollis_core::db::local::set_data_dir(&self.data_dir);
    }

    /// Mirror `main.rs::draw`: record the pane size, then render one frame.
    fn draw(&mut self) {
        let area = self.terminal.get_frame().area();
        let (width, height) = ui::message_viewport(area, &self.app);
        self.app.set_msg_viewport(width, height);
        let app = &self.app;
        self.terminal
            .draw(|frame| ui::render(frame, app))
            .expect("draw to TestBackend");
    }

    /// Run an `Action` and follow its follow-up chain, redrawing after each step.
    async fn pump(&mut self, action: Action) {
        self.activate();
        let mut pending = Some(action);
        while let Some(action) = pending.take() {
            pending = self.app.run(action).await;
            self.draw();
        }
    }

    /// Feed one key press through `App::on_key` and pump the resulting actions.
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

    /// Type characters, one Press each; `\n` maps to Enter.
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

    /// The rendered screen as a newline-joined string.
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

    /// The background colour of the first cell of `needle`, or `None` if not on screen.
    pub fn bg_of(&self, needle: &str) -> Option<ratatui::style::Color> {
        let buffer = self.terminal.backend().buffer();
        let width = buffer.area.width as usize;
        let first = needle.chars().next()?;
        let rest: Vec<char> = needle.chars().skip(1).collect();
        for (i, cell) in buffer.content.iter().enumerate() {
            if !cell.symbol().starts_with(first) {
                continue;
            }
            let row_end = (i / width + 1) * width;
            let follows = rest.iter().enumerate().all(|(k, c)| {
                let j = i + 1 + k;
                j < row_end && buffer.content[j].symbol().starts_with(*c)
            });
            if follows {
                return Some(cell.bg);
            }
        }
        None
    }

    /// This client's username, as its header shows it.
    pub fn username(&self) -> String {
        self.app
            .identity()
            .expect("driver not signed in yet")
            .to_string()
    }

    /// The live state machine, for what rendered text cannot show.
    pub fn app(&self) -> &App {
        &self.app
    }

    /// One deterministic sync round outside the UI loop; ordered rounds settle the DM accept
    /// handshake. Errors are swallowed.
    pub async fn sync_now(&self) {
        self.activate();
        if let Some(user_id) = self.app.signed_in_user_id() {
            let _ = pollis_tui::sync::sync_once(&self.state, user_id).await;
        }
    }

    /// One tick of the real surfacing path: [`Action::Refresh`], then redraw.
    pub async fn refresh(&mut self) {
        self.pump(Action::Refresh).await;
    }

    /// Refresh and redraw until `needle` renders or `timeout` elapses, then panic with the buffer.
    pub async fn wait_for(&mut self, needle: &str, timeout: Duration) {
        let start = Instant::now();
        loop {
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
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Drive the real signup screens via keystrokes and assert it lands on Home.
    pub async fn signup(&mut self, email: &str, otp: &str, pin: &str) {
        let boot = self.app.initial_action();
        self.pump(boot).await;
        assert_eq!(
            &self.app.screen,
            &Screen::Email,
            "fresh device should boot to the Email screen, buffer:\n{}",
            self.buffer_text()
        );

        self.send_keys(email).await;
        self.enter().await;
        assert_eq!(
            &self.app.screen,
            &Screen::Otp,
            "entering an email should advance to the OTP screen, buffer:\n{}",
            self.buffer_text()
        );

        self.send_keys(otp).await;
        self.enter().await;

        // First device: read the Secret Key off the rendered screen and type it back (#1337).
        assert_eq!(
            &self.app.screen,
            &Screen::SaveSecretKey,
            "a first-device signup should show the Secret Key before the PIN, buffer:\n{}",
            self.buffer_text()
        );
        let secret = self.rendered_secret_key();
        self.enter().await;
        self.send_keys("A3-WRONG").await;
        self.enter().await;
        assert_eq!(&self.app.screen, &Screen::SaveSecretKey);
        assert!(
            self.buffer_text().contains("doesn't match"),
            "a wrong key must say so, buffer:\n{}",
            self.buffer_text()
        );
        for _ in 0.."A3-WRONG".len() {
            self.press(KeyCode::Backspace, KeyModifiers::NONE).await;
        }
        self.send_keys(&secret).await;
        self.enter().await;
        assert_eq!(
            &self.app.screen,
            &Screen::SetPin,
            "typing the shown Secret Key back should advance to the SetPin screen, buffer:\n{}",
            self.buffer_text()
        );

        self.send_keys(pin).await;
        self.enter().await;
        assert_eq!(
            &self.app.screen,
            &Screen::Home,
            "setting a PIN should land on Home, buffer:\n{}",
            self.buffer_text()
        );
    }

    /// The `A3-…` Secret Key as the Emergency Kit screen renders it.
    fn rendered_secret_key(&self) -> String {
        let text = self.buffer_text();
        text.split_whitespace()
            .find(|w| w.starts_with("A3-") && w.len() > 20)
            .unwrap_or_else(|| panic!("no Secret Key on screen, buffer:\n{text}"))
            .to_string()
    }

    /// The default dev signup: fixed dev OTP + PIN (the same the smokes use).
    pub async fn signup_dev(&mut self, email: &str) {
        self.signup(email, DEV_OTP, TEST_PIN).await;
    }

    /// Create a 1:1 DM through the core layer, so the test focuses on send and receive.
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
