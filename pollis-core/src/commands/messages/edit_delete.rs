use rusqlite::OptionalExtension;
use std::sync::Arc;
use ulid::Ulid;

use pollis_api::messages::DeleteMessageBody;

use crate::error::Result;
use crate::state::AppState;

/// The `POST /v1/messages/delete` request body.
///
/// The DS reads `actor_id` as the actor when signing is off; on the signed path it must equal the signer.
/// `delete_token` (#1086) is `None` on the admin branch, which is a re-derived permission.
pub fn delete_message_body(
    message_id: &str,
    conversation_id: &str,
    msg_sender_id: &str,
    actor_id: &str,
    delete_token: Option<String>,
) -> DeleteMessageBody {
    DeleteMessageBody {
        message_id: message_id.to_string(),
        conversation_id: conversation_id.to_string(),
        msg_sender_id: Some(msg_sender_id.to_string()),
        actor_id: Some(actor_id.to_string()),
        delete_token,
    }
}

/// Redact for members who fetched, then remove the envelope for those who haven't (also discard's path, #1335).
///
/// The redaction goes first so no fetch sees the original gone without one; removal presents the token (#1086).
pub(super) async fn retract_for_everyone(
    state: &Arc<AppState>,
    conversation_id: &str,
    message_id: &str,
    user_id: &str,
) -> Result<()> {
    send_redaction_message(state, conversation_id, message_id, user_id).await?;
    let delete_token = super::delete_capability::envelope_delete_token(state, message_id).await;
    let body = delete_message_body(message_id, conversation_id, user_id, user_id, delete_token);
    crate::commands::mls::ds_post_ok(state, &body).await
}

/// Delete a message.
///
/// Self- vs admin-delete is chosen by the LOCAL row's author; the envelope's `sender_id` is sealed (#607).
///
/// **Self-delete**: E2EE redaction (recipients verify the author), envelope removal, local soft-delete, realtime hint.
///
/// **Admin-delete** (group admins, not DMs): a plaintext tombstone, since an admin can't author for another member.
///
/// Attachment refs are released via the DS (#690), which collects an object only when nothing references it.
pub async fn delete_message(
    message_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // Deleting an unsent message (#1335) is discarding it.
    if super::outbox::is_unsent(state, &message_id).await? {
        super::outbox::discard_unsent_message(message_id, state).await?;
        return Ok(());
    }
    // Authorship comes from the LOCAL row; the envelope's `sender_id` is always the sealed sentinel (#607).
    let local_row: Option<(String, String)> = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        db.conn()
            .query_row(
                "SELECT sender_id, conversation_id FROM message WHERE id = ?1",
                rusqlite::params![message_id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
    };

    let (msg_sender_id, conversation_id): (String, String) = match local_row {
        Some(t) => t,
        None => {
            // No local copy: only an admin-delete is possible, so leave the author empty.
            match crate::commands::ds_reads::message_conversation(state, &message_id).await? {
                Some(cid) => (String::new(), cid),
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
        // Admin path: one request resolves the channel's group and the caller's role. DMs are not moderatable.
        let role =
            crate::commands::ds_reads::group_role(state, &conversation_id, &user_id).await?;
        if !role.exists {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "only the sender can delete this message"
            )));
        }
        if !role.authorized {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                crate::commands::groups::authz::NOT_A_MEMBER
            )));
        }
        if role.role.as_deref() != Some("admin") {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "only group admins can delete other members' messages"
            )));
        }
        // The resolved group id, so the nudge addresses the group the check ran against.
        let group_id = role.group_id.clone();

        // One DS transaction removes the envelope and pending edit and writes the tombstone; no capability (admin role).
        let body =
            delete_message_body(&message_id, &conversation_id, &msg_sender_id, &user_id, None);
        crate::commands::mls::ds_post_ok(state, &body).await?;

        // Display-only field, so not `envelope_sent_at`.
        let now = chrono::Utc::now().to_rfc3339();

        // Soft-delete locally; the admin may hold no row, and the tombstone propagates anyway.
        let attachments: Vec<(AttachmentRef, bool)> = {
            let guard = state.local_db.lock().await;
            let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

            let content: Option<String> = db.conn()
                .query_row(
                    "SELECT content FROM message WHERE id = ?1",
                    rusqlite::params![message_id],
                    |row| row.get(0),
                )
                .optional()?;

            db.conn().execute(super::outbox::SOFT_DELETE_SQL, rusqlite::params![now, message_id])?;

            let raw = content.unwrap_or_default();
            let attachments = parse_attachment_refs(&raw);
            if attachments.is_empty() {
                Vec::new()
            } else {
                // Ref-count against every local message, not just the admin's; a cheap first pass only.
                let orphaned = filter_orphaned_locally_all(db.conn(), &attachments)?;
                mark_locally_orphaned(attachments, &orphaned)
            }
        };

        // Release EVERY attachment's reference for the deleted message; the DS
        // collects the shared object only when no reference remains.
        for (att, locally_orphaned) in attachments {
            cleanup_attachment(state, &message_id, &att, locally_orphaned).await;
        }

        // Broadcast for cache invalidation; the tombstone is the durable path.
        if let Err(e) = crate::commands::livekit::publish_deleted_message_to_room(
            &state.livekit,
            &group_id,
            Some(&conversation_id),
            None,
            &message_id,
        ).await {
            crate::diag!("[realtime] delete_message: publish to group {group_id}: {e}");
        }

        return Ok(());
    }

    // Self-delete: redaction first, then envelope removal (see `retract_for_everyone`).
    retract_for_everyone(state, &conversation_id, &message_id, &user_id).await?;

    // Soft-delete the local row (the `[deleted]` placeholder recipients see) and find orphans, under one lock.
    let attachments: Vec<(AttachmentRef, bool)> = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

        let content: Option<String> = db.conn()
            .query_row(
                "SELECT content FROM message WHERE id = ?1 AND sender_id = ?2",
                rusqlite::params![message_id, user_id],
                |row| row.get(0),
            )
            .optional()?;

        // Display-only field, so not `envelope_sent_at`.
        let now = chrono::Utc::now().to_rfc3339();
        let rows_affected = db.conn().execute(
            "UPDATE message SET content = NULL, deleted_at = ?1
             WHERE id = ?2 AND sender_id = ?3 AND deleted_at IS NULL",
            rusqlite::params![now, message_id, user_id],
        )?;

        if rows_affected == 0 {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "Message not found or you are not the sender"
            )));
        }

        let raw = content.unwrap_or_default();
        let attachments = parse_attachment_refs(&raw);
        if attachments.is_empty() {
            Vec::new()
        } else {
            let orphaned = filter_orphaned_locally(db.conn(), &user_id, &attachments)?;
            mark_locally_orphaned(attachments, &orphaned)
        }
    };

    // Release every attachment's reference; the DS collects only unreferenced objects.
    for (att, locally_orphaned) in attachments {
        cleanup_attachment(state, &message_id, &att, locally_orphaned).await;
    }

    // Broadcast so connected members apply the redaction now; the envelope is the durable path.
    let (mls_group_id, is_channel) = resolve_mls_group(state, &conversation_id).await?;
    let (channel_arg, dm_arg) = if is_channel {
        (Some(conversation_id.as_str()), None)
    } else {
        (None, Some(conversation_id.as_str()))
    };
    if let Err(e) = crate::commands::livekit::publish_deleted_message_to_room(
        &state.livekit,
        &mls_group_id,
        channel_arg,
        dm_arg,
        &message_id,
    ).await {
        crate::diag!("[realtime] delete_message: publish to room {mls_group_id}: {e}");
    }

    Ok(())
}

/// Resolve the MLS group backing a conversation: a channel maps to its
/// `group_id` (all channels in a group share one MLS group); a DM's MLS group is
/// keyed by the conversation id itself. Returns `(mls_group_id, is_channel)`.
async fn resolve_mls_group(
    state: &Arc<AppState>,
    conversation_id: &str,
) -> Result<(String, bool)> {
    let resolved = crate::commands::ds_reads::catch_up(state, conversation_id, false, false).await?;
    Ok((
        resolved.mls_group_id,
        resolved.kind == pollis_api::directory::ConversationKind::Channel,
    ))
}

/// Test-only: redact as an arbitrary caller, so flows can prove recipients reject a non-author's redaction.
#[cfg(feature = "test-harness")]
pub async fn send_redaction_as(
    state: &Arc<AppState>,
    conversation_id: &str,
    target_message_id: &str,
    user_id: &str,
) -> Result<()> {
    send_redaction_message(state, conversation_id, target_message_id, user_id).await
}

/// Test-only: post an edit as an arbitrary caller, so flows can prove recipients reject a non-author's edit (#607).
#[cfg(feature = "test-harness")]
pub async fn edit_message_as(
    state: &Arc<AppState>,
    conversation_id: &str,
    target_message_id: &str,
    user_id: &str,
    new_content: &str,
) -> Result<()> {
    let envelope_id = Ulid::new().to_string();
    let (mls_group_id, _is_channel) = resolve_mls_group(state, conversation_id).await?;

    // Catch up so the edit is sealed at the current epoch.
    {
        let device_id = state.device_id.lock().await.clone();
        if let Some(ref did) = device_id {
            if let Err(e) =
                crate::commands::mls::poll_mls_welcomes_inner(state, user_id, did).await
            {
                crate::diag!("[messages] edit_message_as: poll_mls_welcomes for {mls_group_id}: {e}");
            }
        }
    }
    if let Err(e) =
        super::catch_up_mls_group_interleaved(state, &mls_group_id, user_id).await
    {
        crate::diag!("[messages] edit_message_as: catch_up_mls_group for {mls_group_id}: {e}");
    }

    // Ask "is there a group?": a failed encrypt can mean a staged commit (#1079), which must not rejoin.
    let needs_repair = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        !crate::commands::mls::has_local_group(db.conn(), &mls_group_id)
    };
    if needs_repair {
        crate::commands::mls::external_join_group(state, &mls_group_id, user_id).await?;
    }

    let plaintext = super::framing::pad(new_content.as_bytes());
    let sealed = {
        // #1079: serialize behind any committer before sealing (see `seal::seal_under_lock`).
        let mls_guard = state.mls_group_lock(&mls_group_id).await?;
        let held = crate::state::MlsLockHeld::from_guard(&mls_guard);
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        super::seal::seal_under_lock(db.conn(), &mls_group_id, conversation_id, &plaintext, &held)?
    };

    // The target's deletion capability (#1086) — this write replaces any pending
    // edit of the target, which is a delete.
    let delete_token =
        super::delete_capability::envelope_delete_token(state, target_message_id).await;

    // Edit envelopes are not sealed; the recipient's author check reads the MLS credential.
    super::seal::post_resealing(
        state,
        super::seal::Target {
            mls_group_id: &mls_group_id,
            conversation_id,
            user_id,
            plaintext: &plaintext,
            envelope_id: &envelope_id,
        },
        sealed,
        |sealed| pollis_api::messages::EditMessageBody {
            envelope_id: envelope_id.clone(),
            conversation_id: conversation_id.to_string(),
            target_message_id: target_message_id.to_string(),
            sender_id: Some(user_id.to_string()),
            ciphertext: sealed.wire(),
            sent_at: sealed.sent_at.clone(),
            generation: Some(sealed.generation),
            epoch: Some(sealed.epoch),
            delete_token: delete_token.clone(),
        },
        |_, _| Ok(()),
    )
    .await?;

    Ok(())
}

/// Send an E2EE "delete for everyone" redaction (`0xF6` frame) as an ordinary envelope; no local row either side.
async fn send_redaction_message(
    state: &Arc<AppState>,
    conversation_id: &str,
    target_message_id: &str,
    user_id: &str,
) -> Result<()> {
    let envelope_id = Ulid::new().to_string();
    let (mls_group_id, _is_channel) = resolve_mls_group(state, conversation_id).await?;

    // Catch up before encrypting (#440), as send and edit do.
    {
        let device_id = state.device_id.lock().await.clone();
        if let Some(ref did) = device_id {
            if let Err(e) =
                crate::commands::mls::poll_mls_welcomes_inner(state, user_id, did).await
            {
                crate::diag!("[messages] send_redaction: poll_mls_welcomes for {mls_group_id}: {e}");
            }
        }
    }
    if let Err(e) =
        super::catch_up_mls_group_interleaved(state, &mls_group_id, user_id).await
    {
        crate::diag!("[messages] send_redaction: catch_up_mls_group for {mls_group_id}: {e}");
    }

    // Repair a missing local group via external join, as `edit_message` does.
    let needs_repair = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        !crate::commands::mls::has_local_group(db.conn(), &mls_group_id)
    };
    if needs_repair {
        crate::commands::mls::external_join_group(state, &mls_group_id, user_id).await?;
    }

    let plaintext = super::framing::pad_redaction(target_message_id);
    let sealed = {
        // #1079: serialize behind any committer before sealing (see `seal::seal_under_lock`).
        let mls_guard = state.mls_group_lock(&mls_group_id).await?;
        let held = crate::state::MlsLockHeld::from_guard(&mls_guard);
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        super::seal::seal_under_lock(db.conn(), &mls_group_id, conversation_id, &plaintext, &held)?
    };

    // A deletion capability like every envelope (#1086).
    let delete_token_hash =
        super::delete_capability::envelope_delete_token_hash(state, &envelope_id).await;

    // Sealed sender is unconditional (#607); recipients authorize by the MLS credential.
    super::seal::post_resealing(
        state,
        super::seal::Target {
            mls_group_id: &mls_group_id,
            conversation_id,
            user_id,
            plaintext: &plaintext,
            envelope_id: &envelope_id,
        },
        sealed,
        |sealed| pollis_api::messages::SendMessageBody {
            id: envelope_id.clone(),
            conversation_id: conversation_id.to_string(),
            sender_id: Some(super::send::SEALED_SENDER_SENTINEL.to_string()),
            ciphertext: sealed.wire(),
            // A redaction is not a reply.
            reply_to_id: None,
            sent_at: sealed.sent_at.clone(),
            sealed: 1,
            generation: Some(sealed.generation),
            epoch: Some(sealed.epoch),
            // A redaction wakes nobody (#987).
            push_to: None,
            push_mentions: None,
            delete_token_hash: delete_token_hash.clone(),
        },
        |_, _| Ok(()),
    )
    .await?;

    Ok(())
}

/// Attachment identifier extracted from a message's plaintext JSON payload.
#[derive(Debug, Clone)]
pub(crate) struct AttachmentRef {
    pub(crate) content_hash: String,
    pub(crate) r2_key: String,
}

/// Parse the `_att` array out of a message's local decrypted content and
/// return the (content_hash, r2_key) pairs. Returns an empty Vec for plain
/// text messages, malformed JSON, or any missing fields.
pub(crate) fn parse_attachment_refs(raw: &str) -> Vec<AttachmentRef> {
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

/// True when `content` carries a valid `_att` array; attachment envelopes stay unpadded.
pub(crate) fn is_attachment_content(content: &str) -> bool {
    !parse_attachment_refs(content).is_empty()
}

/// Pair each attachment with its local orphaned flag, which only gates the R2 delete attempt (#690).
fn mark_locally_orphaned(
    all: Vec<AttachmentRef>,
    orphaned: &[AttachmentRef],
) -> Vec<(AttachmentRef, bool)> {
    let orphaned_hashes: std::collections::HashSet<&str> =
        orphaned.iter().map(|a| a.content_hash.as_str()).collect();
    all.into_iter()
        .map(|att| {
            let is_orphaned = orphaned_hashes.contains(att.content_hash.as_str());
            (att, is_orphaned)
        })
        .collect()
}

/// The attachments none of the user's other non-deleted local messages reference.
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

/// As `filter_orphaned_locally`, but across every sender (admin-delete); the DS count still decides.
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

/// Register a DS reference for each attachment in `content`, keyed by message id (#690).
///
/// Best-effort: never fails the send.
///
/// Returns how many declarations failed, so the backfill below can tell a
/// finished pass from one to try again.
pub(crate) async fn register_attachment_refs(
    state: &Arc<AppState>,
    message_id: &str,
    content: &str,
) -> usize {
    let refs = parse_attachment_refs(content);
    if refs.is_empty() {
        return 0;
    }
    let mut failed = 0;
    // Sealed envelopes name no one, so the DS binds the declaration via the deletion capability.
    let delete_token = super::delete_capability::envelope_delete_token(state, message_id).await;
    for att in refs {
        let body = pollis_api::messages::AttachmentRegisterBody::ForMessage {
            content_hash: att.content_hash.clone(),
            r2_key: att.r2_key,
            message_id: message_id.to_string(),
            delete_token: delete_token.clone(),
        };
        if let Err(e) = crate::commands::mls::ds_post_ok(state, &body).await {
            failed += 1;
            crate::diag!(
                "[send_message] failed to register attachment ref for {} (msg {message_id}): {e}",
                att.content_hash
            );
        }
    }
    failed
}

const ATTACHMENT_REF_BACKFILL_KEY: &str = "attachment_ref_backfill";

/// Unlock passes a backfill may take; a gone envelope is refused for good, so retries are capped.
const ATTACHMENT_REF_BACKFILL_TRIES: u32 = 3;

/// Re-declare the attachment references of this account's own sent messages
/// (#1330), once per device, after unlock.
///
/// Sends before #1330 had their declarations refused; idempotent, background, best-effort.
pub(crate) fn spawn_attachment_ref_backfill(state: &Arc<AppState>, user_id: &str) {
    let state = state.clone();
    let user_id = user_id.to_string();
    tokio::spawn(async move {
        let pending: Vec<(String, String)> = {
            let guard = state.local_db.lock().await;
            let Some(db) = guard.as_ref() else {
                return;
            };
            let tries: Option<String> = match db
                .conn()
                .query_row(
                    "SELECT value FROM kv WHERE key = ?1",
                    rusqlite::params![ATTACHMENT_REF_BACKFILL_KEY],
                    |row| row.get(0),
                )
                .optional()
            {
                Ok(v) => v,
                Err(e) => {
                    crate::diag!("[attachments] backfill state unreadable (non-fatal): {e}");
                    return;
                }
            };
            let tries = match tries.as_deref() {
                Some("done") => return,
                Some(n) => n.parse::<u32>().unwrap_or(ATTACHMENT_REF_BACKFILL_TRIES),
                None => 0,
            };
            if tries >= ATTACHMENT_REF_BACKFILL_TRIES {
                return;
            }
            if let Err(e) = db.conn().execute(
                "INSERT OR REPLACE INTO kv (key, value) VALUES (?1, ?2)",
                rusqlite::params![ATTACHMENT_REF_BACKFILL_KEY, (tries + 1).to_string()],
            ) {
                crate::diag!("[attachments] backfill state unwritable (non-fatal): {e}");
                return;
            }
            let mut stmt = match db.conn().prepare(
                "SELECT id, content FROM message \
                 WHERE sender_id = ?1 AND deleted_at IS NULL AND send_state IS NULL \
                   AND content IS NOT NULL AND content LIKE '{%\"_att\"%'",
            ) {
                Ok(s) => s,
                Err(e) => {
                    crate::diag!("[attachments] backfill query failed (non-fatal): {e}");
                    return;
                }
            };
            let rows = stmt.query_map(rusqlite::params![user_id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            });
            match rows {
                Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
                Err(e) => {
                    crate::diag!("[attachments] backfill query failed (non-fatal): {e}");
                    return;
                }
            }
        };

        let mut failed = 0;
        for (message_id, content) in &pending {
            failed += register_attachment_refs(&state, message_id, content).await;
        }
        crate::diag!(
            "[attachments] backfill declared {} message(s), {failed} refused",
            pending.len()
        );
        if failed == 0 {
            let guard = state.local_db.lock().await;
            if let Some(db) = guard.as_ref() {
                if let Err(e) = db.conn().execute(
                    "INSERT OR REPLACE INTO kv (key, value) VALUES (?1, 'done')",
                    rusqlite::params![ATTACHMENT_REF_BACKFILL_KEY],
                ) {
                    crate::diag!("[attachments] backfill state unwritable (non-fatal): {e}");
                }
            }
        }
    });
}

/// Trigger server-side collection of a deleted message's attachment, and — when
/// the client's local view suggests it may now be orphaned — try to collect the
/// R2 object. Best-effort on both: failures are logged, never bubbled.
///
/// The reference is already released; this asks the DS to collect the row if nothing references the hash.
///
/// `locally_orphaned` only saves a round trip; the DS presign gate is the authority.
async fn cleanup_attachment(
    state: &Arc<AppState>,
    message_id: &str,
    att: &AttachmentRef,
    locally_orphaned: bool,
) {
    // Ask the DS to conditionally collect the shared row. Best-effort.
    let remote_result = async {
        let body = pollis_api::messages::AttachmentDeleteBody {
            content_hash: att.content_hash.clone(),
            message_id: Some(message_id.to_string()),
        };
        crate::commands::mls::ds_post_ok(state, &body).await
    }
    .await;

    if let Err(e) = remote_result {
        crate::diag!(
            "[delete_message] failed to release attachment ref for {} (msg {message_id}): {e}",
            att.content_hash
        );
    }

    if !locally_orphaned {
        // Still referenced by another local message — the DS gate would refuse
        // the R2 delete anyway, so skip the round trip.
        return;
    }

    if let Err(e) = crate::commands::r2::delete_r2_object(state, &att.r2_key).await {
        crate::diag!(
            "[delete_message] failed to delete R2 object {} (hash {}): {e}",
            att.r2_key, att.content_hash
        );
    }
}

/// Edit a message through the edit outbox (#1555).
///
/// Stored first; the row takes the new text only once the DS holds the envelope. Failures stay queued.
///
/// A type='edit' envelope; the DS replaces any prior pending edit.
pub async fn edit_message(
    conversation_id: String,
    message_id: String,
    user_id: String,
    new_content: String,
    state: &Arc<AppState>,
) -> Result<()> {
    {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        super::edit_outbox::enqueue_edit_on(
            db.conn(),
            &conversation_id,
            &message_id,
            &user_id,
            &new_content,
        )?;
    }
    // Test-harness only: the process dies with the edit stored and nothing
    // posted (#1555) — the drain after restart must deliver it everywhere.
    if super::send::test_hooks::take(super::send::test_hooks::Crash::EditAfterEnqueue) {
        return Err(super::send::test_hooks::simulated_crash());
    }
    super::edit_outbox::deliver_edit(state, &message_id).await
}

/// Seal and post one stored edit, then confirm it locally — the network half
/// of an edit, run under the target's outbox lock by
/// [`super::edit_outbox::deliver_edit`].
pub(super) async fn post_edit(
    state: &Arc<AppState>,
    edit: &super::edit_outbox::QueuedEdit,
) -> Result<()> {
    state.check_not_outdated()?;
    let super::edit_outbox::QueuedEdit {
        target_message_id: message_id,
        envelope_id,
        conversation_id,
        sender_id: user_id,
        content: new_content,
    } = edit;

    // Not (or no longer) a member: the outbox records `rejected`.
    let resolved = crate::commands::ds_reads::catch_up(state, conversation_id, false, false).await?;
    if !resolved.authorized {
        return Err(super::outbox::not_a_member());
    }
    let mls_group_id = resolved.mls_group_id;

    // Catch up before encrypting, or the edit seals at a stale epoch (#371).
    {
        let device_id = state.device_id.lock().await.clone();
        if let Some(ref did) = device_id {
            if let Err(e) =
                crate::commands::mls::poll_mls_welcomes_inner(state, user_id, did).await
            {
                crate::diag!("[messages] edit_message: poll_mls_welcomes for {mls_group_id}: {e}");
            }
        }
    }
    // Interleaved catch-up (#440), so no inbound message is stranded; no lock held here.
    if let Err(e) =
        super::catch_up_mls_group_interleaved(state, &mls_group_id, user_id).await
    {
        crate::diag!("[messages] edit_message: catch_up_mls_group for {mls_group_id}: {e}");
    }

    let needs_repair = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        !crate::commands::mls::has_local_group(db.conn(), &mls_group_id)
    };
    if needs_repair {
        // Rejoin via external join; never nuke the shared commit log (docs/mls-reconcile-hardening.md).
        crate::commands::mls::external_join_group(state, &mls_group_id, user_id).await?;
    }

    // Size padding (issue #331 v2, §4.1) — same scheme as the send path:
    // pad TEXT edits to a size bucket; leave attachment edits unpadded.
    let plaintext: Vec<u8> = if is_attachment_content(new_content) {
        new_content.as_bytes().to_vec()
    } else {
        super::framing::pad(new_content.as_bytes())
    };

    let sealed = {
        // #1079: serialize behind any committer before sealing (see `seal::seal_under_lock`).
        let mls_guard = state.mls_group_lock(&mls_group_id).await?;
        let held = crate::state::MlsLockHeld::from_guard(&mls_guard);
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        super::seal::seal_under_lock(db.conn(), &mls_group_id, conversation_id, &plaintext, &held)?
    };

    // Replacing the pending edit is a delete, so it needs the target's capability (#1086).
    let delete_token =
        super::delete_capability::envelope_delete_token(state, message_id).await;

    // Replace any existing edit envelope (one DS transaction), re-sealing if a commit landed (#1041).
    let sealed = super::seal::post_resealing(
        state,
        super::seal::Target {
            mls_group_id: &mls_group_id,
            conversation_id,
            user_id,
            plaintext: &plaintext,
            envelope_id,
        },
        sealed,
        |sealed| pollis_api::messages::EditMessageBody {
            envelope_id: envelope_id.clone(),
            conversation_id: conversation_id.clone(),
            target_message_id: message_id.clone(),
            sender_id: Some(user_id.clone()),
            ciphertext: sealed.wire(),
            sent_at: sealed.sent_at.clone(),
            generation: Some(sealed.generation),
            epoch: Some(sealed.epoch),
            delete_token: delete_token.clone(),
        },
        |_, _| Ok(()),
    )
    .await?;

    // Test-harness only: the DS holds the edit and this device has not
    // recorded that (#1555) — the retry must converge, not fork.
    if super::send::test_hooks::take(super::send::test_hooks::Crash::EditAfterPost) {
        return Err(super::send::test_hooks::simulated_crash());
    }

    // Delivered: the row takes the new text now, and only now.
    {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        super::edit_outbox::confirm_edit_on(db.conn(), edit, &sealed.sent_at)?;
    }

    // Notify via LiveKit; non-fatal.
    let is_channel = mls_group_id != *conversation_id;
    let room_id = mls_group_id;
    if is_channel {
        if let Err(e) = crate::commands::livekit::publish_edited_message_to_room(
            &state.livekit,
            &room_id,
            Some(conversation_id),
            None,
            message_id,
        ).await {
            crate::diag!("[realtime] edit_message: publish to group {room_id}: {e}");
        }
    } else if let Err(e) = crate::commands::livekit::publish_edited_message_to_room(
        &state.livekit,
        &room_id,
        None,
        Some(conversation_id),
        message_id,
    ).await {
        crate::diag!("[realtime] edit_message: publish to DM room {room_id}: {e}");
    }

    Ok(())
}
