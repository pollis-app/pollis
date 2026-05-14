use rusqlite::OptionalExtension;
use std::sync::Arc;
use ulid::Ulid;

use crate::error::Result;
use crate::state::AppState;

/// Delete a message.
///
/// Two paths, selected automatically by comparing `user_id` (the caller) to the
/// stored sender of the target message:
///
/// **Self-delete** (caller is the original sender): removes the envelope from
/// Turso (preventing future delivery to anyone who hasn't fetched it yet) and
/// removes the row from the sender's local message cache. Recipients who
/// already received the message keep it — no retroactive broadcast.
///
/// **Admin-delete** (caller is a different user, must be a group admin in the
/// channel's group): writes a `type='delete'` tombstone envelope to Turso so
/// every other member soft-deletes the message on their next ingest, also
/// removes the original message envelope and any pending edit, soft-deletes
/// the admin's own local row, and broadcasts a `deleted_message` realtime
/// event so currently-connected clients invalidate their cache immediately.
/// Admin-delete is rejected for DM messages (no admin concept in 1:1 DMs).
///
/// Attachment cleanup: if the message had one or more attachments, each
/// content_hash is reference-counted against the caller's other non-deleted
/// local messages. When no other local message references the same hash, the
/// `attachment_object` row is removed from Turso and the R2 object is deleted.
/// R2 deletion is best-effort and must not fail the overall delete; orphaned
/// R2 objects can be reclaimed by a future sweep.
pub async fn delete_message(
    message_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Resolve the message's original sender + conversation. Prefer the remote
    // envelope (authoritative across devices) but fall back to the local row
    // if Turso has already cleaned up the envelope by watermark/TTL.
    let (msg_sender_id, conversation_id): (String, String) = {
        let mut rows = conn.query(
            "SELECT sender_id, conversation_id FROM message_envelope
             WHERE id = ?1 AND type = 'message'",
            libsql::params![message_id.clone()],
        ).await?;
        if let Some(row) = rows.next().await? {
            (row.get::<String>(0)?, row.get::<String>(1)?)
        } else {
            let guard = state.local_db.lock().await;
            let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
            let local: Option<(String, String)> = db.conn()
                .query_row(
                    "SELECT sender_id, conversation_id FROM message WHERE id = ?1",
                    rusqlite::params![message_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            match local {
                Some(t) => t,
                None => {
                    return Err(crate::error::Error::Other(anyhow::anyhow!(
                        "Message not found"
                    )));
                }
            }
        }
    };

    let is_admin_delete = msg_sender_id != user_id;

    if is_admin_delete {
        // Admin path: caller must be an admin in the group that owns this
        // channel. DMs (no `channels` row) are not moderatable.
        let group_id: String = {
            let mut rows = conn.query(
                "SELECT group_id FROM channels WHERE id = ?1",
                libsql::params![conversation_id.clone()],
            ).await?;
            match rows.next().await? {
                Some(row) => row.get(0)?,
                None => {
                    return Err(crate::error::Error::Other(anyhow::anyhow!(
                        "only the sender can delete this message"
                    )));
                }
            }
        };

        let mut role_rows = conn.query(
            "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
            libsql::params![group_id.clone(), user_id.clone()],
        ).await?;
        let role: String = if let Some(row) = role_rows.next().await? {
            row.get(0)?
        } else {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "only the sender can delete this message"
            )));
        };
        if role != "admin" {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "only group admins can delete other members' messages"
            )));
        }

        // Remove the original message envelope and any pending edit so
        // late-joiners or unsynced devices never receive the now-deleted
        // content. Then write the tombstone envelope so every existing
        // member soft-deletes on next ingest.
        conn.execute(
            "DELETE FROM message_envelope WHERE id = ?1",
            libsql::params![message_id.clone()],
        ).await?;
        conn.execute(
            "DELETE FROM message_envelope WHERE target_message_id = ?1 AND type = 'edit'",
            libsql::params![message_id.clone()],
        ).await?;

        let tombstone_id = Ulid::new().to_string();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute(
            "INSERT INTO message_envelope
                 (id, conversation_id, sender_id, ciphertext, sent_at, type, target_message_id)
             VALUES (?1, ?2, ?3, '', ?4, 'delete', ?5)",
            libsql::params![tombstone_id, conversation_id.clone(), user_id.clone(), now.clone(), message_id.clone()],
        ).await?;

        // Soft-delete locally and collect orphaned attachments. The admin
        // may not have a local row (joined after the message was sent and
        // it's already aged out) — that's fine, the tombstone in Turso is
        // what propagates the delete.
        let orphaned: Vec<AttachmentRef> = {
            let guard = state.local_db.lock().await;
            let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

            let content: Option<String> = db.conn()
                .query_row(
                    "SELECT content FROM message WHERE id = ?1",
                    rusqlite::params![message_id],
                    |row| row.get(0),
                )
                .optional()?;

            db.conn().execute(
                "UPDATE message SET content = NULL, deleted_at = ?1
                 WHERE id = ?2 AND deleted_at IS NULL",
                rusqlite::params![now, message_id],
            )?;

            let raw = content.unwrap_or_default();
            let attachments = parse_attachment_refs(&raw);
            if attachments.is_empty() {
                Vec::new()
            } else {
                // Reference-count against every non-deleted local message,
                // not just the admin's own — if any local copy still
                // references the hash (e.g. another member also attached
                // the same file in a separate message they sent), keep R2.
                filter_orphaned_locally_all(db.conn(), &attachments)?
            }
        };

        for att in orphaned {
            cleanup_attachment(&state, &att).await;
        }

        // Broadcast so currently-connected clients invalidate their message
        // cache without waiting for a refetch. Non-fatal — ingest of the
        // tombstone envelope is the durable path.
        if let Err(e) = crate::commands::livekit::publish_deleted_message_to_room(
            &state.livekit,
            &group_id,
            Some(&conversation_id),
            None,
            &user_id,
            &message_id,
        ).await {
            eprintln!("[realtime] delete_message: publish to group {group_id}: {e}");
        }

        return Ok(());
    }

    // Self-delete path (caller is the original sender).
    //
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

    // Read the local plaintext content before deleting so we can inspect any
    // embedded attachment metadata. Then delete the local row and compute
    // which attachments are no longer referenced by any of this user's other
    // non-deleted messages. Done inside a single lock scope to avoid races
    // with concurrent sends.
    let orphaned: Vec<AttachmentRef> = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

        let content: Option<String> = db.conn()
            .query_row(
                "SELECT content FROM message WHERE id = ?1 AND sender_id = ?2",
                rusqlite::params![message_id, user_id],
                |row| row.get(0),
            )
            .optional()?;

        let rows_affected = db.conn().execute(
            "DELETE FROM message WHERE id = ?1 AND sender_id = ?2",
            rusqlite::params![message_id, user_id],
        )?;

        if rows_affected == 0 {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "Message not found or you are not the sender"
            )));
        }

        let raw = match content {
            Some(r) => r,
            None => String::new(),
        };
        let attachments = parse_attachment_refs(&raw);
        if attachments.is_empty() {
            Vec::new()
        } else {
            filter_orphaned_locally(db.conn(), &user_id, &attachments)?
        }
    };

    for att in orphaned {
        cleanup_attachment(&state, &att).await;
    }

    Ok(())
}

/// Attachment identifier extracted from a message's plaintext JSON payload.
#[derive(Debug, Clone)]
struct AttachmentRef {
    content_hash: String,
    r2_key: String,
}

/// Parse the `_att` array out of a message's local plaintext content and
/// return the (content_hash, r2_key) pairs. Returns an empty Vec for plain
/// text messages, malformed JSON, or any missing fields.
fn parse_attachment_refs(raw: &str) -> Vec<AttachmentRef> {
    if !raw.starts_with('{') {
        return Vec::new();
    }
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) else {
        return Vec::new();
    };
    let Some(atts) = parsed.get("_att").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    atts.iter()
        .filter_map(|a| {
            let hash = a.get("hash")?.as_str()?.to_string();
            let key = a.get("key")?.as_str()?.to_string();
            if hash.is_empty() || key.is_empty() {
                return None;
            }
            Some(AttachmentRef { content_hash: hash, r2_key: key })
        })
        .collect()
}

/// Return the subset of the given attachments that are not referenced by any
/// of the user's other non-deleted local messages. Scans the sender's local
/// message cache only — cross-user references are invisible because
/// attachment metadata lives inside the MLS-encrypted payload.
fn filter_orphaned_locally(
    conn: &rusqlite::Connection,
    user_id: &str,
    candidates: &[AttachmentRef],
) -> Result<Vec<AttachmentRef>> {
    let mut stmt = conn.prepare(
        "SELECT content FROM message
         WHERE sender_id = ?1 AND deleted_at IS NULL AND content IS NOT NULL",
    )?;
    let rows = stmt.query_map(rusqlite::params![user_id], |row| row.get::<_, String>(0))?;

    let mut still_referenced = std::collections::HashSet::<String>::new();
    for row in rows {
        let content = row?;
        for att in parse_attachment_refs(&content) {
            still_referenced.insert(att.content_hash);
        }
    }

    Ok(candidates
        .iter()
        .filter(|a| !still_referenced.contains(&a.content_hash))
        .cloned()
        .collect())
}

/// Same as `filter_orphaned_locally` but scans every non-deleted local
/// message regardless of sender. Used by admin-delete: the admin may
/// themselves have re-sent the same attachment, or someone else's message
/// in the admin's local cache may still reference the same hash. Cross-user
/// references on remote devices remain invisible (convergent encryption
/// re-creates the dedup row on re-upload, so a future re-share is safe).
fn filter_orphaned_locally_all(
    conn: &rusqlite::Connection,
    candidates: &[AttachmentRef],
) -> Result<Vec<AttachmentRef>> {
    let mut stmt = conn.prepare(
        "SELECT content FROM message
         WHERE deleted_at IS NULL AND content IS NOT NULL",
    )?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;

    let mut still_referenced = std::collections::HashSet::<String>::new();
    for row in rows {
        let content = row?;
        for att in parse_attachment_refs(&content) {
            still_referenced.insert(att.content_hash);
        }
    }

    Ok(candidates
        .iter()
        .filter(|a| !still_referenced.contains(&a.content_hash))
        .cloned()
        .collect())
}

/// Delete an attachment's Turso dedup row and its R2 object. Best-effort on
/// both: failures are logged, never bubbled. The attachment_object row is
/// removed first — if R2 deletion fails, a future re-upload will re-register
/// the row and overwrite the object, restoring a consistent state.
async fn cleanup_attachment(state: &AppState, att: &AttachmentRef) {
    let remote_result = async {
        let conn = state.remote_db.conn().await?;
        conn.execute(
            "DELETE FROM attachment_object WHERE content_hash = ?1",
            libsql::params![att.content_hash.clone()],
        ).await?;
        Ok::<_, crate::error::Error>(())
    }
    .await;

    if let Err(e) = remote_result {
        eprintln!(
            "[delete_message] failed to remove attachment_object for {}: {e}",
            att.content_hash
        );
    }

    if let Err(e) = crate::commands::r2::delete_r2_object(state, &att.r2_key).await {
        eprintln!(
            "[delete_message] failed to delete R2 object {} (hash {}): {e}",
            att.r2_key, att.content_hash
        );
    }
}

/// Edit a message: updates the sender's local plaintext cache immediately and
/// publishes an encrypted edit envelope to Turso so all other group members
/// receive the updated content on their next fetch.
///
/// The edit envelope uses type='edit' with target_message_id pointing at the
/// original message. A DELETE+INSERT replaces any prior pending edit, so Turso
/// never holds more than one edit per message per conversation.
pub async fn edit_message(
    conversation_id: String,
    message_id: String,
    user_id: String,
    new_content: String,
    state: &Arc<AppState>,
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
        crate::commands::mls::repair_mls_group(state, &mls_group_id, &user_id).await?;
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
