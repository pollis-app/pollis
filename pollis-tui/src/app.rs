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

use crate::auth::{self, Boot};
use crate::enroll::{self, EnrollmentHandle};
use crate::{data, send, sync};

use crate::enroll_flow::{poll_outcome, ApprovalState, EnrollChoice, PinFlow, PollOutcome};
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
    /// Set a PIN for THIS device — shared by first-device signup and a second
    /// device's enroll/recover tail (the flow is tracked by [`App::pin_flow`]).
    SetPin,
    /// Returning user: enter the PIN to unlock the local DB.
    Unlock,
    /// Second device (M4b): this account already exists elsewhere — choose to
    /// enroll via a sibling's approval or recover with the Secret Key.
    EnrollChoice,
    /// Second device (M4b): the enrollment request is out; show its verification
    /// code and poll `enrollment_status` until approved/rejected.
    EnrollWaiting,
    /// Second device (M4b): enter the Secret Key (Emergency Kit) to recover.
    RecoverKey,
    /// Existing device (M4b): the "Pending device enrollments" list — approve or
    /// reject other devices requesting to join this account.
    PendingEnrollments,
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

    // ── M4b: multi-device enrollment / recovery ──────────────────────────────
    /// New device: kick off a sibling-approval enrollment request.
    RequestEnrollment,
    /// New device: poll `enrollment_status` once for the open request.
    PollEnrollment(String),
    /// New device: unwrap the account key with the entered Secret Key.
    Recover,
    /// Existing device: (re)load the pending enrollment requests for this account.
    LoadPendingEnrollments,
    /// Existing device: approve a pending request, confirming its code.
    ApproveEnrollment { request_id: String, code: String },
    /// Existing device: reject a pending request.
    RejectEnrollment(String),
}

impl Action {
    /// Actions the UI refresh tick fires on its own. They run without the
    /// "working…" busy hint — flashing it on a 750ms timer strobes the status
    /// line (and for [`Action::Refresh`], the work is a cheap in-memory
    /// snapshot read anyway).
    pub fn is_background(&self) -> bool {
        matches!(
            self,
            Action::Refresh | Action::PollEnrollment(_) | Action::LoadPendingEnrollments
        )
    }
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

    // ── M4b enrollment / recovery state ──────────────────────────────────────
    /// Which flow the [`Screen::SetPin`] success handler completes into.
    pub pin_flow: PinFlow,
    /// Highlighted option on the [`Screen::EnrollChoice`] screen.
    pub enroll_choice: EnrollChoice,
    /// The open enrollment request (new device): its `request_id` (polled) and
    /// `verification_code` (shown for the sibling to confirm).
    pub enroll_handle: Option<EnrollmentHandle>,
    /// Set once a poll reaches a terminal non-approved state (rejected/expired),
    /// so the waiting screen offers retry instead of continuing to poll.
    pub enroll_stopped: bool,
    /// Existing device: the pending-approvals list + highlight.
    pub approvals: ApprovalState,

    /// The running background poll loop; cancelled on quit for a clean shutdown.
    sync_loop: Option<sync::SyncLoop>,
    /// The last sync round whose snapshot the UI consumed. A refresh tick only
    /// does work when the loop has completed a newer round — the tick itself
    /// must never issue remote queries (that's the sync loop's job).
    last_sync_round: u64,
    /// Last-rendered height (in rows) of the message pane, so the key handler can
    /// scroll by whole pages. Updated by the renderer via [`Self::set_msg_height`].
    msg_height: usize,
    /// Cadence the background sync loop is spawned at on [`Action::EnterHome`].
    /// Defaults to [`SYNC_CADENCE`] (the 4s production value); the e2e driver
    /// overrides it via [`Self::with_sync_cadence`] so a headless test doesn't
    /// wait seconds per round while still exercising the real spawn_loop →
    /// SyncSnapshot → [`Action::Refresh`] path.
    sync_cadence: Duration,
}

impl App {
    /// Production constructor: the background sync loop runs at [`SYNC_CADENCE`].
    pub fn new(state: Arc<AppState>) -> Self {
        Self::with_sync_cadence(state, SYNC_CADENCE)
    }

    /// Like [`Self::new`] but with an injectable background-sync cadence. Used
    /// only by the e2e driver — the binary always uses [`Self::new`] (4s).
    pub fn with_sync_cadence(state: Arc<AppState>, sync_cadence: Duration) -> Self {
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
            pin_flow: PinFlow::FirstDevice,
            enroll_choice: EnrollChoice::Approval,
            enroll_handle: None,
            enroll_stopped: false,
            approvals: ApprovalState::default(),
            sync_loop: None,
            last_sync_round: 0,
            msg_height: 0,
            sync_cadence,
        }
    }

    /// The signed-in user's display name, if any (used by the header bar).
    pub fn identity(&self) -> Option<&str> {
        self.profile.as_ref().map(|p| p.username.as_str())
    }

    /// The signed-in user's id, once a profile has been established (after
    /// verify-otp). Read-only accessor exposed for the e2e driver, which needs
    /// it to establish DM membership through the core command layer before
    /// exercising send/receive through the real UI.
    pub fn signed_in_user_id(&self) -> Option<&str> {
        self.profile.as_ref().map(|p| p.id.as_str())
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
            Screen::EnrollChoice => self.on_enroll_choice_key(key),
            Screen::EnrollWaiting => self.on_enroll_waiting_key(key),
            Screen::RecoverKey => self.on_recover_key(key),
            Screen::PendingEnrollments => self.on_pending_enrollments_key(key),
        }
    }

    /// Enroll-choice screen (second device): pick between sibling-approval and
    /// Secret-Key recovery. ↑/↓ (or `1`/`2`) move the highlight, Enter commits.
    fn on_enroll_choice_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Up | KeyCode::Down | KeyCode::Char('k') | KeyCode::Char('j') => {
                self.enroll_choice = self.enroll_choice.toggle();
                None
            }
            KeyCode::Char('1') => {
                self.enroll_choice = EnrollChoice::Approval;
                self.commit_enroll_choice()
            }
            KeyCode::Char('2') => {
                self.enroll_choice = EnrollChoice::Recover;
                self.commit_enroll_choice()
            }
            KeyCode::Enter => self.commit_enroll_choice(),
            KeyCode::Char('q') => {
                self.should_quit = true;
                None
            }
            _ => None,
        }
    }

    /// Act on the highlighted enroll choice: approval kicks off a request; recovery
    /// opens the Secret-Key entry screen.
    fn commit_enroll_choice(&mut self) -> Option<Action> {
        match self.enroll_choice {
            EnrollChoice::Approval => {
                self.status = Some("Requesting approval from your other device…".to_string());
                Some(Action::RequestEnrollment)
            }
            EnrollChoice::Recover => {
                self.goto(
                    Screen::RecoverKey,
                    "Enter your Secret Key (Emergency Kit), then press Enter.",
                );
                None
            }
        }
    }

    /// Waiting-for-approval screen (second device). While polling, only Ctrl-C
    /// quits. Once a poll comes back rejected/expired, Enter returns to the choice
    /// screen to try again.
    fn on_enroll_waiting_key(&mut self, key: KeyEvent) -> Option<Action> {
        if self.enroll_stopped {
            match key.code {
                KeyCode::Enter => {
                    self.enroll_handle = None;
                    self.enroll_stopped = false;
                    self.goto(Screen::EnrollChoice, "This account already exists on another device.");
                }
                KeyCode::Char('q') => self.should_quit = true,
                _ => {}
            }
        }
        None
    }

    /// Secret-Key entry (second device): a free-text field. Enter recovers; Esc
    /// returns to the choice screen.
    fn on_recover_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc => {
                self.goto(Screen::EnrollChoice, "This account already exists on another device.");
                None
            }
            KeyCode::Enter => {
                if is_blank(&self.input) {
                    self.status = Some("Paste or type your Secret Key, or Esc to go back.".to_string());
                    None
                } else {
                    self.status = Some("Recovering with your Secret Key…".to_string());
                    Some(Action::Recover)
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
                None
            }
            KeyCode::Char(c) => {
                // The Secret Key can carry any non-whitespace glyph (hyphens,
                // mixed case); the core validates it on unwrap.
                if !c.is_whitespace() {
                    self.input.push(c);
                }
                None
            }
            _ => None,
        }
    }

    /// Pending-enrollments list (existing device): ↑/↓ move, `a` approves the
    /// highlighted request (with its shown code), `r` rejects it, Esc/`q` returns
    /// to Home. No modal — a full-screen list with an action line.
    fn on_pending_enrollments_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.status = None;
                self.screen = Screen::Home;
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.approvals.move_selection(-1);
                None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.approvals.move_selection(1);
                None
            }
            KeyCode::Char('a') => match self.approvals.current() {
                Some(req) => {
                    self.status = Some(format!("Approving device {}…", req.new_device_id));
                    Some(Action::ApproveEnrollment {
                        request_id: req.request_id.clone(),
                        code: req.verification_code.clone(),
                    })
                }
                None => {
                    self.status = Some("No pending requests to approve.".to_string());
                    None
                }
            },
            KeyCode::Char('r') => match self.approvals.current() {
                Some(req) => {
                    self.status = Some(format!("Rejecting device {}…", req.new_device_id));
                    Some(Action::RejectEnrollment(req.request_id.clone()))
                }
                None => {
                    self.status = Some("No pending requests to reject.".to_string());
                    None
                }
            },
            _ => None,
        }
    }

    /// The poll action for the waiting screen's refresh tick, if a request is open
    /// and hasn't already reached a terminal state. `None` pauses polling.
    pub fn enrollment_poll_action(&self) -> Option<Action> {
        if self.enroll_stopped {
            return None;
        }
        self.enroll_handle
            .as_ref()
            .map(|h| Action::PollEnrollment(h.request_id.clone()))
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
            // `E` opens the pending device-enrollments list (approve/reject a
            // second device requesting to join this account).
            KeyCode::Char('E') => {
                self.status = Some("Loading pending device enrollments…".to_string());
                self.screen = Screen::PendingEnrollments;
                Some(Action::LoadPendingEnrollments)
            }
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
                        // The branch point (spec §7): a fresh device for an account
                        // that already exists elsewhere must enroll/recover, not
                        // run first-device signup.
                        let enrollment_required = profile.enrollment_required;
                        self.profile = Some(profile);
                        if enrollment_required {
                            self.pin_flow = PinFlow::NewDevice;
                            self.enroll_choice = EnrollChoice::Approval;
                            self.goto(
                                Screen::EnrollChoice,
                                "This account already exists on another device.",
                            );
                        } else {
                            self.pin_flow = PinFlow::FirstDevice;
                            self.goto(Screen::SetPin, "Choose a 4-digit PIN to secure this device.");
                        }
                    }
                    Err(e) => self.fail(format!("{e}")),
                }
            }
            Action::SetPinAndInit => {
                let pin = self.input.clone();
                let user_id = self.user_id();
                // First device: set_pin → initialize_identity. A second device
                // (already holding the account key in `state.unlock`, via approval
                // or recovery): set_pin → finalize → initialize_identity.
                let result = match self.pin_flow {
                    PinFlow::FirstDevice => auth::set_pin_and_init(&self.state, &user_id, &pin).await,
                    PinFlow::NewDevice => {
                        enroll::set_pin_and_finalize(&self.state, &user_id, &pin).await
                    }
                };
                match result {
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
                // Start the background poll loop (spec §6). It does the remote
                // work and publishes a per-round snapshot; the UI's refresh tick
                // consumes that snapshot without issuing queries of its own.
                if self.sync_loop.is_none() {
                    let user_id = self.user_id();
                    self.sync_loop =
                        Some(sync::spawn_loop(self.state.clone(), user_id, self.sync_cadence));
                }
                // Load the tree once immediately (user-initiated, so a visible
                // "working…" is fine) so the sidebar isn't empty while the first
                // background round runs.
                self.refresh_tree().await;
            }
            Action::Refresh => {
                // Timer-driven and must stay cheap: consume the sync loop's
                // latest snapshot; if no new round completed since the last
                // tick there is nothing to do. The only remote call is the
                // open conversation's newest-page read, and only once per
                // completed sync round.
                let snapshot = self
                    .sync_loop
                    .as_ref()
                    .and_then(|l| l.snapshot.borrow().clone());
                if let Some(snap) = snapshot {
                    if snap.round > self.last_sync_round {
                        self.last_sync_round = snap.round;
                        let user_id = self.user_id();
                        self.home.set_tree(snap.tree.clone(), &user_id);
                        self.refresh_open().await;
                        self.home.refreshes = self.home.refreshes.wrapping_add(1);
                    }
                }
            }
            Action::OpenSelected => self.load_open_first_page().await,
            Action::LoadOlder => self.load_open_older().await,
            Action::SendMessage => self.do_send().await,
            Action::AcceptDm(id) => self.do_accept_dm(&id).await,
            Action::SubmitPrompt => self.do_submit_prompt().await,
            Action::RequestEnrollment => self.do_request_enrollment().await,
            Action::PollEnrollment(request_id) => return self.do_poll_enrollment(&request_id).await,
            Action::Recover => self.do_recover().await,
            Action::LoadPendingEnrollments => self.do_load_pending_enrollments().await,
            Action::ApproveEnrollment { request_id, code } => {
                self.do_approve_enrollment(&request_id, &code).await
            }
            Action::RejectEnrollment(request_id) => self.do_reject_enrollment(&request_id).await,
        }
        None
    }

    /// New device: request sibling-approval enrollment. On success show the
    /// verification code and start polling; on failure stay on the choice screen.
    async fn do_request_enrollment(&mut self) {
        let user_id = self.user_id();
        match enroll::request_enrollment(&self.state, user_id).await {
            Ok(handle) => {
                self.enroll_handle = Some(handle);
                self.enroll_stopped = false;
                self.goto(
                    Screen::EnrollWaiting,
                    "Waiting for approval — enter the code below on your other device.",
                );
            }
            Err(e) => {
                self.screen = Screen::EnrollChoice;
                self.status = Some(format!("Couldn't start enrollment: {e}"));
            }
        }
    }

    /// New device: poll the open request once. `Approved` advances to set this
    /// device's PIN (which finalizes enrollment); `Rejected`/`Expired` stop the
    /// poll and let the user retry; `Pending` keeps waiting. Returns a follow-up
    /// action so an approval flows straight into the PIN step.
    async fn do_poll_enrollment(&mut self, request_id: &str) -> Option<Action> {
        match enroll::enrollment_status(&self.state, request_id.to_string()).await {
            Ok(status) => match poll_outcome(&status) {
                PollOutcome::KeepWaiting => {}
                PollOutcome::Approved => {
                    self.enroll_handle = None;
                    // The account key is now in `state.unlock`; set this device's
                    // own PIN, then finalize (via the NewDevice pin_flow tail).
                    self.goto(
                        Screen::SetPin,
                        "Approved! Choose a 4-digit PIN to secure this device.",
                    );
                }
                PollOutcome::Rejected => {
                    self.enroll_stopped = true;
                    self.status =
                        Some("Enrollment was rejected. Press Enter to try again.".to_string());
                }
                PollOutcome::Expired => {
                    self.enroll_stopped = true;
                    self.status =
                        Some("The request expired. Press Enter to try again.".to_string());
                }
            },
            // A transient read error just leaves the waiting screen up; the next
            // tick retries.
            Err(e) => self.status = Some(format!("Still waiting… ({e})")),
        }
        None
    }

    /// New device: Secret-Key recovery. On success the account key is installed —
    /// advance to set this device's PIN (finalize runs in the NewDevice pin tail).
    /// A bad key surfaces the error and keeps the entry screen up.
    async fn do_recover(&mut self) {
        let user_id = self.user_id();
        let secret_key = self.input.trim().to_string();
        match enroll::recover(&self.state, user_id, secret_key).await {
            Ok(()) => {
                self.goto(
                    Screen::SetPin,
                    "Recovered! Choose a 4-digit PIN to secure this device.",
                );
            }
            // Stay on RecoverKey (buffer intact) so the user can correct + retry.
            Err(e) => self.status = Some(format!("Recovery failed: {e}")),
        }
    }

    /// Existing device: load the account's pending enrollment requests, preserving
    /// the highlight across a refresh.
    async fn do_load_pending_enrollments(&mut self) {
        let user_id = self.user_id();
        match enroll::pending_requests(&self.state, user_id).await {
            Ok(requests) => {
                let count = requests.len();
                self.approvals.set_requests(requests);
                if count == 0 {
                    self.status = Some("No devices are waiting for approval.".to_string());
                }
            }
            Err(e) => self.status = Some(format!("Couldn't load requests: {e}")),
        }
    }

    /// Existing device: approve a request (confirming its shown code), then reload.
    async fn do_approve_enrollment(&mut self, request_id: &str, code: &str) {
        match enroll::approve(&self.state, request_id.to_string(), code.to_string()).await {
            Ok(()) => {
                self.status = Some("Device approved.".to_string());
                self.do_load_pending_enrollments().await;
            }
            Err(e) => self.status = Some(format!("Couldn't approve: {e}")),
        }
    }

    /// Existing device: reject a request, then reload the list.
    async fn do_reject_enrollment(&mut self, request_id: &str) {
        match enroll::reject(&self.state, request_id.to_string()).await {
            Ok(()) => {
                self.status = Some("Device rejected.".to_string());
                self.do_load_pending_enrollments().await;
            }
            Err(e) => self.status = Some(format!("Couldn't reject: {e}")),
        }
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
