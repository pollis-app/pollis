use std::sync::Arc;
use ulid::Ulid;

use crate::error::Result;
use crate::state::AppState;

use super::types::Message;

pub async fn send_message(
    conversation_id: String,
    sender_id: String,
    content: String,
    reply_to_id: Option<String>,
    sender_username: Option<String>,
    state: &Arc<AppState>,
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

    // Block enforcement for DMs: if any other participant in this DM
    // has a block relationship with the sender (either direction),
    // silently drop the message. The send appears to succeed to the
    // sender (message is stored locally so their own history looks
    // consistent) but it is NOT encrypted, NOT posted to Turso, and
    // NOT broadcast on LiveKit — the recipient never sees it and no
    // observable signal reveals the block. Group channels are not
    // gated here; blocks in groups are purely render-side on the
    // blocker's client.
    let suppress_delivery = if !is_channel {
        let conn = state.remote_db.conn().await?;
        let mut rows = conn.query(
            "SELECT user_id
             FROM dm_channel_member
             WHERE dm_channel_id = ?1
               AND user_id <> ?2",
            libsql::params![conversation_id.clone(), sender_id.clone()],
        ).await?;
        let mut blocked = false;
        while let Some(row) = rows.next().await? {
            let other: String = row.get(0)?;
            if crate::commands::blocks::is_blocked_either_way(&conn, &sender_id, &other).await? {
                blocked = true;
                break;
            }
        }
        blocked
    } else {
        false
    };

    if suppress_delivery {
        // Write a local-only row so the sender's own conversation
        // view stays consistent (history survives reloads). Empty
        // ciphertext is fine — nothing will ever decrypt this row;
        // it's only read back via the `content` column.
        {
            let guard = state.local_db.lock().await;
            let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(
                anyhow::anyhow!("Not signed in")
            ))?;
            let empty: Vec<u8> = Vec::new();
            db.conn().execute(
                "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, reply_to_id, sent_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                rusqlite::params![id, conversation_id, sender_id, empty, content, reply_to_id, now],
            )?;
        }
        return Ok(Message {
            id,
            conversation_id,
            sender_id,
            content: Some(content),
            reply_to_id,
            sent_at: now,
        });
    }

    // Poll MLS Welcomes — this device may have been added to the group but
    // hasn't applied the Welcome yet.
    {
        let device_id = state.device_id.lock().await.clone();
        if let Some(ref did) = device_id {
            if let Err(e) = crate::commands::mls::poll_mls_welcomes_inner(state, &sender_id, did).await {
                eprintln!("[messages] send_message: poll_mls_welcomes for {mls_group_id}: {e}");
            }
        }
    }

    // Ensure this device has a local MLS group at the current epoch.
    // Processes pending commits; falls back to external-join if needed.
    if let Err(e) = crate::commands::mls::process_pending_commits_inner(state, &mls_group_id, &sender_id).await {
        eprintln!("[messages] send_message: process_pending_commits for {mls_group_id}: {e}");
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

    // @all mention: group messages don't raise OS notifications for every
    // new message, but an explicit `@all` pings every group member's inbox so
    // they get one. Per-user "notifications off" is enforced client-side in
    // notify.ts. Inbox publish (one per member) is fire-and-forget; failures
    // are logged, never fatal to the send. Only meaningful for group channels.
    if is_channel && mentions_all(&content) {
        let member_ids: Vec<String> = {
            let conn = state.remote_db.conn().await?;
            let mut rows = conn.query(
                "SELECT user_id FROM group_member WHERE group_id = ?1 AND user_id <> ?2",
                libsql::params![mls_group_id.clone(), sender_id.clone()],
            ).await?;
            let mut ids = Vec::new();
            while let Some(row) = rows.next().await? {
                ids.push(row.get::<String>(0)?);
            }
            ids
        };
        let payload = serde_json::json!({
            "type": "all_mention",
            "group_id": mls_group_id,
            "channel_id": conversation_id,
            "sender_id": sender_id,
            "sender_username": sender_username,
        });
        for uid in member_ids {
            if let Err(e) = crate::commands::livekit::publish_to_user_inbox(
                &state.config,
                &uid,
                payload.clone(),
            ).await {
                eprintln!("[realtime] send_message: @all inbox publish to {uid}: {e}");
            }
        }
    }

    // Content-free push to recipients' backgrounded/closed apps (#344).
    // Fire-and-forget: a push relay hiccup must never block or fail the send,
    // and foreground recipients already got the LiveKit realtime ping above.
    // Desktop runs this too (its users just have no registered tokens), which
    // is what lets a desktop-sent message wake a recipient's phone.
    //
    // Notification policy mirrors desktop: a DM always notifies its recipient,
    // but a group channel message only notifies on an explicit `@all` (regular
    // channel chatter would be far too noisy — desktop raises no per-message
    // notification for it either; see the @all inbox-ping branch above).
    let should_push = !is_channel || mentions_all(&content);
    if should_push {
        let state = Arc::clone(state);
        let conversation_id = conversation_id.clone();
        let mls_group_id = mls_group_id.clone();
        let sender_id = sender_id.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::commands::push::notify_new_message(
                &conversation_id,
                &mls_group_id,
                is_channel,
                &sender_id,
                &state,
            )
            .await
            {
                eprintln!("[push] send_message notify: {e}");
            }
        });
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

/// True when `content` contains an `@all` mention as a standalone token —
/// i.e. whitespace-delimited and ignoring trailing punctuation, so "@all" and
/// "@all," match but "@allison" and "email@allcorp" do not. Case-insensitive.
fn mentions_all(content: &str) -> bool {
    content.split_whitespace().any(|w| {
        w.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '@')
            .eq_ignore_ascii_case("@all")
    })
}
