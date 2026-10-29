use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub conversation_id: String,
    pub sender_id: String,
    pub content: Option<String>,
    pub reply_to_id: Option<String>,
    /// Thread root ULID (#825), recovered from inside the ciphertext; `None` otherwise.
    pub thread_id: Option<String>,
    pub sent_at: String,
}

/// A message with its group and channel context, used when listing across
/// all channels (e.g. a user's sent message history).
#[derive(Debug, Serialize, Deserialize)]
pub struct MessageWithContext {
    pub group_id: String,
    pub group_name: String,
    pub channel_id: String,
    pub channel_name: String,
    pub id: String,
    pub sender_id: String,
    pub ciphertext: String,
    pub sent_at: String,
}

/// The most recent message in a channel alongside the sender's username,
/// used to populate channel list previews in the sidebar.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChannelPreview {
    pub group_id: String,
    pub group_name: String,
    pub channel_id: String,
    pub channel_name: String,
    pub last_message: Option<String>,
    pub last_sent_at: Option<String>,
    pub last_sender_id: Option<String>,
    pub last_sender_username: Option<String>,
}

/// A single message row returned by the channel message queries.
#[derive(Debug, Serialize, Deserialize)]
pub struct ChannelMessage {
    pub id: String,
    pub conversation_id: String,
    pub sender_id: String,
    pub sender_username: Option<String>,
    pub ciphertext: String,
    pub content: Option<String>,
    pub reply_to_id: Option<String>,
    /// See [`Message::thread_id`].
    pub thread_id: Option<String>,
    pub sent_at: String,
    pub edited_at: Option<String>,
    pub deleted_at: Option<String>,
    /// A placeholder for a message this device was entitled to read but could
    /// not decrypt (#1312). `content` is always `None` when set.
    #[serde(default)]
    pub undecryptable: bool,
    /// Outbox state (#1335): `pending`, `failed` (renderer offers Retry), or `None` once delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_state: Option<String>,
    /// Edit outbox state (#1555), `content` then being the edited text: `pending`, `failed` or `rejected`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_state: Option<String>,
}

/// Opaque pagination cursor — the (sent_at, id) of the oldest row on the
/// current page. Pass it back to fetch the next (older) page.
#[derive(Debug, Serialize, Deserialize)]
pub struct MessageCursor {
    pub sent_at: String,
    pub id: String,
}

/// Messages newest-first, plus a cursor for the next older page (`None` at the start).
#[derive(Debug, Serialize, Deserialize)]
pub struct MessagePage {
    pub messages: Vec<ChannelMessage>,
    pub next_cursor: Option<MessageCursor>,
}

/// How a result page is ordered (#850).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchSort {
    /// `sent_at DESC` — what people want inside one conversation.
    Recent,
    /// bm25, rescored with a recency decay — what people want across everything.
    Relevant,
}

/// Where to draw `<mark>` in a snippet, plus the text to draw it on.
/// Ranges rather than HTML so a message body cannot forge markup. Offsets are UTF-16 units.
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchSnippet {
    pub text: String,
    pub highlights: Vec<(usize, usize)>,
}

/// A search result from the local message cache.
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub message_id: String,
    pub conversation_id: String,
    /// `channel` or `dm`, from `conversation_cache`; `None` if never listed.
    pub conversation_kind: Option<String>,
    /// `#general`, or the peer's username for a DM.
    pub conversation_name: Option<String>,
    pub group_id: Option<String>,
    pub group_name: Option<String>,
    pub sender_id: String,
    pub sender_username: Option<String>,
    pub thread_id: Option<String>,
    pub has_attachment: bool,
    pub has_link: bool,
    pub content: String,
    pub sent_at: String,
    /// A window around the first hit, with highlight ranges, built from `content`.
    pub snippet: SearchSnippet,
}

/// An offset cursor: relevance ordering happens after rows leave SQLite.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchCursor {
    pub offset: i64,
}

/// What this device holds, so "no results" isn't read as "no such message" (#850).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SearchCorpus {
    /// Searchable messages on this device.
    pub message_count: i64,
    /// `sent_at` of the oldest one, i.e. how far back search can see.
    pub earliest_sent_at: Option<String>,
    /// The device-local retention window in days; `0` is Forever.
    pub retention_days: i64,
    /// Whether the one-time backfill is still running, in which case a missing
    /// result may simply not be indexed yet.
    pub indexing: bool,
}

/// One page of search results.
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchPage {
    pub results: Vec<SearchResult>,
    /// Total hits, not just this page.
    pub total: i64,
    pub next_cursor: Option<SearchCursor>,
    /// The ordering actually applied, which the caller may not have specified.
    pub sort: SearchSort,
    /// Only populated on the first page — it does not change while paging.
    pub corpus: SearchCorpus,
}

/// Reply count and last-reply time for one thread (#825), used to render the
/// "N replies" affordance under a root message without loading the thread.
/// Derived from local rows only; the server has no notion of a thread.
#[derive(Debug, Serialize, Deserialize)]
pub struct ThreadSummary {
    /// ULID of the thread root message.
    pub thread_id: String,
    /// Replies this device holds, excluding soft-deleted ones.
    pub reply_count: i64,
    /// `sent_at` of the newest reply this device holds.
    pub last_reply_at: Option<String>,
}
