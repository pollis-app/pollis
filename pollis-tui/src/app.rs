//! The UI state machine.
//!
//! M0 gives us the skeleton (boot → event loop → quit). M1 adds the auth
//! screens: first-device signup (email → OTP → PIN) and returning-user unlock.
//! M2 will graft the group/channel/DM panes onto the [`Screen::Home`] state.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pollis_core::commands::auth::UserProfile;
use pollis_core::state::AppState;

use pollis_tui::auth::{self, Boot};
use pollis_tui::{data, sync};

use crate::home::{
    should_load_older, ConvKind, ConvRef, Focus, HomeState, OpenConversation, RowTarget,
};

/// Background-sync cadence (spec §6: ~3–5 s while foregrounded). The loop polls
/// the Delivery Service and advances local MLS state; the UI re-reads on its own
/// (faster) refresh tick, so a newly-synced message surfaces within a second.
pub const SYNC_CADENCE: Duration = Duration::from_secs(4);

/// Which screen the user is looking at. Text-input buffers live on [`App`], not
/// in the variants, so transitions are cheap and the render code reads one field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    /// Boot-time session probe in flight.
    Booting,
    /// First-device signup: enter email.
    Email,
    /// First-device signup: enter the OTP code.
    Otp,
    /// First-device signup: choose a PIN.
    SetPin,
    /// Returning user: enter the PIN to unlock the local DB.
    Unlock,
    /// Signed in and unlocked — the three-pane client (M2b).
    Home,
    /// Unrecoverable boot/config error — show it and let the user quit.
    Fatal,
}

/// An async operation the render loop should run after drawing a "working"
/// frame. Keeping these out of the synchronous key handler lets the UI paint a
/// status line *before* a network round-trip blocks.
pub enum Action {
    Boot,
    RequestOtp,
    VerifyOtp,
    SetPinAndInit,
    Unlock,
    /// Reached Home: start the background sync loop and load the tree.
    EnterHome,
    /// Re-read the tree (and the open conversation's newest page) — fired by the
    /// UI refresh tick so background-synced data surfaces.
    Refresh,
    /// Open the currently-selected sidebar conversation in the main pane.
    OpenSelected,
    /// Fetch the next older page of the open conversation (scrollback).
    LoadOlder,
}

pub struct App {
    state: Arc<AppState>,
    pub screen: Screen,
    /// Current text-input buffer (email / OTP / PIN, per screen).
    pub input: String,
    /// Transient status or error line shown under the active screen.
    pub status: Option<String>,
    /// True while an [`Action`] is running; drives the "working…" hint.
    pub busy: bool,
    /// Set to true when the user asks to quit.
    pub should_quit: bool,

    /// Email captured on the [`Screen::Email`] step, reused by `verify_otp`.
    email: String,
    /// Profile from `verify_otp` / `get_session` — carries the `user_id` needed
    /// by `set_pin`/`initialize_identity`/`unlock`.
    profile: Option<UserProfile>,

    /// The three-pane client model (populated once [`Screen::Home`] is reached).
    pub home: HomeState,
    /// The running background poll loop; cancelled on quit for a clean shutdown.
    sync_loop: Option<sync::SyncLoop>,
    /// Last-rendered height (in rows) of the message pane, so the key handler can
    /// scroll by whole pages. Updated by the renderer via [`Self::set_msg_height`].
    msg_height: usize,
}

impl App {
    pub fn new(state: Arc<AppState>) -> Self {
        Self {
            state,
            screen: Screen::Booting,
            input: String::new(),
            status: None,
            busy: false,
            should_quit: false,
            email: String::new(),
            profile: None,
            home: HomeState::new(),
            sync_loop: None,
            msg_height: 0,
        }
    }

    /// The signed-in user's display name, if any (used by the header bar).
    pub fn identity(&self) -> Option<&str> {
        self.profile.as_ref().map(|p| p.username.as_str())
    }

    /// Record the message pane's rendered height so page-scroll keys know how far
    /// to jump. Called by the renderer each frame.
    pub fn set_msg_height(&mut self, h: usize) {
        self.msg_height = h;
    }

    /// The first action to run once the terminal is up: probe for a session.
    pub fn initial_action(&self) -> Action {
        Action::Boot
    }

    /// Handle a key press. Returns an [`Action`] when the key commits a step
    /// that needs an async round-trip; the caller runs it after redrawing.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        // Ctrl-C quits from anywhere.
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return None;
        }

        match self.screen {
            Screen::Booting => None,
            Screen::Fatal => {
                // Any key quits the error screen.
                self.should_quit = true;
                None
            }
            Screen::Home => self.on_home_key(key),
            Screen::Email => self.on_text_key(key, InputKind::Email, Action::RequestOtp),
            Screen::Otp => self.on_text_key(key, InputKind::Digits(6), Action::VerifyOtp),
            Screen::SetPin => self.on_text_key(key, InputKind::Digits(4), Action::SetPinAndInit),
            Screen::Unlock => self.on_text_key(key, InputKind::Digits(4), Action::Unlock),
        }
    }

    /// Shared editing behaviour for the text-entry screens.
    fn on_text_key(&mut self, key: KeyEvent, kind: InputKind, submit: Action) -> Option<Action> {
        match key.code {
            KeyCode::Enter => {
                if kind.is_submittable(&self.input) {
                    self.status = None;
                    return Some(submit);
                }
                self.status = Some(kind.hint().to_string());
                None
            }
            KeyCode::Backspace => {
                self.input.pop();
                None
            }
            KeyCode::Char(c) => {
                if kind.accepts(c) {
                    self.input.push(c);
                }
                None
            }
            _ => None,
        }
    }

    /// Handle a key on the three-pane Home screen. Navigation (focus, selection,
    /// group expansion, scrolling) is handled inline; the two things that need an
    /// async DB round-trip — opening a conversation and paging older history —
    /// return an [`Action`] for the caller to run after a redraw.
    fn on_home_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                None
            }
            // Tab cycles focus between the sidebar and the message pane.
            KeyCode::Tab | KeyCode::BackTab => {
                self.home.focus = match self.home.focus {
                    Focus::Sidebar => Focus::Messages,
                    Focus::Messages => Focus::Sidebar,
                };
                None
            }
            KeyCode::Up | KeyCode::Char('k') => self.on_home_up(1),
            KeyCode::Down | KeyCode::Char('j') => self.on_home_down(1),
            KeyCode::PageUp => self.on_home_up(self.msg_height.max(1)),
            KeyCode::PageDown => self.on_home_down(self.msg_height.max(1)),
            KeyCode::Enter => {
                if self.home.focus == Focus::Sidebar {
                    return self.activate_selection();
                }
                None
            }
            _ => None,
        }
    }

    /// Up/`k` (or PageUp): in the sidebar move the highlight, in the message pane
    /// scroll toward older messages, prefetching the next page near the top.
    fn on_home_up(&mut self, amount: usize) -> Option<Action> {
        match self.home.focus {
            Focus::Sidebar => {
                self.home.move_selection(-(amount as i32));
                None
            }
            Focus::Messages => {
                let open = self.home.open.as_mut()?;
                open.scroll = (open.scroll + amount).min(open.messages.len());
                if should_load_older(
                    open.scroll,
                    open.messages.len(),
                    open.older_cursor.is_some(),
                    open.loading,
                ) {
                    open.loading = true;
                    return Some(Action::LoadOlder);
                }
                None
            }
        }
    }

    /// Down/`j` (or PageDown): in the sidebar move the highlight, in the message
    /// pane scroll back toward the newest message.
    fn on_home_down(&mut self, amount: usize) -> Option<Action> {
        match self.home.focus {
            Focus::Sidebar => self.home.move_selection(amount as i32),
            Focus::Messages => {
                if let Some(open) = self.home.open.as_mut() {
                    open.scroll = open.scroll.saturating_sub(amount);
                }
            }
        }
        None
    }

    /// Enter on the sidebar: toggle a group's expansion (inline) or queue opening
    /// the highlighted conversation.
    fn activate_selection(&mut self) -> Option<Action> {
        let target = self.home.current_row().map(|r| r.target.clone())?;
        match target {
            RowTarget::ToggleGroup(i) => {
                if let Some(tree) = self.home.tree.as_ref() {
                    if let Some(group) = tree.groups.get(i) {
                        let id = group.id.clone();
                        if !self.home.expanded.remove(&id) {
                            self.home.expanded.insert(id);
                        }
                        let uid = self.user_id();
                        self.home.rebuild_rows(&uid);
                    }
                }
                None
            }
            RowTarget::Open(conv) => Some(self.begin_open(conv)),
            RowTarget::Header => None,
        }
    }

    /// Install a fresh (loading) [`OpenConversation`] for `conv` and return the
    /// action that fetches its first page. Re-selecting the already-open
    /// conversation is a no-op so a stray Enter doesn't wipe scrollback.
    fn begin_open(&mut self, conv: ConvRef) -> Action {
        let already = self
            .home
            .open
            .as_ref()
            .is_some_and(|o| o.conv_id == conv.id);
        if !already {
            self.home.open = Some(OpenConversation {
                conv_id: conv.id.clone(),
                name: conv.name.clone(),
                kind: Some(conv.kind),
                loading: true,
                ..Default::default()
            });
            self.home.focus = Focus::Messages;
        }
        Action::OpenSelected
    }

    /// Run a queued async [`Action`] against the core command surface. All error
    /// handling funnels through [`Self::fail`] so the UI never dies silently.
    /// The caller sets/clears [`Self::busy`] around this so the "working…" frame
    /// paints before the (possibly network-blocking) call. Returns an optional
    /// **follow-up** action for the caller to run next (e.g. entering Home queues
    /// the first tree load).
    pub async fn run(&mut self, action: Action) -> Option<Action> {
        match action {
            Action::Boot => match auth::boot(&self.state).await {
                Ok(Boot::Returning(profile)) => {
                    self.profile = Some(profile);
                    self.goto(Screen::Unlock, "Welcome back — enter your PIN to unlock.");
                }
                Ok(Boot::Fresh) => {
                    self.goto(Screen::Email, "Sign in — enter your email to get a code.");
                }
                Err(e) => self.fatal(format!("Startup failed: {e}")),
            },
            Action::RequestOtp => {
                let email = self.input.trim().to_string();
                match auth::request_otp(&self.state, &email).await {
                    Ok(()) => {
                        self.email = email;
                        self.goto(Screen::Otp, "We sent you a code — enter it below.");
                    }
                    Err(e) => self.fail(format!("Couldn't send code: {e}")),
                }
            }
            Action::VerifyOtp => {
                let code = self.input.trim().to_string();
                match auth::verify_otp(&self.state, &self.email, &code).await {
                    Ok(profile) => {
                        self.profile = Some(profile);
                        self.goto(Screen::SetPin, "Choose a 4-digit PIN to secure this device.");
                    }
                    Err(e) => self.fail(format!("{e}")),
                }
            }
            Action::SetPinAndInit => {
                let pin = self.input.clone();
                let user_id = self.user_id();
                match auth::set_pin_and_init(&self.state, &user_id, &pin).await {
                    // Reaching Home starts the sync loop + first tree load.
                    Ok(()) => {
                        self.goto(Screen::Home, "You're signed in.");
                        return Some(Action::EnterHome);
                    }
                    Err(e) => self.fail(format!("Couldn't set PIN: {e}")),
                }
            }
            Action::Unlock => {
                let pin = self.input.clone();
                let user_id = self.user_id();
                match auth::unlock(&self.state, &user_id, &pin).await {
                    Ok(()) => {
                        self.goto(Screen::Home, "Unlocked.");
                        return Some(Action::EnterHome);
                    }
                    Err(e) => self.fail(format!("Unlock failed: {e}")),
                }
            }
            Action::EnterHome => {
                // Start the background poll loop (spec §6). It mutates the local
                // DB; the UI's refresh tick re-reads from it.
                if self.sync_loop.is_none() {
                    let user_id = self.user_id();
                    self.sync_loop =
                        Some(sync::spawn_loop(self.state.clone(), user_id, SYNC_CADENCE));
                }
                // Load the tree once immediately so the sidebar isn't empty while
                // the first background round runs.
                return Some(Action::Refresh);
            }
            Action::Refresh => {
                self.refresh_tree().await;
                self.refresh_open().await;
                self.home.refreshes = self.home.refreshes.wrapping_add(1);
            }
            Action::OpenSelected => self.load_open_first_page().await,
            Action::LoadOlder => self.load_open_older().await,
        }
        None
    }

    /// Re-read the conversation tree from the local DB (populated by the sync
    /// loop). Non-fatal: a failed read just leaves the last-good tree up.
    async fn refresh_tree(&mut self) {
        let user_id = self.user_id();
        match data::load_conversations(&self.state, &user_id).await {
            Ok(tree) => self.home.set_tree(tree, &user_id),
            Err(e) => self.status = Some(format!("Sync read failed: {e}")),
        }
    }

    /// Re-read the newest page of the open conversation and merge it in, so a
    /// peer's just-synced message appears without disturbing loaded scrollback.
    async fn refresh_open(&mut self) {
        let Some((conv_id, kind)) = self.open_target() else {
            return;
        };
        let user_id = self.user_id();
        match self.fetch_page(&user_id, &conv_id, kind, None).await {
            Ok(page) => {
                if let Some(open) = self.home.open.as_mut() {
                    open.messages =
                        crate::home::merge_messages(std::mem::take(&mut open.messages), page.messages);
                    open.loading = false;
                    // Only learn the older-cursor if we haven't paged back yet.
                    if open.older_cursor.is_none() && !open.at_beginning {
                        open.older_cursor = page.next_cursor;
                        open.at_beginning = open.older_cursor.is_none();
                    }
                }
            }
            Err(e) => self.status = Some(format!("Read failed: {e}")),
        }
    }

    /// Fetch and install the first (newest) page of the just-opened conversation.
    async fn load_open_first_page(&mut self) {
        let Some((conv_id, kind)) = self.open_target() else {
            return;
        };
        let user_id = self.user_id();
        match self.fetch_page(&user_id, &conv_id, kind, None).await {
            Ok(page) => {
                if let Some(open) = self.home.open.as_mut() {
                    open.messages =
                        crate::home::merge_messages(Vec::new(), page.messages);
                    open.older_cursor = page.next_cursor;
                    open.at_beginning = open.older_cursor.is_none();
                    open.scroll = 0;
                    open.loading = false;
                }
            }
            Err(e) => {
                if let Some(open) = self.home.open.as_mut() {
                    open.loading = false;
                }
                self.status = Some(format!("Couldn't open conversation: {e}"));
            }
        }
    }

    /// Fetch the next older page and prepend it (scrollback / §6 pagination).
    async fn load_open_older(&mut self) {
        let Some((conv_id, kind)) = self.open_target() else {
            return;
        };
        // Take the cursor by value — `MessageCursor` isn't `Clone`, and we
        // replace it with the page's `next_cursor` afterwards.
        let cursor = self.home.open.as_mut().and_then(|o| o.older_cursor.take());
        let user_id = self.user_id();
        match self.fetch_page(&user_id, &conv_id, kind, cursor).await {
            Ok(page) => {
                if let Some(open) = self.home.open.as_mut() {
                    open.messages =
                        crate::home::merge_messages(std::mem::take(&mut open.messages), page.messages);
                    open.older_cursor = page.next_cursor;
                    open.at_beginning = open.older_cursor.is_none();
                    open.loading = false;
                }
            }
            Err(e) => {
                if let Some(open) = self.home.open.as_mut() {
                    open.loading = false;
                }
                self.status = Some(format!("Couldn't load history: {e}"));
            }
        }
    }

    /// Dispatch to the channel vs DM read path for the open conversation.
    async fn fetch_page(
        &self,
        user_id: &str,
        conv_id: &str,
        kind: ConvKind,
        cursor: Option<pollis_core::commands::messages::MessageCursor>,
    ) -> anyhow::Result<pollis_core::commands::messages::MessagePage> {
        match kind {
            ConvKind::Channel => {
                data::channel_messages(&self.state, user_id, conv_id, cursor).await
            }
            ConvKind::Dm | ConvKind::DmRequest => {
                data::dm_messages(&self.state, user_id, conv_id, cursor).await
            }
        }
    }

    /// The (id, kind) of the open conversation, if one is open.
    fn open_target(&self) -> Option<(String, ConvKind)> {
        self.home
            .open
            .as_ref()
            .and_then(|o| o.kind.map(|k| (o.conv_id.clone(), k)))
    }

    /// Cancel the background sync loop on shutdown, giving the current round a
    /// brief window to finish so the terminal restore isn't racing a DB write.
    pub async fn shutdown(&mut self) {
        if let Some(sync_loop) = self.sync_loop.take() {
            let handle = sync_loop.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
        }
    }

    /// The active user's id. Only called on screens reached *after* a profile is
    /// established, so a missing profile is a programming error, not a runtime one.
    fn user_id(&self) -> String {
        self.profile
            .as_ref()
            .map(|p| p.id.clone())
            .expect("user_id requested before profile was set")
    }

    /// Transition to `screen`, clear the input buffer, and set a status line.
    fn goto(&mut self, screen: Screen, status: &str) {
        self.screen = screen;
        self.input.clear();
        self.status = Some(status.to_string());
    }

    /// A recoverable failure: stay on the current screen, clear the buffer, show
    /// the error so the user can retry.
    fn fail(&mut self, msg: String) {
        self.input.clear();
        self.status = Some(msg);
    }

    /// An unrecoverable failure: switch to the fatal screen.
    fn fatal(&mut self, msg: String) {
        self.screen = Screen::Fatal;
        self.status = Some(msg);
    }
}

/// Per-screen input validation, kept out of the key handler.
enum InputKind {
    Email,
    /// A fixed-length numeric code (OTP = 6, PIN = 4).
    Digits(usize),
}

impl InputKind {
    fn accepts(&self, c: char) -> bool {
        match self {
            // Reject whitespace; the DS validates the address itself.
            InputKind::Email => !c.is_whitespace(),
            InputKind::Digits(_) => c.is_ascii_digit(),
        }
    }

    fn is_submittable(&self, input: &str) -> bool {
        match self {
            InputKind::Email => input.trim().contains('@'),
            InputKind::Digits(n) => input.len() == *n,
        }
    }

    fn hint(&self) -> &'static str {
        match self {
            InputKind::Email => "Enter a valid email address.",
            InputKind::Digits(4) => "PIN must be 4 digits.",
            InputKind::Digits(_) => "Enter the 6-digit code.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The auth screens differ only in their `InputKind`; these lock the
    // per-screen validation so a wrong OTP/PIN length or a non-numeric key can't
    // reach the (DB-opening, network-hitting) command layer.

    #[test]
    fn email_needs_an_at_sign_and_rejects_whitespace() {
        let k = InputKind::Email;
        assert!(k.accepts('a'));
        assert!(!k.accepts(' '));
        assert!(!k.is_submittable("nobody"));
        assert!(k.is_submittable("a@b.co"));
    }

    #[test]
    fn otp_is_exactly_six_digits() {
        let k = InputKind::Digits(6);
        assert!(k.accepts('7'));
        assert!(!k.accepts('x'));
        assert!(!k.is_submittable("12345"));
        assert!(k.is_submittable("123456"));
        assert!(!k.is_submittable("1234567"));
    }

    #[test]
    fn pin_is_exactly_four_digits() {
        let k = InputKind::Digits(4);
        assert!(!k.is_submittable("123"));
        assert!(k.is_submittable("1234"));
        assert!(!k.is_submittable("12345"));
        assert_eq!(k.hint(), "PIN must be 4 digits.");
    }
}
