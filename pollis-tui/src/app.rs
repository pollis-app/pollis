//! The UI state machine.
//!
//! M0 gives us the skeleton (boot → event loop → quit). M1 adds the auth
//! screens: first-device signup (email → OTP → PIN) and returning-user unlock.
//! M2 will graft the group/channel/DM panes onto the [`Screen::Home`] state.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use pollis_core::commands::auth::UserProfile;
use pollis_core::state::AppState;

use crate::auth::{self, Boot};

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
    /// Signed in and unlocked. M2 replaces this with the three-pane client.
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
        }
    }

    /// The signed-in user's display name, if any (used by the header bar).
    pub fn identity(&self) -> Option<&str> {
        self.profile.as_ref().map(|p| p.username.as_str())
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
            Screen::Home => {
                if key.code == KeyCode::Char('q') {
                    self.should_quit = true;
                }
                None
            }
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

    /// Run a queued async [`Action`] against the core command surface. All error
    /// handling funnels through [`Self::fail`] so the UI never dies silently.
    /// The caller sets/clears [`Self::busy`] around this so the "working…" frame
    /// paints before the (possibly network-blocking) call.
    pub async fn run(&mut self, action: Action) {
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
                    Ok(()) => self.goto(Screen::Home, "You're signed in."),
                    Err(e) => self.fail(format!("Couldn't set PIN: {e}")),
                }
            }
            Action::Unlock => {
                let pin = self.input.clone();
                let user_id = self.user_id();
                match auth::unlock(&self.state, &user_id, &pin).await {
                    Ok(()) => self.goto(Screen::Home, "Unlocked."),
                    Err(e) => self.fail(format!("Unlock failed: {e}")),
                }
            }
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
