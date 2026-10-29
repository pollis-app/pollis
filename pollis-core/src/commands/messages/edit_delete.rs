use rusqlite::OptionalExtension;
use std::sync::Arc;
use ulid::Ulid;

use pollis_api::messages::DeleteMessageBody;

use crate::error::Result;
use crate::state::AppState;

/// The `POST /v1/messages/delete` request body.
///
/// One place for the self-delete and admin-delete call sites to agree which id
/// goes in which slot (both are `String`, so the type cannot check it).
/// `actor_id` is never a permission grant: on the signed path the DS requires
/// it to equal the authenticated user. `delete_token` (#1086) is `None` on the
/// admin branch.
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
/// already fetched the message, then the removal of its envelope (and any
/// pending edit) from the DS for those who have not. Deleting an envelope the
/// DS does not hold is a no-op, so this is also how a DISCARD withdraws an
/// unsent message whose post may have landed (#1335).
///
/// The redaction goes first, so there is no window where the original is gone
/// but no redaction is in flight. The removal proves authorship with the
/// deletion capability (#1086); `None` leaves the DS on its legacy membership
/// check.
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
/// Self-delete vs admin-delete is decided by comparing `user_id` to the author
/// in the LOCAL `message` row; the remote envelope's `sender_id` is the sealed
/// sentinel (#607) and never consulted for authorship.
///
/// - **Self-delete**: an E2EE redaction (verified against the target's author by
///   each recipient), removal of the envelope and any pending edit, a local
///   soft-delete and a realtime hint.
/// - **Admin-delete** (group admins only, never DMs): a server-authorized
///   `type='delete'` tombstone, since an admin cannot author an MLS message for
///   another member, plus the same removal, soft-delete and hint.
///
/// Attachment references are released through the DS (#690), which collects
/// the object only when no message anywhere references it. R2 deletion is best
/// effort and never fails the delete.
pub async fn delete_message(
    message_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // Deleting a message the outbox still holds (#1335) is discarding it.
    if super::outbox::is_unsent(state, &message_id).await? {
        super::outbox::discard_unsent_message(message_id, state).await?;
        return Ok(());
    }
    // Authorship must come from the LOCAL row: the remote envelope's
    // `sender_id` is always the sealed sentinel (#607), so it serves only as a
    // conversation_id fallback when this device holds no copy.
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
            // No local copy: only an admin-delete is possible. An empty author
            // forces the admin branch.
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
        // One request answers "is this a group channel" and "am I its admin".
        // DMs have no admin concept.
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
        // The resolved group id from the same answer that authorized the
        // delete, so the realtime nudge cannot address a different group.
        let group_id = role.group_id.clone();

        // One DS transaction removes the envelope and any pending edit and
        // writes the tombstone; the DS re-verifies admin authority. No
        // capability: an admin cannot compute another member's token by design.
        let body =
            delete_message_body(&message_id, &conversation_id, &msg_sender_id, &user_id, None);
        crate::commands::mls::ds_post_ok(state, &body).await?;

        // Display-only field, so it deliberately skips `envelope_sent_at`.
        let now = chrono::Utc::now().to_rfc3339();

        // The admin may hold no local row; the tombstone is what propagates.
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
                // Count references across every local message, not just the
                // admin's own. A cheap first pass; the DS is the authority.
                let orphaned = filter_orphaned_locally_all(db.conn(), &attachments)?;
                mark_locally_orphaned(attachments, &orphaned)
            }
        };

        for (att, locally_orphaned) in attachments {
            cleanup_attachment(state, &message_id, &att, locally_orphaned).await;
        }

        // Non-fatal: the tombstone envelope is the durable path.
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

    // Self-delete: "delete for everyone".
    retract_for_everyone(state, &conversation_id, &message_id, &user_id).await?;

    // Read the content before soft-deleting to find attachments. The sender
    // sees the same `[deleted]` placeholder recipients do. One lock scope to
    // avoid racing concurrent sends.
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

        // Display-only field, so it deliberately skips `envelope_sent_at`.
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

    // Non-fatal: the redaction envelope is the durable path.
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

/// Returns `(mls_group_id, is_channel)`: a channel's MLS group is its
/// `group_id`, a DM's is the conversation id.
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

/// Test-only: send a redaction as an arbitrary caller, bypassing the self-sender
/// gate, so the flows suite can prove a recipient rejects a redaction from a
/// non-author.
#[cfg(feature = "test-harness")]
pub async fn send_redaction_as(
    state: &Arc<AppState>,
    conversation_id: &str,
    target_message_id: &str,
    user_id: &str,
) -> Result<()> {
    send_redaction_message(state, conversation_id, target_message_id, user_id).await
}

/// Test-only: publish an edit as an arbitrary caller, bypassing the author gate,
/// so the flows suite can prove a recipient rejects an edit from a non-author
/// (#607).
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

    // "Is there a group?", not "can I encrypt?": a failed encrypt can also mean a
    // commit is staged (#1079), which must not trigger a rejoin.
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
        // #1079: `seal::seal_under_lock`. MLS lock before `local_db`, dropped
        // before the post's own catch-up can want it.
        let mls_guard = state.mls_group_lock(&mls_group_id).await;
        let held = crate::state::MlsLockHeld::from_guard(&mls_guard);
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        super::seal::seal_under_lock(db.conn(), &mls_group_id, conversation_id, &plaintext, &held)?
    };

    // Replacing a pending edit of the target is a delete, so it needs the
    // target's capability (#1086).
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

/// Send an E2EE "delete for everyone" redaction for `target_message_id`: a
/// `0xF6` frame ([`super::framing::pad_redaction`]) on the ordinary send path,
/// so it inherits encryption, watermarks, GC and offline delivery. A control
/// message: no `message` row on either side.
async fn send_redaction_message(
    state: &Arc<AppState>,
    conversation_id: &str,
    target_message_id: &str,
    user_id: &str,
) -> Result<()> {
    let envelope_id = Ulid::new().to_string();
    let (mls_group_id, _is_channel) = resolve_mls_group(state, conversation_id).await?;

    // Catch up before encrypting so the redaction is sealed at the current epoch
    // and no current-epoch inbound message is stranded (#440).
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

    // Repair a missing local group (a wiped local DB) via external join.
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
        // #1079: `seal::seal_under_lock`. MLS lock before `local_db`, dropped
        // before the post's own catch-up can want it.
        let mls_guard = state.mls_group_lock(&mls_group_id).await;
        let held = crate::state::MlsLockHeld::from_guard(&mls_guard);
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        super::seal::seal_under_lock(db.conn(), &mls_group_id, conversation_id, &plaintext, &held)?
    };

    // A capability like every envelope (#1086): deleting a redaction would stop
    // recipients ever applying it.
    let delete_token_hash =
        super::delete_capability::envelope_delete_token_hash(state, &envelope_id).await;

    // Sealed like a normal send (#607); recipients authorize the redaction from
    // the MLS credential inside the ciphertext.
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
            // A deletion wakes nobody (#987).
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

/// Parse the `_att` array out of a message's decrypted content. Empty for plain
/// text, malformed JSON, or missing fields.
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

/// True when `content` carries attachment refs rather than plain text. Size
/// padding (#331) applies to text only: an attachment's size is inherent and R2
/// dedup depends on it.
pub(crate) fn is_attachment_content(content: &str) -> bool {
    !parse_attachment_refs(content).is_empty()
}

/// Pair every attachment with whether the local ref-count flagged it orphaned.
/// The flag only decides whether to attempt the R2 delete; the reference
/// release runs regardless (#690).
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

/// The attachments no other non-deleted local message of this user references.
/// Cross-user references are invisible: attachment metadata is inside the
/// encrypted payload.
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

/// `filter_orphaned_locally` across every sender, for admin-delete. The DS
/// reference count is what actually gates collection.
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

/// Register a server-side reference for each attachment in `content`, keyed by
/// the message id (#690), so the DS never collects an object still referenced.
///
/// Best effort: never fails the send. Returns how many declarations failed, so
/// the backfill can tell a finished pass from one to retry.
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
    // The envelope is sealed, so the DS binds the declaration to its author
    // through the deletion capability; without it every declaration is refused.
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

/// Unlocks a backfill pass may take before it stops. A message whose envelope
/// is gone is refused for good, so a pass with failures cannot tell "DS not
/// deployed yet" from "nothing left to declare"; three tries covers the first
/// without retrying the second forever.
const ATTACHMENT_REF_BACKFILL_TRIES: u32 = 3;

/// Re-declare the attachment references of this account's own sent messages
/// (#1330), once per device, in the background after unlock. Idempotent on the
/// DS, so a sibling device repeating it is harmless.
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

/// Ask the DS to collect a deleted message's attachment object, and try the R2
/// delete when nothing local still references it. Best effort; failures are
/// logged.
///
/// The reference was already released when the envelope went (#690); the DS
/// collects only when no remaining message references the hash.
/// `locally_orphaned` only skips a pointless round trip: the `/v1/r2/presign`
/// gate is the authority, so a refused delete here is expected and benign.
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
        return;
    }

    if let Err(e) = crate::commands::r2::delete_r2_object(state, &att.r2_key).await {
        crate::diag!(
            "[delete_message] failed to delete R2 object {} (hash {}): {e}",
            att.r2_key, att.content_hash
        );
    }
}

/// Edit a message: update the sender's local copy and publish an encrypted
/// `type='edit'` envelope. The DS replaces any pending edit, so it holds at
/// most one per message.
pub async fn edit_message(
    conversation_id: String,
    message_id: String,
    user_id: String,
    new_content: String,
    state: &Arc<AppState>,
) -> Result<()> {
    // An unsent message (#1335) has no envelope for an edit to target.
    if super::outbox::is_unsent(state, &message_id).await? {
        return Err(crate::error::Error::Other(anyhow::anyhow!(
            "message has not been sent yet"
        )));
    }
    let envelope_id = Ulid::new().to_string();

    let (mls_group_id, _) = resolve_mls_group(state, &conversation_id).await?;

    // Catch up before encrypting, or the edit may be sealed at a stale epoch
    // (#371).
    {
        let device_id = state.device_id.lock().await.clone();
        if let Some(ref did) = device_id {
            if let Err(e) =
                crate::commands::mls::poll_mls_welcomes_inner(state, &user_id, did).await
            {
                crate::diag!("[messages] edit_message: poll_mls_welcomes for {mls_group_id}: {e}");
            }
        }
    }
    // The interleaved catch-up, not the commit-only replay: advancing to head
    // would otherwise strand an unfetched current-epoch message (#440). No MLS
    // lock is held here, so it cannot deadlock.
    if let Err(e) =
        super::catch_up_mls_group_interleaved(state, &mls_group_id, &user_id).await
    {
        crate::diag!("[messages] edit_message: catch_up_mls_group for {mls_group_id}: {e}");
    }

    let needs_repair = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        !crate::commands::mls::has_local_group(db.conn(), &mls_group_id)
    };

    if needs_repair {
        // Rejoin this device from GroupInfo. Never nuke the shared commit log to
        // repair one device (docs/mls-reconcile-hardening.md, INV-1/INV-4).
        crate::commands::mls::external_join_group(state, &mls_group_id, &user_id).await?;
    }

    // Size padding (#331) for text only, as on the send path.
    let plaintext: Vec<u8> = if is_attachment_content(&new_content) {
        new_content.as_bytes().to_vec()
    } else {
        super::framing::pad(new_content.as_bytes())
    };

    let sealed = {
        // #1079: `seal::seal_under_lock`. MLS lock before `local_db`, dropped
        // before the post's own catch-up can want it.
        let mls_guard = state.mls_group_lock(&mls_group_id).await;
        let held = crate::state::MlsLockHeld::from_guard(&mls_guard);
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

        let sealed = super::seal::seal_under_lock(
            db.conn(),
            &mls_group_id,
            &conversation_id,
            &plaintext,
            &held,
        )?;

        let rows_affected = db.conn().execute(
            "UPDATE message SET content = ?1, edited_at = ?2
             WHERE id = ?3 AND sender_id = ?4 AND deleted_at IS NULL",
            rusqlite::params![new_content, sealed.sent_at, message_id, user_id],
        )?;

        if rows_affected == 0 {
            return Err(crate::error::Error::Other(anyhow::anyhow!(
                "Message not found, already deleted, or you are not the sender"
            )));
        }

        sealed
    };

    // Replacing the pending edit is a delete, so it needs the target's
    // capability (#1086): one member cannot clobber another author's edit.
    let delete_token =
        super::delete_capability::envelope_delete_token(state, &message_id).await;

    // Re-sealed at the new epoch if a commit landed since the catch-up (#1041).
    super::seal::post_resealing(
        state,
        super::seal::Target {
            mls_group_id: &mls_group_id,
            conversation_id: &conversation_id,
            user_id: &user_id,
            plaintext: &plaintext,
            envelope_id: &envelope_id,
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
        |conn, sealed| {
            conn.execute(
                "UPDATE message SET edited_at = ?1 WHERE id = ?2",
                rusqlite::params![sealed.sent_at, message_id],
            )?;
            Ok(())
        },
    )
    .await?;

    // Non-fatal cache-invalidation hint.
    let is_channel = mls_group_id != conversation_id;
    let room_id = mls_group_id;
    if is_channel {
        if let Err(e) = crate::commands::livekit::publish_edited_message_to_room(
            &state.livekit,
            &room_id,
            Some(&conversation_id),
            None,
            &message_id,
        ).await {
            crate::diag!("[realtime] edit_message: publish to group {room_id}: {e}");
        }
    } else {
        if let Err(e) = crate::commands::livekit::publish_edited_message_to_room(
            &state.livekit,
            &room_id,
            None,
            Some(&conversation_id),
            &message_id,
        ).await {
            crate::diag!("[realtime] edit_message: publish to DM room {room_id}: {e}");
        }
    }

    Ok(())
}
