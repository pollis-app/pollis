//! Pure state + transition rules for the enrollment / recovery screens; the
//! async work lives in `app.rs` (same split as `home.rs`).

use crate::enroll::{EnrollmentStatus, PendingEnrollmentRequest};

/// Which tail the shared [`crate::app::Screen::SetPin`] screen runs; the two
/// flows differ only there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PinFlow {
    /// `set_pin` → `initialize_identity`.
    #[default]
    FirstDevice,
    /// Account key already in `state.unlock` (approval or recovery):
    /// `set_pin` → `enroll::finalize` → `initialize_identity`.
    NewDevice,
}

/// Offered when `verify_otp` reports `enrollment_required`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EnrollChoice {
    /// `enroll::request_enrollment`.
    #[default]
    Approval,
    /// `enroll::recover`.
    Recover,
}

impl EnrollChoice {
    /// Display order.
    pub const ALL: [EnrollChoice; 2] = [EnrollChoice::Approval, EnrollChoice::Recover];

    pub fn toggle(self) -> Self {
        match self {
            EnrollChoice::Approval => EnrollChoice::Recover,
            EnrollChoice::Recover => EnrollChoice::Approval,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            EnrollChoice::Approval => "Enroll via approval from another device",
            EnrollChoice::Recover => "Recover with my Secret Key",
        }
    }
}

/// A `Copy`/`Eq` projection of [`EnrollmentStatus`], so the waiting screen is
/// testable without a live request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollOutcome {
    KeepWaiting,
    /// The account key is installed; advance to this device's PIN.
    Approved,
    Rejected,
    Expired,
}

pub fn poll_outcome(status: &EnrollmentStatus) -> PollOutcome {
    match status {
        EnrollmentStatus::Pending => PollOutcome::KeepWaiting,
        EnrollmentStatus::Approved => PollOutcome::Approved,
        EnrollmentStatus::Rejected => PollOutcome::Rejected,
        EnrollmentStatus::Expired => PollOutcome::Expired,
    }
}

/// The existing-device "Pending device enrollments" list (a full screen, not a
/// modal).
#[derive(Debug, Default)]
pub struct ApprovalState {
    pub requests: Vec<PendingEnrollmentRequest>,
    pub selected: usize,
}

impl ApprovalState {
    /// Replace the list, clamping the selection into the new length.
    pub fn set_requests(&mut self, requests: Vec<PendingEnrollmentRequest>) {
        self.requests = requests;
        self.selected = clamp_index(self.requests.len(), self.selected);
    }

    pub fn move_selection(&mut self, dir: i32) {
        self.selected = step_index(self.requests.len(), self.selected, dir);
    }

    pub fn current(&self) -> Option<&PendingEnrollmentRequest> {
        self.requests.get(self.selected)
    }
}

/// Clamp `idx` into `[0, len)` (or 0 when empty).
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

    fn req(id: &str) -> PendingEnrollmentRequest {
        PendingEnrollmentRequest {
            request_id: id.to_string(),
            new_device_id: format!("dev-{id}"),
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
        s.set_requests(vec![req("a"), req("b"), req("c")]);
        s.selected = 2;
        assert_eq!(s.current().map(|r| r.request_id.as_str()), Some("c"));
        s.set_requests(vec![req("a")]);
        assert_eq!(s.selected, 0);
        assert_eq!(s.current().map(|r| r.request_id.as_str()), Some("a"));
        s.set_requests(vec![]);
        assert_eq!(s.selected, 0);
        assert!(s.current().is_none());
    }

    #[test]
    fn approval_state_moves_within_bounds() {
        let mut s = ApprovalState::default();
        s.set_requests(vec![req("a"), req("b")]);
        s.move_selection(1);
        assert_eq!(s.selected, 1);
        s.move_selection(1);
        assert_eq!(s.selected, 1);
        s.move_selection(-1);
        assert_eq!(s.selected, 0);
    }
}
