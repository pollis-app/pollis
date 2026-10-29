//! The UI state machine: auth screens, enrollment, and [`Screen::Home`].

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

/// Background-sync cadence (spec §6: ~3–5 s while foregrounded).
pub const SYNC_CADENCE: Duration = Duration::from_secs(4);

/// Text-input buffers live on [`App`], not in the variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Booting,
    Email,
    Otp,
    /// First-device signup: show the Secret Key and have the user type it back
    /// (#1337). Phase in [`App::secret_confirming`].
    SaveSecretKey,
    /// Shared by first-device signup and a second device's tail; see
    /// [`App::pin_flow`].
    SetPin,
    Unlock,
    /// Second device: enroll via approval or recover with the Secret Key.
    EnrollChoice,
    /// Second device: show the verification code and poll `enrollment_status`.
    EnrollWaiting,
    RecoverKey,
    PendingEnrollments,
    /// Existing device: type the code shown on the new device (#1096).
    ///
    /// A separate screen because the list's keys (`a`/`r`/`j`/`k`) are also
    /// characters a code can contain.
    ApproveEnrollment,
    Home,
    /// Unrecoverable boot/config error.
    Fatal,
}

/// An async operation the render loop runs after painting a "working" frame,
/// so feedback appears before a network round-trip blocks.
pub enum Action {
    Boot,
    RequestOtp,
    VerifyOtp,
    SetPinAndInit,
    Unlock,
    /// Start the background sync loop and load the tree.
    EnterHome,
    /// Fired by the UI refresh tick.
    Refresh,
    OpenSelected,
    LoadOlder,
    SendMessage,
    AcceptDm(String),
    /// Dispatches on the current [`HomeMode::Prompt`] kind.
    SubmitPrompt,
    DeleteMessage(String),
    OpenThread(String),
    /// Under the same id (#1335).
    RetryMessage(String),
    RequestEnrollment,
    PollEnrollment(String),
    Recover,
    LoadPendingEnrollments,
    ApproveEnrollment { request_id: String, code: String },
    RejectEnrollment(String),
}

impl Action {
    /// Timer-fired actions; they skip the "working…" hint, which would strobe
    /// the status line.
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
    /// The active screen's text-input buffer.
    pub input: String,
    pub status: Option<String>,
    /// True while an [`Action`] is running.
    pub busy: bool,
    pub should_quit: bool,

    email: String,
    /// Its `new_secret_key` is always moved into [`Self::pending_secret`] the
    /// moment it arrives.
    profile: Option<UserProfile>,
    /// Held only while [`Screen::SaveSecretKey`] is up; wiped on drop. Never
    /// written anywhere unless the user asks for the Emergency Kit file.
    pending_secret: Option<Zeroizing<String>>,
    /// On [`Screen::SaveSecretKey`]: true once the user is typing it back.
    pub secret_confirming: bool,

    pub home: HomeState,

    /// Which tail the [`Screen::SetPin`] success handler runs.
    pub pin_flow: PinFlow,
    pub enroll_choice: EnrollChoice,
    pub enroll_handle: Option<EnrollmentHandle>,
    /// Set once a poll is rejected/expired, so the waiting screen offers retry
    /// instead of polling.
    pub enroll_stopped: bool,
    pub approvals: ApprovalState,

    sync_loop: Option<sync::SyncLoop>,
    /// The last sync round the UI consumed. A refresh tick works only when a
    /// newer round exists; the tick itself never issues remote queries.
    last_sync_round: u64,
    /// The message pane's last-rendered size, for page scrolling and keeping
    /// the selection on screen.
    msg_height: usize,
    msg_width: u16,
    /// Overridden by the e2e driver via [`Self::with_sync_cadence`].
    sync_cadence: Duration,
}

impl App {
    pub fn new(state: Arc<AppState>) -> Self {
        Self::with_sync_cadence(state, SYNC_CADENCE)
    }

    /// Injectable sync cadence, for the e2e driver.
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

    pub fn identity(&self) -> Option<&str> {
        self.profile.as_ref().map(|p| p.username.as_str())
    }

    pub fn signed_in_user_id(&self) -> Option<&str> {
        self.profile.as_ref().map(|p| p.id.as_str())
    }

    /// Called before each frame; see [`crate::ui::message_viewport`].
    pub fn set_msg_viewport(&mut self, width: u16, height: usize) {
        self.msg_width = width;
        self.msg_height = height;
    }

    pub fn pending_secret(&self) -> Option<&str> {
        self.pending_secret.as_ref().map(|s| s.as_str())
    }

    /// The same layout the renderer draws, so selection scrolling measures
    /// what is on screen.
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

    pub fn initial_action(&self) -> Action {
        Action::Boot
    }

    /// Returns an [`Action`] when the key needs an async round-trip; the caller
    /// runs it after redrawing.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Action> {
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.should_quit = true;
            return None;
        }

        match self.screen {
            Screen::Booting => None,
            Screen::Fatal => {
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

    /// Two phases, like desktop's SaveSecretKeyScreen (#1337): show the key,
    /// then only a typed-back match moves on to the PIN.
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
                    // Drop (and so wipe) the only copy held.
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

    /// While polling, only Ctrl-C quits; once rejected/expired, Enter retries.
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
                // The core validates the key on unwrap.
                if !c.is_whitespace() {
                    self.input.push(c);
                }
                None
            }
            _ => None,
        }
    }

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
            // `a` opens code entry rather than approving: a one-key approval
            // lets a substituted ephemeral key through (#1096).
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

    /// Type the code shown on the new device, then `Enter` (#1096).
    ///
    /// Input goes through `pollis_core`'s normalizer, so a lookalike is dropped
    /// rather than mapped: turning a typed `O` into `0` would let a mis-read
    /// code compare equal, the substitution this screen exists to catch.
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
                    // Full, or not a code character: say which.
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

    /// The waiting screen's poll action; `None` pauses polling.
    pub fn enrollment_poll_action(&self) -> Option<Action> {
        if self.enroll_stopped {
            return None;
        }
        self.enroll_handle
            .as_ref()
            .map(|h| Action::PollEnrollment(h.request_id.clone()))
    }

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

    fn on_home_key(&mut self, key: KeyEvent) -> Option<Action> {
        match self.home.mode {
            HomeMode::Navigate => self.on_home_nav_key(key),
            HomeMode::Compose => self.on_compose_key(key),
            HomeMode::Prompt(_) => self.on_prompt_key(key),
            HomeMode::ConfirmDelete { .. } => self.on_confirm_delete_key(key),
        }
    }

    /// Message keys (`r`, `e`, `x`, `+`, `t`, `R`) act on the message selected
    /// in the message pane.
    fn on_home_nav_key(&mut self, key: KeyEvent) -> Option<Action> {
        match key.code {
            KeyCode::Char('q') => {
                self.should_quit = true;
                None
            }
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
            KeyCode::Char('i') => self.begin_compose(),
            KeyCode::Char('a') => self.begin_accept_dm(),
            KeyCode::Char('g') => self.begin_prompt(PromptKind::NewGroup),
            KeyCode::Char('c') => self.begin_new_channel_prompt(),
            KeyCode::Char('d') => self.begin_prompt(PromptKind::StartDm),
            KeyCode::Char('v') => self.begin_invite_prompt(),
            KeyCode::Char('r') => self.begin_reply(),
            KeyCode::Char('e') => self.begin_edit(),
            KeyCode::Char('x') | KeyCode::Delete => self.begin_delete(),
            KeyCode::Char('+') => self.begin_react(),
            KeyCode::Char('t') => self.begin_open_thread(),
            KeyCode::Char('R') => self.begin_retry(),
            KeyCode::Char('E') => {
                self.status = Some("Loading pending device enrollments…".to_string());
                self.screen = Screen::PendingEnrollments;
                Some(Action::LoadPendingEnrollments)
            }
            KeyCode::Enter => {
                if self.home.focus == Focus::Sidebar {
                    return self.activate_selection();
                }
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

    /// Enter sends (empty is a no-op); Alt-Enter or Ctrl-J adds a line break.
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

    /// `y` deletes, `n`/Esc keeps; any other key does nothing, so a stray key
    /// cannot delete.
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

    /// Whether the open conversation can be written to, explaining why not. An
    /// un-accepted DM request has no send path until accepted.
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

    /// `R`: retry a message the outbox could not deliver (#1335).
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

    fn begin_react(&mut self) -> Option<Action> {
        let (id, _, _, body) = self.selected_message_or_hint()?;
        if body.is_none() {
            self.status = Some("That message can't be reacted to.".to_string());
            return None;
        }
        self.begin_prompt(PromptKind::React { message_id: id })
    }

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

    fn begin_prompt(&mut self, kind: PromptKind) -> Option<Action> {
        self.status = Some(format!("{} — Enter to submit, Esc to cancel.", kind.label()));
        self.home.mode = HomeMode::Prompt(kind);
        self.input.clear();
        None
    }

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

    fn leave_input_mode(&mut self) {
        self.home.mode = HomeMode::Navigate;
        self.home.compose = ComposeTarget::New;
        self.input.clear();
        self.status = None;
    }

    /// In the message pane, prefetches history as the selection nears the top
    /// of what is loaded.
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

    /// Pages the message pane only, prefetching older history near the top.
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

    /// Re-selecting the already-open conversation keeps it, so a stray Enter
    /// doesn't wipe scrollback.
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

    /// Run a queued [`Action`]; errors land on the status line, never a crash.
    /// Returns an optional follow-up action for the caller to run next.
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
                        // Spec §7: an account that exists elsewhere must
                        // enroll/recover, not run first-device signup.
                        let enrollment_required = profile.enrollment_required;
                        let secret = profile.new_secret_key.take().map(Zeroizing::new);
                        self.profile = Some(profile);
                        if let (false, Some(secret)) = (enrollment_required, secret) {
                            // The Secret Key exists only in this response.
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
                let result = match self.pin_flow {
                    PinFlow::FirstDevice => auth::set_pin_and_init(&self.state, &user_id, &pin).await,
                    PinFlow::NewDevice => {
                        enroll::set_pin_and_finalize(&self.state, &user_id, &pin).await
                    }
                };
                match result {
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
                if self.sync_loop.is_none() {
                    let user_id = self.user_id();
                    self.sync_loop =
                        Some(sync::spawn_loop(self.state.clone(), user_id, self.sync_cadence));
                }
                // Load once now so the sidebar isn't empty during the first round.
                self.refresh_tree().await;
            }
            Action::Refresh => {
                // Timer-driven, so cheap: the only remote call is the open
                // conversation's newest page, once per completed sync round.
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

    async fn do_poll_enrollment(&mut self, request_id: &str) -> Option<Action> {
        match enroll::enrollment_status(&self.state, request_id.to_string()).await {
            Ok(status) => match poll_outcome(&status) {
                PollOutcome::KeepWaiting => {}
                PollOutcome::Approved => {
                    self.enroll_handle = None;
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
            // A transient error leaves the screen up; the next tick retries.
            Err(e) => self.status = Some(format!("Still waiting… ({e})")),
        }
        None
    }

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
            // Buffer kept so the user can correct and retry.
            Err(e) => self.status = Some(format!("Recovery failed: {e}")),
        }
    }

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

    async fn do_approve_enrollment(&mut self, request_id: &str, code: &str) {
        match enroll::approve(&self.state, request_id.to_string(), code.to_string()).await {
            Ok(()) => {
                self.status = Some("Device approved.".to_string());
                self.do_load_pending_enrollments().await;
            }
            Err(e) => self.status = Some(format!("Couldn't approve: {e}")),
        }
    }

    async fn do_reject_enrollment(&mut self, request_id: &str) {
        match enroll::reject(&self.state, request_id.to_string()).await {
            Ok(()) => {
                self.status = Some("Device rejected.".to_string());
                self.do_load_pending_enrollments().await;
            }
            Err(e) => self.status = Some(format!("Couldn't reject: {e}")),
        }
    }

    /// Send per [`HomeState::compose`]. A new message stays in compose; a reply
    /// or edit returns to navigation. On failure the text is kept for a retry.
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

    async fn do_retry(&mut self, message_id: &str) {
        self.status = Some(match send::retry(&self.state, message_id).await {
            Ok(()) => "Sent.".to_string(),
            Err(e) => format!("Still not sent: {e}"),
        });
        self.refresh_open().await;
    }

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

    /// Submit the active prompt, then land on the new conversation (#1337). On
    /// failure the prompt stays open, buffer intact.
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

    /// `id` is a channel or DM id, or a group id (lands on its first channel).
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

    /// Non-fatal: a failed read leaves the last-good tree up.
    async fn refresh_tree(&mut self) {
        let user_id = self.user_id();
        match data::load_conversations(&self.state, &user_id).await {
            Ok(tree) => self.home.set_tree(tree, &user_id),
            Err(e) => self.status = Some(format!("Sync read failed: {e}")),
        }
    }

    /// Merge the newest page in without disturbing loaded scrollback.
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
                    // Learn the older cursor only if we haven't paged back yet.
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

    /// Reactions, thread counts and open-thread replies. Best effort: a failure
    /// leaves the last-known values up.
    async fn refresh_extras(&mut self) {
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

    async fn load_open_older(&mut self) {
        let Some((conv_id, kind)) = self.open_target() else {
            return;
        };
        // `MessageCursor` isn't `Clone`; replaced by the page's `next_cursor`.
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

    fn open_target(&self) -> Option<(String, ConvKind)> {
        self.home
            .open
            .as_ref()
            .and_then(|o| o.kind.map(|k| (o.conv_id.clone(), k)))
    }

    /// Gives the current sync round a brief window to finish, so the terminal
    /// restore isn't racing a DB write.
    pub async fn shutdown(&mut self) {
        if let Some(sync_loop) = self.sync_loop.take() {
            let handle = sync_loop.cancel();
            let _ = tokio::time::timeout(Duration::from_secs(2), handle).await;
        }
    }

    /// Only called after a profile is established; a missing one is a bug.
    fn user_id(&self) -> String {
        self.profile
            .as_ref()
            .map(|p| p.id.clone())
            .expect("user_id requested before profile was set")
    }

    fn goto(&mut self, screen: Screen, status: &str) {
        self.screen = screen;
        self.input.clear();
        self.status = Some(status.to_string());
    }

    /// Recoverable: stay on the current screen.
    fn fail(&mut self, msg: String) {
        self.input.clear();
        self.status = Some(msg);
    }

    fn fatal(&mut self, msg: String) {
        self.screen = Screen::Fatal;
        self.status = Some(msg);
    }
}

/// Same rule as desktop's SaveSecretKeyScreen: whitespace, dashes and the
/// `A3-` prefix ignored, case-insensitive.
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

/// Word for word desktop's `auth:emergencyKit.document`
/// (`frontend/src/i18n/locales/en/auth.json`); a test enforces it.
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

/// Write the Emergency Kit to the current directory, owner-only and never
/// overwriting. Not the data dir: a recovery key beside the keystore it
/// recovers is no backup.
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

enum InputKind {
    Email,
    /// OTP = 6, PIN = 4.
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

/// What a keystroke means on the code entry screen (#1096). Separate from the
/// handler so the rule "`Enter` approves only a complete code" is testable
/// without building an `App`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CodeEntry {
    Type(char),
    Backspace,
    Approve,
    /// `Enter` on a short code.
    Incomplete,
    Cancel,
    Ignore,
}

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

    /// #1096: `Enter` approves only a full code.
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

    /// The list's command keys are plain input on the entry screen.
    #[test]
    fn on_the_entry_screen_the_list_keys_are_only_input() {
        for c in ['a', 'r', 'j', 'k', 'q'] {
            assert_eq!(
                classify_code_entry(KeyCode::Char(c), 0),
                CodeEntry::Type(c),
                "'{c}' must be typed, never treated as a command here"
            );
        }
        assert_eq!(classify_code_entry(KeyCode::Esc, 3), CodeEntry::Cancel);
        assert_eq!(classify_code_entry(KeyCode::Backspace, 3), CodeEntry::Backspace);
        assert_eq!(classify_code_entry(KeyCode::Tab, 8), CodeEntry::Ignore);
        assert_eq!(classify_code_entry(KeyCode::Delete, 8), CodeEntry::Ignore);
    }

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

    /// Read from desktop's catalogue, so a wording change there fails here.
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
