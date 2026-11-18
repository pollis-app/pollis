//! What a handler asks for, and the grants that decide whether it happens
//! (#1182, job loop step 6).
//!
//! A handler never acts. It returns [`Action`]s; the loop checks each against
//! the installation's [`Grants`] and applies only what passes, through the
//! normal send path. In this ticket the grant is hard-coded: `post` to a text
//! channel of the bot's own group. #2005 replaces the hard-coded grant with the
//! admin-approved one; #2004 adds the App actions (`reply`, `react`, …) as new
//! variants, each refused unless granted.

use serde::Serialize;

/// The most text one `post` carries, in characters.
pub const MAX_POST_CHARS: usize = 4000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Post `text` into `channel` (an id, or a name with or without `#`).
    Post { channel: String, text: String },
}

/// An action that passed the grant check, with its channel resolved to an id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckedAction {
    Post { channel_id: String, text: String },
}

/// Why an action was refused. Each names what to change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Refusal {
    /// The channel is not a text channel of the bot's group.
    ChannelNotGranted { channel: String },
    EmptyText,
    TextTooLong { chars: usize, max: usize },
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::ChannelNotGranted { channel } => write!(
                f,
                "`post` to `{channel}` refused: not a text channel of this bot's group (use a channel id or name from the group)"
            ),
            Refusal::EmptyText => f.write_str("`post` refused: the text is empty"),
            Refusal::TextTooLong { chars, max } => {
                write!(f, "`post` refused: the text is {chars} characters, the limit is {max}")
            }
        }
    }
}

/// A channel of the bot's group, as the grant check sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelInfo {
    pub id: String,
    pub name: String,
    /// Only text channels can be posted to.
    pub is_text: bool,
}

/// What an installation may do. Today: post to its own group's text channels.
#[derive(Debug, Clone)]
pub struct Grants {
    channels: Vec<ChannelInfo>,
}

impl Grants {
    /// The hard-coded #1182 grant over the bot's group's current channels.
    pub fn own_group(channels: Vec<ChannelInfo>) -> Self {
        Self { channels }
    }

    pub fn check(&self, action: &Action) -> Result<CheckedAction, Refusal> {
        match action {
            Action::Post { channel, text } => {
                let channel_id = self
                    .resolve(channel)
                    .ok_or_else(|| Refusal::ChannelNotGranted { channel: channel.clone() })?;
                if text.trim().is_empty() {
                    return Err(Refusal::EmptyText);
                }
                let chars = text.chars().count();
                if chars > MAX_POST_CHARS {
                    return Err(Refusal::TextTooLong { chars, max: MAX_POST_CHARS });
                }
                Ok(CheckedAction::Post { channel_id, text: text.clone() })
            }
        }
    }

    /// An exact id wins; otherwise a case-insensitive name, `#` optional. A
    /// name two channels share is ambiguous and resolves to nothing.
    fn resolve(&self, channel: &str) -> Option<String> {
        let text_channels = self.channels.iter().filter(|c| c.is_text);
        if let Some(c) = text_channels.clone().find(|c| c.id == channel) {
            return Some(c.id.clone());
        }
        let wanted = channel.trim().trim_start_matches('#');
        let mut by_name = text_channels.filter(|c| c.name.eq_ignore_ascii_case(wanted));
        let first = by_name.next()?;
        if by_name.next().is_some() {
            return None;
        }
        Some(first.id.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grants() -> Grants {
        Grants::own_group(vec![
            ChannelInfo { id: "01CHAN".into(), name: "alerts".into(), is_text: true },
            ChannelInfo { id: "01VOICE".into(), name: "standup".into(), is_text: false },
            ChannelInfo { id: "01DUP1".into(), name: "dup".into(), is_text: true },
            ChannelInfo { id: "01DUP2".into(), name: "Dup".into(), is_text: true },
        ])
    }

    fn post(channel: &str, text: &str) -> Action {
        Action::Post { channel: channel.into(), text: text.into() }
    }

    #[test]
    fn a_channel_of_the_group_resolves_by_id_or_name() {
        let g = grants();
        for name in ["01CHAN", "alerts", "#alerts", "ALERTS"] {
            assert_eq!(
                g.check(&post(name, "hi")),
                Ok(CheckedAction::Post { channel_id: "01CHAN".into(), text: "hi".into() }),
                "{name}"
            );
        }
    }

    #[test]
    fn a_channel_outside_the_group_is_refused() {
        let g = grants();
        assert_eq!(
            g.check(&post("01ELSEWHERE", "hi")),
            Err(Refusal::ChannelNotGranted { channel: "01ELSEWHERE".into() })
        );
    }

    #[test]
    fn a_voice_channel_and_an_ambiguous_name_are_refused() {
        let g = grants();
        assert!(matches!(g.check(&post("standup", "hi")), Err(Refusal::ChannelNotGranted { .. })));
        assert!(matches!(g.check(&post("01VOICE", "hi")), Err(Refusal::ChannelNotGranted { .. })));
        assert!(matches!(g.check(&post("dup", "hi")), Err(Refusal::ChannelNotGranted { .. })));
    }

    #[test]
    fn empty_and_oversized_text_is_refused() {
        let g = grants();
        assert_eq!(g.check(&post("alerts", "   ")), Err(Refusal::EmptyText));
        let long = "x".repeat(MAX_POST_CHARS + 1);
        assert!(matches!(g.check(&post("alerts", &long)), Err(Refusal::TextTooLong { .. })));
    }
}
