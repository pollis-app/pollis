//! Abuse reports (#1213).
//!
//! Wire types only. Messages are end-to-end encrypted, so a report names WHO
//! is reported, WHY, and optionally WHERE (the conversation and message ids,
//! which the server already sees as routing metadata). There is no field that
//! could carry message text, by design: the operator acts on accounts and
//! patterns, never on content it cannot read.

use serde::{Deserialize, Serialize};

/// Why an account is being reported. A closed set, mirrored by the CHECK on
/// `user_report.reason`, so an unknown reason fails to parse before it can
/// reach the database.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReportReason {
    Spam,
    Harassment,
    Illegal,
    Other,
}

impl ReportReason {
    /// The value stored in `user_report.reason`.
    pub fn as_str(self) -> &'static str {
        match self {
            ReportReason::Spam => "spam",
            ReportReason::Harassment => "harassment",
            ReportReason::Illegal => "illegal",
            ReportReason::Other => "other",
        }
    }
}

// ── POST /v1/reports ─────────────────────────────────────────────────────────

/// Report an account, optionally pointing at one message of theirs.
///
/// `reporter_id`, when signed, must equal the authenticated user: you can only
/// file reports as yourself. `message_id` requires `conversation_id`, and the
/// DS refuses a report against yourself; both are also table constraints.
#[derive(Serialize, Deserialize)]
pub struct ReportUserBody {
    pub reporter_id: String,
    pub reported_id: String,
    pub reason: ReportReason,
    pub conversation_id: Option<String>,
    pub message_id: Option<String>,
}
