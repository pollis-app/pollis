//! Typed voice-join failures, in one always-compiled place.
//!
//! `Error` reaches the renderer as its `Display` string, so the wire form is
//! `voice_join:<kind>` or `voice_join:<kind>: <detail>` — a stable prefix and
//! kind the renderer matches on (`frontend/src/voice/joinError.ts`), never the
//! English detail.

use serde::{Deserialize, Serialize};

pub const WIRE_PREFIX: &str = "voice_join:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum VoiceJoinError {
    /// A session is already live; leave first.
    AlreadyJoined,
    /// Another join is in flight.
    JoinInProgress,
    /// No LiveKit address from the DS or the build.
    NotConfigured,
    /// The DS could not be reached (network, DNS, 5xx).
    DsUnreachable { detail: String },
    /// The DS refused the voice token: not a member of the channel.
    NotMember { detail: String },
    /// No local MLS group backs this room, so no voice key can be derived.
    NoMlsGroup { detail: String },
    /// The SFU rejected the token (401/403).
    TokenRejected { detail: String },
    /// The SFU could not be reached (connect timeout, handshake, signal failure).
    LivekitUnreachable { detail: String },
    /// Connected, but the mic track could not be published.
    PublishFailed { detail: String },
    /// A join phase overran `VOICE_JOIN_DEADLINE`.
    Deadline { phase: String },
    /// `leave_voice_channel` arrived while the join was in flight.
    Cancelled,
    /// Anything else; the detail is diagnostic only.
    Internal { detail: String },
}

impl VoiceJoinError {
    /// The snake_case kind the renderer matches on.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::AlreadyJoined => "already_joined",
            Self::JoinInProgress => "join_in_progress",
            Self::NotConfigured => "not_configured",
            Self::DsUnreachable { .. } => "ds_unreachable",
            Self::NotMember { .. } => "not_member",
            Self::NoMlsGroup { .. } => "no_mls_group",
            Self::TokenRejected { .. } => "token_rejected",
            Self::LivekitUnreachable { .. } => "livekit_unreachable",
            Self::PublishFailed { .. } => "publish_failed",
            Self::Deadline { .. } => "deadline",
            Self::Cancelled => "cancelled",
            Self::Internal { .. } => "internal",
        }
    }

    fn detail(&self) -> Option<&str> {
        match self {
            Self::DsUnreachable { detail }
            | Self::NotMember { detail }
            | Self::NoMlsGroup { detail }
            | Self::TokenRejected { detail }
            | Self::LivekitUnreachable { detail }
            | Self::PublishFailed { detail }
            | Self::Internal { detail } => Some(detail),
            Self::Deadline { phase } => Some(phase),
            _ => None,
        }
    }

    /// Parse the wire form back; `None` for anything that isn't one.
    pub fn parse_wire(s: &str) -> Option<(String, Option<String>)> {
        let rest = s.strip_prefix(WIRE_PREFIX)?;
        let (kind, detail) = match rest.split_once(": ") {
            Some((k, d)) => (k, Some(d.to_string())),
            None => (rest, None),
        };
        if kind.is_empty() || !kind.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
            return None;
        }
        Some((kind.to_string(), detail))
    }
}

impl std::fmt::Display for VoiceJoinError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{WIRE_PREFIX}{}", self.kind())?;
        if let Some(detail) = self.detail() {
            // One line: the renderer splits on the first ": ".
            let detail: String = detail
                .chars()
                .map(|c| if c == '\n' { ' ' } else { c })
                .collect();
            write!(f, ": {detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for VoiceJoinError {}

impl From<VoiceJoinError> for crate::error::Error {
    fn from(e: VoiceJoinError) -> Self {
        crate::error::Error::VoiceJoin(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_form_round_trips_kind_and_detail() {
        let cases = [
            VoiceJoinError::AlreadyJoined,
            VoiceJoinError::Cancelled,
            VoiceJoinError::DsUnreachable {
                detail: "connection refused".into(),
            },
            VoiceJoinError::TokenRejected {
                detail: "client error: 401 - bad".into(),
            },
            VoiceJoinError::Deadline {
                phase: "connect".into(),
            },
            VoiceJoinError::Internal {
                detail: "multi\nline".into(),
            },
        ];
        for e in cases {
            let wire = e.to_string();
            let (kind, detail) = VoiceJoinError::parse_wire(&wire).expect("parses");
            assert_eq!(kind, e.kind(), "{wire}");
            assert_eq!(detail.is_some(), e.detail().is_some(), "{wire}");
            assert!(
                !wire.contains('\n'),
                "detail must stay on one line: {wire:?}"
            );
        }
    }

    #[test]
    fn plain_strings_are_not_wire_errors() {
        assert!(VoiceJoinError::parse_wire("LiveKit connect: timed out").is_none());
        assert!(VoiceJoinError::parse_wire("voice_join:").is_none());
        assert!(VoiceJoinError::parse_wire("voice_join:Not Member").is_none());
    }

    /// `Error`'s Serialize is its Display, so the renderer sees exactly the wire form.
    #[test]
    fn the_app_error_serialises_to_the_wire_form() {
        let e: crate::error::Error = VoiceJoinError::NotMember {
            detail: "403".into(),
        }
        .into();
        let json = serde_json::to_string(&e).unwrap();
        assert_eq!(json, "\"voice_join:not_member: 403\"");
    }
}
