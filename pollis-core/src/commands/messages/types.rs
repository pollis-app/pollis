use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub conversation_id: String,
    pub sender_id: String,
    pub content: Option<String>,
    pub reply_to_id: Option<String>,
    /// Thread root (#825), or `None` for an ordinary channel message. Recovered
    /// from inside the MLS ciphertext, never a server-visible field.
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
    /// The durable outbox (#1335): `pending`, `failed` (the renderer offers
    /// Retry) or `rejected` for this device's own unsent message; `None` (and
    /// omitted) for everything delivered or received.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub send_state: Option<String>,
}

/// Pagination cursor: the `(sent_at, id)` of the oldest row on the current
/// page. Pass it back to fetch the next older page.
#[derive(Debug, Serialize, Deserialize)]
pub struct MessageCursor {
    pub sent_at: String,
    pub id: String,
}

/// A page of messages, newest-first. `next_cursor` is `None` once a page comes
/// back short: the beginning of history.
#[derive(Debug, Serialize, Deserialize)]
pub struct MessagePage {
    pub messages: Vec<ChannelMessage>,
    pub next_cursor: Option<MessageCursor>,
}

/// How a result page is ordered (#850).
///
/// The default is picked from the query (`search::search_messages`); the user
/// can override it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchSort {
    /// `sent_at DESC` — what people want inside one conversation.
    Recent,
    /// bm25, rescored with a recency decay — what people want across everything.
    Relevant,
}

/// Where to draw `<mark>` in a snippet, plus the text to draw it on.
///
/// Ranges rather than HTML, so a message body cannot forge markup. Offsets are
/// UTF-16 code units, which is what JavaScript string indices are.
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
    /// `channel` or `dm`, from `conversation_cache`. `None` when this device has
    /// never listed the conversation; the caller then renders the id.
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
    /// A window around the first hit, with highlight ranges.
    pub snippet: SearchSnippet,
}

/// Search pagination cursor. An offset, because relevance order is decided
/// after the rows leave SQLite, so no keyset cursor can describe it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchCursor {
    pub offset: i64,
}

/// What this device actually holds, so the UI never implies "no results" means
/// "no such message" (#850).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct SearchCorpus {
    /// Searchable messages on this device.
    pub message_count: i64,
    /// `sent_at` of the oldest one, i.e. how far back search can see.
    pub earliest_sent_at: Option<String>,
    /// The device-local retention window in days; `0` is Forever.
    pub retention_days: i64,
    /// The one-time backfill is still running: a missing result may not be
    /// indexed yet.
    pub indexing: bool,
}

/// One page of search results.
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchPage {
    pub results: Vec<SearchResult>,
    /// Total hits, not just this page (cheap: ~1.4 ms even for a common term).
    pub total: i64,
    pub next_cursor: Option<SearchCursor>,
    /// The ordering actually applied, which the caller may not have specified.
    pub sort: SearchSort,
    /// Only populated on the first page.
    pub corpus: SearchCorpus,
}

/// Reply count and last-reply time for one thread (#825), for the "N replies"
/// affordance. Describes what this device holds; the server has no notion of
/// a thread.
#[derive(Debug, Serialize, Deserialize)]
pub struct ThreadSummary {
    /// ULID of the thread root message.
    pub thread_id: String,
    /// Replies this device holds, excluding soft-deleted ones.
    pub reply_count: i64,
    /// `sent_at` of the newest reply this device holds.
    pub last_reply_at: Option<String>,
}
