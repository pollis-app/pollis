use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub conversation_id: String,
    pub sender_id: String,
    pub content: Option<String>,
    pub reply_to_id: Option<String>,
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

/// A search result from the local message cache.
#[derive(Debug, Serialize, Deserialize)]
pub struct SearchResult {
    pub message_id: String,
    pub conversation_id: String,
    pub sender_id: String,
    pub content: String,
    pub sent_at: String,
    /// Surrounding context — same as content for now.
    pub snippet: String,
}
