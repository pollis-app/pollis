//! Abuse reports (#1213).
//!
//! Wire types only; no field can carry message text, by design.

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

/// Report an account, optionally pointing at one message of theirs.
///
/// A signed `reporter_id` must equal the caller; self-reports and a bare `message_id` are refused.
#[derive(Serialize, Deserialize)]
pub struct ReportUserBody {
    pub reporter_id: String,
    pub reported_id: String,
    pub reason: ReportReason,
    pub conversation_id: Option<String>,
    pub message_id: Option<String>,
}
