//! Pure UI-state + decision logic for the M4b multi-device enrollment / recovery
//! screens — the interactive shell on top of the already-gated `pollis_tui::enroll`
//! data path (`enroll_smoke` / `recover_smoke`).
//!
//! Everything here is a **pure function of state**, mirroring the `home.rs`
//! discipline: the async work (requesting enrollment, polling status, approving)
//! lives in `app.rs`; the branch/transition rules — which flow a profile takes,
//! what a status poll means, list-selection movement — live here so they are
//! unit-tested in isolation and the screens stay correct-by-construction.

use crate::enroll::{EnrollmentStatus, PendingEnrollmentRequest};

/// Which onboarding flow the shared [`crate::app::Screen::SetPin`] screen finishes
/// into. First-device signup and a second device diverge only in the SetPin tail
/// (`initialize_identity` alone vs `finalize` → `initialize_identity`), so one
/// flag on `App` is enough to route the success handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PinFlow {
    /// First device: `set_pin` → `initialize_identity` (unchanged M1 path).
    #[default]
    FirstDevice,
    /// A second device that has already installed the account key into
    /// `state.unlock` (via sibling approval OR Secret-Key recovery):
    /// `set_pin` → `enroll::finalize` → `initialize_identity`.
    NewDevice,
}

/// The two paths offered on the enroll-choice screen when `verify_otp` reports
/// `enrollment_required` (this email already has an account on another device).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EnrollChoice {
    /// Ask an existing device to approve this one (`enroll::request_enrollment`).
    #[default]
    Approval,
    /// Unwrap the account key with the saved Secret Key (`enroll::recover`).
    Recover,
}

impl EnrollChoice {
    /// The choices in display order (Approval first — the common case).
    pub const ALL: [EnrollChoice; 2] = [EnrollChoice::Approval, EnrollChoice::Recover];

    /// Flip to the other choice (Up/Down on a two-item list).
    pub fn toggle(self) -> Self {
        match self {
            EnrollChoice::Approval => EnrollChoice::Recover,
            EnrollChoice::Recover => EnrollChoice::Approval,
        }
    }

    /// The one-line label shown for this choice.
    pub fn label(self) -> &'static str {
        match self {
            EnrollChoice::Approval => "Enroll via approval from another device",
            EnrollChoice::Recover => "Recover with my Secret Key",
        }
    }
}

/// What a new device's poll of `enrollment_status` means for the UI. Kept a pure
/// projection of [`EnrollmentStatus`] (which isn't `Copy`/`Eq`) so the waiting
/// screen's state machine is unit-tested without a live request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollOutcome {
    /// Still `Pending` — keep polling on the next tick.
    KeepWaiting,
    /// `Approved` — the account key is installed; advance to set this device's PIN.
    Approved,
    /// A sibling device rejected the request. Terminal; let the user retry/quit.
    Rejected,
    /// The request's TTL elapsed. Terminal; let the user retry/quit.
    Expired,
}

/// Map a raw `enrollment_status` poll to its UI outcome.
pub fn poll_outcome(status: &EnrollmentStatus) -> PollOutcome {
    match status {
        EnrollmentStatus::Pending => PollOutcome::KeepWaiting,
        EnrollmentStatus::Approved => PollOutcome::Approved,
        EnrollmentStatus::Rejected => PollOutcome::Rejected,
        EnrollmentStatus::Expired => PollOutcome::Expired,
    }
}

/// The existing-device "Pending device enrollments" view state: the fetched
/// requests plus which row is highlighted. No modal — this backs a full-screen
/// list (see `ui.rs`). Selection movement/clamping is pure so it survives a
/// live-refresh of the list without landing out of bounds.
#[derive(Debug, Default)]
pub struct ApprovalState {
    pub requests: Vec<PendingEnrollmentRequest>,
    /// Index into `requests` of the highlighted row.
    pub selected: usize,
}

impl ApprovalState {
    /// Replace the list (from `enroll::pending_requests`), clamping the selection
    /// so a shrunk list can't leave the highlight past the end.
    pub fn set_requests(&mut self, requests: Vec<PendingEnrollmentRequest>) {
        self.requests = requests;
        self.selected = clamp_index(self.requests.len(), self.selected);
    }

    /// Move the highlight by `dir` (+1 down / -1 up), staying within the list.
    pub fn move_selection(&mut self, dir: i32) {
        self.selected = step_index(self.requests.len(), self.selected, dir);
    }

    /// The highlighted request, if the list is non-empty.
    pub fn current(&self) -> Option<&PendingEnrollmentRequest> {
        self.requests.get(self.selected)
    }
}

/// Clamp `idx` into `[0, len)` (or 0 when empty). Keeps a re-fetched, shorter
/// list from stranding the highlight off the end.
pub fn clamp_index(len: usize, idx: usize) -> usize {
    if len == 0 {
        0
    } else {
        idx.min(len - 1)
    }
}

/// Move `idx` by `dir` within `[0, len)`, sticking at the ends (no wrap).
pub fn step_index(len: usize, idx: usize, dir: i32) -> usize {
    if len == 0 {
        return 0;
    }
    let last = len as i32 - 1;
    (idx as i32 + dir).clamp(0, last) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(id: &str, code: &str) -> PendingEnrollmentRequest {
        PendingEnrollmentRequest {
            request_id: id.to_string(),
            new_device_id: format!("dev-{id}"),
            verification_code: code.to_string(),
            created_at: "t".to_string(),
            expires_at: "t".to_string(),
        }
    }

    #[test]
    fn enroll_choice_toggles_and_labels_both_paths() {
        assert_eq!(EnrollChoice::Approval.toggle(), EnrollChoice::Recover);
        assert_eq!(EnrollChoice::Recover.toggle(), EnrollChoice::Approval);
        assert_eq!(EnrollChoice::ALL.len(), 2);
        assert!(EnrollChoice::Approval.label().contains("approval"));
        assert!(EnrollChoice::Recover.label().contains("Secret Key"));
    }

    #[test]
    fn poll_outcome_projects_every_status() {
        assert_eq!(poll_outcome(&EnrollmentStatus::Pending), PollOutcome::KeepWaiting);
        assert_eq!(poll_outcome(&EnrollmentStatus::Approved), PollOutcome::Approved);
        assert_eq!(poll_outcome(&EnrollmentStatus::Rejected), PollOutcome::Rejected);
        assert_eq!(poll_outcome(&EnrollmentStatus::Expired), PollOutcome::Expired);
    }

    #[test]
    fn step_index_sticks_at_ends_and_survives_empty() {
        assert_eq!(step_index(3, 0, -1), 0);
        assert_eq!(step_index(3, 0, 1), 1);
        assert_eq!(step_index(3, 2, 1), 2);
        assert_eq!(step_index(0, 0, 1), 0);
    }

    #[test]
    fn clamp_index_pulls_a_stale_selection_into_range() {
        assert_eq!(clamp_index(0, 5), 0);
        assert_eq!(clamp_index(3, 5), 2);
        assert_eq!(clamp_index(3, 1), 1);
    }

    #[test]
    fn approval_state_clamps_selection_when_the_list_shrinks() {
        let mut s = ApprovalState::default();
        s.set_requests(vec![req("a", "111111"), req("b", "222222"), req("c", "333333")]);
        s.selected = 2;
        assert_eq!(s.current().map(|r| r.request_id.as_str()), Some("c"));
        // A refresh that drops the last two requests must not strand the highlight.
        s.set_requests(vec![req("a", "111111")]);
        assert_eq!(s.selected, 0);
        assert_eq!(s.current().map(|r| r.request_id.as_str()), Some("a"));
        // Emptied list → no current, selection safe at 0.
        s.set_requests(vec![]);
        assert_eq!(s.selected, 0);
        assert!(s.current().is_none());
    }

    #[test]
    fn approval_state_moves_within_bounds() {
        let mut s = ApprovalState::default();
        s.set_requests(vec![req("a", "1"), req("b", "2")]);
        s.move_selection(1);
        assert_eq!(s.selected, 1);
        // Off the bottom stays put.
        s.move_selection(1);
        assert_eq!(s.selected, 1);
        s.move_selection(-1);
        assert_eq!(s.selected, 0);
    }
}
