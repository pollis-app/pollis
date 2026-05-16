use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

use crate::db::queries::MESSAGES_BY_SENDER as QUERY_MESSAGES_BY_SENDER;
use crate::db::queries::CHANNEL_PREVIEWS as QUERY_CHANNEL_PREVIEWS;

use super::ingest::{ingest_channel_envelopes_inner, ingest_dm_envelopes_inner};
use super::types::{
    ChannelMessage, ChannelPreview, Message, MessageCursor, MessagePage, MessageWithContext,
    SearchResult,
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
            "SELECT id, conversation_id, sender_id, content, reply_to_id, sent_at
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
                sent_at: row.get(5)?,
            }),
        )?;

        rows.filter_map(|r| r.ok()).collect()
    } else {
        let mut stmt = db.conn().prepare(
            "SELECT id, conversation_id, sender_id, content, reply_to_id, sent_at
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
                sent_at: row.get(5)?,
            }),
        )?;

        rows.filter_map(|r| r.ok()).collect()
    };

    Ok(messages)
}

/// Fetch a page of messages for a channel the user is a member of.
///
/// First call: omit `cursor` to get the most recent `limit` messages.
/// Subsequent calls: pass the `next_cursor` from the previous response to
/// walk backwards through history. Results are ordered newest-first.
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
    attach_sender_usernames(state, &mut messages).await?;

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

/// Read a page of messages for a conversation from the local `message` table,
/// newest-first. Used by both channel and DM read paths after ingest has
/// persisted any new envelopes.
async fn read_local_channel_page(
    state: &Arc<AppState>,
    conversation_id: &str,
    cursor: &Option<MessageCursor>,
    limit: i64,
) -> Result<Vec<ChannelMessage>> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

    fn row_to_message(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChannelMessage> {
        let ct: Vec<u8> = row.get(3)?;
        let content: Option<String> = row.get(4)?;
        let deleted_at: Option<String> = row.get(8)?;
        Ok(ChannelMessage {
            id: row.get(0)?,
            conversation_id: row.get(1)?,
            sender_id: row.get(2)?,
            sender_username: None,
            ciphertext: format!("mls:{}", hex::encode(&ct)),
            // Soft-deleted messages mask content to None regardless of cache.
            content: if deleted_at.is_some() { None } else { content },
            reply_to_id: row.get(5)?,
            sent_at: row.get(6)?,
            edited_at: row.get(7)?,
            deleted_at,
        })
    }

    let mut rows: Vec<ChannelMessage> = Vec::new();
    match cursor {
        None => {
            let mut stmt = db.conn().prepare(
                "SELECT id, conversation_id, sender_id, ciphertext, content, reply_to_id, sent_at, edited_at, deleted_at
                 FROM message
                 WHERE conversation_id = ?1
                 ORDER BY sent_at DESC, id DESC
                 LIMIT ?2"
            )?;
            let mapped = stmt.query_map(rusqlite::params![conversation_id, limit], row_to_message)?;
            for r in mapped {
                if let Ok(m) = r {
                    rows.push(m);
                }
            }
        }
        Some(c) => {
            let mut stmt = db.conn().prepare(
                "SELECT id, conversation_id, sender_id, ciphertext, content, reply_to_id, sent_at, edited_at, deleted_at
                 FROM message
                 WHERE conversation_id = ?1
                   AND (sent_at < ?2 OR (sent_at = ?2 AND id < ?3))
                 ORDER BY sent_at DESC, id DESC
                 LIMIT ?4"
            )?;
            let mapped = stmt.query_map(rusqlite::params![conversation_id, c.sent_at, c.id, limit], row_to_message)?;
            for r in mapped {
                if let Ok(m) = r {
                    rows.push(m);
                }
            }
        }
    }

    Ok(rows)
}

/// Batch-resolve sender usernames from the remote `users` table and attach
/// them to the page. A missing user (deleted, never existed) simply stays as
/// `None` on that message.
async fn attach_sender_usernames(
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
    let placeholders = (1..=ids_vec.len())
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!("SELECT id, username FROM users WHERE id IN ({placeholders})");
    let params: Vec<libsql::Value> = ids_vec
        .iter()
        .map(|s| libsql::Value::Text(s.clone()))
        .collect();

    let conn = state.remote_db.conn().await?;
    let mut rows = conn.query(&sql, libsql::params_from_iter(params)).await?;
    let mut map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    while let Some(row) = rows.next().await? {
        let id: String = row.get(0)?;
        let name: String = row.get(1)?;
        map.insert(id, name);
    }
    for m in messages.iter_mut() {
        m.sender_username = map.get(&m.sender_id).cloned();
    }
    Ok(())
}

/// Attach sender usernames from the local `user_cache` table; for any
/// sender_ids missing from the cache, do one batched remote fetch and
/// write the results back. After the first read of a channel/DM, the
/// cache is warm and subsequent reads are zero-remote.
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
        let placeholders = (1..=ids_vec.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("SELECT id, username FROM user_cache WHERE id IN ({placeholders})");
        let mut stmt = db.conn().prepare(&sql)?;
        let mapped = stmt.query_map(rusqlite::params_from_iter(ids_vec.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        for r in mapped {
            if let Ok((id, name)) = r {
                found.insert(id, name);
            }
        }
        ids_vec.iter().filter(|i| !found.contains_key(*i)).cloned().collect()
    };

    if !missing.is_empty() {
        let placeholders = (1..=missing.len())
            .map(|i| format!("?{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!("SELECT id, username FROM users WHERE id IN ({placeholders})");
        let params: Vec<libsql::Value> = missing
            .iter()
            .map(|s| libsql::Value::Text(s.clone()))
            .collect();
        let conn = state.remote_db.conn().await?;
        match conn.query(&sql, libsql::params_from_iter(params)).await {
            Ok(mut rows) => {
                let mut fetched: Vec<(String, String)> = Vec::new();
                while let Some(row) = rows.next().await? {
                    let id: String = row.get(0)?;
                    let name: String = row.get(1)?;
                    fetched.push((id.clone(), name.clone()));
                    found.insert(id, name);
                }
                drop(rows);
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
                // Offline or transient — leave the missing names as None.
                // Next ingest / read while online will fill them in.
                eprintln!("[messages] attach_sender_usernames_local: remote fallback failed: {e}");
            }
        }
    }

    for m in messages.iter_mut() {
        m.sender_username = found.get(&m.sender_id).cloned();
    }
    Ok(())
}

/// Local-only read of a channel page. Does NOT trigger ingest — callers
/// fire `ingest_channel_envelopes` separately, off the critical render
/// path. Usernames come from the local `user_cache` (with a one-shot
/// remote fallback for cache misses, see `attach_sender_usernames_local`).
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

/// All messages sent by a given user across all their channels,
/// ordered by group name, then channel name, then timestamp.
pub async fn list_messages_by_sender(
    sender_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<MessageWithContext>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(QUERY_MESSAGES_BY_SENDER, libsql::params![sender_id]).await?;

    let mut messages = Vec::new();
    while let Some(row) = rows.next().await? {
        messages.push(MessageWithContext {
            group_id: row.get(0)?,
            group_name: row.get(1)?,
            channel_id: row.get(2)?,
            channel_name: row.get(3)?,
            id: row.get(4)?,
            sender_id: row.get(5)?,
            ciphertext: row.get(6)?,
            sent_at: row.get(7)?,
        });
    }

    Ok(messages)
}

/// Last message and sender username for every channel the given user belongs to,
/// ordered most-recently-active first. Channels with no messages appear last.
pub async fn list_channel_previews(
    user_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<ChannelPreview>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(QUERY_CHANNEL_PREVIEWS, libsql::params![user_id]).await?;

    let mut previews = Vec::new();
    while let Some(row) = rows.next().await? {
        previews.push(ChannelPreview {
            group_id: row.get(0)?,
            group_name: row.get(1)?,
            channel_id: row.get(2)?,
            channel_name: row.get(3)?,
            last_message: row.get(4)?,
            last_sent_at: row.get(5)?,
            last_sender_id: row.get(6)?,
            last_sender_username: row.get(7)?,
        });
    }

    Ok(previews)
}

/// Fetch a page of messages for a DM channel the user is a member of.
/// Results are ordered newest-first.
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
    attach_sender_usernames(state, &mut messages).await?;

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

/// Search the local plaintext message cache using a LIKE query.
/// Only messages where content IS NOT NULL are searched (i.e. decrypted messages).
/// Results are ordered newest-first.
pub async fn search_messages(
    query: String,
    limit: Option<i64>,
    state: &Arc<AppState>,
) -> Result<Vec<SearchResult>> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
    let limit = limit.unwrap_or(50);
    let pattern = format!("%{}%", query);

    let mut stmt = db.conn().prepare(
        "SELECT id, conversation_id, sender_id, content, sent_at
         FROM message
         WHERE content IS NOT NULL AND content LIKE ?1
         ORDER BY sent_at DESC LIMIT ?2"
    )?;

    let rows = stmt.query_map(
        rusqlite::params![pattern, limit],
        |row| {
            let content: String = row.get(3)?;
            let snippet = content.clone();
            Ok(SearchResult {
                message_id: row.get(0)?,
                conversation_id: row.get(1)?,
                sender_id: row.get(2)?,
                content,
                sent_at: row.get(4)?,
                snippet,
            })
        },
    )?;

    Ok(rows.filter_map(|r| r.ok()).collect())
}
