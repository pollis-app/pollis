use rusqlite::OptionalExtension;
use std::sync::Arc;
use ulid::Ulid;

use pollis_api::messages::DeleteMessageBody;

use crate::error::Result;
use crate::state::AppState;

/// The `POST /v1/messages/delete` body, built in one place so the self- and
/// admin-delete paths agree on which id goes in which `String` slot.
///
/// `actor_id` is the acting user when request signing is off; on the signed
/// path the DS requires it to equal the authenticated user, so it never widens
/// permission. `delete_token` (#1086) is `None` on the admin branch, which is a
/// re-derived permission rather than a claim of authorship.
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

/// The "for everyone" half of a self-delete: an E2EE redaction for members who
/// already fetched the message, then removal of its envelope and pending edit
/// from the DS. Also how a discard withdraws an unsent message whose post may
/// have landed (#1335).
///
/// The redaction goes first so no in-progress fetch sees the original vanish
/// without a redaction in flight. The removal proves authorship with the
/// deletion capability (#1086).
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

/// Delete a message: the author's E2EE redaction ("delete for everyone"), or a
/// group admin's server-authorized tombstone. Self vs admin is decided from the
/// LOCAL row's credential-authenticated sender, never the sealed envelope
/// (#607). Attachment references are released through the DS, which collects
/// an object only when no message references it (#690). See
/// `.codesight/wiki/mls.md` (message deletion).
pub async fn delete_message(
    message_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // Deleting a message the outbox still holds is discarding it (#1335).
    if super::outbox::is_unsent(state, &message_id).await? {
        super::outbox::discard_unsent_message(message_id, state).await?;
        return Ok(());
    }
    // Authorship comes from the local row; the remote `sender_id` is always the
    // sealed sentinel (#607) and would misroute every self-delete to admin.
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
            // No local copy: only an admin-delete is possible, so leave the
            // author empty to force the admin branch.
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
        // Admin path: the caller must be an admin of the channel's group; DMs
        // (`!role.exists`) are not moderatable.
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
        // The group id from the same answer that authorized the delete, so the
        // realtime nudge cannot address a different group.
        let group_id = role.group_id.clone();

        // The DS removes the envelope and pending edit and writes the tombstone
        // in one transaction, re-verifying admin authority itself. No
        // capability: an admin cannot compute another member's token by design.
        let body =
            delete_message_body(&message_id, &conversation_id, &msg_sender_id, &user_id, None);
        crate::commands::mls::ds_post_ok(state, &body).await?;

        // A display field, never compared against a cursor, so not
        // `envelope_sent_at`.
        let now = chrono::Utc::now().to_rfc3339();

        // The admin may hold no local row; the tombstone propagates the delete.
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
                let orphaned = filter_orphaned_locally_all(db.conn(), &attachments)?;
                mark_locally_orphaned(attachments, &orphaned)
            }
        };

        for (att, locally_orphaned) in attachments {
            cleanup_attachment(state, &message_id, &att, locally_orphaned).await;
        }

        // A hint only; ingesting the tombstone is the durable path.
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

    retract_for_everyone(state, &conversation_id, &message_id, &user_id).await?;

    // Soft-delete (not remove) so the sender sees the same `[deleted]`
    // placeholder as recipients; one lock scope so a concurrent send cannot
    // race the orphan count.
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

        // A display field, never compared against a cursor, so not
        // `envelope_sent_at`.
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

    for (att, locally_orphaned) in attachments {
        cleanup_attachment(state, &message_id, &att, locally_orphaned).await;
    }

    // A hint only; ingesting the redaction envelope is the durable path.
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

/// Returns `(mls_group_id, is_channel)` for a conversation.
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

/// Test-only: send a redaction as any caller, bypassing [`delete_message`]'s
/// author gate, to prove recipients reject a redaction from a non-author.
#[cfg(feature = "test-harness")]
pub async fn send_redaction_as(
    state: &Arc<AppState>,
    conversation_id: &str,
    target_message_id: &str,
    user_id: &str,
) -> Result<()> {
    send_redaction_message(state, conversation_id, target_message_id, user_id).await
}

/// Test-only: publish an edit as any caller, bypassing [`edit_message`]'s author
/// gate, to prove recipients reject an edit from a non-author (#607).
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

    // Asked as "is there a group?", not "can I encrypt?": a failed encrypt can
    // also mean a staged commit (#1079), which must not trigger a rejoin.
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
        // Group lock before `local_db`, dropped before the post's own catch-up
        // (#1079, `seal::seal_under_lock`).
        let mls_guard = state.mls_group_lock(&mls_group_id).await;
        let held = crate::state::MlsLockHeld::from_guard(&mls_guard);
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        super::seal::seal_under_lock(db.conn(), &mls_group_id, conversation_id, &plaintext, &held)?
    };

    // Replacing a pending edit is a delete, so it needs the capability (#1086).
    let delete_token =
        super::delete_capability::envelope_delete_token(state, target_message_id).await;

    // Edit envelopes are not sealed; the recipient's author check reads the MLS
    // credential, not `sender_id`.
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

/// Send an E2EE redaction ([`super::framing::pad_redaction`]) as an ordinary
/// message envelope. Recipients apply it only if its MLS-authenticated author
/// is the target's author; no local `message` row is written for it.
async fn send_redaction_message(
    state: &Arc<AppState>,
    conversation_id: &str,
    target_message_id: &str,
    user_id: &str,
) -> Result<()> {
    let envelope_id = Ulid::new().to_string();
    let (mls_group_id, _is_channel) = resolve_mls_group(state, conversation_id).await?;

    // Catch up first: seal at the current epoch, and do not strand an
    // un-ingested current-epoch message when this device advances (#440).
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
        // Group lock before `local_db`, dropped before the post's own catch-up
        // (#1079, `seal::seal_under_lock`).
        let mls_guard = state.mls_group_lock(&mls_group_id).await;
        let held = crate::state::MlsLockHeld::from_guard(&mls_guard);
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        super::seal::seal_under_lock(db.conn(), &mls_group_id, conversation_id, &plaintext, &held)?
    };

    // Deleting a redaction would undo it, so it carries a capability too (#1086).
    let delete_token_hash =
        super::delete_capability::envelope_delete_token_hash(state, &envelope_id).await;

    // Sealed like every send (#607); recipients authorize on the MLS credential.
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

/// The `_att` refs in a message's decrypted content; empty for plain text or
/// anything malformed.
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

/// True when `content` is an attachment envelope, which is sent unpadded
/// (#331, see `framing`).
pub(crate) fn is_attachment_content(content: &str) -> bool {
    !parse_attachment_refs(content).is_empty()
}

/// Pair each attachment with whether the local pass found it orphaned. The flag
/// only decides whether to attempt the R2 delete (#690).
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

/// The candidates no other non-deleted local message of `user_id` references.
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

/// [`filter_orphaned_locally`] over every sender (admin-delete). Only a cheap
/// first pass; the DS reference count gates collection.
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

/// Declare a DS reference for each attachment in `content` (#690) so the object
/// is not collected while this message exists. Best-effort, never fails the
/// send; returns how many declarations failed.
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
    // The envelope is sealed, so the DS binds the declaration to its author by
    // the deletion capability; without it every declaration is refused.
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

/// A message whose envelope is gone is refused for good, so a pass cannot tell
/// "DS not deployed yet" from "nothing left"; a few tries covers the first.
const ATTACHMENT_REF_BACKFILL_TRIES: u32 = 3;

/// Re-declare the attachment references of this account's own sent messages,
/// once per device, in the background after unlock (#1330). Idempotent on the
/// DS.
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

/// Ask the DS to collect a deleted message's attachment row, and when
/// `locally_orphaned` also try the R2 delete. Best-effort; failures are logged.
///
/// The reference was already released when the envelope was deleted (#690).
/// The DS collects only when no message references the hash, and its presign
/// gate refuses an R2 delete while one does, so a logged refusal is benign.
async fn cleanup_attachment(
    state: &Arc<AppState>,
    message_id: &str,
    att: &AttachmentRef,
    locally_orphaned: bool,
) {
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
        // The DS gate would refuse the R2 delete anyway.
        return;
    }

    if let Err(e) = crate::commands::r2::delete_r2_object(state, &att.r2_key).await {
        crate::diag!(
            "[delete_message] failed to delete R2 object {} (hash {}): {e}",
            att.r2_key, att.content_hash
        );
    }
}

/// Edit a message through the edit outbox (#1555): stored first, the `message`
/// row updated only once the DS holds the edit ([`super::edit_outbox`]). A
/// failed attempt stays queued and the error is returned.
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
    // Test harness: die with the edit stored and nothing posted (#1555).
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

    // Not a member: no retry can help, so the outbox records `rejected`.
    let resolved = crate::commands::ds_reads::catch_up(state, conversation_id, false, false).await?;
    if !resolved.authorized {
        return Err(super::outbox::not_a_member());
    }
    let mls_group_id = resolved.mls_group_id;

    // Catch up first or the edit may be sealed at a stale epoch (#371).
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
    // The interleaved (ingesting) catch-up, not commit-only, so advancing does
    // not strand an unfetched current-epoch message (#440). No group lock is
    // held here, so it cannot deadlock.
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
        // Rejoin this device via GroupInfo; never repair by touching the shared
        // commit log (docs/mls-reconcile-hardening.md INV-1/INV-4).
        crate::commands::mls::external_join_group(state, &mls_group_id, user_id).await?;
    }

    let plaintext: Vec<u8> = if is_attachment_content(new_content) {
        new_content.as_bytes().to_vec()
    } else {
        super::framing::pad(new_content.as_bytes())
    };

    let sealed = {
        // Group lock before `local_db`, dropped before the post's own catch-up
        // (#1079, `seal::seal_under_lock`).
        let mls_guard = state.mls_group_lock(&mls_group_id).await;
        let held = crate::state::MlsLockHeld::from_guard(&mls_guard);
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        super::seal::seal_under_lock(db.conn(), &mls_group_id, conversation_id, &plaintext, &held)?
    };

    // Replacing a pending edit is a delete, so it needs the capability (#1086);
    // this is what stops a member clobbering another author's unfetched edit.
    let delete_token =
        super::delete_capability::envelope_delete_token(state, message_id).await;

    // The DS replaces any pending edit, including this envelope if an earlier
    // attempt landed unseen; re-sealed if a commit landed since catch-up (#1041).
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

    // Test harness: the DS holds the edit but this device has not recorded it.
    if super::send::test_hooks::take(super::send::test_hooks::Crash::EditAfterPost) {
        return Err(super::send::test_hooks::simulated_crash());
    }

    {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        super::edit_outbox::confirm_edit_on(db.conn(), edit, &sealed.sent_at)?;
    }

    // A hint only; errors are logged.
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
