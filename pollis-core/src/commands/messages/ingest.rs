use rusqlite::OptionalExtension;
use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

// Envelope cleanup: TTL gate OR watermark gate. Watermark gate is keyed on
// (user, device) — a multi-device user whose other device hasn't synced keeps
// envelopes alive until either every device catches up or the TTL expires.
const CLEANUP_CHANNEL_ENVELOPES: &str = "\
DELETE FROM message_envelope
 WHERE conversation_id = ?1
   AND (
     sent_at < datetime('now', '-30 days')
     OR sent_at < (
       SELECT CASE
                WHEN COUNT(ud.device_id) = COUNT(cw.last_fetched_at)
                THEN MIN(cw.last_fetched_at)
                ELSE NULL
              END
       FROM group_member gm
       JOIN channels c ON c.id = ?1 AND c.group_id = gm.group_id
       JOIN user_device ud ON ud.user_id = gm.user_id
       LEFT JOIN conversation_watermark cw
              ON cw.conversation_id = ?1
             AND cw.user_id = ud.user_id
             AND cw.device_id = ud.device_id
     )
   )";

const CLEANUP_DM_ENVELOPES: &str = "\
DELETE FROM message_envelope
 WHERE conversation_id = ?1
   AND (
     sent_at < datetime('now', '-30 days')
     OR sent_at < (
       SELECT CASE
                WHEN COUNT(ud.device_id) = COUNT(cw.last_fetched_at)
                THEN MIN(cw.last_fetched_at)
                ELSE NULL
              END
       FROM dm_channel_member dcm
       JOIN user_device ud ON ud.user_id = dcm.user_id
       LEFT JOIN conversation_watermark cw
              ON cw.conversation_id = ?1
             AND cw.user_id = ud.user_id
             AND cw.device_id = ud.device_id
       WHERE dcm.dm_channel_id = ?1
     )
   )";

/// Pull new envelopes for a channel from remote, decrypt, and persist into the
/// local `message` table (the authoritative history for this device). Applies
/// any pending edit envelopes, advances this user's watermark to the max
/// sent_at successfully persisted up to the first decrypt failure, and runs
/// envelope GC. Idempotent — repeated calls are a no-op on local state.
pub async fn ingest_channel_envelopes_inner(
    state: &Arc<AppState>,
    user_id: &str,
    channel_id: &str,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    // Single round-trip: resolve mls_group_id AND confirm membership. Returns
    // a row only when the channel exists and the user is a member; otherwise
    // short-circuit so the read path falls back to whatever is local.
    let mls_group_id: String = {
        let mut rows = conn.query(
            "SELECT c.group_id FROM channels c
             JOIN group_member gm ON gm.group_id = c.group_id
             WHERE c.id = ?1 AND gm.user_id = ?2
             LIMIT 1",
            libsql::params![channel_id.to_string(), user_id.to_string()],
        ).await?;
        match rows.next().await? {
            Some(row) => row.get::<String>(0)?,
            None => return Ok(()),
        }
    };

    let device_id = state.device_id.lock().await.clone();

    // Advance MLS epoch before decryption so pre-ingest-window commits apply.
    if let Some(ref did) = device_id {
        if let Err(e) = crate::commands::mls::poll_mls_welcomes_inner(state, user_id, did).await {
            eprintln!("[ingest] poll_mls_welcomes for {mls_group_id}: {e}");
        }
    }
    if let Err(e) = crate::commands::mls::process_pending_commits_inner(state, &mls_group_id, user_id).await {
        eprintln!("[ingest] process_pending_commits for {mls_group_id}: {e}");
    }

    // Pull only envelopes past this device's watermark. Steady state returns
    // zero rows. Empty-string COALESCE handles the no-watermark-yet case
    // (first ingest for this conversation on this device). Ordering by
    // sent_at ASC is critical so MLS decryption sees epochs in commit order
    // and the watermark stops at the first decrypt failure.
    let envelopes: Vec<(String, String, String, Option<String>, Option<String>, String, String)> = {
        let mut out = Vec::new();
        let did_param = device_id.clone().unwrap_or_default();
        let mut rows = conn.query(
            "SELECT id, sender_id, ciphertext, reply_to_id, target_message_id, sent_at, type
             FROM message_envelope
             WHERE conversation_id = ?1
               AND sent_at > COALESCE(
                   (SELECT last_fetched_at FROM conversation_watermark
                    WHERE conversation_id = ?1 AND user_id = ?2 AND device_id = ?3),
                   ''
               )
             ORDER BY sent_at ASC, id ASC",
            libsql::params![channel_id.to_string(), user_id.to_string(), did_param],
        ).await?;
        while let Some(row) = rows.next().await? {
            out.push((
                row.get::<String>(0)?,
                row.get::<String>(1)?,
                row.get::<String>(2)?,
                row.get::<Option<String>>(3)?,
                row.get::<Option<String>>(4)?,
                row.get::<String>(5)?,
                row.get::<String>(6)?,
            ));
        }
        out
    };

    let watermark_ts: Option<String> = persist_envelopes_locally(
        state,
        channel_id,
        &mls_group_id,
        &envelopes,
    ).await?;

    // DS seam: advance this device's watermark + run envelope GC through the
    // Delivery Service when configured; else direct. Both are best-effort (the
    // direct path logs and continues), so DS failures are logged too.
    if let (Some(ts), Some(did)) = (watermark_ts, device_id.as_ref()) {
        match state.config.pollis_delivery_url.as_deref() {
            Some(_) => {
                let body = serde_json::json!({
                    "conversation_id": channel_id,
                    "user_id": user_id,
                    "device_id": did,
                    "last_fetched_at": ts,
                });
                if let Err(e) = crate::commands::mls::ds_post_ok(state, "/v1/watermarks/advance", &body).await {
                    eprintln!("[watermark] ingest_channel: DS advance failed: {e}");
                }
            }
            None => {
                if let Err(e) = conn.execute(
                    "INSERT INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(conversation_id, user_id, device_id) DO UPDATE SET
                       last_fetched_at = MAX(last_fetched_at, excluded.last_fetched_at)",
                    libsql::params![channel_id.to_string(), user_id.to_string(), did.clone(), ts],
                ).await {
                    eprintln!("[watermark] ingest_channel: upsert failed: {e}");
                }
            }
        }
    }

    match state.config.pollis_delivery_url.as_deref() {
        Some(_) => {
            let body = serde_json::json!({ "conversation_id": channel_id, "is_dm": false });
            if let Err(e) = crate::commands::mls::ds_post_ok(state, "/v1/envelopes/gc", &body).await {
                eprintln!("[ingest] channel cleanup (DS) failed: {e}");
            }
        }
        None => {
            if let Err(e) = conn.execute(
                CLEANUP_CHANNEL_ENVELOPES,
                libsql::params![channel_id.to_string()],
            ).await {
                eprintln!("[ingest] channel cleanup failed: {e}");
            }
        }
    }

    Ok(())
}

/// Shared envelope-persist loop used by both channel and DM ingest. The
/// `mls_group_id` differs (channel → group_id, DM → conversation_id) so callers
/// resolve it and pass it in.
///
/// Returns the max sent_at across envelopes that were either successfully
/// handled (decrypted + stored, or applied as an edit) or already present
/// locally (idempotent no-op). A decrypt failure does *not* advance the
/// watermark past its envelope — this keeps failed envelopes alive in
/// `message_envelope` so a subsequent ingest (after commits/welcomes catch
/// up) can retry them. The 30-day absolute cutoff in CLEANUP_*_ENVELOPES is
/// the backstop for envelopes that are permanently undecryptable (e.g.
/// encrypted to an epoch this device never joined, like pre-join history
/// for a newly-added member).
async fn persist_envelopes_locally(
    state: &Arc<AppState>,
    conversation_id: &str,
    mls_group_id: &str,
    envelopes: &[(String, String, String, Option<String>, Option<String>, String, String)],
) -> Result<Option<String>> {
    let guard = state.local_db.lock().await;
    let db = guard.as_ref().ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

    let mut candidate: Option<String> = None;
    let mut advance = |sent_at: &str| {
        match candidate.as_deref() {
            Some(current) if current >= sent_at => {}
            _ => candidate = Some(sent_at.to_string()),
        }
    };

    for (id, sender_id, ciphertext, reply_to_id, target_id, sent_at, env_type) in envelopes {
        match env_type.as_str() {
            "message" => {
                let exists: bool = db.conn().query_row(
                    "SELECT 1 FROM message WHERE id = ?1",
                    rusqlite::params![id],
                    |_| Ok(true),
                ).optional()?.unwrap_or(false);
                if exists {
                    advance(sent_at);
                    continue;
                }
                let ct_bytes = ciphertext.strip_prefix("mls:")
                    .and_then(|h| hex::decode(h).ok());
                let plaintext = ct_bytes.as_ref()
                    .and_then(|b| crate::commands::mls::try_mls_decrypt(db.conn(), mls_group_id, b))
                    .and_then(|b| String::from_utf8(b).ok());
                if let (Some(text), Some(bytes)) = (plaintext, ct_bytes) {
                    let _ = db.conn().execute(
                        "INSERT OR IGNORE INTO message
                         (id, conversation_id, sender_id, ciphertext, content, reply_to_id, sent_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                        rusqlite::params![id, conversation_id, sender_id, bytes, text, reply_to_id, sent_at],
                    );
                    advance(sent_at);
                }
                // Decrypt failed — leave the envelope in message_envelope for
                // a future retry and do NOT advance the watermark past it.
            }
            "edit" => {
                if let Some(tid) = target_id.as_ref() {
                    let plaintext = ciphertext.strip_prefix("mls:")
                        .and_then(|h| hex::decode(h).ok())
                        .and_then(|b| crate::commands::mls::try_mls_decrypt(db.conn(), mls_group_id, &b))
                        .and_then(|b| String::from_utf8(b).ok());
                    if let Some(text) = plaintext {
                        let now = chrono::Utc::now().to_rfc3339();
                        let _ = db.conn().execute(
                            "UPDATE message SET content = ?1, edited_at = ?2
                             WHERE id = ?3 AND deleted_at IS NULL",
                            rusqlite::params![text, now, tid],
                        );
                        advance(sent_at);
                    }
                }
            }
            "delete" => {
                // Admin-issued tombstone for someone else's message. The
                // ciphertext is empty (no plaintext to decrypt) — the only
                // payload is target_message_id. Soft-delete the local row so
                // the read path masks content as "[deleted]".
                if let Some(tid) = target_id.as_ref() {
                    let now = chrono::Utc::now().to_rfc3339();
                    let _ = db.conn().execute(
                        "UPDATE message SET content = NULL, deleted_at = ?1
                         WHERE id = ?2 AND deleted_at IS NULL",
                        rusqlite::params![now, tid],
                    );
                    advance(sent_at);
                }
            }
            _ => {
                advance(sent_at);
            }
        }
    }
    Ok(candidate)
}

/// Frontend-triggerable ingest for a channel. Used by LiveKit real-time hints
/// and channel-focus pre-warm paths that want to persist new envelopes without
/// reading a page.
pub async fn ingest_channel_envelopes(
    user_id: String,
    channel_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    ingest_channel_envelopes_inner(state, &user_id, &channel_id).await
}

/// Pull new envelopes for a DM from remote, decrypt, and persist into the local
/// `message` table. DM MLS group is keyed by conversation_id directly.
pub async fn ingest_dm_envelopes_inner(
    state: &Arc<AppState>,
    user_id: &str,
    dm_channel_id: &str,
) -> Result<()> {
    let conn = state.remote_db.conn().await?;

    let is_member: bool = {
        let mut rows = conn.query(
            "SELECT 1 FROM dm_channel_member
             WHERE dm_channel_id = ?1 AND user_id = ?2
             LIMIT 1",
            libsql::params![dm_channel_id.to_string(), user_id.to_string()],
        ).await?;
        rows.next().await?.is_some()
    };
    if !is_member {
        return Ok(());
    }

    let device_id = state.device_id.lock().await.clone();

    if let Some(ref did) = device_id {
        if let Err(e) = crate::commands::mls::poll_mls_welcomes_inner(state, user_id, did).await {
            eprintln!("[ingest] poll_mls_welcomes for DM {dm_channel_id}: {e}");
        }
    }
    if let Err(e) = crate::commands::mls::process_pending_commits_inner(state, dm_channel_id, user_id).await {
        eprintln!("[ingest] process_pending_commits for DM {dm_channel_id}: {e}");
    }

    // Re-verify each peer's `account_id_pub` against the local TOFU pin on
    // every ingest. If a server-side swap or peer account reset has changed
    // the key, this emits a `KeyChanged` realtime event so the conversation
    // gets an inline banner (Signal-style) the moment we observe the change,
    // not only when the user opens the peer's profile.
    let peer_ids: Vec<String> = {
        let mut out = Vec::new();
        let mut rows = conn.query(
            "SELECT user_id FROM dm_channel_member \
             WHERE dm_channel_id = ?1 AND user_id <> ?2",
            libsql::params![dm_channel_id.to_string(), user_id.to_string()],
        ).await?;
        while let Some(row) = rows.next().await? {
            out.push(row.get::<String>(0)?);
        }
        out
    };
    for peer_id in &peer_ids {
        if let Err(e) = crate::commands::safety::check_and_pin_account_key(state, peer_id).await {
            eprintln!("[ingest] check_and_pin_account_key for {peer_id}: {e}");
        }
    }

    // Watermark-filtered envelope read — see ingest_channel_envelopes_inner.
    let envelopes: Vec<(String, String, String, Option<String>, Option<String>, String, String)> = {
        let mut out = Vec::new();
        let did_param = device_id.clone().unwrap_or_default();
        let mut rows = conn.query(
            "SELECT id, sender_id, ciphertext, reply_to_id, target_message_id, sent_at, type
             FROM message_envelope
             WHERE conversation_id = ?1
               AND sent_at > COALESCE(
                   (SELECT last_fetched_at FROM conversation_watermark
                    WHERE conversation_id = ?1 AND user_id = ?2 AND device_id = ?3),
                   ''
               )
             ORDER BY sent_at ASC, id ASC",
            libsql::params![dm_channel_id.to_string(), user_id.to_string(), did_param],
        ).await?;
        while let Some(row) = rows.next().await? {
            out.push((
                row.get::<String>(0)?,
                row.get::<String>(1)?,
                row.get::<String>(2)?,
                row.get::<Option<String>>(3)?,
                row.get::<Option<String>>(4)?,
                row.get::<String>(5)?,
                row.get::<String>(6)?,
            ));
        }
        out
    };

    let watermark_ts: Option<String> = persist_envelopes_locally(
        state,
        dm_channel_id,
        dm_channel_id,
        &envelopes,
    ).await?;

    // DS seam — see ingest_channel_envelopes_inner. DM cleanup uses the DM query.
    if let (Some(ts), Some(did)) = (watermark_ts, device_id.as_ref()) {
        match state.config.pollis_delivery_url.as_deref() {
            Some(_) => {
                let body = serde_json::json!({
                    "conversation_id": dm_channel_id,
                    "user_id": user_id,
                    "device_id": did,
                    "last_fetched_at": ts,
                });
                if let Err(e) = crate::commands::mls::ds_post_ok(state, "/v1/watermarks/advance", &body).await {
                    eprintln!("[watermark] ingest_dm: DS advance failed: {e}");
                }
            }
            None => {
                if let Err(e) = conn.execute(
                    "INSERT INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at)
                     VALUES (?1, ?2, ?3, ?4)
                     ON CONFLICT(conversation_id, user_id, device_id) DO UPDATE SET
                       last_fetched_at = MAX(last_fetched_at, excluded.last_fetched_at)",
                    libsql::params![dm_channel_id.to_string(), user_id.to_string(), did.clone(), ts],
                ).await {
                    eprintln!("[watermark] ingest_dm: upsert failed: {e}");
                }
            }
        }
    }

    match state.config.pollis_delivery_url.as_deref() {
        Some(_) => {
            let body = serde_json::json!({ "conversation_id": dm_channel_id, "is_dm": true });
            if let Err(e) = crate::commands::mls::ds_post_ok(state, "/v1/envelopes/gc", &body).await {
                eprintln!("[ingest] dm cleanup (DS) failed: {e}");
            }
        }
        None => {
            if let Err(e) = conn.execute(
                CLEANUP_DM_ENVELOPES,
                libsql::params![dm_channel_id.to_string()],
            ).await {
                eprintln!("[ingest] dm cleanup failed: {e}");
            }
        }
    }

    Ok(())
}

/// Frontend-triggerable ingest for a DM channel.
pub async fn ingest_dm_envelopes(
    user_id: String,
    dm_channel_id: String,
    state: &Arc<AppState>,
) -> Result<()> {
    ingest_dm_envelopes_inner(state, &user_id, &dm_channel_id).await
}
