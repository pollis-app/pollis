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
use pollis_tui::{data, send, sync};

use crate::home::{
    is_blank, should_load_older, ConvKind, ConvRef, Focus, HomeMode, HomeState, OpenConversation,
    PromptKind, RowTarget,
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
    /// Send the compose buffer to the open conversation (§8 "Send message").
    SendMessage,
    /// Accept the given pending DM request (§8 "Accept DM request").
    AcceptDm(String),
    /// Submit the active create/invite prompt (new group/channel, start DM,
    /// invite), dispatching on the current [`HomeMode::Prompt`] kind.
    SubmitPrompt,
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

    /// Handle a key on the three-pane Home screen. The screen has three input
    /// modes — navigate, compose (typing a message), and prompt (a create/invite
    /// bottom bar) — each with its own handler. Compose/prompt keystrokes edit
    /// [`Self::input`]; the write-triggering keys return an [`Action`].
    fn on_home_key(&mut self, key: KeyEvent) -> Option<Action> {
        match self.home.mode {
            HomeMode::Navigate => self.on_home_nav_key(key),
            HomeMode::Compose => self.on_compose_key(key),
            HomeMode::Prompt(_) => self.on_prompt_key(key),
        }
    }

    /// Navigate mode: tree movement + the command keys that open compose or a
    /// create/invite prompt. The two async reads (open a conversation, page
    /// history) return an [`Action`]; the write-triggering keys hand off to the
    /// `begin_*` helpers.
    fn on_home_nav_key(&mut self, key: KeyEvent) -> Option<Action> {
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
            // `i` (or Enter on the message pane) enters compose for the open conv.
            KeyCode::Char('i') => self.begin_compose(),
            // `a` accepts the selected pending DM request.
            KeyCode::Char('a') => self.begin_accept_dm(),
            // Create/invite flows open an inline bottom-bar prompt.
            KeyCode::Char('g') => self.begin_prompt(PromptKind::NewGroup),
            KeyCode::Char('c') => self.begin_new_channel_prompt(),
            KeyCode::Char('d') => self.begin_prompt(PromptKind::StartDm),
            KeyCode::Char('v') => self.begin_invite_prompt(),
            KeyCode::Enter => {
                if self.home.focus == Focus::Sidebar {
                    return self.activate_selection();
                }
                // Message pane focused → start composing.
                self.begin_compose()
            }
            _ => None,
        }
    }

    /// Compose mode: type into the buffer; Enter sends (empty is a no-op); Esc
    /// leaves so navigation keys work again.
    fn on_compose_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => {
                self.leave_input_mode();
                None
            }
            KeyCode::Enter => {
                if is_blank(&self.input) {
                    // Empty/whitespace-only input does not send.
                    None
                } else {
                    self.status = Some("Sending…".to_string());
                    Some(Action::SendMessage)
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
                None
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                None
            }
            _ => None,
        }
    }

    /// Prompt mode: type into the buffer; Enter submits (empty is rejected with a
    /// hint); Esc cancels.
    fn on_prompt_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => {
                self.leave_input_mode();
                None
            }
            KeyCode::Enter => {
                if is_blank(&self.input) {
                    self.status = Some("Enter a value, or Esc to cancel.".to_string());
                    None
                } else {
                    Some(Action::SubmitPrompt)
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
                None
            }
            KeyCode::Char(c) => {
                self.input.push(c);
                None
            }
            _ => None,
        }
    }

    /// Enter compose mode for the open conversation. Guards: nothing open →
    /// guidance; an un-accepted DM request → tell the user to accept it first
    /// (its MLS tree has no send path until accepted).
    fn begin_compose(&mut self) -> Option<Action> {
        match self.home.open.as_ref().map(|o| o.kind) {
            None => {
                self.status = Some("Open a conversation first (Enter on the sidebar).".to_string());
            }
            Some(Some(ConvKind::DmRequest)) => {
                self.status =
                    Some("Accept this request first (press a) before sending.".to_string());
            }
            Some(_) => {
                self.home.mode = HomeMode::Compose;
                self.home.focus = Focus::Messages;
                self.input.clear();
                self.status =
                    Some("Composing — type a message, Enter to send, Esc to cancel.".to_string());
            }
        }
        None
    }

    /// Accept the selected pending DM request, if the highlighted row is one.
    fn begin_accept_dm(&mut self) -> Option<Action> {
        match self.home.selected_dm_request() {
            Some(id) => {
                self.status = Some("Accepting…".to_string());
                Some(Action::AcceptDm(id))
            }
            None => {
                self.status =
                    Some("Highlight a pending request (in Requests) to accept.".to_string());
                None
            }
        }
    }

    /// Open a create/invite prompt (bottom input bar) collecting text for `kind`.
    fn begin_prompt(&mut self, kind: PromptKind) -> Option<Action> {
        self.status = Some(format!("{} — Enter to submit, Esc to cancel.", kind.label()));
        self.home.mode = HomeMode::Prompt(kind);
        self.input.clear();
        None
    }

    /// Open the "new channel" prompt scoped to the selected group (or the group
    /// owning the selected channel). Needs a group in context.
    fn begin_new_channel_prompt(&mut self) -> Option<Action> {
        match self.home.context_group() {
            Some((group_id, group_name)) => self.begin_prompt(PromptKind::NewChannel {
                group_id,
                group_name,
            }),
            None => {
                self.status =
                    Some("Highlight a group (or one of its channels) first.".to_string());
                None
            }
        }
    }

    /// Open the "invite to group" prompt scoped to the group in context.
    fn begin_invite_prompt(&mut self) -> Option<Action> {
        match self.home.context_group() {
            Some((group_id, group_name)) => self.begin_prompt(PromptKind::Invite {
                group_id,
                group_name,
            }),
            None => {
                self.status = Some("Highlight a group to invite someone to.".to_string());
                None
            }
        }
    }

    /// Leave compose/prompt back to navigation, clearing the shared buffer.
    fn leave_input_mode(&mut self) {
        self.home.mode = HomeMode::Navigate;
        self.input.clear();
        self.status = None;
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
            Action::SendMessage => self.do_send().await,
            Action::AcceptDm(id) => self.do_accept_dm(&id).await,
            Action::SubmitPrompt => self.do_submit_prompt().await,
        }
        None
    }

    /// Send the compose buffer to the open conversation, then re-read its newest
    /// page so the sent message appears immediately (the core stores it locally
    /// on send). Stays in compose mode on success so the user can keep typing; a
    /// failure surfaces on the status line and the text is preserved for a retry.
    async fn do_send(&mut self) {
        let Some(conv_id) = self.home.open.as_ref().map(|o| o.conv_id.clone()) else {
            self.leave_input_mode();
            return;
        };
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return;
        }
        let user_id = self.user_id();
        let username = self.identity().map(|s| s.to_string());
        match send::send_text(&self.state, &user_id, username, &conv_id, &text).await {
            Ok(_) => {
                self.input.clear();
                self.status = None;
                // Pin to newest and surface the just-sent message.
                if let Some(open) = self.home.open.as_mut() {
                    open.scroll = 0;
                }
                self.refresh_open().await;
            }
            Err(e) => self.status = Some(format!("Send failed: {e}")),
        }
    }

    /// Accept a pending DM request, then refresh the tree so it moves from
    /// Requests to Direct Messages.
    async fn do_accept_dm(&mut self, dm_id: &str) {
        let user_id = self.user_id();
        match send::accept_dm(&self.state, &user_id, dm_id).await {
            Ok(()) => {
                self.status = Some("Request accepted.".to_string());
                self.refresh_tree().await;
            }
            Err(e) => self.status = Some(format!("Couldn't accept request: {e}")),
        }
    }

    /// Dispatch the active create/invite prompt on its [`HomeMode::Prompt`] kind,
    /// then refresh the tree so a new group/channel/DM appears. Any failure (user
    /// not found, not an admin, …) lands on the status line — never a panic. The
    /// buffer is consumed and the screen returns to navigation on success.
    async fn do_submit_prompt(&mut self) {
        let HomeMode::Prompt(kind) = self.home.mode.clone() else {
            return;
        };
        let value = self.input.trim().to_string();
        if value.is_empty() {
            return;
        }
        let user_id = self.user_id();
        let result: anyhow::Result<&'static str> = match kind {
            PromptKind::NewGroup => send::new_group(&self.state, &user_id, &value)
                .await
                .map(|_| "Group created."),
            PromptKind::NewChannel { group_id, .. } => {
                send::new_channel(&self.state, &group_id, &user_id, &value)
                    .await
                    .map(|_| "Channel created.")
            }
            PromptKind::StartDm => self.resolve_and_start_dm(&user_id, &value).await,
            PromptKind::Invite { group_id, .. } => {
                send::invite(&self.state, &group_id, &user_id, &value)
                    .await
                    .map(|_| "Invite sent.")
            }
        };
        match result {
            Ok(done) => {
                self.leave_input_mode();
                self.status = Some(done.to_string());
                self.refresh_tree().await;
            }
            // Stay in the prompt (buffer intact) so the user can correct + retry.
            Err(e) => self.status = Some(format!("{e}")),
        }
    }

    /// Look a user up by username/email, then open a DM to them. A miss is a
    /// clean error, not a panic — the prompt stays open for a corrected try.
    async fn resolve_and_start_dm(
        &self,
        user_id: &str,
        identifier: &str,
    ) -> anyhow::Result<&'static str> {
        let found =
            pollis_core::commands::user::search_user_by_username(identifier.to_string(), &self.state)
                .await?;
        let Some(other) = found else {
            anyhow::bail!("No user found for “{identifier}”.");
        };
        if other.id == user_id {
            anyhow::bail!("That's you — pick someone else.");
        }
        send::start_dm(&self.state, user_id, &other.id).await?;
        Ok("DM started.")
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
