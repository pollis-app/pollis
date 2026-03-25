use serde::{Deserialize, Serialize};
use tauri::State;
use std::sync::Arc;
use ulid::Ulid;

use crate::error::Result;
use crate::state::AppState;
use crate::signal::group::{SenderKeyState, SenderKeyMessage};
use crate::signal::session;
use crate::signal::crypto;
use crate::signal::identity::load_x25519_secret;

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
    let db = state.local_db.lock().await;
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

/// Distribute the sender's SenderKeyState to all members of a group channel.
/// Fetches member identity keys and SPKs from remote DB, encrypts the state
/// for each recipient, and stores in sender_key_dist.
async fn distribute_sender_key_to_group_members(
    conn: &libsql::Connection,
    channel_id: &str,
    sender_id: &str,
    state_to_distribute: &SenderKeyState,
) -> Result<()> {
    // Get group_id for this channel
    let mut rows = conn.query(
        "SELECT group_id FROM channels WHERE id = ?1",
        libsql::params![channel_id],
    ).await?;

    let group_id: String = if let Some(row) = rows.next().await? {
        row.get(0)?
    } else {
        return Ok(());
    };

    // Get all members of the group with their keys
    let mut member_rows = conn.query(
        "SELECT u.id, u.identity_key,
                (SELECT spk.public_key FROM signed_prekey spk WHERE spk.user_id = u.id ORDER BY spk.key_id DESC LIMIT 1) AS spk_pub,
                (SELECT spk.key_id FROM signed_prekey spk WHERE spk.user_id = u.id ORDER BY spk.key_id DESC LIMIT 1) AS spk_id
         FROM group_member gm
         JOIN users u ON u.id = gm.user_id
         WHERE gm.group_id = ?1 AND gm.user_id != ?2
           AND u.identity_key IS NOT NULL",
        libsql::params![group_id, sender_id],
    ).await?;

    let mut distributed = 0usize;
    while let Some(row) = member_rows.next().await? {
        let member_id: String = row.get(0)?;
        let ik_hex: String = row.get(1)?;
        let spk_hex: Option<String> = row.get(2)?;
        let spk_id: Option<i64> = row.get(3)?;

        let spk_hex = match spk_hex {
            Some(s) => s,
            None => {
                eprintln!("[dist] skipping member {member_id}: no SPK");
                continue;
            }
        };
        let spk_id = match spk_id {
            Some(id) => id,
            None => {
                eprintln!("[dist] skipping member {member_id}: no SPK id");
                continue;
            }
        };

        let ik_bytes = match hex::decode(&ik_hex) {
            Ok(b) if b.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&b);
                arr
            }
            _ => {
                eprintln!("[dist] skipping member {member_id}: bad identity_key hex (len={})", ik_hex.len());
                continue;
            }
        };

        let spk_bytes = match hex::decode(&spk_hex) {
            Ok(b) if b.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&b);
                arr
            }
            _ => {
                eprintln!("[dist] skipping member {member_id}: bad SPK hex");
                continue;
            }
        };

        let (encrypted_state, ephemeral_key) = match crypto::encrypt_sender_key_for_recipient(
            state_to_distribute,
            &ik_bytes,
            &spk_bytes,
        ) {
            Ok(result) => result,
            Err(e) => {
                eprintln!("[dist] encrypt_sender_key_for_recipient failed for member {member_id}: {e}");
                continue;
            }
        };

        let dist_id = Ulid::new().to_string();
        match conn.execute(
            "INSERT OR REPLACE INTO sender_key_dist
             (id, channel_id, sender_id, recipient_id, encrypted_state, ephemeral_key, spk_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            libsql::params![
                dist_id,
                channel_id,
                sender_id,
                member_id.clone(),
                encrypted_state,
                ephemeral_key,
                spk_id,
            ],
        ).await {
            Ok(_) => {
                eprintln!("[dist] distributed sender key to member {member_id} on channel {channel_id}");
                distributed += 1;
            }
            Err(e) => eprintln!("[dist] INSERT failed for member {member_id}: {e}"),
        }
    }
    eprintln!("[dist] group distribution done: {distributed} new member(s) on channel {channel_id}");

    Ok(())
}

/// Distribute the sender's SenderKeyState to all members of a DM channel.
async fn distribute_sender_key_to_dm_members(
    conn: &libsql::Connection,
    dm_channel_id: &str,
    sender_id: &str,
    state_to_distribute: &SenderKeyState,
) -> Result<()> {
    let mut member_rows = conn.query(
        "SELECT u.id, u.identity_key,
                (SELECT spk.public_key FROM signed_prekey spk WHERE spk.user_id = u.id ORDER BY spk.key_id DESC LIMIT 1) AS spk_pub,
                (SELECT spk.key_id FROM signed_prekey spk WHERE spk.user_id = u.id ORDER BY spk.key_id DESC LIMIT 1) AS spk_id
         FROM dm_channel_member dcm
         JOIN users u ON u.id = dcm.user_id
         WHERE dcm.dm_channel_id = ?1 AND dcm.user_id != ?2
           AND u.identity_key IS NOT NULL",
        libsql::params![dm_channel_id, sender_id],
    ).await?;

    let mut distributed = 0usize;
    while let Some(row) = member_rows.next().await? {
        let member_id: String = row.get(0)?;
        let ik_hex: String = row.get(1)?;
        let spk_hex: Option<String> = row.get(2)?;
        let spk_id: Option<i64> = row.get(3)?;

        let spk_hex = match spk_hex {
            Some(s) => s,
            None => {
                eprintln!("[dist] DM skipping member {member_id}: no SPK");
                continue;
            }
        };
        let spk_id = match spk_id {
            Some(id) => id,
            None => {
                eprintln!("[dist] DM skipping member {member_id}: no SPK id");
                continue;
            }
        };

        let ik_bytes = match hex::decode(&ik_hex) {
            Ok(b) if b.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&b);
                arr
            }
            _ => {
                eprintln!("[dist] DM skipping member {member_id}: bad identity_key hex");
                continue;
            }
        };

        let spk_bytes = match hex::decode(&spk_hex) {
            Ok(b) if b.len() == 32 => {
                let mut arr = [0u8; 32];
                arr.copy_from_slice(&b);
                arr
            }
            _ => {
                eprintln!("[dist] DM skipping member {member_id}: bad SPK hex");
                continue;
            }
        };

        let (encrypted_state, ephemeral_key) = match crypto::encrypt_sender_key_for_recipient(
            state_to_distribute,
            &ik_bytes,
            &spk_bytes,
        ) {
            Ok(result) => result,
            Err(e) => {
                eprintln!("[dist] DM encrypt failed for member {member_id}: {e}");
                continue;
            }
        };

        let dist_id = Ulid::new().to_string();
        match conn.execute(
            "INSERT OR REPLACE INTO sender_key_dist
             (id, channel_id, sender_id, recipient_id, encrypted_state, ephemeral_key, spk_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            libsql::params![
                dist_id,
                dm_channel_id,
                sender_id,
                member_id.clone(),
                encrypted_state,
                ephemeral_key,
                spk_id,
            ],
        ).await {
            Ok(_) => {
                eprintln!("[dist] DM distributed sender key to member {member_id} on channel {dm_channel_id}");
                distributed += 1;
            }
            Err(e) => eprintln!("[dist] DM INSERT failed for member {member_id}: {e}"),
        }
    }
    eprintln!("[dist] DM distribution done: {distributed} new member(s) on channel {dm_channel_id}");

    Ok(())
}

#[tauri::command]
pub async fn send_message(
    conversation_id: String,
    sender_id: String,
    content: String,
    reply_to_id: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<Message> {
    state.check_not_outdated()?;
    let id = Ulid::new().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    // Encrypt the message using sender's SenderKeyState
    let (_sender_key_msg, ciphertext_json, pre_encrypt_state) = {
        let db = state.local_db.lock().await;

        // Load or create SenderKeyState for this channel
        let mut sender_key = match session::load_sender_key(db.conn(), &conversation_id, &sender_id)? {
            Some(key) => key,
            None => SenderKeyState::new(),
        };

        // Capture state before encrypt so we can distribute it to new members.
        // Recipients need the pre-encrypt state to decrypt this and future messages.
        let pre_state = sender_key.clone();

        let msg = sender_key.encrypt(content.as_bytes())?;
        let ciphertext_json = serde_json::to_string(&msg)?;
        let ciphertext_bytes = ciphertext_json.as_bytes().to_vec();

        // Save updated state to local DB
        session::save_sender_key(db.conn(), &conversation_id, &sender_id, &sender_key)?;

        // Store in local message table (ciphertext + plaintext content for own messages)
        db.conn().execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, reply_to_id, sent_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            rusqlite::params![
                id,
                conversation_id,
                sender_id,
                ciphertext_bytes,
                content,
                reply_to_id,
                now,
            ],
        )?;

        (msg, ciphertext_json, pre_state)
    };

    // Write envelope to Turso for offline delivery (use same id as local message)
    let conn = state.remote_db.conn().await?;
    conn.execute(
        "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, reply_to_id, sent_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        libsql::params![id.clone(), conversation_id.clone(), sender_id.clone(), ciphertext_json.clone(), reply_to_id.clone(), now.clone()],
    ).await?;

    // Distribute pre-encrypt sender key state to any members who don't have it yet.
    // Using the pre-encrypt state ensures recipients can decrypt the message just sent
    // and all future messages. The NOT EXISTS filter in each distribute function means
    // this is a no-op when all members already have our key.
    let is_dm = match conn.query(
        "SELECT 1 FROM dm_channel WHERE id = ?1",
        libsql::params![conversation_id.clone()],
    ).await {
        Ok(mut rows) => rows.next().await.ok().flatten().is_some(),
        Err(_) => false,
    };

    if is_dm {
        let _ = distribute_sender_key_to_dm_members(
            &conn,
            &conversation_id,
            &sender_id,
            &pre_encrypt_state,
        ).await;
    } else {
        let _ = distribute_sender_key_to_group_members(
            &conn,
            &conversation_id,
            &sender_id,
            &pre_encrypt_state,
        ).await;
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

#[tauri::command]
pub async fn poll_pending_messages(
    user_id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<Message>> {
    let conn = state.remote_db.conn().await?;

    let mut rows = conn.query(
        "SELECT id, conversation_id, sender_id, ciphertext, sent_at
         FROM message_envelope
         WHERE conversation_id IN (
             SELECT id FROM channels WHERE group_id IN (
                 SELECT group_id FROM group_member WHERE user_id = ?1
             )
         )
         AND delivered = 0",
        libsql::params![user_id.clone()],
    ).await?;

    let mut messages = Vec::new();
    while let Some(row) = rows.next().await? {
        messages.push(Message {
            id: row.get(0)?,
            conversation_id: row.get(1)?,
            sender_id: row.get(2)?,
            // Return the ciphertext as content; caller can decrypt separately
            content: Some(row.get(3)?),
            reply_to_id: None,
            sent_at: row.get(4)?,
        });
    }

    Ok(messages)
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

/// Attempt to decrypt a ciphertext stored in a ChannelMessage row.
/// Returns decrypted content or None if decryption fails (graceful degradation).
fn try_decrypt_message(
    local_conn: &rusqlite::Connection,
    ciphertext: &str,
    conversation_id: &str,
    sender_id: &str,
) -> Option<String> {
    let msg: SenderKeyMessage = match serde_json::from_str(ciphertext) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("[decrypt] failed to parse ciphertext from {sender_id}: {e}");
            return None;
        }
    };

    let mut peer_state = match session::load_peer_sender_key(local_conn, conversation_id, sender_id) {
        Ok(Some(s)) => s,
        Ok(None) => {
            eprintln!("[decrypt] no peer sender key for {sender_id} on channel {conversation_id}");
            return None;
        }
        Err(e) => {
            eprintln!("[decrypt] error loading peer sender key for {sender_id}: {e}");
            return None;
        }
    };

    let plaintext = match peer_state.decrypt(&msg) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[decrypt] decryption failed for msg iteration={} from {sender_id} (peer state iteration={}): {e}", msg.iteration, peer_state.iteration);
            return None;
        }
    };

    // Save updated peer state after successful decryption
    let _ = session::save_peer_sender_key(local_conn, conversation_id, sender_id, &peer_state);

    String::from_utf8(plaintext).ok()
}

/// Fetch any sender key distributions from Turso that we haven't yet ingested
/// into local DB. Called before decrypting a channel's messages.
/// Skips senders whose peer key already exists locally (chain may have advanced).
///
/// To avoid holding a `&rusqlite::Connection` across `.await` points (which
/// would make the future non-`Send`), this function is split into two phases:
/// 1. Fetch all distribution rows from remote (async, no local DB reference held).
/// 2. Pass the collected rows to the caller for synchronous local DB writes.
async fn fetch_sender_key_distributions(
    remote_conn: &libsql::Connection,
    user_id: &str,
    channel_id: &str,
) -> Result<Vec<(String, SenderKeyState)>> {
    // Use the dedicated X25519 identity key (not the Ed25519 signing key).
    // users.identity_key stores the X25519 public key; x25519_ik_private is the matching secret.
    let our_ik_secret = match load_x25519_secret("x25519_ik_private").await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("[dist] x25519_ik_private not found for user {user_id}: {e}");
            return Ok(vec![]);
        }
    };

    let mut rows = remote_conn.query(
        "SELECT sender_id, encrypted_state, ephemeral_key, spk_id
         FROM sender_key_dist
         WHERE recipient_id = ?1 AND channel_id = ?2
         ORDER BY id DESC",
        libsql::params![user_id, channel_id],
    ).await?;

    let mut result = Vec::new();
    while let Some(row) = rows.next().await? {
        let sender_id: String = row.get(0)?;
        let encrypted_state: String = row.get(1)?;
        let ephemeral_key: String = row.get(2)?;
        let spk_id: i64 = row.get(3)?;

        let spk_key_name = format!("spk_{}", spk_id);
        let our_spk_secret = match load_x25519_secret(&spk_key_name).await {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[dist] SPK '{spk_key_name}' not found while decrypting dist from {sender_id}: {e}");
                continue;
            }
        };

        let state = match crypto::decrypt_sender_key_distribution(
            &encrypted_state,
            &ephemeral_key,
            &our_ik_secret,
            &our_spk_secret,
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("[dist] failed to decrypt sender key distribution from {sender_id} on channel {channel_id}: {e}");
                continue;
            }
        };

        eprintln!("[dist] ingested sender key from {sender_id} for channel {channel_id} (iteration={})", state.iteration);
        result.push((sender_id, state));
    }

    Ok(result)
}

/// Save fetched sender key distributions to local DB.
/// Distributions arrive newest-first (ORDER BY id DESC). For each sender:
/// - If no local key exists: save the distribution.
/// - If a local key exists with the same chain_id: skip (local state has ratcheted forward).
/// - If a local key exists with a DIFFERENT chain_id: the sender reset their key; update.
fn ingest_sender_key_distributions(
    local_conn: &rusqlite::Connection,
    channel_id: &str,
    distributions: Vec<(String, SenderKeyState)>,
) {
    // Track which senders have already been saved in this batch (first = newest wins).
    let mut saved: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (sender_id, state) in distributions {
        if saved.contains(&sender_id) {
            continue;
        }

        let existing = session::load_peer_sender_key(local_conn, channel_id, &sender_id)
            .ok()
            .flatten();

        match existing {
            None => {
                // No local state — save the distribution.
                let _ = session::save_peer_sender_key(local_conn, channel_id, &sender_id, &state);
                saved.insert(sender_id);
            }
            Some(local) if local.chain_id != state.chain_id => {
                // Sender reset their sender key (new chain_id) — update to the new distribution.
                let _ = session::save_peer_sender_key(local_conn, channel_id, &sender_id, &state);
                saved.insert(sender_id);
            }
            Some(_) => {
                // Same chain_id: local state has ratcheted forward via decryption — keep it.
                saved.insert(sender_id);
            }
        }
    }
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

    // Fetch sender key distributions from remote (no local DB reference held during await)
    let distributions = fetch_sender_key_distributions(&conn, &user_id, &channel_id).await
        .unwrap_or_default();

    // Ingest fetched distributions into local DB (synchronous, no await)
    let db = state.local_db.lock().await;
    ingest_sender_key_distributions(db.conn(), &channel_id, distributions);

    // Decrypt in oldest-first order so the ratchet chain advances correctly.
    // The SQL query returns newest-first; sort ascending here and reverse after.
    raw_messages.sort_by(|a, b| a.6.cmp(&b.6).then(a.0.cmp(&b.0)));

    let mut messages: Vec<ChannelMessage> = raw_messages.into_iter().map(|(id, conv_id, sender_id, sender_username, ciphertext, reply_to_id, sent_at)| {
        let content = if sender_id == user_id {
            // Own message: read plaintext we stored locally at send time
            db.conn().query_row(
                "SELECT content FROM message WHERE id = ?1",
                rusqlite::params![&id],
                |row| row.get::<_, Option<String>>(0),
            ).ok().flatten()
        } else {
            // Peer message: check local cache first so the ratchet doesn't need
            // to replay already-decrypted messages after a refresh.
            let cached: Option<String> = db.conn().query_row(
                "SELECT content FROM message WHERE id = ?1",
                rusqlite::params![&id],
                |row| row.get::<_, Option<String>>(0),
            ).ok().flatten();

            // Only treat as a cache hit if we actually have decrypted content.
            // A row with content=NULL means a prior decryption failed — retry.
            if let Some(text) = cached {
                Some(text)
            } else {
                let plaintext = try_decrypt_message(db.conn(), &ciphertext, &conv_id, &sender_id);
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
/// Mirrors get_channel_messages: ingests sender key distributions from remote,
/// decrypts peer messages, caches plaintext locally, returns newest-first.
#[tauri::command]
pub async fn get_dm_messages(
    user_id: String,
    dm_channel_id: String,
    limit: Option<i64>,
    cursor: Option<MessageCursor>,
    state: State<'_, Arc<AppState>>,
) -> Result<MessagePage> {
    let limit = limit.unwrap_or(50);
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

    let distributions = fetch_sender_key_distributions(&conn, &user_id, &dm_channel_id).await
        .unwrap_or_default();

    let db = state.local_db.lock().await;
    ingest_sender_key_distributions(db.conn(), &dm_channel_id, distributions);

    // Decrypt in oldest-first order so the ratchet chain advances correctly.
    raw_messages.sort_by(|a, b| a.6.cmp(&b.6).then(a.0.cmp(&b.0)));

    let mut messages: Vec<ChannelMessage> = raw_messages.into_iter().map(|(id, conv_id, sender_id, sender_username, ciphertext, reply_to_id, sent_at)| {
        let content = if sender_id == user_id {
            db.conn().query_row(
                "SELECT content FROM message WHERE id = ?1",
                rusqlite::params![&id],
                |row| row.get::<_, Option<String>>(0),
            ).ok().flatten()
        } else {
            let cached: Option<String> = db.conn().query_row(
                "SELECT content FROM message WHERE id = ?1",
                rusqlite::params![&id],
                |row| row.get::<_, Option<String>>(0),
            ).ok().flatten();

            if let Some(text) = cached {
                Some(text)
            } else {
                let plaintext = try_decrypt_message(db.conn(), &ciphertext, &conv_id, &sender_id);
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
    let db = state.local_db.lock().await;
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
            |row| Ok((row.get(0)?, row.get(5)?)),
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
            |row| Ok((row.get(0)?, row.get(5)?)),
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

#[cfg(test)]
mod encryption_tests {
    use rusqlite::Connection;
    use crate::signal::group::{SenderKeyState, SenderKeyMessage};
    use crate::signal::session;
    use crate::signal::crypto;
    use x25519_dalek::{StaticSecret, PublicKey as X25519PublicKey};
    use rand::rngs::OsRng;

    const LOCAL_SCHEMA: &str = include_str!("../db/migrations/local_schema.sql");
    const REMOTE_V001: &str = include_str!("../db/migrations/remote_schema.sql");

    fn local_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn.execute_batch("
            CREATE TABLE IF NOT EXISTS schema_migrations (
                version INTEGER PRIMARY KEY,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            );
        ").unwrap();
        conn.execute_batch(LOCAL_SCHEMA).unwrap();
        conn
    }

    fn remote_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
        conn.execute_batch(REMOTE_V001).unwrap();
        conn
    }

    fn make_keypair() -> (StaticSecret, X25519PublicKey) {
        let secret = StaticSecret::random_from_rng(OsRng);
        let public = X25519PublicKey::from(&secret);
        (secret, public)
    }

    #[test]
    fn sent_message_is_never_stored_as_plaintext() {
        let remote = remote_db();
        let plaintext = b"hello world";

        // Setup remote schema
        remote.execute("INSERT INTO users (id, email) VALUES ('alice', 'alice@x.com')", []).unwrap();
        remote.execute("INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'G', 'alice')", []).unwrap();
        remote.execute("INSERT INTO channels (id, group_id, name) VALUES ('ch1', 'g1', 'general')", []).unwrap();

        let mut state = SenderKeyState::new();
        let msg = state.encrypt(plaintext).expect("encrypt");
        let ciphertext_json = serde_json::to_string(&msg).expect("serialize");

        // Insert as the app would
        remote.execute(
            "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at)
             VALUES ('m1', 'ch1', 'alice', ?1, '2024-01-01T10:00:00Z')",
            rusqlite::params![ciphertext_json],
        ).unwrap();

        let stored: String = remote.query_row(
            "SELECT ciphertext FROM message_envelope WHERE id = 'm1'",
            [],
            |row| row.get(0),
        ).unwrap();

        // The stored value must not be the plaintext
        assert_ne!(stored.as_bytes(), plaintext);
        // The stored value must not contain "hello world" as a substring
        assert!(
            !stored.contains("hello world"),
            "stored ciphertext must not contain plaintext"
        );
    }

    #[test]
    fn encrypt_then_decrypt_roundtrip() {
        let local = local_db();
        let channel_id = "ch1";
        let sender_id = "alice";
        let plaintext = "hello world";

        let mut alice_state = SenderKeyState::new();
        let msg = alice_state.encrypt(plaintext.as_bytes()).expect("encrypt");

        // Save alice's state as peer state for bob's perspective
        session::save_peer_sender_key(&local, channel_id, sender_id, &alice_state)
            .expect("save peer state");

        let ciphertext_json = serde_json::to_string(&msg).expect("serialize");

        // Bob decrypts using saved peer state
        let loaded_state = session::load_peer_sender_key(&local, channel_id, sender_id)
            .expect("load peer state")
            .expect("state should exist");

        // Re-create alice's key at the same iteration
        let mut alice_state2 = SenderKeyState::new();
        // Use alice_state's values (same chain)
        alice_state2.chain_id = alice_state.chain_id.clone();
        alice_state2.iteration = 0; // before the message was sent
        alice_state2.chain_key = loaded_state.chain_key.clone();

        // Use loaded_state to decrypt
        let mut decrypt_state = loaded_state;
        // Roll back the iteration (loaded state is after encrypt)
        // Actually alice_state after encrypt is at iteration=1; need pre-encrypt state
        // So we save state BEFORE encrypting

        // Redo the test properly: save state before encrypt
        let local2 = local_db();
        let mut pre_state = SenderKeyState::new();
        session::save_peer_sender_key(&local2, channel_id, sender_id, &pre_state)
            .expect("save pre-encrypt state");

        let msg2 = pre_state.encrypt(plaintext.as_bytes()).expect("encrypt");
        let ciphertext_json2 = serde_json::to_string(&msg2).expect("serialize");

        // Load pre-encrypt state (this is what bob has)
        let mut bob_state = session::load_peer_sender_key(&local2, channel_id, sender_id)
            .expect("load").expect("exists");

        let decoded_msg: SenderKeyMessage = serde_json::from_str(&ciphertext_json2).expect("parse");
        let decrypted = bob_state.decrypt(&decoded_msg).expect("decrypt");

        assert_eq!(decrypted, plaintext.as_bytes());
        assert_eq!(String::from_utf8(decrypted).unwrap(), plaintext);
    }

    #[test]
    fn each_message_produces_different_ciphertext() {
        let mut state = SenderKeyState::new();
        let plaintext = b"hello world";

        let msg1 = state.encrypt(plaintext).expect("encrypt 1");
        let msg2 = state.encrypt(plaintext).expect("encrypt 2");

        assert_ne!(msg1.ciphertext, msg2.ciphertext, "each message must have unique ciphertext");
    }

    #[test]
    fn sender_key_distribution_bob_can_receive_alice_key() {
        let (alice_ik_secret, alice_ik_pub) = make_keypair();
        let (alice_spk_secret, alice_spk_pub) = make_keypair();
        let (bob_ik_secret, bob_ik_pub) = make_keypair();
        let (bob_spk_secret, bob_spk_pub) = make_keypair();

        let alice_state = SenderKeyState::new();
        let bob_ik_bytes: [u8; 32] = *bob_ik_pub.as_bytes();
        let bob_spk_bytes: [u8; 32] = *bob_spk_pub.as_bytes();

        // Alice distributes her sender key to Bob
        let (encrypted_hex, ephemeral_hex) = crypto::encrypt_sender_key_for_recipient(
            &alice_state,
            &bob_ik_bytes,
            &bob_spk_bytes,
        ).expect("alice encrypts sender key for bob");

        // Bob decrypts alice's sender key
        let recovered_state = crypto::decrypt_sender_key_distribution(
            &encrypted_hex,
            &ephemeral_hex,
            &bob_ik_secret,
            &bob_spk_secret,
        ).expect("bob decrypts alice's sender key");

        assert_eq!(alice_state.chain_id, recovered_state.chain_id);
        assert_eq!(alice_state.chain_key, recovered_state.chain_key);
        assert_eq!(alice_state.iteration, recovered_state.iteration);

        // Bob can now decrypt a message alice sends
        let mut alice_encrypt_state = alice_state.clone();
        let msg = alice_encrypt_state.encrypt(b"secret message").expect("alice encrypts");

        // Bob uses recovered state (at same iteration as alice_state before encryption)
        let mut bob_decrypt_state = recovered_state;
        let plaintext = bob_decrypt_state.decrypt(&msg).expect("bob decrypts");
        assert_eq!(plaintext, b"secret message");

        // Suppress unused variable warnings
        let _ = (alice_ik_secret, alice_ik_pub, alice_spk_secret, alice_spk_pub);
    }

    #[test]
    fn ratchet_advances_five_messages() {
        let local = local_db();
        let channel_id = "ch1";
        let alice_id = "alice";

        // Alice creates her sender key state
        let mut alice_state = SenderKeyState::new();

        // Save alice's state to bob's local DB (simulating distribution)
        session::save_peer_sender_key(&local, channel_id, alice_id, &alice_state)
            .expect("save alice's initial state for bob");

        // Alice encrypts 5 messages
        let plaintexts = ["msg 1", "msg 2", "msg 3", "msg 4", "msg 5"];
        let mut msgs = Vec::new();
        for text in &plaintexts {
            let msg = alice_state.encrypt(text.as_bytes()).expect("alice encrypts");
            msgs.push(msg);
        }

        // Bob decrypts all 5 in order using the saved state
        let mut bob_state = session::load_peer_sender_key(&local, channel_id, alice_id)
            .expect("load").expect("exists");

        for (i, msg) in msgs.iter().enumerate() {
            let plaintext = bob_state.decrypt(msg).expect("bob decrypts");
            assert_eq!(
                String::from_utf8(plaintext).unwrap(),
                plaintexts[i],
                "message {} should decrypt correctly", i + 1
            );
        }
    }
}
