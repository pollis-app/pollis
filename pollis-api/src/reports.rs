//! Abuse reports (#1213).
//!
//! A report names who, why and optionally where (ids the server already sees).
//! There is no field that could carry message text: the operator acts on
//! accounts and patterns, never on content.

use serde::{Deserialize, Serialize};

/// Why an account is being reported: a closed set mirrored by the CHECK on
/// `user_report.reason`.
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
/// `reporter_id`, when signed, must equal the authenticated user. `message_id`
/// requires `conversation_id`, and self-reports are refused (also table
/// constraints).
#[derive(Serialize, Deserialize)]
pub struct ReportUserBody {
    pub reporter_id: String,
    pub reported_id: String,
    pub reason: ReportReason,
    pub conversation_id: Option<String>,
    pub message_id: Option<String>,
}
