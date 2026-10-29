use std::sync::Arc;

use rusqlite::OptionalExtension;

use crate::error::Result;
use crate::state::AppState;


use super::ingest::{ingest_channel_envelopes_inner, ingest_dm_envelopes_inner};
use super::types::{
    ChannelMessage, Message, MessageCursor, MessagePage, ThreadSummary,
};

pub async fn list_messages(
    conversation_id: String,
    limit: Option<i64>,
    before_id: Option<String>,
    state: &Arc<AppState>,
) -> Result<Vec<Message>> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
    let limit = limit.unwrap_or(50);

    let messages = if let Some(before) = before_id {
        let before_time: String = db.conn().query_row(
            "SELECT sent_at FROM message WHERE id = ?1",
            rusqlite::params![before],
            |row| row.get(0),
        ).unwrap_or_else(|_| chrono::Utc::now().to_rfc3339());

        let mut stmt = db.conn().prepare(
            "SELECT id, conversation_id, sender_id, content, reply_to_id, thread_id, sent_at
             FROM message
             WHERE conversation_id = ?1 AND sent_at < ?2
             ORDER BY sent_at DESC LIMIT ?3"
        )?;

        let rows = stmt.query_map(
            rusqlite::params![conversation_id, before_time, limit],
            |row| Ok(Message {
                id: row.get(0)?,
                conversation_id: row.get(1)?,
                sender_id: row.get(2)?,
                content: row.get(3)?,
                reply_to_id: row.get(4)?,
                thread_id: row.get(5)?,
                sent_at: row.get(6)?,
            }),
        )?;

        rows.filter_map(|r| r.ok()).collect()
    } else {
        let mut stmt = db.conn().prepare(
            "SELECT id, conversation_id, sender_id, content, reply_to_id, thread_id, sent_at
             FROM message
             WHERE conversation_id = ?1
             ORDER BY sent_at DESC LIMIT ?2"
        )?;

        let rows = stmt.query_map(
            rusqlite::params![conversation_id, limit],
            |row| Ok(Message {
                id: row.get(0)?,
                conversation_id: row.get(1)?,
                sender_id: row.get(2)?,
                content: row.get(3)?,
                reply_to_id: row.get(4)?,
                thread_id: row.get(5)?,
                sent_at: row.get(6)?,
            }),
        )?;

        rows.filter_map(|r| r.ok()).collect()
    };

    Ok(messages)
}

/// Ingest, then fetch a page of a channel's messages, newest-first. Pass the
/// previous `next_cursor` to walk back through history.
pub async fn get_channel_messages(
    user_id: String,
    channel_id: String,
    limit: Option<i64>,
    cursor: Option<MessageCursor>,
    state: &Arc<AppState>,
) -> Result<MessagePage> {
    let limit = limit.unwrap_or(50);

    ingest_channel_envelopes_inner(state, &user_id, &channel_id).await?;

    let mut messages = read_local_channel_page(state, &channel_id, &cursor, limit).await?;
    attach_sender_usernames_local(state, &mut messages).await?;

    let next_cursor = if messages.len() == limit as usize {
        messages.last().map(|m| MessageCursor {
            sent_at: m.sent_at.clone(),
            id: m.id.clone(),
        })
    } else {
        None
    };

    Ok(MessagePage { messages, next_cursor })
}

/// Map a `message` row to a [`ChannelMessage`]. Every caller must SELECT this
/// column order: `id, conversation_id, sender_id, ciphertext, content,
/// reply_to_id, sent_at, edited_at, deleted_at, thread_id, undecryptable,
/// send_state`.
fn row_to_channel_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChannelMessage> {
    let ct: Vec<u8> = row.get(3)?;
    let content: Option<String> = row.get(4)?;
    let deleted_at: Option<String> = row.get(8)?;
    Ok(ChannelMessage {
        id: row.get(0)?,
        conversation_id: row.get(1)?,
        sender_id: row.get(2)?,
        sender_username: None,
        ciphertext: format!("mls:{}", hex::encode(&ct)),
        content: if deleted_at.is_some() { None } else { content },
        reply_to_id: row.get(5)?,
        thread_id: row.get(9)?,
        sent_at: row.get(6)?,
        edited_at: row.get(7)?,
        deleted_at,
        undecryptable: row.get::<_, Option<String>>(10)?.is_some(),
        send_state: row.get(11)?,
    })
}

/// Read a page of a conversation from the local `message` table, newest-first.
async fn read_local_channel_page(
    state: &Arc<AppState>,
    conversation_id: &str,
    cursor: &Option<MessageCursor>,
    limit: i64,
) -> Result<Vec<ChannelMessage>> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

    let mut rows: Vec<ChannelMessage> = Vec::new();
    match cursor {
        None => {
            let mut stmt = db.conn().prepare(
                "SELECT id, conversation_id, sender_id, ciphertext, content, reply_to_id, sent_at, edited_at, deleted_at, thread_id, undecryptable, send_state
                 FROM message
                 WHERE conversation_id = ?1
                 ORDER BY sent_at DESC, id DESC
                 LIMIT ?2"
            )?;
            let mapped = stmt.query_map(rusqlite::params![conversation_id, limit], row_to_channel_message)?;
            for m in mapped.flatten() {
                rows.push(m);
            }
        }
        Some(c) => {
            let mut stmt = db.conn().prepare(
                "SELECT id, conversation_id, sender_id, ciphertext, content, reply_to_id, sent_at, edited_at, deleted_at, thread_id, undecryptable, send_state
                 FROM message
                 WHERE conversation_id = ?1
                   AND (sent_at < ?2 OR (sent_at = ?2 AND id < ?3))
                 ORDER BY sent_at DESC, id DESC
                 LIMIT ?4"
            )?;
            let mapped = stmt.query_map(rusqlite::params![conversation_id, c.sent_at, c.id, limit], row_to_channel_message)?;
            for m in mapped.flatten() {
                rows.push(m);
            }
        }
    }

    Ok(rows)
}

/// Attach sender usernames from `user_cache`, with one batched remote fetch
/// (written back) for any misses.
async fn attach_sender_usernames_local(
    state: &Arc<AppState>,
    messages: &mut [ChannelMessage],
) -> Result<()> {
    if messages.is_empty() {
        return Ok(());
    }

    let mut ids: std::collections::HashSet<String> = std::collections::HashSet::new();
    for m in messages.iter() {
        ids.insert(m.sender_id.clone());
    }
    let ids_vec: Vec<String> = ids.into_iter().collect();

    let mut found: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let missing: Vec<String> = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        // Chunked (#916): bounded by the page size, but by nothing that says so.
        for chunk in crate::db::chunk::bind_chunks(&ids_vec, 0) {
            let sql = format!(
                "SELECT id, username FROM user_cache WHERE id IN ({})",
                crate::db::chunk::placeholders(chunk.len(), 1)
            );
            let mut stmt = db.conn().prepare(&sql)?;
            let mapped = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for (id, name) in mapped.flatten() {
                found.insert(id, name);
            }
        }
        ids_vec.iter().filter(|i| !found.contains_key(*i)).cloned().collect()
    };

    if !missing.is_empty() {
        match crate::commands::ds_reads::users(state, missing.clone(), None).await {
            Ok(users) => {
                let fetched: Vec<(String, String)> = users
                    .into_iter()
                    .map(|u| (u.id, u.username))
                    .collect();
                for (id, name) in &fetched {
                    found.insert(id.clone(), name.clone());
                }
                if !fetched.is_empty() {
                    let guard = state.local_db.lock().await;
                    if let Some(db) = guard.as_ref() {
                        for (id, name) in &fetched {
                            let _ = db.conn().execute(
                                "INSERT INTO user_cache (id, username, updated_at) VALUES (?1, ?2, datetime('now'))
                                 ON CONFLICT(id) DO UPDATE SET username = ?2, updated_at = datetime('now')",
                                rusqlite::params![id, name],
                            );
                        }
                    }
                }
            }
            Err(e) => {
                // Offline or transient: names stay None until a later read.
                crate::diag!("[messages] attach_sender_usernames_local: remote fallback failed: {e}");
            }
        }
    }

    for m in messages.iter_mut() {
        m.sender_username = found.get(&m.sender_id).cloned();
    }
    Ok(())
}

/// Local-only read of a channel page; callers fire `ingest_channel_envelopes`
/// separately, off the render path.
pub async fn read_channel_messages(
    channel_id: String,
    limit: Option<i64>,
    cursor: Option<MessageCursor>,
    state: &Arc<AppState>,
) -> Result<MessagePage> {
    let limit = limit.unwrap_or(50);
    let mut messages = read_local_channel_page(state, &channel_id, &cursor, limit).await?;
    attach_sender_usernames_local(state, &mut messages).await?;

    let next_cursor = if messages.len() == limit as usize {
        messages.last().map(|m| MessageCursor {
            sent_at: m.sent_at.clone(),
            id: m.id.clone(),
        })
    } else {
        None
    };

    Ok(MessagePage { messages, next_cursor })
}

/// Local-only read of a DM page. Mirrors `read_channel_messages`.
pub async fn read_dm_messages(
    dm_channel_id: String,
    limit: Option<i64>,
    cursor: Option<MessageCursor>,
    state: &Arc<AppState>,
) -> Result<MessagePage> {
    let limit = limit.unwrap_or(50);
    let mut messages = read_local_channel_page(state, &dm_channel_id, &cursor, limit).await?;
    attach_sender_usernames_local(state, &mut messages).await?;

    let next_cursor = if messages.len() == limit as usize {
        messages.last().map(|m| MessageCursor {
            sent_at: m.sent_at.clone(),
            id: m.id.clone(),
        })
    } else {
        None
    };

    Ok(MessagePage { messages, next_cursor })
}

/// Bound-parameter chunk size for [`read_last_messages`], well under older
/// SQLite builds' 999-variable limit.
const LAST_MESSAGE_ID_CHUNK: usize = 400;

/// The batched newest-per-conversation SELECT, for `n` bound ids. Its ordering
/// (`sent_at DESC, id DESC`) must match `read_local_channel_page`, or a preview
/// and the top of the opened conversation could show different messages.
pub(crate) fn last_messages_sql(n: usize) -> String {
    let placeholders = (1..=n).map(|i| format!("?{i}")).collect::<Vec<_>>().join(",");
    format!(
        "SELECT id, conversation_id, sender_id, ciphertext, content, reply_to_id, sent_at, edited_at, deleted_at, thread_id, undecryptable, send_state
         FROM (
           SELECT id, conversation_id, sender_id, ciphertext, content, reply_to_id, sent_at, edited_at, deleted_at, thread_id, undecryptable, send_state,
                  ROW_NUMBER() OVER (PARTITION BY conversation_id ORDER BY sent_at DESC, id DESC) AS rn
           FROM message
           WHERE conversation_id IN ({placeholders})
         )
         WHERE rn = 1"
    )
}

/// The newest message in each of `conversation_ids`, in one round trip (#874),
/// for the sidebar previews. Conversations with no local messages are absent.
pub async fn read_last_messages(
    conversation_ids: Vec<String>,
    state: &Arc<AppState>,
) -> Result<Vec<ChannelMessage>> {
    if conversation_ids.is_empty() {
        return Ok(Vec::new());
    }

    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let ids: Vec<String> = conversation_ids
        .into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect();

    // rusqlite statements are not `Send`: scope the DB read in a block (an
    // explicit `drop()` is not enough for the generated future).
    let mut messages: Vec<ChannelMessage> = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

        let mut out: Vec<ChannelMessage> = Vec::with_capacity(ids.len());
        for chunk in ids.chunks(LAST_MESSAGE_ID_CHUNK) {
            let sql = last_messages_sql(chunk.len());
            let mut stmt = db.conn().prepare(&sql)?;
            let mapped =
                stmt.query_map(rusqlite::params_from_iter(chunk.iter()), row_to_channel_message)?;
            for m in mapped.flatten() {
                out.push(m);
            }
        }
        out
    };

    attach_sender_usernames_local(state, &mut messages).await?;
    Ok(messages)
}

/// Ingest, then fetch a page of a DM's messages, newest-first.
pub async fn get_dm_messages(
    user_id: String,
    dm_channel_id: String,
    limit: Option<i64>,
    cursor: Option<MessageCursor>,
    state: &Arc<AppState>,
) -> Result<MessagePage> {
    let limit = limit.unwrap_or(50);

    ingest_dm_envelopes_inner(state, &user_id, &dm_channel_id).await?;

    let mut messages = read_local_channel_page(state, &dm_channel_id, &cursor, limit).await?;
    attach_sender_usernames_local(state, &mut messages).await?;

    let next_cursor = if messages.len() == limit as usize {
        messages.last().map(|m| MessageCursor {
            sent_at: m.sent_at.clone(),
            id: m.id.clone(),
        })
    } else {
        None
    };

    Ok(MessagePage { messages, next_cursor })
}

/// A page of a conversation centred on one message (#850).
///
/// The jump-to-message flow (a search hit, a permalink) in one round trip:
/// the `limit` newest messages at or before the `(sent_at, id)` anchor plus the
/// `limit` oldest after it, newest-first. A message this device does not hold
/// yields an empty page.
pub async fn read_messages_around(
    conversation_id: String,
    message_id: String,
    limit: Option<i64>,
    state: &Arc<AppState>,
) -> Result<MessagePage> {
    let limit = limit.unwrap_or(25).clamp(1, 200);

    let mut messages: Vec<ChannelMessage> = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

        let anchor: Option<(String, String)> = db
            .conn()
            .query_row(
                "SELECT sent_at, id FROM message WHERE id = ?1 AND conversation_id = ?2",
                rusqlite::params![message_id, conversation_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;

        let Some((anchor_at, anchor_id)) = anchor else {
            return Ok(MessagePage { messages: Vec::new(), next_cursor: None });
        };

        let mut stmt = db.conn().prepare(
            "SELECT id, conversation_id, sender_id, ciphertext, content, reply_to_id, sent_at, edited_at, deleted_at, thread_id, undecryptable, send_state
             FROM message
             WHERE conversation_id = ?1
               AND (sent_at < ?2 OR (sent_at = ?2 AND id <= ?3))
             ORDER BY sent_at DESC, id DESC
             LIMIT ?4",
        )?;
        let mut out: Vec<ChannelMessage> = stmt
            .query_map(
                rusqlite::params![conversation_id, anchor_at, anchor_id, limit],
                row_to_channel_message,
            )?
            .flatten()
            .collect();

        let mut stmt_newer = db.conn().prepare(
            "SELECT id, conversation_id, sender_id, ciphertext, content, reply_to_id, sent_at, edited_at, deleted_at, thread_id, undecryptable, send_state
             FROM message
             WHERE conversation_id = ?1
               AND (sent_at > ?2 OR (sent_at = ?2 AND id > ?3))
             ORDER BY sent_at ASC, id ASC
             LIMIT ?4",
        )?;
        let newer: Vec<ChannelMessage> = stmt_newer
            .query_map(
                rusqlite::params![conversation_id, anchor_at, anchor_id, limit],
                row_to_channel_message,
            )?
            .flatten()
            .collect();

        let mut page: Vec<ChannelMessage> = newer.into_iter().rev().collect();
        page.append(&mut out);
        page
    };

    attach_sender_usernames_local(state, &mut messages).await?;

    // The cursor points at the oldest row, so scrolling back continues normally.
    let next_cursor = messages.last().map(|m| MessageCursor {
        sent_at: m.sent_at.clone(),
        id: m.id.clone(),
    });

    Ok(MessagePage { messages, next_cursor })
}

/// The `limit` OLDEST messages strictly newer than `cursor`, newest-first, as
/// a `(rows, next_cursor)` pair (#1039). See [`read_messages_after`].
///
/// Split out so the paging contract (feeding `next_cursor` back visits every
/// row once, then stops) is testable against a bare connection.
pub(crate) fn read_after_on(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    cursor: &MessageCursor,
    limit: i64,
) -> rusqlite::Result<(Vec<ChannelMessage>, Option<MessageCursor>)> {
    let mut stmt = conn.prepare(
        "SELECT id, conversation_id, sender_id, ciphertext, content, reply_to_id, sent_at, edited_at, deleted_at, thread_id, undecryptable, send_state
         FROM message
         WHERE conversation_id = ?1
           AND (sent_at > ?2 OR (sent_at = ?2 AND id > ?3))
         ORDER BY sent_at ASC, id ASC
         LIMIT ?4",
    )?;
    let oldest_first: Vec<ChannelMessage> = stmt
        .query_map(
            rusqlite::params![conversation_id, cursor.sent_at, cursor.id, limit],
            row_to_channel_message,
        )?
        .flatten()
        .collect();

    // A short page is the tail of what this device holds. The cursor points at
    // the newest row returned: the edge the next forward page continues from.
    let next_cursor = if oldest_first.len() == limit as usize {
        oldest_first.last().map(|m| MessageCursor {
            sent_at: m.sent_at.clone(),
            id: m.id.clone(),
        })
    } else {
        None
    };

    let messages: Vec<ChannelMessage> = oldest_first.into_iter().rev().collect();
    Ok((messages, next_cursor))
}

/// A page of a conversation NEWER than a cursor — the forward read (#1039).
///
/// After a jump to an old message the reader scrolls forward; without this the
/// gap between the anchored page and the live tail silently vanished (#1039).
/// `cursor` is exclusive in `(sent_at, id)` order. `next_cursor` is `None` once
/// a page comes back short: the anchored window has caught up with the tail.
pub async fn read_messages_after(
    conversation_id: String,
    cursor: MessageCursor,
    limit: Option<i64>,
    state: &Arc<AppState>,
) -> Result<MessagePage> {
    let limit = limit.unwrap_or(50).clamp(1, 200);

    let (mut messages, next_cursor) = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        read_after_on(db.conn(), &conversation_id, &cursor, limit)?
    };

    attach_sender_usernames_local(state, &mut messages).await?;

    Ok(MessagePage { messages, next_cursor })
}

/// One thread's replies, oldest-first (#825).
///
/// Purely local: `thread_id` lives inside the ciphertext, so what this device
/// has decrypted is the thread. The root message is not included.
pub async fn read_thread_messages(
    thread_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<ChannelMessage>> {
    // rusqlite statements are not `Send`: scope the DB read in a block (an
    // explicit `drop()` is not enough for the generated future).
    let mut messages: Vec<ChannelMessage> = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

        let mut stmt = db.conn().prepare(
            "SELECT id, conversation_id, sender_id, ciphertext, content, reply_to_id, sent_at, edited_at, deleted_at, thread_id, undecryptable, send_state
             FROM message
             WHERE thread_id = ?1
             ORDER BY sent_at ASC, id ASC",
        )?;
        let mapped = stmt.query_map(rusqlite::params![thread_id], row_to_channel_message)?;
        mapped.filter_map(|r| r.ok()).collect()
    };

    attach_sender_usernames_local(state, &mut messages).await?;
    Ok(messages)
}

/// Reply count and last-reply time for every thread in a conversation (#825).
///
/// Soft-deleted replies are excluded, so a thread whose replies were all
/// deleted stops advertising itself.
pub async fn list_thread_summaries(
    conversation_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<ThreadSummary>> {
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

    let mut stmt = db.conn().prepare(
        "SELECT thread_id, COUNT(*) AS reply_count, MAX(sent_at) AS last_reply_at
         FROM message
         WHERE conversation_id = ?1
           AND thread_id IS NOT NULL
           AND deleted_at IS NULL
         GROUP BY thread_id",
    )?;
    let rows = stmt.query_map(rusqlite::params![conversation_id], |row| {
        Ok(ThreadSummary {
            thread_id: row.get(0)?,
            reply_count: row.get(1)?,
            last_reply_at: row.get(2)?,
        })
    })?;
    Ok(rows.filter_map(|r| r.ok()).collect())
}
