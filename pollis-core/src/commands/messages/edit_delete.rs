use rusqlite::OptionalExtension;
use std::sync::Arc;
use ulid::Ulid;

use pollis_api::messages::DeleteMessageBody;

use crate::error::Result;
use crate::state::AppState;

/// The `POST /v1/messages/delete` request body.
///
/// The DS reads `actor_id` as the acting user whenever request signing is off
/// (`pollis_delivery::writes::resolve_actor`: auth on → the signed user, auth off
/// → this field, missing → `Forbidden`). Omitting it made the endpoint return a
/// hard 403 on every no-auth deployment while the authenticated path looked
/// healthy.
///
/// Since #875 the OMISSION is caught by the type: [`DeleteMessageBody`] is the
/// same struct `pollis-delivery` parses, and a Rust struct literal must name
/// every field, so a call site that forgets `actor_id` does not compile. This
/// builder survives that change because it still gives the two call sites
/// (self-delete and admin-delete) one place to agree on which id goes in which
/// slot — a thing the type cannot check, since both are `String`.
///
/// `actor_id` is NOT a permission grant on the signed path: `resolve_actor`
/// requires it to equal the authenticated user and rejects otherwise, so sending
/// it can never widen what the caller may do.
pub fn delete_message_body(
    message_id: &str,
    conversation_id: &str,
    msg_sender_id: &str,
    actor_id: &str,
) -> DeleteMessageBody {
    DeleteMessageBody {
        message_id: message_id.to_string(),
        conversation_id: conversation_id.to_string(),
        msg_sender_id: Some(msg_sender_id.to_string()),
        actor_id: Some(actor_id.to_string()),
    }
}

/// Delete a message.
///
/// Two paths, selected automatically by comparing `user_id` (the caller) to the
/// target message's author as recorded in the LOCAL `message` row (the
/// credential-authenticated sender). The remote envelope's `sender_id` is now the
/// sealed sentinel (#607) and is never consulted for authorship:
///
/// **Self-delete** (caller is the original sender) — "delete for everyone":
/// sends an **E2EE redaction control message** (an MLS application message whose
/// plaintext is a `0xF6` framing frame carrying the target id, indistinguishable
/// on the wire from a short text message) so every member who already received
/// the message soft-deletes it on their next ingest — but only after each
/// recipient verifies the redaction's MLS-authenticated author matches the
/// target message's author (`ingest::decrypt_and_persist_one`), so neither the
/// server nor another member can redact a message they did not author. Also
/// removes the original envelope + any pending edit from Turso (so a recipient
/// who has NOT fetched yet never receives it, and the ciphertext does not linger
/// at rest), soft-deletes the sender's own local row, and broadcasts a
/// `deleted_message` realtime event for immediate cache invalidation. The
/// durable path is the redaction envelope; the realtime ping is only a hint.
///
/// **Admin-delete** (caller is a different user, must be a group admin in the
/// channel's group): writes a `type='delete'` tombstone envelope to Turso so
/// every other member soft-deletes the message on their next ingest, also
/// removes the original message envelope and any pending edit, soft-deletes
/// the admin's own local row, and broadcasts a `deleted_message` realtime
/// event so currently-connected clients invalidate their cache immediately.
/// Admin-delete is server-authorized (moderation) and uses the plaintext
/// tombstone because an admin generally cannot author an MLS message on behalf
/// of another member. Admin-delete is rejected for DM messages (no admin
/// concept in 1:1 DMs).
///
/// Attachment cleanup: if the message had one or more attachments, each
/// content_hash's reference is RELEASED server-side through the DS (#690). The
/// DS reference-counts the shared, convergent `attachment_object` row and
/// collects it — and permits the R2 object's deletion — only when NO message
/// anywhere still references the hash, so a delete in one conversation can no
/// longer strand a copy in another. The local ref-count pass survives as a cheap
/// first pass (skip the R2 round trip when this device still references the hash),
/// but the client no longer assumes its local view is complete: the server-side
/// count is the authority. R2 deletion is best-effort and must not fail the
/// overall delete; orphaned R2 objects can be reclaimed by a future sweep.
pub async fn delete_message(
    message_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Resolve the message's AUTHOR + conversation. Authorship MUST come from the
    // LOCAL `message` row — its `sender_id` is the credential-authenticated author
    // (written from the MLS credential at ingest, or self-attributed at send).
    // The remote `message_envelope.sender_id` is now ALWAYS the sealed sentinel
    // (#607), so it can no longer reveal who authored the message; it serves only
    // as a fallback for the conversation_id when this device holds no local copy
    // (an admin deleting a message they never fetched). Deriving self-vs-admin
    // from the sealed envelope would misroute EVERY self-delete to the admin path.
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
            // No local copy: this can only be an admin-delete (a self-delete
            // requires you authored — and therefore hold — the message). Take the
            // conversation from the remote envelope and leave the author unknown
            // (empty), which forces the admin branch below.
            let mut rows = conn.query(
                "SELECT conversation_id FROM message_envelope
                 WHERE id = ?1 AND type = 'message'",
                libsql::params![message_id.clone()],
            ).await?;
            match rows.next().await? {
                Some(row) => (String::new(), row.get::<String>(0)?),
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

        // #875: non-membership used to answer "only the sender can delete this
        // message" — the same string as the DM case above, which is a different
        // condition. A DM genuinely has no admin concept; being outside the
        // group is being outside the group, and the shared preflight says so.
        crate::commands::groups::authz::require_admin(
            &conn,
            &group_id,
            &user_id,
            "delete other members' messages",
        )
        .await?;

        // Remove the original message envelope and any pending edit so
        // late-joiners or unsynced devices never receive the now-deleted
        // content. Then write the tombstone envelope so every existing
        // member soft-deletes on next ingest. The server re-verifies admin
        // authority (it must not trust the client's branch choice). DS seam:
        // route the whole 3-write op (one transaction server-side) through the
        // Delivery Service.
        let body = delete_message_body(&message_id, &conversation_id, &msg_sender_id, &user_id);
        crate::commands::mls::ds_post_ok(state, &body).await?;

        // Local `message.deleted_at` only — a display/presence field, never
        // compared lexically against a cursor, so it deliberately does NOT go
        // through `envelope_sent_at` (which exists for the watermark-ordered
        // `message_envelope.sent_at` column).
        let now = chrono::Utc::now().to_rfc3339();

        // Soft-delete locally and collect orphaned attachments. The admin
        // may not have a local row (joined after the message was sent and
        // it's already aged out) — that's fine, the tombstone in Turso is
        // what propagates the delete.
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
                // This is a cheap first pass; the DS is the authority.
                let orphaned = filter_orphaned_locally_all(db.conn(), &attachments)?;
                mark_locally_orphaned(attachments, &orphaned)
            }
        };

        // Release EVERY attachment's reference for the deleted message; the DS
        // collects the shared object only when no reference remains.
        for (att, locally_orphaned) in attachments {
            cleanup_attachment(state, &message_id, &att, locally_orphaned).await;
        }

        // Broadcast so currently-connected clients invalidate their message
        // cache without waiting for a refetch. Non-fatal — ingest of the
        // tombstone envelope is the durable path.
        if let Err(e) = crate::commands::livekit::publish_deleted_message_to_room(
            &state.livekit,
            &group_id,
            Some(&conversation_id),
            None,
            &message_id,
        ).await {
            eprintln!("[realtime] delete_message: publish to group {group_id}: {e}");
        }

        return Ok(());
    }

    // Self-delete path (caller is the original sender) — "delete for everyone".
    //
    // Two facets, both needed:
    //   1. Send an E2EE redaction control message so members who ALREADY fetched
    //      the message soft-delete their local copy (verified author-side — see
    //      `send_redaction_message`).
    //   2. Remove the original envelope + any pending edit from Turso so a member
    //      who has NOT fetched yet never receives it and the ciphertext does not
    //      linger at rest.
    // The redaction is sent FIRST so there is no window where the original is
    // gone but no redaction is in flight for an in-progress fetch.
    send_redaction_message(state, &conversation_id, &message_id, &user_id).await?;

    // Remove the original envelope (best-effort — may already be GC'd) and any
    // pending edit. DS seam: route both deletes (one transaction server-side,
    // scoped to the authenticated sender) through the Delivery Service.
    let body = delete_message_body(&message_id, &conversation_id, &user_id, &user_id);
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Read the local plaintext content before soft-deleting so we can inspect
    // any embedded attachment metadata. Then SOFT-delete the local row (content
    // cleared, `deleted_at` set) — the sender sees the same `[deleted]`
    // placeholder every recipient does, rather than the message silently
    // vanishing only for them. Compute which attachments are no longer
    // referenced by any of this user's other non-deleted messages. Done inside a
    // single lock scope to avoid races with concurrent sends.
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

        // Local `message.deleted_at` only — a display/presence field, never
        // compared lexically against a cursor, so it deliberately does NOT go
        // through `envelope_sent_at` (which exists for the watermark-ordered
        // `message_envelope.sent_at` column).
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

        let raw = match content {
            Some(r) => r,
            None => String::new(),
        };
        let attachments = parse_attachment_refs(&raw);
        if attachments.is_empty() {
            Vec::new()
        } else {
            let orphaned = filter_orphaned_locally(db.conn(), &user_id, &attachments)?;
            mark_locally_orphaned(attachments, &orphaned)
        }
    };

    // Release EVERY attachment's reference for the deleted message; the DS
    // collects the shared object (and permits the R2 delete) only when no
    // reference remains.
    for (att, locally_orphaned) in attachments {
        cleanup_attachment(state, &message_id, &att, locally_orphaned).await;
    }

    // Broadcast so currently-connected members invalidate their cache and apply
    // the redaction immediately, without waiting for a poll. Non-fatal — ingest
    // of the redaction envelope is the durable path.
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
        eprintln!("[realtime] delete_message: publish to room {mls_group_id}: {e}");
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
    let conn = state.remote_db.conn().await?;
    let mut rows = conn.query(
        "SELECT group_id FROM channels WHERE id = ?1",
        libsql::params![conversation_id.to_string()],
    ).await?;
    Ok(match rows.next().await? {
        Some(row) => (row.get::<String>(0)?, true),
        None => (conversation_id.to_string(), false),
    })
}

/// Test-only: send a redaction as an ARBITRARY caller, bypassing the
/// self-sender gate in [`delete_message`]. Lets the flows suite prove the
/// security invariant that a recipient rejects a redaction whose
/// MLS-authenticated author is NOT the target message's author (a member cannot
/// redact another member's message). Not reachable in production — the real
/// `delete_message` routes a non-author to the admin-authorized path.
#[cfg(feature = "test-harness")]
pub async fn send_redaction_as(
    state: &Arc<AppState>,
    conversation_id: &str,
    target_message_id: &str,
    user_id: &str,
) -> Result<()> {
    send_redaction_message(state, conversation_id, target_message_id, user_id).await
}

/// Test-only: publish an edit envelope for `target_message_id` as an ARBITRARY
/// caller, bypassing the author self-gate in [`edit_message`] (which only edits a
/// message the caller sent and has locally). Lets the flows suite prove the
/// client-side edit-author enforcement (#607): a recipient must REJECT an edit
/// whose MLS-authenticated author is NOT the target message's author. Not
/// reachable in production — the real `edit_message` edits only the caller's own
/// message, and the DS now membership-gates (no longer author-gates) the write,
/// so this models a member forging an edit for someone else's message.
#[cfg(feature = "test-harness")]
pub async fn edit_message_as(
    state: &Arc<AppState>,
    conversation_id: &str,
    target_message_id: &str,
    user_id: &str,
    new_content: &str,
) -> Result<()> {
    let envelope_id = Ulid::new().to_string();
    let now = super::envelope_sent_at();
    let (mls_group_id, _is_channel) = resolve_mls_group(state, conversation_id).await?;

    // Catch up (welcomes + interleaved) so the edit is sealed at the current
    // epoch — identical pre-op catch-up to `edit_message` / `send_redaction_message`.
    {
        let device_id = state.device_id.lock().await.clone();
        if let Some(ref did) = device_id {
            if let Err(e) =
                crate::commands::mls::poll_mls_welcomes_inner(state, user_id, did).await
            {
                eprintln!("[messages] edit_message_as: poll_mls_welcomes for {mls_group_id}: {e}");
            }
        }
    }
    if let Err(e) =
        super::catch_up_mls_group_interleaved(state, &mls_group_id, user_id).await
    {
        eprintln!("[messages] edit_message_as: catch_up_mls_group for {mls_group_id}: {e}");
    }

    // Encrypt the padded new content, repairing the local group if it is missing.
    let needs_repair = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        crate::commands::mls::try_mls_encrypt(
            db.conn(),
            &mls_group_id,
            &super::framing::pad(new_content.as_bytes()),
        )
        .is_none()
    };
    if needs_repair {
        crate::commands::mls::external_join_group(state, &mls_group_id, user_id).await?;
    }

    let ciphertext_remote = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        let plaintext = super::framing::pad(new_content.as_bytes());
        let mls_bytes = crate::commands::mls::try_mls_encrypt(db.conn(), &mls_group_id, &plaintext)
            .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!(
                "MLS group not initialized for conversation {conversation_id}"
            )))?;
        format!("mls:{}", hex::encode(&mls_bytes))
    };

    // Edit envelopes are NOT sealed (the DS membership-gates the write on the
    // authenticated writer, so `sender_id` binds to the caller); the recipient's
    // author check reads the MLS credential, not this column.
    let body = pollis_api::messages::EditMessageBody {
        envelope_id,
        conversation_id: conversation_id.to_string(),
        target_message_id: target_message_id.to_string(),
        sender_id: Some(user_id.to_string()),
        ciphertext: ciphertext_remote,
        sent_at: now,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

/// Send an E2EE "delete for everyone" redaction for `target_message_id`: an MLS
/// application message whose plaintext is a `0xF6` redaction frame (see
/// [`super::framing::pad_redaction`]). It rides the ordinary send path — a
/// `type='message'` envelope on `/v1/messages/send` — so it inherits MLS
/// encryption, per-conversation watermarks, envelope GC, and offline delivery
/// with no schema, DS, or migration change. Recipients recognise the frame in
/// [`super::ingest`] and soft-delete the target only if the redaction's
/// MLS-authenticated author matches the target's author. It is a control
/// message: no local `message` row is written for it on either side.
async fn send_redaction_message(
    state: &Arc<AppState>,
    conversation_id: &str,
    target_message_id: &str,
    user_id: &str,
) -> Result<()> {
    let envelope_id = Ulid::new().to_string();
    let now = super::envelope_sent_at();
    let (mls_group_id, _is_channel) = resolve_mls_group(state, conversation_id).await?;

    // Catch up MLS before encrypting so the redaction is sealed at the current
    // epoch (recipients at head can decrypt it) and so an un-ingested
    // current-epoch inbound message is not stranded when this device advances
    // (issue #440) — identical to the send/edit pre-op catch-up.
    {
        let device_id = state.device_id.lock().await.clone();
        if let Some(ref did) = device_id {
            if let Err(e) =
                crate::commands::mls::poll_mls_welcomes_inner(state, user_id, did).await
            {
                eprintln!("[messages] send_redaction: poll_mls_welcomes for {mls_group_id}: {e}");
            }
        }
    }
    if let Err(e) =
        super::catch_up_mls_group_interleaved(state, &mls_group_id, user_id).await
    {
        eprintln!("[messages] send_redaction: catch_up_mls_group for {mls_group_id}: {e}");
    }

    // Encrypt the redaction frame. Repair the local group via external-join if
    // it is missing (a wiped local DB), mirroring `edit_message`.
    let needs_repair = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        crate::commands::mls::try_mls_encrypt(
            db.conn(),
            &mls_group_id,
            &super::framing::pad_redaction(target_message_id),
        )
        .is_none()
    };
    if needs_repair {
        crate::commands::mls::external_join_group(state, &mls_group_id, user_id).await?;
    }

    let ciphertext_remote = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        let plaintext = super::framing::pad_redaction(target_message_id);
        let mls_bytes = crate::commands::mls::try_mls_encrypt(db.conn(), &mls_group_id, &plaintext)
            .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!(
                "MLS group not initialized for conversation {conversation_id}"
            )))?;
        format!("mls:{}", hex::encode(&mls_bytes))
    };

    // Blind the envelope sender exactly like a normal send — sealing is
    // UNCONDITIONAL (#607). The true author is the MLS credential inside the
    // ciphertext, which is what the recipient's redaction-authorization check
    // reads; the stored `sender_id` is always the non-identifying sentinel.
    let body = pollis_api::messages::SendMessageBody {
        id: envelope_id,
        conversation_id: conversation_id.to_string(),
        sender_id: Some(super::send::SEALED_SENDER_SENTINEL.to_string()),
        ciphertext: ciphertext_remote,
        // A redaction is not a reply.
        reply_to_id: None,
        sent_at: now,
        sealed: 1,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    Ok(())
}

/// Attachment identifier extracted from a message's plaintext JSON payload.
#[derive(Debug, Clone)]
pub(crate) struct AttachmentRef {
    pub(crate) content_hash: String,
    pub(crate) r2_key: String,
}

/// Parse the `_att` array out of a message's local plaintext content and
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

/// True when a message's plaintext `content` carries attachment refs (a valid
/// `_att` array) rather than being plain text. Size-padding (issue #331 v2,
/// `docs/metadata-minimization-design.md` §4.1) is scoped to TEXT envelopes
/// only — an attachment blob's size is inherent and R2 dedup depends on it — so
/// the send/edit path leaves attachment envelopes unpadded.
pub(crate) fn is_attachment_content(content: &str) -> bool {
    !parse_attachment_refs(content).is_empty()
}

/// Pair every attachment with whether the local ref-count pass flagged it
/// orphaned. `orphaned` is the subset returned by `filter_orphaned_locally*`
/// (attachments no remaining local message references); membership is matched on
/// `content_hash`. The flag only decides whether it is worth ATTEMPTING the R2
/// delete — the reference release runs for every attachment regardless (#690).
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

/// Register a server-side reference for each attachment carried by `content`,
/// keyed by the sending message's id (#690). Called on the send path so the DS
/// can reference-count the shared, convergent `attachment_object` row: it now
/// refuses to collect that row — or its R2 object — while ANY message still
/// references the hash, closing the cross-conversation stranding this ticket
/// fixes.
///
/// Best-effort: a failure here NEVER fails the send. The envelope is already the
/// durable delivery; a missing reference only risks the pre-#690 status quo for
/// this one object (it could be collected early if every OTHER reference is
/// released), so this is never worse than before server-side counting existed.
pub(crate) async fn register_attachment_refs(
    state: &Arc<AppState>,
    message_id: &str,
    content: &str,
) {
    for att in parse_attachment_refs(content) {
        let body = pollis_api::messages::AttachmentRegisterBody {
            content_hash: att.content_hash.clone(),
            r2_key: att.r2_key,
            message_id: Some(message_id.to_string()),
        };
        if let Err(e) = crate::commands::mls::ds_post_ok(state, &body).await {
            eprintln!(
                "[send_message] failed to register attachment ref for {} (msg {message_id}): {e}",
                att.content_hash
            );
        }
    }
}

/// Trigger server-side collection of a deleted message's attachment, and — when
/// the client's local view suggests it may now be orphaned — try to collect the
/// R2 object. Best-effort on both: failures are logged, never bubbled.
///
/// The reference itself is already released at this point: the DS derives the
/// reference count from message-envelope existence (#690), and this delete's
/// `/v1/messages/delete` (issued earlier in `delete_message`) removed the
/// envelope, so the reference stopped counting the moment the message went. This
/// `/v1/attachments/delete` call therefore does not release anything — it asks
/// the DS to COLLECT the shared `attachment_object` row, which it does only when
/// NO still-existing message references the hash. A hash another conversation
/// still references survives; the client's local view is not assumed complete.
///
/// `locally_orphaned` is a cheap first pass over THIS device's other non-deleted
/// messages: `true` means nothing local still references the hash, so it is worth
/// attempting the R2 delete. It only saves a pointless round trip — it is not
/// trusted for correctness. The DS `/v1/r2/presign` gate is the authority and
/// refuses to mint a `delete` while a cross-conversation reference the client
/// cannot see still exists, so a refused delete (surfacing here as a logged
/// error) is the expected, benign outcome that keeps the shared object alive.
async fn cleanup_attachment(
    state: &Arc<AppState>,
    message_id: &str,
    att: &AttachmentRef,
    locally_orphaned: bool,
) {
    // DS seam: ask the DS to conditionally collect the shared dedup row (the
    // reference was already released by deleting the message envelope above).
    // Best-effort — failures are logged, never bubbled.
    let remote_result = async {
        let body = pollis_api::messages::AttachmentDeleteBody {
            content_hash: att.content_hash.clone(),
            message_id: Some(message_id.to_string()),
        };
        crate::commands::mls::ds_post_ok(state, &body).await
    }
    .await;

    if let Err(e) = remote_result {
        eprintln!(
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
    let now = super::envelope_sent_at();

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

    // Catch up MLS state before encrypting. Without this, an edit can be
    // emitted at a stale epoch — recipients at the current epoch will fail
    // to decrypt it. send_message does the same two-step catch-up here
    // (poll_mls_welcomes + catch-up) and edit_message was missing it. See
    // issue #371 scenario 2.
    {
        let device_id = state.device_id.lock().await.clone();
        if let Some(ref did) = device_id {
            if let Err(e) =
                crate::commands::mls::poll_mls_welcomes_inner(state, &user_id, did).await
            {
                eprintln!("[messages] edit_message: poll_mls_welcomes for {mls_group_id}: {e}");
            }
        }
    }
    // Use the INTERLEAVED ingesting catch-up (not the commit-only
    // `process_pending_commits_inner`): editing advances this device to head, and
    // with `max_past_epochs = 0` a current-epoch inbound message we haven't
    // fetched yet would be stranded when we advance past its epoch (issue #440,
    // the committer strand). Safe here — edit_message holds no MLS group lock, so
    // the catch-up re-acquiring it cannot deadlock.
    if let Err(e) =
        super::catch_up_mls_group_interleaved(state, &mls_group_id, &user_id).await
    {
        eprintln!("[messages] edit_message: catch_up_mls_group for {mls_group_id}: {e}");
    }

    // Encrypt the new content with MLS and update local cache atomically.
    // First attempt — if the group is missing (e.g. local DB was wiped),
    // transparently repair and retry.
    let needs_repair = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        crate::commands::mls::try_mls_encrypt(db.conn(), &mls_group_id, new_content.as_bytes()).is_none()
    };

    if needs_repair {
        // Local MLS state is missing (e.g. a local DB wipe). Rejoin THIS device
        // from the group's published GroupInfo — the same non-destructive
        // recovery process_pending_commits uses. We must NEVER nuke the shared
        // commit log to repair one device: deleting canonical history destroys
        // every member's ability to decrypt past messages and can fork the
        // group. See docs/mls-reconcile-hardening.md (INV-1/INV-4).
        crate::commands::mls::external_join_group(state, &mls_group_id, &user_id).await?;
    }

    let ciphertext_remote = {
        let guard = state.local_db.lock().await;
        let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

        // Size padding (issue #331 v2, §4.1) — same scheme as the send path:
        // pad TEXT edits to a size bucket; leave attachment edits unpadded.
        let plaintext: Vec<u8> = if is_attachment_content(&new_content) {
            new_content.as_bytes().to_vec()
        } else {
            super::framing::pad(new_content.as_bytes())
        };

        let mls_bytes = crate::commands::mls::try_mls_encrypt(db.conn(), &mls_group_id, &plaintext)
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

    // Replace any existing edit envelope for this message with the new one
    // (DELETE + INSERT, single transaction on the DS side). DS seam: route the
    // replace through the Delivery Service.
    let body = pollis_api::messages::EditMessageBody {
        envelope_id,
        conversation_id: conversation_id.clone(),
        target_message_id: message_id.clone(),
        sender_id: Some(user_id),
        ciphertext: ciphertext_remote,
        sent_at: now,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

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
            &message_id,
        ).await {
            eprintln!("[realtime] edit_message: publish to DM room {room_id}: {e}");
        }
    }

    Ok(())
}
