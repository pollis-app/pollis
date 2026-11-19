//! What the runtime hands to a handler.

use serde::de::DeserializeOwned;

use crate::error::Result;

/// Who sent a message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Author {
    /// Opaque user id.
    pub id: String,
    pub display_name: String,
    /// True for bots, including this one.
    pub is_bot: bool,
}

/// Attachment metadata. Apps never receive attachment bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub mime_type: String,
    /// Bytes.
    pub size: u64,
}

/// A message in a channel the App was granted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// Opaque message id.
    pub id: String,
    /// Opaque channel id. Pass it to `ctx.post` to answer in the same channel.
    pub channel: String,
    pub author: Author,
    pub text: String,
    pub attachments: Vec<Attachment>,
    /// The message this one replies to, if any.
    pub reply_to: Option<String>,
    /// The thread root this message belongs to, if any.
    pub thread: Option<String>,
    /// Unix milliseconds.
    pub sent_at: u64,
    /// Unix milliseconds of the last edit, if edited.
    pub edited_at: Option<u64>,
}

/// A slash command addressed to this App: `/name args`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// The command name, without the slash.
    pub name: String,
    /// Everything after the name (and after `@bot`, if the member addressed one), trimmed.
    pub args: String,
    /// The message that carried the command. Reply to it to answer.
    pub message: Message,
}

/// A schedule firing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Schedule {
    pub name: String,
    /// The slot this firing is for, Unix milliseconds.
    pub slot: u64,
}

/// A request a member sent to this App.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub id: String,
    pub requester: Author,
    pub body: String,
}

/// An inbound webhook delivery: the request body as sent, and its
/// `content-type`, `user-agent` and `x-*` headers (never `authorization`,
/// cookies or the signature).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Webhook {
    pub delivery_id: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Webhook {
    /// The body as UTF-8 text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body parsed as JSON.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        Ok(serde_json::from_slice(&self.body)?)
    }

    /// A header's value, by case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// A message, or a message's id: what `ctx.reply`, `ctx.react`, `ctx.edit_own`
/// and `ctx.delete_own` accept.
pub trait MessageRef {
    /// The message's id.
    fn message_id(&self) -> &str;
}

impl MessageRef for Message {
    fn message_id(&self) -> &str {
        &self.id
    }
}

impl MessageRef for str {
    fn message_id(&self) -> &str {
        self
    }
}

impl MessageRef for String {
    fn message_id(&self) -> &str {
        self
    }
}

impl<T: MessageRef + ?Sized> MessageRef for &T {
    fn message_id(&self) -> &str {
        (**self).message_id()
    }
}
