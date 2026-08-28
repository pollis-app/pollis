use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub conversation_id: String,
    pub sender_id: String,
    pub content: Option<String>,
    pub reply_to_id: Option<String>,
    /// ULID of the thread root this message replies into (#825), or `None` for
    /// an ordinary channel message. Recovered from inside the MLS ciphertext —
    /// never a server-visible field.
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
}

/// Opaque pagination cursor — the (sent_at, id) of the oldest row on the
/// current page. Pass it back to fetch the next (older) page.
#[derive(Debug, Serialize, Deserialize)]
pub struct MessageCursor {
    pub sent_at: String,
    pub id: String,
}

/// Result of a channel message fetch: the messages (newest-first) and an
/// optional cursor for fetching the next older page. `next_cursor` is `None`
/// when fewer than `limit` rows were returned, meaning the beginning of
/// history has been reached.
#[derive(Debug, Serialize, Deserialize)]
pub struct MessagePage {
    pub messages: Vec<ChannelMessage>,
    pub next_cursor: Option<MessageCursor>,
}

/// How a result page is ordered (#850).
///
/// Both orderings are cheap, and neither is right everywhere: Slack defaults to
/// relevance, Discord to recency. The default is picked from the query — see
/// `search::search_messages` — and the user can always override it.
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
/// Ranges rather than HTML: the renderer builds `<mark>` elements from these
/// without `dangerouslySetInnerHTML`, and a structured answer cannot carry
/// markup that a message body forged. Offsets are UTF-16 code units, i.e. what
/// JavaScript's string indices actually are.
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
    /// never listed the conversation — the caller then renders the id, which is
    /// what search did for every result before #850.
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
    /// A window around the first hit, with highlight ranges. Built in Rust from
    /// `content`, never by FTS5's `snippet()` — a contentless index has no
    /// column text to return.
    pub snippet: SearchSnippet,
}

/// Opaque pagination cursor for search. An offset, because relevance ordering
/// is decided after the rows leave SQLite and a keyset cursor cannot describe a
/// position in an order the database never produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchCursor {
    pub offset: i64,
}

/// What this device actually holds, so the UI can say so instead of implying
/// that "no results" means "no such message" (§6 of #850).
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
    /// Total hits, not just this page. `count(*) … MATCH` is ~1.4 ms even for a
    /// term present in half the corpus, so "About N results" is free.
    pub total: i64,
    pub next_cursor: Option<SearchCursor>,
    /// The ordering actually applied, which the caller may not have specified.
    pub sort: SearchSort,
    /// Only populated on the first page — it does not change while paging.
    pub corpus: SearchCorpus,
}

/// Reply count and last-reply time for one thread (#825), used to render the
/// "N replies" affordance under a root message without loading the thread.
///
/// Derived entirely from decrypted local rows — the server has no notion of a
/// thread, so these numbers describe what THIS device holds.
#[derive(Debug, Serialize, Deserialize)]
pub struct ThreadSummary {
    /// ULID of the thread root message.
    pub thread_id: String,
    /// Replies this device holds, excluding soft-deleted ones.
    pub reply_count: i64,
    /// `sent_at` of the newest reply this device holds.
    pub last_reply_at: Option<String>,
}
