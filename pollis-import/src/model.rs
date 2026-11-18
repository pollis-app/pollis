//! The normalised shape of an export, independent of where it came from.
//!
//! A source parser (today only [`crate::discord`]) produces an [`Export`]; the
//! planner reads nothing else. Fields hold what the source said, not what
//! Pollis will do with it: renaming, flattening and dropping are decisions the
//! plan makes and reports.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::Serialize;

/// One community: a Discord guild, a Slack workspace.
#[derive(Debug, Clone, Serialize)]
pub struct Export {
    pub source: Source,
    pub community: Community,
    pub channels: Vec<Channel>,
    /// Everyone who authored or was mentioned, keyed by source user id.
    pub people: BTreeMap<String, Person>,
    /// Custom emoji seen anywhere in the export (inline or as reactions),
    /// keyed by source emoji id. An export has no emoji list of its own, so
    /// emoji nobody used are not here.
    pub emoji: BTreeMap<String, CustomEmoji>,
    /// Problems found while reading that did not stop the parse.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Source {
    /// A DiscordChatExporter JSON export.
    DiscordChatExporter,
}

#[derive(Debug, Clone, Serialize)]
pub struct Community {
    pub id: String,
    pub name: String,
    pub icon: Option<Media>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum ChannelKind {
    Text,
    Voice,
    Announcement,
    Forum,
    Stage,
    /// A thread; its parent channel is [`Channel::parent_id`].
    Thread,
    /// A DM or group DM. Never imported into a group.
    Direct,
    /// A type this parser does not know, kept verbatim for the report.
    Other(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct Channel {
    pub id: String,
    pub name: String,
    pub kind: ChannelKind,
    pub topic: Option<String>,
    /// Discord's category (folder) for a channel.
    pub category: Option<Category>,
    /// For a thread: the channel it lives in.
    pub parent_id: Option<String>,
    /// Oldest first.
    pub messages: Vec<Message>,
    /// The export file this channel was read from, for the report.
    pub file: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Category {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum MessageKind {
    /// Something a person wrote (including a reply).
    Normal,
    /// Joins, pins, renames, calls, boosts: platform notices, not content.
    System(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub id: String,
    /// Unix milliseconds, from the snowflake.
    pub sent_at_ms: u64,
    /// The source's own timestamp string, kept for display.
    pub timestamp: String,
    pub edited_at: Option<String>,
    pub kind: MessageKind,
    pub author_id: String,
    pub content: String,
    pub reply_to: Option<String>,
    pub pinned: bool,
    pub attachments: Vec<Attachment>,
    pub reactions: Vec<Reaction>,
    pub mention_ids: Vec<String>,
    /// Link previews and rich embeds. Counted, never imported: Pollis
    /// deliberately renders no generic link previews.
    pub embed_count: usize,
    pub stickers: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Attachment {
    pub id: String,
    pub file_name: String,
    pub size_bytes: u64,
    pub media: Media,
}

/// Where an image or file's bytes are.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum Media {
    /// Downloaded beside the export (`--media`). The path is resolved against
    /// the export file's directory.
    Local(PathBuf),
    /// Only a URL. Discord's CDN links expire, so these are usually gone.
    Remote(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct Reaction {
    /// The custom emoji id, or `None` for a Unicode emoji.
    pub emoji_id: Option<String>,
    /// The Unicode character(s) or the custom emoji's name.
    pub emoji: String,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Person {
    pub id: String,
    /// The account name (`name` in Discord).
    pub username: String,
    /// The server nickname, if one was set.
    pub nickname: Option<String>,
    pub is_bot: bool,
    pub avatar: Option<Media>,
}

impl Person {
    /// The name people saw in the channel.
    pub fn display_name(&self) -> &str {
        self.nickname.as_deref().unwrap_or(&self.username)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CustomEmoji {
    pub id: String,
    pub name: String,
    pub animated: bool,
    pub image: Option<Media>,
    /// How many times it appeared inline or as a reaction.
    pub uses: u64,
}
