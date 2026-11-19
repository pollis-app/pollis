//! The normalised shape of an export, independent of where it came from.
//!
//! A source parser ([`crate::discord`], [`crate::slack`]) produces an [`Export`]; the
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
    /// Every category known, including empty ones (a snapshot lists those;
    /// from channel files alone only the categories that hold one).
    pub categories: Vec<Category>,
    /// Everyone who authored or was mentioned, keyed by source user id.
    pub people: BTreeMap<String, Person>,
    /// Custom emoji seen anywhere in the export (inline or as reactions),
    /// keyed by source emoji id. An export has no emoji list of its own, so
    /// emoji nobody used are not here.
    pub emoji: BTreeMap<String, CustomEmoji>,
    /// The server's roles. From a DCE export only the roles its authors
    /// carried (no permissions); from a guild snapshot every role.
    pub roles: Vec<Role>,
    /// The guild owner's user id, when a guild snapshot named it.
    pub owner_id: Option<String>,
    /// Slack user groups (`@design-team`). Pollis has no mention tags yet
    /// (#294), so they are reported, never imported.
    pub user_groups: Vec<UserGroup>,
    /// Whether a snapshot supplied what the export lacks: for Discord
    /// (`discord-guild.json`) positions, roles with permissions, overwrites
    /// and the full emoji list; for Slack (`slack-workspace.json`) the
    /// workspace's name and icon, its emoji and its user groups.
    pub has_snapshot: bool,
    /// Problems found while reading that did not stop the parse.
    pub warnings: Vec<String>,
}

/// A server role. Pollis has only member and admin, so roles are collapsed
/// and every role is reported.
#[derive(Debug, Clone, Serialize)]
pub struct Role {
    pub id: String,
    pub name: String,
    /// The permission bitfield, when the source gave one (a snapshot does,
    /// a DCE export does not).
    pub permissions: Option<u64>,
    /// Higher is more senior, as in Discord.
    pub position: i64,
    /// Owned by an integration (a bot's own role); never assigned by people.
    pub managed: bool,
    /// The `@everyone` role (its id is the guild's id).
    pub everyone: bool,
}

impl Role {
    /// Discord's `ADMINISTRATOR` and `MANAGE_GUILD` bits: holders of either
    /// run the server, so they map to a Pollis admin.
    pub const ADMIN_BITS: u64 = (1 << 3) | (1 << 5);

    pub fn grants_admin(&self) -> bool {
        self.permissions.is_some_and(|p| p & Self::ADMIN_BITS != 0)
    }
}

/// A per-channel permission overwrite. Pollis channels are visible to every
/// group member, so none survives; each is reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Overwrite {
    pub target: OverwriteTarget,
    pub allow: u64,
    pub deny: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum OverwriteTarget {
    Role(String),
    Member(String),
}

/// Discord's `VIEW_CHANNEL` bit: denying it to `@everyone` makes a channel private.
pub const VIEW_CHANNEL: u64 = 1 << 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// A DiscordChatExporter JSON export, optionally with a guild snapshot.
    DiscordChatExporter,
    /// A Slack workspace export (ZIP or folder), optionally with a workspace snapshot.
    SlackExport,
}

/// A Slack user group (`usergroups.list`). Reported only (#294).
#[derive(Debug, Clone, Serialize)]
pub struct UserGroup {
    pub id: String,
    /// The mention handle, without `@`.
    pub handle: String,
    pub name: String,
    pub description: Option<String>,
    /// Source user ids.
    pub member_ids: Vec<String>,
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
    /// Discord's sort position within its category, when the source gave one.
    pub position: Option<i64>,
    /// Per-channel permission overwrites (snapshot only).
    pub overwrites: Vec<Overwrite>,
    /// The source itself calls the channel private (Slack's `is_private`).
    /// Discord says so only through overwrites, which the plan reads.
    pub private: bool,
    /// Archived on the source (Slack): skipped unless the admin opts in.
    pub archived: bool,
    /// Source user ids of the channel's members, when the source lists them
    /// (Slack's `members`). The plan turns them into a per-person roster.
    pub members: Vec<String>,
    /// Oldest first.
    pub messages: Vec<Message>,
    /// The export file this channel was read from, for the report; `None`
    /// for a channel known only from the guild snapshot (nothing exported).
    pub file: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Category {
    pub id: String,
    pub name: String,
    /// Discord's sort position among categories, when the source gave one.
    pub position: Option<i64>,
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
    /// Ids of the roles they hold (from the snapshot's member list, else the
    /// roles DCE recorded on their messages).
    pub role_ids: Vec<String>,
    /// Listed in the snapshot's member list: a current member, whether or
    /// not they wrote anything.
    pub listed_member: bool,
    /// Their email address, when the source shares it (Slack does; Discord
    /// never). Used to invite people who already have a Pollis account.
    pub email: Option<String>,
    /// The source marks them as running the workspace (Slack's admins and
    /// owners). Discord's equivalent comes from role permissions instead.
    pub source_admin: bool,
    /// A deactivated account (Slack's `deleted`): never invited.
    pub deactivated: bool,
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
    /// Slack's `alias:other`: the source id of the emoji whose picture this
    /// name points at. Imported as its own shortcode for the same picture.
    pub alias_of: Option<String>,
}
