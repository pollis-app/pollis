use serde::{Deserialize, Serialize};
use tauri::State;
use std::sync::Arc;
use ulid::Ulid;

use crate::error::Result;
use crate::state::AppState;

const QUERY_MESSAGES_BY_SENDER: &str = include_str!("../db/queries/messages_by_sender.sql");
const QUERY_CHANNEL_PREVIEWS: &str = include_str!("../db/queries/channel_previews.sql");
const QUERY_CHANNEL_MESSAGES_INITIAL: &str = include_str!("../db/queries/channel_messages_initial.sql");
const QUERY_CHANNEL_MESSAGES_CURSOR: &str = include_str!("../db/queries/channel_messages_cursor.sql");
const QUERY_DM_CHANNEL_MESSAGES_INITIAL: &str = include_str!("../db/queries/dm_channel_messages_initial.sql");
const QUERY_DM_CHANNEL_MESSAGES_CURSOR: &str = include_str!("../db/queries/dm_channel_messages_cursor.sql");

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

#[tauri::command]
pub async fn list_messages(
    conversation_id: String,
    limit: Option<i64>,
    before_id: Option<String>,
    state: State<'_, Arc<AppState>>,
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

#[tauri::command]
pub async fn send_message(
    conversation_id: String,
    sender_id: String,
    content: String,
    reply_to_id: Option<String>,
    sender_username: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<Message> {
    state.check_not_outdated()?;
    let id = Ulid::new().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    // For group channels, all channels share the group's MLS group (keyed by group_id).
    // For DM conversations, the MLS group is keyed by conversation_id directly.
    // is_channel = true means conversation_id is a channel ID; group_id is the LiveKit room name.
    let (mls_group_id, is_channel) = {
        let conn = state.remote_db.conn().await?;
        let mut rows = conn.query(
            "SELECT group_id FROM channels WHERE id = ?1",
            libsql::params![conversation_id.clone()],
        ).await?;
        match rows.next().await? {
            Some(row) => (row.get::<String>(0)?, true),
            None => (conversation_id.clone(), false),
        }
    };

    // Poll MLS Welcomes — this device may have been added to the group but
    // hasn't applied the Welcome yet.
    {
        let device_id = state.device_id.lock().await.clone();
        if let Some(ref did) = device_id {
            if let Err(e) = crate::commands::mls::poll_mls_welcomes_inner(state.inner(), &sender_id, did).await {
                eprintln!("[messages] send_message: poll_mls_welcomes for {mls_group_id}: {e}");
            }
        }
    }

    // Process pending commits so the local epoch is current before encrypting.
    if let Err(e) = crate::commands::mls::process_pending_commits_inner(state.inner(), &mls_group_id).await {
        eprintln!("[messages] send_message: process_pending_commits for {mls_group_id}: {e}");
    }

    // If the MLS group still doesn't exist locally after polling welcomes and
    // processing commits, return an error rather than repairing.  Repair creates
    // a divergent group that breaks all other participants.
    {
        let has_group = {
            let guard = state.local_db.lock().await;
            guard.as_ref().map_or(false, |db| {
                crate::commands::mls::has_local_group(db.conn(), &mls_group_id)
            })
        };
        if !has_group {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "MLS group not available — this device hasn't received a Welcome yet for conversation {conversation_id}"
            )));
        }
    }

    let ciphertext_remote = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

        let mls_bytes = crate::commands::mls::try_mls_encrypt(db.conn(), &mls_group_id, content.as_bytes())
            .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!(
                "MLS group not initialized for conversation {conversation_id}"
            )))?;

        let mls_ct_str = format!("mls:{}", hex::encode(&mls_bytes));

        db.conn().execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, reply_to_id, sent_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![id, conversation_id, sender_id, mls_bytes, content, reply_to_id, now],
        )?;

        mls_ct_str
    };

    // Post to Turso for offline delivery.
    let conn = state.remote_db.conn().await?;
    conn.execute(
        "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, reply_to_id, sent_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        libsql::params![id.clone(), conversation_id.clone(), sender_id.clone(), ciphertext_remote, reply_to_id.clone(), now.clone()],
    ).await?;

    // Notify recipients via LiveKit. Non-fatal — errors are logged, not returned.
    let uname = sender_username.as_deref();
    if is_channel {
        // One LiveKit room per group covers all its channels.
        // Receivers filter by channel_id in the event payload.
        if let Err(e) = crate::commands::livekit::publish_new_message_to_room(
            &state.livekit,
            &mls_group_id,
            Some(&conversation_id),
            None,
            &sender_id,
            uname,
        ).await {
            eprintln!("[realtime] send_message: publish to group {mls_group_id}: {e}");
        }
    } else {
        // DM: publish directly to the shared DM room (conversation_id is the room name).
        // Both participants are connected to this room via connect_rooms.
        if let Err(e) = crate::commands::livekit::publish_new_message_to_room(
            &state.livekit,
            &conversation_id,
            None,
            Some(&conversation_id),
            &sender_id,
            uname,
        ).await {
            eprintln!("[realtime] send_message: publish to DM room {conversation_id}: {e}");
        }
    }

    Ok(Message {
        id,
        conversation_id,
        sender_id,
        content: Some(content),
        reply_to_id,
        sent_at: now,
    })
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

/// Fetch a page of messages for a channel the user is a member of.
///
/// First call: omit `cursor` to get the most recent `limit` messages.
/// Subsequent calls: pass the `next_cursor` from the previous response to
/// walk backwards through history. Results are ordered newest-first.
#[tauri::command]
pub async fn get_channel_messages(
    user_id: String,
    channel_id: String,
    limit: Option<i64>,
    cursor: Option<MessageCursor>,
    state: State<'_, Arc<AppState>>,
) -> Result<MessagePage> {
    let limit = limit.unwrap_or(50);
    let conn = state.remote_db.conn().await?;

    // All channels in a group share one MLS group (keyed by group_id).
    let mls_group_id = {
        let mut gid_rows = conn.query(
            "SELECT group_id FROM channels WHERE id = ?1",
            libsql::params![channel_id.clone()],
        ).await?;
        match gid_rows.next().await? {
            Some(row) => row.get::<String>(0)?,
            None => channel_id.clone(),
        }
    };

    // Poll MLS Welcomes first — this device may have been added to the group
    // (e.g. creator's second device, or invite acceptance) but hasn't applied
    // the Welcome yet.  Without this, process_pending_commits has no group to
    // apply commits to, and the fallback repair creates a divergent group.
    {
        let device_id = state.device_id.lock().await.clone();
        if let Some(ref did) = device_id {
            if let Err(e) = crate::commands::mls::poll_mls_welcomes_inner(state.inner(), &user_id, did).await {
                eprintln!("[messages] poll_mls_welcomes for {mls_group_id}: {e}");
            }
        }
    }

    // Process any pending MLS commits (membership changes) before decrypting
    // so the local epoch is current and messages from the new epoch are readable.
    if let Err(e) = crate::commands::mls::process_pending_commits_inner(state.inner(), &mls_group_id).await {
        eprintln!("[messages] process_pending_commits for {mls_group_id}: {e}");
    }

    // If the MLS group still doesn't exist locally after polling welcomes and
    // processing commits, log a warning.  Do NOT repair — repair creates an
    // independent group with different keys and deletes the commit log, which
    // breaks every other device/user already in the real group.  Messages will
    // remain encrypted (content=null) until this device receives a proper Welcome.
    {
        let has_group = {
            let guard = state.local_db.lock().await;
            guard.as_ref().map_or(false, |db| {
                crate::commands::mls::has_local_group(db.conn(), &mls_group_id)
            })
        };
        if !has_group {
            eprintln!("[messages] MLS group {mls_group_id} missing locally — messages will be encrypted until a Welcome is received");
        }
    }

    let mut rows = match cursor {
        None => {
            conn.query(
                QUERY_CHANNEL_MESSAGES_INITIAL,
                libsql::params![user_id.clone(), channel_id.clone(), limit],
            ).await?
        }
        Some(c) => {
            conn.query(
                QUERY_CHANNEL_MESSAGES_CURSOR,
                libsql::params![user_id.clone(), channel_id.clone(), c.sent_at, c.id, limit],
            ).await?
        }
    };

    let mut raw_messages = Vec::new();
    while let Some(row) = rows.next().await? {
        raw_messages.push((
            row.get::<String>(0)?,
            row.get::<String>(1)?,
            row.get::<String>(2)?,
            row.get::<Option<String>>(3)?,
            row.get::<String>(4)?,
            row.get::<Option<String>>(5)?,
            row.get::<String>(6)?,
        ));
    }

    // Fetch any pending edit envelopes for this conversation and apply them to
    // local DB before processing message envelopes. This ensures the message
    // processing loop below reads up-to-date plaintext from the local cache.
    let edit_envelopes: Vec<(String, String, String)> = {
        let mut edits = Vec::new();
        if let Ok(mut edit_rows) = conn.query(
            "SELECT id, target_message_id, ciphertext FROM message_envelope
             WHERE conversation_id = ?1 AND type = 'edit'
             ORDER BY sent_at ASC",
            libsql::params![channel_id.clone()],
        ).await {
            while let Ok(Some(row)) = edit_rows.next().await {
                if let (Ok(id), Ok(target), Ok(ct)) = (
                    row.get::<String>(0),
                    row.get::<String>(1),
                    row.get::<String>(2),
                ) {
                    edits.push((id, target, ct));
                }
            }
        }
        edits
    };

    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

    // Apply edit envelopes: decrypt and patch the local message cache.
    for (_edit_id, target_id, ciphertext) in &edit_envelopes {
        let plaintext = ciphertext.strip_prefix("mls:")
            .and_then(|hex_str| hex::decode(hex_str).ok())
            .and_then(|bytes| crate::commands::mls::try_mls_decrypt(db.conn(), &mls_group_id, &bytes))
            .and_then(|b| String::from_utf8(b).ok());
        if let Some(text) = plaintext {
            let now = chrono::Utc::now().to_rfc3339();
            let _ = db.conn().execute(
                "UPDATE message SET content = ?1, edited_at = ?2
                 WHERE id = ?3 AND deleted_at IS NULL",
                rusqlite::params![text, now, target_id],
            );
        }
    }

    // Sort oldest-first so MLS epoch ordering is preserved during decryption.
    raw_messages.sort_by(|a, b| a.0.cmp(&b.0));

    let mut messages: Vec<ChannelMessage> = raw_messages.into_iter().map(|(id, conv_id, sender_id, sender_username, ciphertext, reply_to_id, sent_at)| {
        // Read local state: content (plaintext cache), edited_at, deleted_at.
        let local_row: Option<(Option<String>, Option<String>, Option<String>)> = db.conn().query_row(
            "SELECT content, edited_at, deleted_at FROM message WHERE id = ?1",
            rusqlite::params![&id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).ok();

        let (edited_at, deleted_at) = local_row
            .as_ref()
            .map(|(_, e, d)| (e.clone(), d.clone()))
            .unwrap_or((None, None));

        let content = if deleted_at.is_some() {
            // Soft-deleted messages show no content.
            None
        } else {
            // Check local cache first (covers own messages sent from this
            // device and previously-decrypted peer messages).
            let cached = local_row.and_then(|(c, _, _)| c);
            if cached.is_some() {
                cached
            } else {
                // Cache miss — decrypt via MLS. For own-user messages sent
                // from a different device, sender_id == user_id but the
                // plaintext only exists on the sending device's local DB.
                let plaintext = ciphertext.strip_prefix("mls:")
                    .and_then(|hex_str| hex::decode(hex_str).ok())
                    .and_then(|bytes| crate::commands::mls::try_mls_decrypt(db.conn(), &mls_group_id, &bytes))
                    .and_then(|b| String::from_utf8(b).ok());
                if let Some(ref text) = plaintext {
                    let _ = db.conn().execute(
                        "INSERT OR REPLACE INTO message
                         (id, conversation_id, sender_id, ciphertext, content, sent_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        rusqlite::params![id, conv_id, sender_id, ciphertext.as_bytes(), text, sent_at],
                    );
                }
                plaintext
            }
        };
        ChannelMessage {
            id,
            conversation_id: conv_id,
            sender_id,
            sender_username,
            ciphertext,
            content,
            reply_to_id,
            sent_at,
            edited_at,
            deleted_at,
        }
    }).collect();

    // Restore newest-first order for the response (frontend expects newest at top).
    messages.reverse();

    // If we got a full page there are likely more; expose a cursor to the
    // oldest row so the caller can fetch the next older page.
    let next_cursor = if messages.len() == limit as usize {
        messages.last().map(|m| MessageCursor {
            sent_at: m.sent_at.clone(),
            id: m.id.clone(),
        })
    } else {
        None
    };

    // Upsert the caller's watermark using the latest sent_at from the returned
    // messages, not wall-clock time. This ensures cleanup only removes envelopes
    // that every member has actually fetched past. Skip if there are no messages
    // (nothing new was fetched, so the watermark should not advance).
    let max_sent_at = messages.iter().map(|m| m.sent_at.as_str()).max().map(str::to_owned);
    if let Some(watermark_ts) = max_sent_at {
        if let Err(e) = conn.execute(
            "INSERT INTO conversation_watermark (conversation_id, user_id, last_fetched_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(conversation_id, user_id) DO UPDATE SET
               last_fetched_at = MAX(last_fetched_at, excluded.last_fetched_at)",
            libsql::params![channel_id.clone(), user_id.clone(), watermark_ts],
        ).await {
            eprintln!("[watermark] get_channel_messages: upsert failed: {e}");
        }
    }

    // Delete envelopes that all current group members have fetched past.
    if let Err(e) = conn.execute(
        "DELETE FROM message_envelope
         WHERE conversation_id = ?1
         AND sent_at < datetime('now', '-30 days')
         AND sent_at < (
             SELECT CASE
                 WHEN COUNT(gm.user_id) = COUNT(cw.last_fetched_at)
                 THEN MIN(cw.last_fetched_at)
                 ELSE NULL
             END
             FROM group_member gm
             JOIN channels c ON c.id = ?1 AND c.group_id = gm.group_id
             LEFT JOIN conversation_watermark cw
                    ON cw.conversation_id = ?1 AND cw.user_id = gm.user_id
         )",
        libsql::params![channel_id.clone()],
    ).await {
        eprintln!("[watermark] get_channel_messages: cleanup failed: {e}");
    }

    Ok(MessagePage { messages, next_cursor })
}

/// All messages sent by a given user across all their channels,
/// ordered by group name, then channel name, then timestamp.
#[tauri::command]
pub async fn list_messages_by_sender(
    sender_id: String,
    state: State<'_, Arc<AppState>>,
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
#[tauri::command]
pub async fn list_channel_previews(
    user_id: String,
    state: State<'_, Arc<AppState>>,
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
#[tauri::command]
pub async fn get_dm_messages(
    user_id: String,
    dm_channel_id: String,
    limit: Option<i64>,
    cursor: Option<MessageCursor>,
    state: State<'_, Arc<AppState>>,
) -> Result<MessagePage> {
    let limit = limit.unwrap_or(50);

    // Poll MLS Welcomes first — this device may have a pending Welcome.
    {
        let device_id = state.device_id.lock().await.clone();
        if let Some(ref did) = device_id {
            if let Err(e) = crate::commands::mls::poll_mls_welcomes_inner(state.inner(), &user_id, did).await {
                eprintln!("[messages] poll_mls_welcomes for DM {dm_channel_id}: {e}");
            }
        }
    }

    // Process any pending MLS commits before decrypting.
    if let Err(e) = crate::commands::mls::process_pending_commits_inner(state.inner(), &dm_channel_id).await {
        eprintln!("[messages] process_pending_commits for DM {dm_channel_id}: {e}");
    }

    // If the MLS group still doesn't exist locally, log a warning.  Do NOT
    // repair — see comment in get_channel_messages for rationale.
    {
        let has_group = {
            let guard = state.local_db.lock().await;
            guard.as_ref().map_or(false, |db| {
                crate::commands::mls::has_local_group(db.conn(), &dm_channel_id)
            })
        };
        if !has_group {
            eprintln!("[messages] MLS group for DM {dm_channel_id} missing locally — messages will be encrypted until a Welcome is received");
        }
    }

    let conn = state.remote_db.conn().await?;

    let mut rows = match cursor {
        None => {
            conn.query(
                QUERY_DM_CHANNEL_MESSAGES_INITIAL,
                libsql::params![user_id.clone(), dm_channel_id.clone(), limit],
            ).await?
        }
        Some(c) => {
            conn.query(
                QUERY_DM_CHANNEL_MESSAGES_CURSOR,
                libsql::params![user_id.clone(), dm_channel_id.clone(), c.sent_at, c.id, limit],
            ).await?
        }
    };

    let mut raw_messages = Vec::new();
    while let Some(row) = rows.next().await? {
        raw_messages.push((
            row.get::<String>(0)?,
            row.get::<String>(1)?,
            row.get::<String>(2)?,
            row.get::<Option<String>>(3)?,
            row.get::<String>(4)?,
            row.get::<Option<String>>(5)?,
            row.get::<String>(6)?,
        ));
    }

    // Fetch pending edit envelopes and apply them before reading message cache.
    let edit_envelopes: Vec<(String, String, String)> = {
        let mut edits = Vec::new();
        if let Ok(mut edit_rows) = conn.query(
            "SELECT id, target_message_id, ciphertext FROM message_envelope
             WHERE conversation_id = ?1 AND type = 'edit'
             ORDER BY sent_at ASC",
            libsql::params![dm_channel_id.clone()],
        ).await {
            while let Ok(Some(row)) = edit_rows.next().await {
                if let (Ok(id), Ok(target), Ok(ct)) = (
                    row.get::<String>(0),
                    row.get::<String>(1),
                    row.get::<String>(2),
                ) {
                    edits.push((id, target, ct));
                }
            }
        }
        edits
    };

    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

    // Apply edit envelopes: decrypt and patch the local message cache.
    // DM MLS group is keyed by conversation_id directly.
    for (_edit_id, target_id, ciphertext) in &edit_envelopes {
        let plaintext = ciphertext.strip_prefix("mls:")
            .and_then(|hex_str| hex::decode(hex_str).ok())
            .and_then(|bytes| crate::commands::mls::try_mls_decrypt(db.conn(), &dm_channel_id, &bytes))
            .and_then(|b| String::from_utf8(b).ok());
        if let Some(text) = plaintext {
            let now = chrono::Utc::now().to_rfc3339();
            let _ = db.conn().execute(
                "UPDATE message SET content = ?1, edited_at = ?2
                 WHERE id = ?3 AND deleted_at IS NULL",
                rusqlite::params![text, now, target_id],
            );
        }
    }

    // Sort oldest-first so MLS epoch ordering is preserved during decryption.
    raw_messages.sort_by(|a, b| a.0.cmp(&b.0));

    let mut messages: Vec<ChannelMessage> = raw_messages.into_iter().map(|(id, conv_id, sender_id, sender_username, ciphertext, reply_to_id, sent_at)| {
        // Read local state: content (plaintext cache), edited_at, deleted_at.
        let local_row: Option<(Option<String>, Option<String>, Option<String>)> = db.conn().query_row(
            "SELECT content, edited_at, deleted_at FROM message WHERE id = ?1",
            rusqlite::params![&id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        ).ok();

        let (edited_at, deleted_at) = local_row
            .as_ref()
            .map(|(_, e, d)| (e.clone(), d.clone()))
            .unwrap_or((None, None));

        let content = if deleted_at.is_some() {
            None
        } else {
            let cached = local_row.and_then(|(c, _, _)| c);
            if cached.is_some() {
                cached
            } else {
                let plaintext = ciphertext.strip_prefix("mls:")
                    .and_then(|hex_str| hex::decode(hex_str).ok())
                    .and_then(|bytes| crate::commands::mls::try_mls_decrypt(db.conn(), &conv_id, &bytes))
                    .and_then(|b| String::from_utf8(b).ok());
                if let Some(ref text) = plaintext {
                    let _ = db.conn().execute(
                        "INSERT OR REPLACE INTO message
                         (id, conversation_id, sender_id, ciphertext, content, sent_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                        rusqlite::params![id, conv_id, sender_id, ciphertext.as_bytes(), text, sent_at],
                    );
                }
                plaintext
            }
        };
        ChannelMessage {
            id,
            conversation_id: conv_id,
            sender_id,
            sender_username,
            ciphertext,
            content,
            reply_to_id,
            sent_at,
            edited_at,
            deleted_at,
        }
    }).collect();

    messages.reverse();

    let next_cursor = if messages.len() == limit as usize {
        messages.last().map(|m| MessageCursor {
            sent_at: m.sent_at.clone(),
            id: m.id.clone(),
        })
    } else {
        None
    };

    // Upsert the caller's watermark using the latest sent_at from the returned
    // messages, not wall-clock time. This ensures cleanup only removes envelopes
    // that every member has actually fetched past. Skip if there are no messages
    // (nothing new was fetched, so the watermark should not advance).
    let max_sent_at = messages.iter().map(|m| m.sent_at.as_str()).max().map(str::to_owned);
    if let Some(watermark_ts) = max_sent_at {
        if let Err(e) = conn.execute(
            "INSERT INTO conversation_watermark (conversation_id, user_id, last_fetched_at)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(conversation_id, user_id) DO UPDATE SET
               last_fetched_at = MAX(last_fetched_at, excluded.last_fetched_at)",
            libsql::params![dm_channel_id.clone(), user_id.clone(), watermark_ts],
        ).await {
            eprintln!("[watermark] get_dm_messages: upsert failed: {e}");
        }
    }

    // Delete envelopes that all current DM members have fetched past.
    if let Err(e) = conn.execute(
        "DELETE FROM message_envelope
         WHERE conversation_id = ?1
         AND sent_at < datetime('now', '-30 days')
         AND sent_at < (
             SELECT CASE
                 WHEN COUNT(dcm.user_id) = COUNT(cw.last_fetched_at)
                 THEN MIN(cw.last_fetched_at)
                 ELSE NULL
             END
             FROM dm_channel_member dcm
             LEFT JOIN conversation_watermark cw
                    ON cw.conversation_id = ?1 AND cw.user_id = dcm.user_id
             WHERE dcm.dm_channel_id = ?1
         )",
        libsql::params![dm_channel_id.clone()],
    ).await {
        eprintln!("[watermark] get_dm_messages: cleanup failed: {e}");
    }

    Ok(MessagePage { messages, next_cursor })
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

/// Search the local plaintext message cache using a LIKE query.
/// Only messages where content IS NOT NULL are searched (i.e. decrypted messages).
/// Results are ordered newest-first.
#[tauri::command]
pub async fn search_messages(
    query: String,
    limit: Option<i64>,
    state: State<'_, Arc<AppState>>,
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

/// Delete a message: removes the envelope from Turso (preventing future delivery)
/// and removes it from the sender's local message cache. Recipients who already
/// have the message keep it — there is intentionally no retroactive deletion from
/// other devices. Any pending edit envelope for this message is also removed.
///
/// Best-effort on Turso: if the envelope was already cleaned up by the watermark
/// mechanism the remote delete is a no-op, but the local delete still proceeds.
#[tauri::command]
pub async fn delete_message(
    message_id: String,
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Remove the message envelope. Best-effort — may already be gone.
    conn.execute(
        "DELETE FROM message_envelope WHERE id = ?1 AND sender_id = ?2",
        libsql::params![message_id.clone(), user_id.clone()],
    ).await?;

    // Remove any pending edit envelope for this message.
    conn.execute(
        "DELETE FROM message_envelope WHERE target_message_id = ?1 AND type = 'edit'",
        libsql::params![message_id.clone()],
    ).await?;

    // Remove from the sender's local plaintext cache.
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

    let rows_affected = db.conn().execute(
        "DELETE FROM message WHERE id = ?1 AND sender_id = ?2",
        rusqlite::params![message_id, user_id],
    )?;

    if rows_affected == 0 {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "Message not found or you are not the sender"
        )));
    }

    Ok(())
}

/// Edit a message: updates the sender's local plaintext cache immediately and
/// publishes an encrypted edit envelope to Turso so all other group members
/// receive the updated content on their next fetch.
///
/// The edit envelope uses type='edit' with target_message_id pointing at the
/// original message. A DELETE+INSERT replaces any prior pending edit, so Turso
/// never holds more than one edit per message per conversation.
#[tauri::command]
pub async fn edit_message(
    conversation_id: String,
    message_id: String,
    user_id: String,
    new_content: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let envelope_id = Ulid::new().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    // Resolve the MLS group for this conversation (channel → group_id, DM → conversation_id).
    let mls_group_id = {
        let conn = state.remote_db.conn().await?;
        let mut rows = conn.query(
            "SELECT group_id FROM channels WHERE id = ?1",
            libsql::params![conversation_id.clone()],
        ).await?;
        match rows.next().await? {
            Some(row) => row.get::<String>(0)?,
            None => conversation_id.clone(),
        }
    };

    // Encrypt the new content with MLS and update local cache atomically.
    // First attempt — if the group is missing (e.g. local DB was wiped),
    // transparently repair and retry.
    let needs_repair = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        crate::commands::mls::try_mls_encrypt(db.conn(), &mls_group_id, new_content.as_bytes()).is_none()
    };

    if needs_repair {
        crate::commands::mls::repair_mls_group(state.inner(), &mls_group_id, &user_id).await?;
    }

    let ciphertext_remote = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

        let mls_bytes = crate::commands::mls::try_mls_encrypt(db.conn(), &mls_group_id, new_content.as_bytes())
            .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!(
                "MLS group not initialized for conversation {conversation_id}"
            )))?;

        let rows_affected = db.conn().execute(
            "UPDATE message SET content = ?1, edited_at = ?2
             WHERE id = ?3 AND sender_id = ?4 AND deleted_at IS NULL",
            rusqlite::params![new_content, now, message_id, user_id],
        )?;

        if rows_affected == 0 {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "Message not found, already deleted, or you are not the sender"
            )));
        }

        format!("mls:{}", hex::encode(&mls_bytes))
    };

    // Replace any existing edit envelope for this message with the new one.
    // DELETE + INSERT rather than ON CONFLICT to stay compatible with older libsql.
    let conn = state.remote_db.conn().await?;
    conn.execute(
        "DELETE FROM message_envelope
         WHERE conversation_id = ?1 AND target_message_id = ?2 AND type = 'edit'",
        libsql::params![conversation_id.clone(), message_id.clone()],
    ).await?;

    conn.execute(
        "INSERT INTO message_envelope
             (id, conversation_id, sender_id, ciphertext, sent_at, type, target_message_id)
         VALUES (?1, ?2, ?3, ?4, ?5, 'edit', ?6)",
        libsql::params![envelope_id, conversation_id.clone(), user_id.clone(), ciphertext_remote, now, message_id.clone()],
    ).await?;

    // Notify recipients via LiveKit so they invalidate their cache immediately.
    // Non-fatal — errors are logged, not returned.
    let is_channel = mls_group_id != conversation_id;
    let room_id = mls_group_id;
    if is_channel {
        if let Err(e) = crate::commands::livekit::publish_edited_message_to_room(
            &state.livekit,
            &room_id,
            Some(&conversation_id),
            None,
            &user_id,
            &message_id,
        ).await {
            eprintln!("[realtime] edit_message: publish to group {room_id}: {e}");
        }
    } else {
        if let Err(e) = crate::commands::livekit::publish_edited_message_to_room(
            &state.livekit,
            &room_id,
            None,
            Some(&conversation_id),
            &user_id,
            &message_id,
        ).await {
            eprintln!("[realtime] edit_message: publish to DM room {room_id}: {e}");
        }
    }

    Ok(())
}

/// Aggregated emoji reaction for a message.
/// `user_ids` is the list of users who reacted with this emoji.
#[derive(Debug, Serialize, Deserialize)]
pub struct Reaction {
    pub emoji: String,
    pub user_ids: Vec<String>,
    pub count: u32,
}

/// Add an emoji reaction to a message.
/// Silently succeeds if the reaction already exists (UNIQUE constraint).
#[tauri::command]
pub async fn add_reaction(
    message_id: String,
    user_id: String,
    emoji: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;
    let id = Ulid::new().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    conn.execute(
        "INSERT OR IGNORE INTO message_reaction (id, message_id, user_id, emoji, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        libsql::params![id, message_id, user_id, emoji, now],
    ).await?;

    Ok(())
}

/// Remove an emoji reaction from a message.
/// Silently succeeds if the reaction does not exist.
#[tauri::command]
pub async fn remove_reaction(
    message_id: String,
    user_id: String,
    emoji: String,
    state: State<'_, Arc<AppState>>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    conn.execute(
        "DELETE FROM message_reaction WHERE message_id = ?1 AND user_id = ?2 AND emoji = ?3",
        libsql::params![message_id, user_id, emoji],
    ).await?;

    Ok(())
}

/// Get all reactions for a message, grouped by emoji.
#[tauri::command]
pub async fn get_reactions(
    message_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<Reaction>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT emoji, user_id FROM message_reaction WHERE message_id = ?1 ORDER BY created_at ASC",
        libsql::params![message_id],
    ).await?;

    // Collect all (emoji, user_id) rows and group by emoji.
    let mut grouped: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    while let Some(row) = rows.next().await? {
        let emoji: String = row.get(0)?;
        let uid: String = row.get(1)?;
        grouped.entry(emoji).or_default().push(uid);
    }

    let reactions: Vec<Reaction> = grouped
        .into_iter()
        .map(|(emoji, user_ids)| {
            let count = user_ids.len() as u32;
            Reaction { emoji, user_ids, count }
        })
        .collect();

    Ok(reactions)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    const REMOTE_V001: &str = include_str!("../db/migrations/remote_schema.sql");

    // Both queries operate on the remote schema. Tests use rusqlite in-memory
    // (same SQLite dialect, no libsql threading conflict in test binaries).
    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn.execute_batch(REMOTE_V001).unwrap();
        // Apply migration 000010 columns (can't modify remote_schema.sql).
        conn.execute_batch(
            "ALTER TABLE message_envelope ADD COLUMN type TEXT NOT NULL DEFAULT 'message';
             ALTER TABLE message_envelope ADD COLUMN target_message_id TEXT;"
        ).unwrap();
        conn
    }

    fn setup(conn: &Connection) {
        // Users
        conn.execute("INSERT INTO users (id, email, username) VALUES ('alice', 'alice@x.com', 'alice')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('bob',   'bob@x.com',   'bob')", []).unwrap();

        // Groups (alphabetical names so ordering is deterministic)
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g-personal', 'personal', 'alice')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g-work',     'work',     'alice')", []).unwrap();

        // Both users are members of both groups
        for gid in ["g-personal", "g-work"] {
            for uid in ["alice", "bob"] {
                conn.execute(
                    "INSERT INTO group_member (group_id, user_id) VALUES (?1, ?2)",
                    rusqlite::params![gid, uid],
                ).unwrap();
            }
        }

        // Channels (alphabetical names within groups)
        conn.execute("INSERT INTO channels (id, group_id, name, created_at) VALUES ('ch-personal-random',  'g-personal', 'random',      '2024-01-01T00:00:00Z')", []).unwrap();
        conn.execute("INSERT INTO channels (id, group_id, name, created_at) VALUES ('ch-work-engineering', 'g-work',     'engineering', '2024-01-01T00:00:00Z')", []).unwrap();
        conn.execute("INSERT INTO channels (id, group_id, name, created_at) VALUES ('ch-work-general',     'g-work',     'general',     '2024-01-01T00:00:00Z')", []).unwrap();

        // Messages — alice sends in all three channels, bob sends once in work/general
        conn.execute("INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) VALUES ('m1', 'ch-work-general',     'alice', 'hello team',  '2024-01-01T10:00:00Z')", []).unwrap();
        conn.execute("INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) VALUES ('m2', 'ch-work-general',     'bob',   'hi alice',    '2024-01-01T10:01:00Z')", []).unwrap();
        conn.execute("INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) VALUES ('m3', 'ch-work-engineering', 'alice', 'ship it',     '2024-01-02T09:00:00Z')", []).unwrap();
        conn.execute("INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) VALUES ('m4', 'ch-personal-random',  'alice', 'lol',         '2024-01-03T12:00:00Z')", []).unwrap();
        conn.execute("INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) VALUES ('m5', 'ch-work-general',     'alice', 'see you all', '2024-01-04T17:00:00Z')", []).unwrap();
    }

    #[test]
    fn messages_by_sender_ordered_by_group_then_channel_then_time() {
        let conn = db();
        setup(&conn);

        let mut stmt = conn.prepare(super::QUERY_MESSAGES_BY_SENDER).unwrap();
        // (group_name, channel_name, sent_at)
        let results: Vec<(String, String, String)> = stmt.query_map(
            rusqlite::params!["alice"],
            |row| Ok((row.get(1)?, row.get(3)?, row.get(7)?)),
        ).unwrap().map(|r| r.unwrap()).collect();

        // alice sent 4 messages: m1, m3, m4, m5 (not m2 which is bob's)
        // Expected order: personal/random, work/engineering, work/general (x2 by time)
        assert_eq!(results.len(), 4);
        assert_eq!(results[0], ("personal".into(), "random".into(),      "2024-01-03T12:00:00Z".into()));
        assert_eq!(results[1], ("work".into(),     "engineering".into(), "2024-01-02T09:00:00Z".into()));
        assert_eq!(results[2], ("work".into(),     "general".into(),     "2024-01-01T10:00:00Z".into()));
        assert_eq!(results[3], ("work".into(),     "general".into(),     "2024-01-04T17:00:00Z".into()));
    }

    #[test]
    fn messages_by_sender_excludes_other_senders() {
        let conn = db();
        setup(&conn);

        let mut stmt = conn.prepare(super::QUERY_MESSAGES_BY_SENDER).unwrap();
        let count = stmt.query_map(rusqlite::params!["bob"], |_| Ok(()))
            .unwrap().count();

        // Bob only sent m2
        assert_eq!(count, 1);
    }

    #[test]
    fn channel_previews_ordered_most_recent_first() {
        let conn = db();
        setup(&conn);

        let mut stmt = conn.prepare(super::QUERY_CHANNEL_PREVIEWS).unwrap();
        // (channel_id, last_message, last_sender_username)
        let results: Vec<(String, Option<String>, Option<String>)> = stmt.query_map(
            rusqlite::params!["alice"],
            |row| Ok((row.get(2)?, row.get(4)?, row.get(7)?)),
        ).unwrap().map(|r| r.unwrap()).collect();

        assert_eq!(results.len(), 3, "alice belongs to 3 channels");

        // Most recent activity:
        //   work/general     — m5 at 2024-01-04 (sender: alice)
        //   personal/random  — m4 at 2024-01-03 (sender: alice)
        //   work/engineering — m3 at 2024-01-02 (sender: alice)
        let ids: Vec<&str> = results.iter().map(|(id, _, _)| id.as_str()).collect();
        assert_eq!(ids, ["ch-work-general", "ch-personal-random", "ch-work-engineering"]);

        let (_, msg, sender) = &results[0];
        assert_eq!(msg.as_deref(), Some("see you all"));
        assert_eq!(sender.as_deref(), Some("alice"));
    }

    #[test]
    fn channel_previews_last_message_is_most_recent_not_first() {
        let conn = db();
        setup(&conn);

        // work/general has m1 (alice), m2 (bob), m5 (alice) — preview should show m5
        let mut stmt = conn.prepare(super::QUERY_CHANNEL_PREVIEWS).unwrap();
        let results: Vec<(String, Option<String>, Option<String>)> = stmt.query_map(
            rusqlite::params!["bob"],
            |row| Ok((row.get(2)?, row.get(4)?, row.get(7)?)),
        ).unwrap().map(|r| r.unwrap()).collect();

        let general = results.iter().find(|(id, _, _)| id == "ch-work-general").unwrap();
        assert_eq!(general.1.as_deref(), Some("see you all"), "preview should show m5 not m1 or m2");
        assert_eq!(general.2.as_deref(), Some("alice"), "sender of last message is alice");
    }

    #[test]
    fn channel_previews_empty_channel_appears_last() {
        let conn = db();
        setup(&conn);

        // Add an empty channel both users are in (via g-work membership)
        conn.execute("INSERT INTO channels (id, group_id, name, created_at) VALUES ('ch-work-quiet', 'g-work', 'quiet', '2024-01-01T00:00:00Z')", []).unwrap();

        let mut stmt = conn.prepare(super::QUERY_CHANNEL_PREVIEWS).unwrap();
        let results: Vec<(String, Option<String>)> = stmt.query_map(
            rusqlite::params!["bob"],
            |row| Ok((row.get(2)?, row.get(4)?)),
        ).unwrap().map(|r| r.unwrap()).collect();

        let last = results.last().unwrap();
        assert_eq!(last.0, "ch-work-quiet");
        assert!(last.1.is_none(), "empty channel has no last_message");
    }

    // ── channel message pagination ──────────────────────────────────────────

    fn setup_pagination(conn: &Connection) {
        conn.execute("INSERT INTO users (id, email, username) VALUES ('alice', 'alice@x.com', 'alice')", []).unwrap();
        conn.execute("INSERT INTO users (id, email, username) VALUES ('bob',   'bob@x.com',   'bob')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'work', 'alice')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob')", []).unwrap();
        conn.execute("INSERT INTO channels (id, group_id, name) VALUES ('ch1', 'g1', 'general')", []).unwrap();

        // Insert 10 messages with distinct timestamps so ordering is fully deterministic.
        for i in 1..=10usize {
            conn.execute(
                "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at)
                 VALUES (?1, 'ch1', 'alice', ?2, ?3)",
                rusqlite::params![
                    format!("m{i:02}"),
                    format!("msg {i}"),
                    format!("2024-01-01T10:{i:02}:00Z"),
                ],
            ).unwrap();
        }
    }

    #[test]
    fn initial_load_returns_newest_first() {
        let conn = db();
        setup_pagination(&conn);

        let mut stmt = conn.prepare(super::QUERY_CHANNEL_MESSAGES_INITIAL).unwrap();
        let ids: Vec<String> = stmt.query_map(
            rusqlite::params!["alice", "ch1", 3],
            |row| row.get(0),
        ).unwrap().map(|r| r.unwrap()).collect();

        // Newest 3: m10, m09, m08
        assert_eq!(ids, ["m10", "m09", "m08"]);
    }

    #[test]
    fn cursor_load_returns_next_older_page() {
        let conn = db();
        setup_pagination(&conn);

        // Page 1: newest 4 → m10, m09, m08, m07
        let mut stmt = conn.prepare(super::QUERY_CHANNEL_MESSAGES_INITIAL).unwrap();
        let page1: Vec<(String, String)> = stmt.query_map(
            rusqlite::params!["alice", "ch1", 4],
            |row| Ok((row.get(0)?, row.get(6)?)),
        ).unwrap().map(|r| r.unwrap()).collect();

        assert_eq!(page1[0].0, "m10");
        assert_eq!(page1[3].0, "m07");

        // Cursor is the oldest row on page 1 (m07)
        let (cursor_id, cursor_sent_at) = &page1[3];

        // Page 2: 4 messages older than m07 → m06, m05, m04, m03
        let mut stmt = conn.prepare(super::QUERY_CHANNEL_MESSAGES_CURSOR).unwrap();
        let page2: Vec<String> = stmt.query_map(
            rusqlite::params!["alice", "ch1", cursor_sent_at, cursor_id, 4],
            |row| row.get(0),
        ).unwrap().map(|r| r.unwrap()).collect();

        assert_eq!(page2, ["m06", "m05", "m04", "m03"]);
    }

    #[test]
    fn cursor_load_final_page_returns_remaining_messages() {
        let conn = db();
        setup_pagination(&conn);

        // Seek past the first 7 messages to get the last 3
        let cursor_sent_at = "2024-01-01T10:04:00Z"; // m04
        let cursor_id = "m04";

        let mut stmt = conn.prepare(super::QUERY_CHANNEL_MESSAGES_CURSOR).unwrap();
        let ids: Vec<String> = stmt.query_map(
            rusqlite::params!["alice", "ch1", cursor_sent_at, cursor_id, 10],
            |row| row.get(0),
        ).unwrap().map(|r| r.unwrap()).collect();

        // Only m03, m02, m01 remain
        assert_eq!(ids, ["m03", "m02", "m01"]);
    }

    #[test]
    fn cursor_handles_timestamp_tie() {
        let conn = db();
        // Two messages share the same sent_at — the id tiebreaker must prevent skips.
        conn.execute("INSERT INTO users (id, email, username) VALUES ('alice', 'alice@x.com', 'alice')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'g', 'alice')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice')", []).unwrap();
        conn.execute("INSERT INTO channels (id, group_id, name) VALUES ('ch1', 'g1', 'c')", []).unwrap();

        // m-a and m-b have identical timestamps; m-z is older
        let ts = "2024-01-01T10:00:00Z";
        conn.execute("INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) VALUES ('m-a', 'ch1', 'alice', 'a', ?1)", rusqlite::params![ts]).unwrap();
        conn.execute("INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) VALUES ('m-b', 'ch1', 'alice', 'b', ?1)", rusqlite::params![ts]).unwrap();
        conn.execute("INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) VALUES ('m-z', 'ch1', 'alice', 'z', '2024-01-01T09:00:00Z')", []).unwrap();

        // Page 1: limit 2 → the two tied messages (m-b then m-a, id DESC)
        let mut stmt = conn.prepare(super::QUERY_CHANNEL_MESSAGES_INITIAL).unwrap();
        let page1: Vec<(String, String)> = stmt.query_map(
            rusqlite::params!["alice", "ch1", 2],
            |row| Ok((row.get(0)?, row.get(6)?)),
        ).unwrap().map(|r| r.unwrap()).collect();

        assert_eq!(page1.len(), 2);
        // oldest on page 1 is m-a (id 'm-a' < 'm-b' in DESC order)
        let (cursor_id, cursor_sent_at) = &page1[1];

        // Page 2: should contain exactly m-z, not a duplicate or skip
        let mut stmt = conn.prepare(super::QUERY_CHANNEL_MESSAGES_CURSOR).unwrap();
        let page2: Vec<String> = stmt.query_map(
            rusqlite::params!["alice", "ch1", cursor_sent_at, cursor_id, 10],
            |row| row.get(0),
        ).unwrap().map(|r| r.unwrap()).collect();

        assert_eq!(page2, ["m-z"], "timestamp tie must not cause m-z to be skipped or duplicated");
    }

    #[test]
    fn non_member_gets_no_messages() {
        let conn = db();
        setup_pagination(&conn);

        // carol is not in any group
        conn.execute("INSERT INTO users (id, email, username) VALUES ('carol', 'carol@x.com', 'carol')", []).unwrap();

        let mut stmt = conn.prepare(super::QUERY_CHANNEL_MESSAGES_INITIAL).unwrap();
        let count = stmt.query_map(
            rusqlite::params!["carol", "ch1", 50],
            |_| Ok(()),
        ).unwrap().count();

        assert_eq!(count, 0, "non-member should see no messages");
    }

    #[test]
    fn channel_previews_excludes_channels_user_is_not_in() {
        let conn = db();
        setup(&conn);

        // Carol has her own group — bob is not a member
        conn.execute("INSERT INTO users (id, email, username) VALUES ('carol', 'carol@x.com', 'carol')", []).unwrap();
        conn.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g-secret', 'secret', 'carol')", []).unwrap();
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g-secret', 'carol')", []).unwrap();
        conn.execute("INSERT INTO channels (id, group_id, name) VALUES ('ch-secret', 'g-secret', 'private')", []).unwrap();

        let mut stmt = conn.prepare(super::QUERY_CHANNEL_PREVIEWS).unwrap();
        let channel_ids: Vec<String> = stmt.query_map(
            rusqlite::params!["bob"],
            |row| row.get(2),
        ).unwrap().map(|r| r.unwrap()).collect();

        assert!(!channel_ids.contains(&"ch-secret".to_string()));
    }
}
