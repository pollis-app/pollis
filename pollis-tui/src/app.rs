//! The UI state machine.
//!
//! M0 gives us the skeleton (boot → event loop → quit). M1 adds the auth
//! screens: first-device signup (email → OTP → PIN) and returning-user unlock.
//! M2 grafts the group/channel/DM panes onto the [`Screen::Home`] state.

use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pollis_core::commands::auth::UserProfile;
use pollis_core::state::AppState;
use zeroize::Zeroizing;

use crate::auth::{self, Boot};
use crate::enroll::{self, EnrollmentHandle};
use crate::{data, send, sync};

use crate::enroll_flow::{poll_outcome, ApprovalState, EnrollChoice, PinFlow, PollOutcome};
use crate::content;
use crate::home::{
    is_blank, should_load_older, ComposeTarget, ConvKind, ConvRef, Focus, HomeMode, HomeState,
    OpenConversation, PromptKind, RowTarget, ThreadView, PREFETCH_MARGIN,
};
use crate::timeline;

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
    /// First-device signup: show the account's Secret Key (the Emergency Kit)
    /// and have the user type it back before going on (#1337). Desktop and
    /// mobile have always shown it; the CLI skipped it, so a CLI-only account
    /// had no recovery path at all. Phase in [`App::secret_confirming`].
    SaveSecretKey,
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
    /// Existing device: type the verification code shown on the NEW device to
    /// approve the highlighted request (#1096).
    ///
    /// A separate screen rather than an inline field because the list's keymap
    /// needs `a`/`r`/`j`/`k`, and every one of those is also a character a
    /// verification code can contain — so the two cannot share a keystroke.
    /// Reached with `a` from [`Screen::PendingEnrollments`]; `Enter` approves
    /// once the code is complete, `Esc` goes back.
    ApproveEnrollment,
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
    /// Delete one of the user's own messages (after the `y` confirmation).
    DeleteMessage(String),
    /// Open the thread rooted at this message in the message pane.
    OpenThread(String),
    /// Deliver one unsent message again (#1335), under the same id.
    RetryMessage(String),

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
    /// by `set_pin`/`initialize_identity`/`unlock`. Its `new_secret_key` is
    /// always taken out (into [`Self::pending_secret`]) the moment it arrives.
    profile: Option<UserProfile>,
    /// The first-device Secret Key, held only while [`Screen::SaveSecretKey`]
    /// is up and wiped on drop. Never written anywhere unless the user asks for
    /// the Emergency Kit file.
    pending_secret: Option<Zeroizing<String>>,
    /// On [`Screen::SaveSecretKey`]: false = showing the key, true = the user
    /// is typing it back to confirm they saved it.
    pub secret_confirming: bool,

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
    /// scroll by whole pages. Updated each frame via [`Self::set_msg_viewport`].
    msg_height: usize,
    /// Last-rendered width (in columns) of the message pane — the layout the
    /// key handler measures to keep the selected message on screen.
    msg_width: u16,
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
            pending_secret: None,
            secret_confirming: false,
            home: HomeState::new(),
            pin_flow: PinFlow::FirstDevice,
            enroll_choice: EnrollChoice::Approval,
            enroll_handle: None,
            enroll_stopped: false,
            approvals: ApprovalState::default(),
            sync_loop: None,
            last_sync_round: 0,
            msg_height: 0,
            msg_width: 80,
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

    /// Record the message pane's inner size (see [`crate::ui::message_viewport`])
    /// so page keys jump a screen and selection keeps the highlighted message on
    /// screen. Called before each frame is drawn.
    pub fn set_msg_viewport(&mut self, width: u16, height: usize) {
        self.msg_width = width;
        self.msg_height = height;
    }

    /// The Secret Key on screen, while [`Screen::SaveSecretKey`] shows it.
    pub fn pending_secret(&self) -> Option<&str> {
        self.pending_secret.as_ref().map(|s| s.as_str())
    }

    /// The open conversation laid out at the pane's current width — the same
    /// layout the renderer draws, so selection scrolling measures what is on
    /// screen.
    pub fn open_layout(&self) -> Vec<timeline::Block> {
        match &self.home.open {
            Some(open) => timeline::layout_open(
                open,
                self.signed_in_user_id().unwrap_or(""),
                self.identity().unwrap_or(""),
                self.msg_width,
            ),
            None => Vec::new(),
        }
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
            Screen::SaveSecretKey => self.on_save_secret_key(key),
            Screen::SetPin => self.on_text_key(key, InputKind::Digits(4), Action::SetPinAndInit),
            Screen::Unlock => self.on_text_key(key, InputKind::Digits(4), Action::Unlock),
            Screen::EnrollChoice => self.on_enroll_choice_key(key),
            Screen::EnrollWaiting => self.on_enroll_waiting_key(key),
            Screen::RecoverKey => self.on_recover_key(key),
            Screen::PendingEnrollments => self.on_pending_enrollments_key(key),
            Screen::ApproveEnrollment => self.on_approve_enrollment_key(key),
        }
    }

    /// The Secret Key screen (#1337). Two phases, like desktop's
    /// SaveSecretKeyScreen: first the key is shown (`s` writes the Emergency
    /// Kit file, Enter continues), then the user types or pastes it back, and
    /// only a match moves on to the PIN. Esc from the second phase shows the
    /// key again.
    fn on_save_secret_key(&mut self, key: KeyEvent) -> Option<Action> {
        if !self.secret_confirming {
            match key.code {
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    let secret = self.pending_secret.as_ref()?;
                    self.status = Some(match write_emergency_kit(secret, &self.email) {
                        Ok(path) => format!(
                            "Emergency Kit saved to {} — move it somewhere safe.",
                            path.display()
                        ),
                        Err(e) => format!("Couldn't save the Emergency Kit: {e}"),
                    });
                }
                KeyCode::Enter => {
                    self.secret_confirming = true;
                    self.input.clear();
                    self.status = Some(
                        "Type or paste your Secret Key to confirm you saved it.".to_string(),
                    );
                }
                _ => {}
            }
            return None;
        }
        match key.code {
            KeyCode::Esc => {
                self.secret_confirming = false;
                self.input.clear();
                self.status = Some("Here it is again — save it, then press Enter.".to_string());
            }
            KeyCode::Enter => {
                let matches = self
                    .pending_secret
                    .as_ref()
                    .is_some_and(|secret| secret_keys_match(secret, &self.input));
                if matches {
                    // Done with it: drop (and so wipe) the only copy we held.
                    self.pending_secret = None;
                    self.secret_confirming = false;
                    self.goto(Screen::SetPin, "Saved. Now choose a 4-digit PIN for this device.");
                } else {
                    self.status = Some(
                        "That doesn't match — try again (Esc shows the key again).".to_string(),
                    );
                }
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) if !c.is_control() => {
                self.input.push(c);
            }
            _ => {}
        }
        None
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
            // #1096: `a` no longer approves. It opens the code-entry screen,
            // because the approver has to type the code shown on the NEW device
            // — the list cannot display one (nothing fetches it) and a one-key
            // approval is exactly what let a substituted ephemeral key through.
            KeyCode::Char('a') => {
                match self.approvals.current() {
                    Some(req) => {
                        let device = req.new_device_id.clone();
                        self.input.clear();
                        self.status = Some(format!(
                            "Type the code shown on device {device} to approve it."
                        ));
                        self.screen = Screen::ApproveEnrollment;
                    }
                    None => {
                        self.status = Some("No pending requests to approve.".to_string());
                    }
                }
                None
            }
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

    /// Type the verification code shown on the NEW device, then `Enter` (#1096).
    ///
    /// `self.input` is the per-screen buffer every other typed screen uses.
    /// Characters go through `pollis_core`'s own normalizer — the same function
    /// the alphabet and length come from — so a lookalike is DROPPED rather than
    /// mapped: turning a typed `O` into `0` would let a mis-read code compare
    /// equal to the one Rust derives, which is the substitution this whole
    /// screen exists to catch.
    fn on_approve_enrollment_key(&mut self, key: KeyEvent) -> Option<Action> {
        use pollis_core::commands::device_enrollment::{
            normalize_enrollment_sas, ENROLLMENT_SAS_LEN,
        };
        match classify_code_entry(key.code, self.input.chars().count()) {
            CodeEntry::Cancel => {
                self.input.clear();
                self.status = None;
                self.screen = Screen::PendingEnrollments;
                None
            }
            CodeEntry::Backspace => {
                self.input.pop();
                None
            }
            CodeEntry::Incomplete => {
                self.status = Some(format!(
                    "The code is {ENROLLMENT_SAS_LEN} characters — read it off the new device's screen."
                ));
                None
            }
            CodeEntry::Approve => match self.approvals.current() {
                Some(req) => {
                    self.status = Some(format!("Approving device {}…", req.new_device_id));
                    Some(Action::ApproveEnrollment {
                        request_id: req.request_id.clone(),
                        code: self.input.clone(),
                    })
                }
                None => {
                    self.status = Some("That request is no longer pending.".to_string());
                    self.screen = Screen::PendingEnrollments;
                    None
                }
            },
            CodeEntry::Type(c) => {
                let next = normalize_enrollment_sas(&format!("{}{c}", self.input));
                if next == self.input {
                    // Either the buffer is full or the character is not one a
                    // code can contain. Say which, rather than swallowing it.
                    self.status = Some(if self.input.chars().count() == ENROLLMENT_SAS_LEN {
                        "The code is complete — press Enter to approve.".to_string()
                    } else {
                        format!("'{c}' is not a character a code contains.")
                    });
                } else {
                    self.input = next;
                    self.status = None;
                }
                None
            }
            CodeEntry::Ignore => None,
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

    /// Handle a key on the three-pane Home screen. The screen has four input
    /// modes — navigate, compose (typing a message), prompt (a create/invite/
    /// react bottom bar) and confirm-delete — each with its own handler.
    /// Compose/prompt keystrokes edit [`Self::input`]; the write-triggering
    /// keys return an [`Action`].
    fn on_home_key(&mut self, key: KeyEvent) -> Option<Action> {
        match self.home.mode {
            HomeMode::Navigate => self.on_home_nav_key(key),
            HomeMode::Compose => self.on_compose_key(key),
            HomeMode::Prompt(_) => self.on_prompt_key(key),
            HomeMode::ConfirmDelete { .. } => self.on_confirm_delete_key(key),
        }
    }

    /// Navigate mode: tree movement + the command keys that open compose or a
    /// create/invite prompt. The two async reads (open a conversation, page
    /// history) return an [`Action`]; the write-triggering keys hand off to the
    /// `begin_*` helpers. The message keys (`r` reply, `e` edit, `x` delete,
    /// `+` react, `t` thread) act on the message selected in the message pane.
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
                self.status = None;
                None
            }
            KeyCode::Esc => {
                self.on_home_escape();
                None
            }
            KeyCode::Up | KeyCode::Char('k') => self.on_home_up(1),
            KeyCode::Down | KeyCode::Char('j') => self.on_home_down(1),
            KeyCode::PageUp => self.on_page(true),
            KeyCode::PageDown => self.on_page(false),
            // `i` (or Enter on the message pane) enters compose for the open conv.
            KeyCode::Char('i') => self.begin_compose(),
            // `a` accepts the selected pending DM request.
            KeyCode::Char('a') => self.begin_accept_dm(),
            // Create/invite flows open an inline bottom-bar prompt.
            KeyCode::Char('g') => self.begin_prompt(PromptKind::NewGroup),
            KeyCode::Char('c') => self.begin_new_channel_prompt(),
            KeyCode::Char('d') => self.begin_prompt(PromptKind::StartDm),
            KeyCode::Char('v') => self.begin_invite_prompt(),
            // Message actions on the selected message (#1337).
            KeyCode::Char('r') => self.begin_reply(),
            KeyCode::Char('e') => self.begin_edit(),
            KeyCode::Char('x') | KeyCode::Delete => self.begin_delete(),
            KeyCode::Char('+') => self.begin_react(),
            KeyCode::Char('t') => self.begin_open_thread(),
            // `R`: retry a message the outbox could not deliver (#1335).
            KeyCode::Char('R') => self.begin_retry(),
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

    /// Esc in navigation backs out one level: close the open thread, else drop
    /// the message selection, else hand focus back to the sidebar.
    fn on_home_escape(&mut self) {
        self.status = None;
        if self.home.focus == Focus::Sidebar {
            return;
        }
        let Some(open) = self.home.open.as_mut() else {
            self.home.focus = Focus::Sidebar;
            return;
        };
        if let Some(thread) = open.thread.take() {
            open.selected = Some(thread.root_id);
            open.scroll = 0;
            self.keep_selection_visible();
            return;
        }
        if open.selected.take().is_some() {
            open.scroll = 0;
            return;
        }
        self.home.focus = Focus::Sidebar;
    }

    /// Compose mode: type into the buffer; Enter sends (empty is a no-op);
    /// Alt-Enter or Ctrl-J adds a line break; Esc leaves so navigation keys
    /// work again.
    fn on_compose_key(&mut self, key: KeyEvent) -> Option<Action> {
        let newline = (key.code == KeyCode::Enter && key.modifiers.contains(KeyModifiers::ALT))
            || (key.code == KeyCode::Char('j') && key.modifiers.contains(KeyModifiers::CONTROL));
        if newline {
            self.input.push('\n');
            return None;
        }
        match key.code {
            KeyCode::Esc => {
                self.leave_input_mode();
                None
            }
            KeyCode::Enter => {
                let attachment_edit = self.editing_attachment_message();
                if is_blank(&self.input) && !attachment_edit {
                    // Empty/whitespace-only input does not send.
                    None
                } else {
                    self.status = Some(match self.home.compose {
                        ComposeTarget::Edit { .. } => "Saving edit…".to_string(),
                        _ => "Sending…".to_string(),
                    });
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

    /// Whether the compose bar is editing a message that carries attachments —
    /// the one case an empty buffer is a real edit (it clears the caption).
    fn editing_attachment_message(&self) -> bool {
        let ComposeTarget::Edit { message_id } = &self.home.compose else {
            return false;
        };
        self.home
            .open
            .as_ref()
            .and_then(|o| o.find(message_id))
            .and_then(|m| m.content.as_deref())
            .is_some_and(|raw| !content::parse_content(raw).attachments.is_empty())
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

    /// The delete confirmation in the input bar: `y` deletes, `n` or Esc keeps
    /// the message. Nothing else does anything, so a stray key cannot delete.
    fn on_confirm_delete_key(&mut self, key: KeyEvent) -> Option<Action> {
        let HomeMode::ConfirmDelete { message_id, .. } = self.home.mode.clone() else {
            return None;
        };
        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.home.mode = HomeMode::Navigate;
                self.status = Some("Deleting…".to_string());
                Some(Action::DeleteMessage(message_id))
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                self.leave_input_mode();
                self.status = Some("Kept.".to_string());
                None
            }
            _ => None,
        }
    }

    /// Enter compose mode for the open conversation. Guards: nothing open →
    /// guidance; an un-accepted DM request → tell the user to accept it first
    /// (its MLS tree has no send path until accepted).
    fn begin_compose(&mut self) -> Option<Action> {
        if self.compose_guard() {
            self.home.compose = ComposeTarget::New;
            self.home.mode = HomeMode::Compose;
            self.home.focus = Focus::Messages;
            self.input.clear();
            let in_thread = self.home.open.as_ref().is_some_and(|o| o.thread.is_some());
            self.status = Some(if in_thread {
                "Replying in thread — Enter to send, Alt-Enter for a new line, Esc to cancel."
                    .to_string()
            } else {
                "Composing — Enter to send, Alt-Enter for a new line, Esc to cancel.".to_string()
            });
        }
        None
    }

    /// Whether the open conversation can be written to, explaining why not.
    fn compose_guard(&mut self) -> bool {
        match self.home.open.as_ref().map(|o| o.kind) {
            None => {
                self.status = Some("Open a conversation first (Enter on the sidebar).".to_string());
                false
            }
            Some(Some(ConvKind::DmRequest)) => {
                self.status =
                    Some("Accept this request first (press a) before sending.".to_string());
                false
            }
            Some(_) => true,
        }
    }

    /// The selected message, or a hint saying how to select one.
    fn selected_message_or_hint(&mut self) -> Option<(String, String, String, Option<String>)> {
        let picked = self.home.open.as_ref().and_then(|o| o.selected_message()).map(|m| {
            (
                m.id.clone(),
                m.sender_id.clone(),
                m.sender_username.clone().unwrap_or_else(|| m.sender_id.clone()),
                if m.deleted_at.is_some() || m.undecryptable {
                    None
                } else {
                    m.content.clone()
                },
            )
        });
        if picked.is_none() {
            self.status = Some(
                "Select a message first: Tab to the message pane, then ↑/↓.".to_string(),
            );
        }
        picked
    }

    /// `r`: quote-reply to the selected message.
    fn begin_reply(&mut self) -> Option<Action> {
        let (id, _, sender, body) = self.selected_message_or_hint()?;
        if !self.compose_guard() {
            return None;
        }
        let preview = content::preview(body.as_deref().unwrap_or(""), 40);
        self.home.compose = ComposeTarget::Reply {
            message_id: id,
            sender: sender.clone(),
            preview,
        };
        self.home.mode = HomeMode::Compose;
        self.home.focus = Focus::Messages;
        self.input.clear();
        self.status = Some(format!("Replying to {sender} — Enter to send, Esc to cancel."));
        None
    }

    /// `e`: edit the selected message, if it is the user's own.
    fn begin_edit(&mut self) -> Option<Action> {
        let (id, sender_id, _, body) = self.selected_message_or_hint()?;
        if sender_id != self.user_id() {
            self.status = Some("You can only edit your own messages.".to_string());
            return None;
        }
        let Some(raw) = body else {
            self.status = Some("That message can't be edited.".to_string());
            return None;
        };
        self.home.compose = ComposeTarget::Edit { message_id: id };
        self.home.mode = HomeMode::Compose;
        self.home.focus = Focus::Messages;
        self.input = content::parse_content(&raw).text;
        self.status = Some("Editing — Enter to save, Esc to cancel.".to_string());
        None
    }

    /// `x` / Delete: ask to delete the selected message, if it is the user's own.
    fn begin_delete(&mut self) -> Option<Action> {
        let (id, sender_id, _, body) = self.selected_message_or_hint()?;
        if sender_id != self.user_id() {
            self.status = Some("You can only delete your own messages.".to_string());
            return None;
        }
        let preview = content::preview(body.as_deref().unwrap_or(""), 40);
        self.status = None;
        self.home.mode = HomeMode::ConfirmDelete {
            message_id: id,
            preview,
        };
        None
    }

    /// `R`: send the selected message again, if the outbox could not deliver
    /// it (#1335). A message that went out has nothing to retry.
    fn begin_retry(&mut self) -> Option<Action> {
        let picked = self
            .home
            .open
            .as_ref()
            .and_then(|o| o.selected_message())
            .map(|m| (m.id.clone(), m.send_state.is_some()));
        match picked {
            None => {
                self.selected_message_or_hint();
                None
            }
            Some((_, false)) => {
                self.status = Some("That message was sent — nothing to retry.".to_string());
                None
            }
            Some((id, true)) => {
                self.status = Some("Sending again…".to_string());
                Some(Action::RetryMessage(id))
            }
        }
    }

    /// `+`: react to the selected message (emoji or `:shortcode:` prompt).
    fn begin_react(&mut self) -> Option<Action> {
        let (id, _, _, body) = self.selected_message_or_hint()?;
        if body.is_none() {
            self.status = Some("That message can't be reacted to.".to_string());
            return None;
        }
        self.begin_prompt(PromptKind::React { message_id: id })
    }

    /// `t`: open the thread rooted at the selected message.
    fn begin_open_thread(&mut self) -> Option<Action> {
        if self.home.open.as_ref().is_some_and(|o| o.thread.is_some()) {
            self.status = Some("Already in a thread — Esc to go back.".to_string());
            return None;
        }
        let (id, _, _, body) = self.selected_message_or_hint()?;
        if body.is_none() {
            self.status = Some("That message has no thread.".to_string());
            return None;
        }
        Some(Action::OpenThread(id))
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
        self.home.compose = ComposeTarget::New;
        self.input.clear();
        self.status = None;
    }

    /// Up/`k`: in the sidebar move the highlight; in the message pane select
    /// the next older message, keeping it on screen and prefetching history as
    /// the selection nears the top of what is loaded.
    fn on_home_up(&mut self, amount: usize) -> Option<Action> {
        match self.home.focus {
            Focus::Sidebar => {
                self.home.move_selection(-(amount as i32));
                None
            }
            Focus::Messages => {
                let open = self.home.open.as_mut()?;
                open.step_selection(-1);
                let index = open
                    .selected
                    .as_deref()
                    .and_then(|id| open.displayed().iter().position(|m| m.id == id));
                let near_top = index.is_some_and(|i| i < PREFETCH_MARGIN);
                let load = near_top
                    && open.thread.is_none()
                    && open.older_cursor.is_some()
                    && !open.loading;
                if load {
                    open.loading = true;
                }
                self.keep_selection_visible();
                if load {
                    return Some(Action::LoadOlder);
                }
                None
            }
        }
    }

    /// Down/`j`: in the sidebar move the highlight; in the message pane select
    /// the next newer message (past the newest, back to following new ones).
    fn on_home_down(&mut self, amount: usize) -> Option<Action> {
        match self.home.focus {
            Focus::Sidebar => self.home.move_selection(amount as i32),
            Focus::Messages => {
                if let Some(open) = self.home.open.as_mut() {
                    open.step_selection(1);
                    if open.selected.is_none() {
                        open.scroll = 0;
                    }
                }
                self.keep_selection_visible();
            }
        }
        None
    }

    /// PageUp / PageDown: scroll the message pane a screen at a time (the
    /// sidebar is short enough to need no paging), prefetching older history
    /// near the top.
    fn on_page(&mut self, up: bool) -> Option<Action> {
        let page = self.msg_height.max(1);
        let total = timeline::line_count(&self.open_layout());
        let open = self.home.open.as_mut()?;
        let max_scroll = total.saturating_sub(page);
        if up {
            open.scroll = (open.scroll + page).min(max_scroll);
            if open.thread.is_none()
                && should_load_older(
                    open.scroll + page,
                    total,
                    open.older_cursor.is_some(),
                    open.loading,
                )
            {
                open.loading = true;
                return Some(Action::LoadOlder);
            }
        } else {
            open.scroll = open.scroll.saturating_sub(page);
        }
        None
    }

    /// Adjust the message pane's scroll so the selected message is on screen.
    fn keep_selection_visible(&mut self) {
        let Some(selected) = self.home.open.as_ref().and_then(|o| o.selected.clone()) else {
            return;
        };
        let blocks = self.open_layout();
        let height = self.msg_height.max(1);
        if let Some(open) = self.home.open.as_mut() {
            open.scroll = timeline::scroll_to_show(&blocks, &selected, height, open.scroll);
        }
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
                    Ok(mut profile) => {
                        // The branch point (spec §7): a fresh device for an account
                        // that already exists elsewhere must enroll/recover, not
                        // run first-device signup.
                        let enrollment_required = profile.enrollment_required;
                        let secret = profile.new_secret_key.take().map(Zeroizing::new);
                        self.profile = Some(profile);
                        if let (false, Some(secret)) = (enrollment_required, secret) {
                            // First device: the account's Secret Key exists only
                            // in this response. Show it before anything else, as
                            // desktop's SaveSecretKeyScreen and mobile's
                            // emergency-kit screen do.
                            self.pin_flow = PinFlow::FirstDevice;
                            self.pending_secret = Some(secret);
                            self.secret_confirming = false;
                            self.goto(
                                Screen::SaveSecretKey,
                                "Save your Secret Key — it is shown once and never stored.",
                            );
                        } else if enrollment_required {
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
            Action::SubmitPrompt => return self.do_submit_prompt().await,
            Action::DeleteMessage(id) => self.do_delete(&id).await,
            Action::OpenThread(id) => self.do_open_thread(&id).await,
            Action::RetryMessage(id) => self.do_retry(&id).await,
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

    /// Send the compose buffer to the open conversation — as a new message, a
    /// quote reply, a thread reply, or an edit of the user's own message,
    /// per [`HomeState::compose`] — then re-read its newest page so the result
    /// appears immediately (the core stores it locally on send). A new message
    /// stays in compose so the user can keep typing; a reply or edit returns to
    /// navigation. A failure surfaces on the status line and the text is kept
    /// for a retry.
    async fn do_send(&mut self) {
        let Some((conv_id, thread_root)) = self
            .home
            .open
            .as_ref()
            .map(|o| (o.conv_id.clone(), o.thread.as_ref().map(|t| t.root_id.clone())))
        else {
            self.leave_input_mode();
            return;
        };
        // Trailing whitespace and blank lines go; the user's line breaks stay.
        let text = self.input.trim_end().trim_start_matches('\n').to_string();
        let user_id = self.user_id();
        let username = self.identity().map(|s| s.to_string());
        let target = self.home.compose.clone();
        let result: anyhow::Result<&'static str> = match &target {
            ComposeTarget::New | ComposeTarget::Reply { .. } => {
                if text.trim().is_empty() {
                    return;
                }
                let reply_to = match &target {
                    ComposeTarget::Reply { message_id, .. } => Some(message_id.clone()),
                    _ => None,
                };
                send::send_text_with(
                    &self.state,
                    &user_id,
                    username,
                    &conv_id,
                    &text,
                    reply_to,
                    thread_root,
                )
                .await
                .map(|_| "")
            }
            ComposeTarget::Edit { message_id } => {
                // Keep the attachments of a captioned message; replace the text.
                let raw = self
                    .home
                    .open
                    .as_ref()
                    .and_then(|o| o.find(message_id))
                    .and_then(|m| m.content.clone())
                    .unwrap_or_default();
                let body = content::replace_caption(&raw, &text);
                send::edit(&self.state, &conv_id, message_id, &user_id, &body)
                    .await
                    .map(|_| "Edited.")
            }
        };
        match result {
            Ok(done) => {
                self.input.clear();
                self.status = if done.is_empty() {
                    None
                } else {
                    Some(done.to_string())
                };
                if !matches!(target, ComposeTarget::New) {
                    self.home.mode = HomeMode::Navigate;
                    self.home.compose = ComposeTarget::New;
                }
                // Pin to newest and surface the result.
                if let Some(open) = self.home.open.as_mut() {
                    if matches!(target, ComposeTarget::New | ComposeTarget::Reply { .. }) {
                        open.selected = None;
                        open.scroll = 0;
                    }
                }
                self.refresh_open().await;
            }
            Err(e) => {
                self.status = Some(match target {
                    ComposeTarget::Edit { .. } => format!("Edit failed: {e}"),
                    _ => format!("Send failed: {e}"),
                })
            }
        }
    }

    /// Retry one unsent message and report how it went; the refresh shows the
    /// row settled, or still marked not sent.
    async fn do_retry(&mut self, message_id: &str) {
        self.status = Some(match send::retry(&self.state, message_id).await {
            Ok(()) => "Sent.".to_string(),
            Err(e) => format!("Still not sent: {e}"),
        });
        self.refresh_open().await;
    }

    /// Delete one of the user's own messages (or discard an unsent one), then
    /// re-read so it disappears.
    async fn do_delete(&mut self, message_id: &str) {
        // A message the outbox still holds is discarded, not deleted (#1335):
        // the core withdraws it from everyone first if it may have landed.
        let unsent = self
            .home
            .open
            .as_ref()
            .and_then(|o| o.selected_message())
            .is_some_and(|m| m.id == message_id && m.send_state.is_some());
        if unsent {
            match send::discard(&self.state, message_id).await {
                Ok(_) => {
                    self.status = Some("Discarded.".to_string());
                    if let Some(open) = self.home.open.as_mut() {
                        open.selected = None;
                    }
                    self.refresh_open().await;
                }
                Err(e) => self.status = Some(format!("Discard failed: {e}")),
            }
            return;
        }
        let user_id = self.user_id();
        match send::delete(&self.state, message_id, &user_id).await {
            Ok(()) => {
                self.status = Some("Deleted.".to_string());
                if let Some(open) = self.home.open.as_mut() {
                    if open.selected.as_deref() == Some(message_id) {
                        open.selected = None;
                    }
                }
                self.refresh_open().await;
            }
            Err(e) => self.status = Some(format!("Delete failed: {e}")),
        }
    }

    /// Open the thread rooted at `root_id` in the message pane.
    async fn do_open_thread(&mut self, root_id: &str) {
        match data::thread_replies(&self.state, root_id).await {
            Ok(replies) => {
                let count = replies.len();
                if let Some(open) = self.home.open.as_mut() {
                    open.thread = Some(ThreadView {
                        root_id: root_id.to_string(),
                        replies,
                    });
                    open.selected = None;
                    open.scroll = 0;
                }
                self.home.focus = Focus::Messages;
                self.status = Some(if count == 0 {
                    "Thread — no replies yet. i to reply, Esc to go back.".to_string()
                } else {
                    format!(
                        "Thread — {count} repl{}. i to reply, Esc to go back.",
                        if count == 1 { "y" } else { "ies" }
                    )
                });
                self.refresh_extras().await;
            }
            Err(e) => self.status = Some(format!("Couldn't open thread: {e}")),
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

    /// Dispatch the active prompt on its [`HomeMode::Prompt`] kind, then
    /// refresh the tree so a new group/channel/DM appears — and land on it:
    /// the new conversation is highlighted and opened with the message pane
    /// focused, rather than leaving the user wherever they were (#1337). Any
    /// failure (user not found, not an admin, …) lands on the status line —
    /// never a panic — and the prompt stays open, buffer intact, for a retry.
    async fn do_submit_prompt(&mut self) -> Option<Action> {
        let HomeMode::Prompt(kind) = self.home.mode.clone() else {
            return None;
        };
        let value = self.input.trim().to_string();
        if value.is_empty() {
            return None;
        }
        if let PromptKind::React { message_id } = &kind {
            return self.do_react(message_id, &value).await;
        }
        let user_id = self.user_id();
        // (status, conversation to land on)
        let result: anyhow::Result<(&'static str, Option<String>)> = match kind {
            PromptKind::NewGroup => send::new_group(&self.state, &user_id, &value)
                .await
                .map(|g| ("Group created.", Some(g.id))),
            PromptKind::NewChannel { group_id, .. } => {
                send::new_channel(&self.state, &group_id, &user_id, &value)
                    .await
                    .map(|c| ("Channel created.", Some(c.id)))
            }
            PromptKind::StartDm => self
                .resolve_and_start_dm(&user_id, &value)
                .await
                .map(|id| ("DM started.", Some(id))),
            PromptKind::Invite { group_id, .. } => {
                send::invite(&self.state, &group_id, &user_id, &value)
                    .await
                    .map(|_| ("Invite sent.", None))
            }
            PromptKind::React { .. } => unreachable!("handled above"),
        };
        match result {
            Ok((done, land_on)) => {
                self.leave_input_mode();
                self.status = Some(done.to_string());
                self.refresh_tree().await;
                if let Some(id) = land_on {
                    return self.land_on(&id);
                }
                None
            }
            Err(e) => {
                self.status = Some(format!("{e}"));
                None
            }
        }
    }

    /// Highlight and open the conversation `id` names — a channel or DM id, or
    /// a group id, which lands on that group's first channel.
    fn land_on(&mut self, id: &str) -> Option<Action> {
        let conv_id = self
            .home
            .tree
            .as_ref()
            .and_then(|t| t.groups.iter().find(|g| g.id == id))
            .and_then(|g| g.channels.first().map(|c| c.id.clone()))
            .unwrap_or_else(|| id.to_string());
        let conv = self.home.select_conversation(&conv_id)?;
        let action = self.begin_open(conv);
        self.home.focus = Focus::Messages;
        Some(action)
    }

    /// Toggle a reaction on `message_id` from what was typed at the prompt.
    async fn do_react(&mut self, message_id: &str, typed: &str) -> Option<Action> {
        let Some(emoji) = content::resolve_reaction(typed) else {
            self.status = Some(format!(
                "“{typed}” isn't an emoji or a known :shortcode: — try :+1: or paste one."
            ));
            return None;
        };
        let user_id = self.user_id();
        let already_mine = self
            .home
            .open
            .as_ref()
            .and_then(|o| o.reactions.get(message_id))
            .is_some_and(|rs| {
                rs.iter()
                    .any(|r| r.emoji == emoji && r.user_ids.contains(&user_id))
            });
        match send::toggle_reaction(&self.state, message_id, &user_id, &emoji, already_mine).await {
            Ok(added) => {
                self.leave_input_mode();
                self.status = Some(if added {
                    format!("Reacted {emoji}.")
                } else {
                    format!("Removed {emoji}.")
                });
                self.refresh_extras().await;
            }
            Err(e) => self.status = Some(format!("Couldn't react: {e}")),
        }
        None
    }

    /// Look a user up by username/email, then open a DM to them, returning its
    /// id. A miss is a clean error, not a panic — the prompt stays open for a
    /// corrected try.
    async fn resolve_and_start_dm(&self, user_id: &str, identifier: &str) -> anyhow::Result<String> {
        let found =
            pollis_core::commands::user::search_user_by_username(identifier.to_string(), &self.state)
                .await?;
        let Some(other) = found else {
            anyhow::bail!("No user found for “{identifier}”.");
        };
        if other.id == user_id {
            anyhow::bail!("That's you — pick someone else.");
        }
        let dm = send::start_dm(&self.state, user_id, &other.id).await?;
        Ok(dm.id)
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
                self.refresh_extras().await;
            }
            Err(e) => self.status = Some(format!("Read failed: {e}")),
        }
    }

    /// What the pane shows beside the messages themselves: the reactions on
    /// the newest displayed messages (one batched DS read), the thread reply
    /// counts and, with a thread open, its replies (both local). Best effort —
    /// a failure leaves the last-known values up rather than blanking them.
    async fn refresh_extras(&mut self) {
        /// The newest this many displayed messages get their reactions read.
        const REACTION_WINDOW: usize = 60;
        let Some((conv_id, root, ids)) = self.home.open.as_ref().map(|o| {
            let ids: Vec<String> = o
                .displayed()
                .iter()
                .rev()
                .filter(|m| !m.undecryptable && m.deleted_at.is_none())
                .take(REACTION_WINDOW)
                .map(|m| m.id.clone())
                .collect();
            (
                o.conv_id.clone(),
                o.thread.as_ref().map(|t| t.root_id.clone()),
                ids,
            )
        }) else {
            return;
        };
        let replies = match &root {
            Some(root_id) => data::thread_replies(&self.state, root_id).await.ok(),
            None => None,
        };
        let counts = data::thread_counts(&self.state, &conv_id).await.ok();
        let reactions = data::reactions(&self.state, ids).await;
        let Some(open) = self.home.open.as_mut() else {
            return;
        };
        if open.conv_id != conv_id {
            return;
        }
        if let (Some(replies), Some(thread)) = (replies, open.thread.as_mut()) {
            thread.replies = replies;
        }
        if let Some(counts) = counts {
            open.thread_counts = counts;
        }
        match reactions {
            Ok(reactions) => open.reactions = reactions,
            Err(e) => {
                pollis_core::diag!("[tui] reactions read failed: {e}");
            }
        }
        self.keep_selection_visible();
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
                self.refresh_extras().await;
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
                self.refresh_extras().await;
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

/// Compare a typed Secret Key with the real one the way desktop's
/// SaveSecretKeyScreen does: whitespace, dashes and the `A3-` version prefix
/// ignored, case-insensitive — so a key pasted with or without its grouping
/// still counts.
pub(crate) fn secret_keys_match(secret: &str, typed: &str) -> bool {
    fn normalize(s: &str) -> Zeroizing<String> {
        let compact = Zeroizing::new(s.chars().filter(|c| !c.is_whitespace()).collect::<String>());
        let upper = Zeroizing::new(compact.to_uppercase());
        let body = upper
            .strip_prefix("A3-")
            .or_else(|| upper.strip_prefix("A3"))
            .unwrap_or(&upper);
        Zeroizing::new(body.chars().filter(|&c| c != '-').collect())
    }
    let want = normalize(secret);
    !want.is_empty() && *want == *normalize(typed)
}

/// The Emergency Kit document, word for word the one desktop downloads and
/// mobile shares (`auth:emergencyKit.document` in
/// `frontend/src/i18n/locales/en/auth.json`) — a kit that reads differently
/// per platform is a support problem the day someone compares them.
fn emergency_kit_document(secret: &str, generated: &str) -> String {
    let lines = [
        "POLLIS — EMERGENCY KIT",
        "======================",
        "",
        "Your Secret Key is the only way to recover access to your account",
        "from a new device when you don't have any other Pollis device with",
        "you. Treat it like a master password.",
        "",
        "If you lose this key AND lose access to all of your devices,",
        "your account is unrecoverable. Pollis cannot reset it for you.",
        "",
        "  SECRET KEY:",
        "",
    ];
    let tail = [
        "",
        "Store this file somewhere safe (a password manager, encrypted",
        "backup, or printed and locked away). Anyone with this key + your",
        "email address can sign in as you on a new device.",
        "",
    ];
    let mut doc = lines.join("\n");
    doc.push('\n');
    doc.push_str(&format!("    {secret}\n"));
    doc.push_str(&tail.join("\n"));
    doc.push('\n');
    doc.push_str(&format!("Generated: {generated}\n"));
    doc
}

/// Write the Emergency Kit to the current directory — the terminal's
/// equivalent of desktop's download — as
/// `pollis-emergency-kit-<email>-<time>.txt`, owner-only and never overwriting
/// an existing file. Deliberately NOT the data dir: a recovery key kept beside
/// the keystore it recovers is no backup.
fn write_emergency_kit(secret: &str, email: &str) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write as _;
    let now = chrono::Local::now();
    let safe_email: String = email
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('_')
        .to_string();
    let mut name = String::from("pollis-emergency-kit");
    if cfg!(debug_assertions) {
        name.push_str("-DEV");
    }
    if !safe_email.is_empty() {
        name.push('-');
        name.push_str(&safe_email);
    }
    name.push_str(&format!("-{}.txt", now.format("%Y%m%d-%H%M%S")));
    let dir = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let path = dir.join(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(pollis_core::private_fs::FILE_MODE);
    }
    let mut file = options.open(&path)?;
    let doc = Zeroizing::new(emergency_kit_document(secret, &now.to_rfc3339()));
    file.write_all(doc.as_bytes())?;
    file.sync_all()?;
    Ok(path)
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

/// What a keystroke means on the verification-code entry screen (#1096).
///
/// Pure, and separate from the handler, for one reason: the rule that `Enter`
/// approves ONLY a complete code is the gate that replaced a one-keystroke
/// approval, and it has to be testable. Building an `App` needs a full `Config`,
/// so the handler itself is not — this is, and the handler is a thin match over
/// it. Same shape as [`InputKind`], which the auth screens' validation uses for
/// the same reason.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CodeEntry {
    /// Offer this character to the normalizer.
    Type(char),
    Backspace,
    /// The buffer holds a full code — approve the highlighted request with it.
    Approve,
    /// `Enter` on a short code. Does nothing except explain the width.
    Incomplete,
    /// Back to the list, buffer discarded.
    Cancel,
    Ignore,
}

/// Classify `code` against a buffer of `typed` characters.
pub(crate) fn classify_code_entry(code: KeyCode, typed: usize) -> CodeEntry {
    use pollis_core::commands::device_enrollment::ENROLLMENT_SAS_LEN;
    match code {
        KeyCode::Esc => CodeEntry::Cancel,
        KeyCode::Backspace => CodeEntry::Backspace,
        KeyCode::Enter => {
            if typed == ENROLLMENT_SAS_LEN {
                CodeEntry::Approve
            } else {
                CodeEntry::Incomplete
            }
        }
        KeyCode::Char(c) => CodeEntry::Type(c),
        _ => CodeEntry::Ignore,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The auth screens differ only in their `InputKind`; these lock the
    // per-screen validation so a wrong OTP/PIN length or a non-numeric key can't
    // reach the (DB-opening, network-hitting) command layer.

    /// #1096: the gate that replaced a one-keystroke approval. `Enter` must
    /// produce an approval ONLY when the buffer holds a full code — otherwise
    /// the screen is back to approving whatever happens to be there.
    #[test]
    fn enter_approves_only_a_complete_code() {
        use pollis_core::commands::device_enrollment::ENROLLMENT_SAS_LEN;

        for typed in 0..ENROLLMENT_SAS_LEN {
            assert_eq!(
                classify_code_entry(KeyCode::Enter, typed),
                CodeEntry::Incomplete,
                "{typed} of {ENROLLMENT_SAS_LEN} characters must not approve"
            );
        }
        assert_eq!(
            classify_code_entry(KeyCode::Enter, ENROLLMENT_SAS_LEN),
            CodeEntry::Approve
        );
    }

    /// The list keys the previous design used — `a` to approve, `j`/`k` to move
    /// — are all characters a code can contain, which is why code entry is its
    /// own screen. On THAT screen every one of them is just input.
    #[test]
    fn on_the_entry_screen_the_list_keys_are_only_input() {
        for c in ['a', 'r', 'j', 'k', 'q'] {
            assert_eq!(
                classify_code_entry(KeyCode::Char(c), 0),
                CodeEntry::Type(c),
                "'{c}' must be typed, never treated as a command here"
            );
        }
        // Esc is the way out; it is not a code character.
        assert_eq!(classify_code_entry(KeyCode::Esc, 3), CodeEntry::Cancel);
        assert_eq!(classify_code_entry(KeyCode::Backspace, 3), CodeEntry::Backspace);
        // Anything else is inert rather than accidentally submitting.
        assert_eq!(classify_code_entry(KeyCode::Tab, 8), CodeEntry::Ignore);
        assert_eq!(classify_code_entry(KeyCode::Delete, 8), CodeEntry::Ignore);
    }

    /// The Secret Key confirmation accepts the key however it was pasted —
    /// grouped or not, any case, with or without the version prefix — and
    /// nothing else, the same rule as desktop's SaveSecretKeyScreen.
    #[test]
    fn a_typed_secret_key_matches_however_it_was_pasted() {
        let key = "A3-ABCDE-FGHJK-MNPQR-STVWX-YZ012-34567";
        assert!(secret_keys_match(key, key));
        assert!(secret_keys_match(key, "a3-abcde-fghjk-mnpqr-stvwx-yz012-34567"));
        assert!(secret_keys_match(key, " ABCDEFGHJKMNPQRSTVWXYZ01234567 "));
        assert!(secret_keys_match(key, "A3 ABCDE FGHJK MNPQR STVWX YZ012 34567"));
        assert!(!secret_keys_match(key, "A3-ABCDE-FGHJK-MNPQR-STVWX-YZ012-3456"));
        assert!(!secret_keys_match(key, ""));
        assert!(!secret_keys_match("", ""));
    }

    /// The kit the CLI writes is byte-for-byte the document desktop downloads
    /// and mobile shares — read from desktop's own catalogue, so a wording
    /// change there fails here instead of drifting.
    #[test]
    fn the_emergency_kit_is_the_desktop_document() {
        let catalogue: serde_json::Value = serde_json::from_str(include_str!(
            "../../frontend/src/i18n/locales/en/auth.json"
        ))
        .expect("auth.json");
        let template = catalogue["emergencyKit"]["document"]
            .as_str()
            .expect("emergencyKit.document");
        let expected = template
            .replace("{{secretKey}}", "A3-KEY")
            .replace("{{generated}}", "2026-10-15T00:00:00Z");
        assert_eq!(emergency_kit_document("A3-KEY", "2026-10-15T00:00:00Z"), expected);
    }

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
