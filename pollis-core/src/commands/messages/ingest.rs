use rusqlite::OptionalExtension;
use std::collections::HashMap;
use std::sync::Arc;

use crate::error::Result;
use crate::state::AppState;

/// One row from `message_envelope`:
/// `(id, sender_id, ciphertext, reply_to_id, target_message_id, sent_at, type)`.
type EnvelopeRow = (
    String,
    String,
    String,
    Option<String>,
    Option<String>,
    String,
    String,
);

/// Pull new envelopes for a channel from remote, decrypt, and persist into the
/// local `message` table (the authoritative history for this device). Applies
/// any pending edit/delete envelopes, advances this user's watermark over the
/// contiguous prefix of definitively-handled envelopes (stopping at the first
/// one that must be retried on a later pass), and runs envelope GC. Idempotent —
/// repeated calls are a no-op on local state.
///
/// Decryption is INTERLEAVED with commit replay (issue #418): see
/// [`ingest_envelopes_interleaved`].
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

    // Drain welcomes first so a just-joined member has a local group before the
    // commit replay runs. Commit replay itself is driven by the interleaved
    // pass below (which decrypts each epoch's messages as it advances), NOT here.
    if let Some(ref did) = device_id {
        if let Err(e) = crate::commands::mls::poll_mls_welcomes_inner(state, user_id, did).await {
            eprintln!("[ingest] poll_mls_welcomes for {mls_group_id}: {e}");
        }
    }

    // Pull only envelopes past this device's watermark. Steady state returns
    // zero rows. Empty-string COALESCE handles the no-watermark-yet case
    // (first ingest for this conversation on this device). Ordering by
    // sent_at ASC is the watermark's cursor order; the interleaved pass routes
    // each envelope to its MLS epoch independently of this ordering.
    let envelopes: Vec<EnvelopeRow> = {
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

    let watermark_ts: Option<String> = ingest_envelopes_interleaved(
        state,
        user_id,
        channel_id,
        &mls_group_id,
        &envelopes,
    ).await?;

    // Advance this device's watermark + run envelope GC through the Delivery
    // Service. Both are best-effort, so DS failures are logged and ignored.
    if let (Some(ts), Some(did)) = (watermark_ts, device_id.as_ref()) {
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

    let body = serde_json::json!({ "conversation_id": channel_id, "is_dm": false });
    if let Err(e) = crate::commands::mls::ds_post_ok(state, "/v1/envelopes/gc", &body).await {
        eprintln!("[ingest] channel cleanup (DS) failed: {e}");
    }

    Ok(())
}

/// Catch this device up on `mls_group_id` and decrypt the backlog in a single
/// epoch-stepped pass, used by both channel and DM ingest. The `mls_group_id`
/// differs (channel → group_id, DM → conversation_id) so callers resolve it and
/// pass it in.
///
/// ## Why interleave (issue #418)
///
/// `max_past_epochs` is left at openmls's default of 0: the ratchet keys for an
/// epoch are discarded the instant the group advances past it (this is the
/// forward-secrecy property we deliberately keep). The old path applied EVERY
/// pending commit first — jumping the local group straight to head — and only
/// then decrypted the fetched backlog. Any message sealed at an intermediate
/// epoch (one the member churned past while offline) was then encrypted to keys
/// that no longer existed and decrypted as `WrongEpoch`, and — because the buggy
/// watermark took the global max sent_at across successes — a later head-epoch
/// success leapfrogged the watermark past the earlier failures, so they were
/// never re-fetched and were PERMANENTLY DROPPED. That violates the core
/// invariant that a current member can read every message sent while they were a
/// member.
///
/// The fix decrypts each epoch's messages WHILE the group is still at that
/// epoch, driven by the commit replay's per-epoch hook:
/// ```text
///   decrypt envelopes whose MLS epoch == initial_epoch
///   for each pending commit in epoch order:
///       apply the commit            (group advances to the next epoch)
///       decrypt envelopes whose MLS epoch == the new current epoch
/// ```
/// Each envelope's epoch is read by PARSING it (not inferred from `sent_at`).
///
/// ## Watermark
///
/// Returns the sent_at the device's watermark should advance to: the max over
/// the contiguous (sent_at-ordered) prefix of envelopes that are definitively
/// handled, stopping STRICTLY BEFORE the first envelope that must be retried
/// later. "Handled" = decrypted this pass, OR permanently undeliverable to us
/// (an unreachable pre-join epoch, or unparseable bytes), OR an epoch-independent
/// tombstone. "Retry later" = an MLS epoch beyond the highest epoch this pass
/// reached (`max_fired_epoch`) — the commit that would let us decrypt it hasn't
/// arrived yet. Stopping before such an envelope (rather than taking a global
/// max) is what guarantees an undecryptable-for-now message is re-fetched on a
/// later pass instead of being skipped. Pre-join messages do NOT stop the
/// watermark (they are handled), so they can't wedge it; the Delivery Service's
/// envelope GC is the backstop that eventually removes them from the fetch set.
async fn ingest_envelopes_interleaved(
    state: &Arc<AppState>,
    user_id: &str,
    conversation_id: &str,
    mls_group_id: &str,
    envelopes: &[EnvelopeRow],
) -> Result<Option<String>> {
    // Pre-parse each message/edit envelope's MLS epoch (delete/unknown carry
    // none) and index the envelopes by epoch so the per-epoch hook can decrypt
    // exactly the ones sealed at the epoch the replay just reached.
    let mut by_epoch: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut epoch_of: Vec<Option<u64>> = Vec::with_capacity(envelopes.len());
    for (i, env) in envelopes.iter().enumerate() {
        let (_, _, ciphertext, _, _, _, env_type) = env;
        let epoch = match env_type.as_str() {
            "message" | "edit" => ciphertext
                .strip_prefix("mls:")
                .and_then(|h| hex::decode(h).ok())
                .and_then(|b| crate::commands::mls::envelope_epoch(&b)),
            _ => None,
        };
        epoch_of.push(epoch);
        if let Some(e) = epoch {
            by_epoch.entry(e).or_default().push(i);
        }
    }

    // Drive the commit replay, decrypting each epoch's messages/edits as the
    // group reaches it. `max_fired_epoch` records the highest epoch the replay
    // actually reached this pass — the watermark's "have we caught up to it yet?"
    // boundary. Scoped so the hook's borrows end before the watermark loop.
    let mut max_fired_epoch: Option<u64> = None;
    {
        let mut on_epoch = |conn: &rusqlite::Connection, epoch: u64| {
            max_fired_epoch = Some(max_fired_epoch.map_or(epoch, |m| m.max(epoch)));
            if let Some(indices) = by_epoch.get(&epoch) {
                for &i in indices {
                    decrypt_and_persist_one(conn, conversation_id, mls_group_id, &envelopes[i]);
                }
            }
        };
        if let Err(e) = crate::commands::mls::process_pending_commits_inner_with_hook(
            state,
            mls_group_id,
            user_id,
            &mut on_epoch,
        )
        .await
        {
            eprintln!("[ingest] process_pending_commits for {mls_group_id}: {e}");
        }
    }

    // Apply the epoch-independent envelopes (admin-issued delete tombstones)
    // after decryption, so a tombstone whose target was just decrypted this pass
    // still lands. The ciphertext is empty — there is nothing to decrypt.
    {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        for (_, _, _, _, target_id, _, env_type) in envelopes {
            if env_type == "delete" {
                if let Some(tid) = target_id.as_ref() {
                    let now = chrono::Utc::now().to_rfc3339();
                    let _ = db.conn().execute(
                        "UPDATE message SET content = NULL, deleted_at = ?1
                         WHERE id = ?2 AND deleted_at IS NULL",
                        rusqlite::params![now, tid],
                    );
                }
            }
        }
    }

    // Is this envelope definitively handled (so the watermark may advance over
    // it), or must a later pass retry it?
    let is_handled = |i: usize, env_type: &str| -> bool {
        match env_type {
            "message" | "edit" => match (epoch_of[i], max_fired_epoch) {
                // Epoch within this pass's reach: decrypted now, or an
                // unreachable pre-join epoch we will never decrypt. Either way,
                // permanently handled — advancing past it can't drop a message.
                (Some(ep), Some(max)) => ep <= max,
                // Unparseable bytes are never MLS-decryptable → permanently
                // handled (advancing past avoids wedging on a corrupt row).
                (None, _) => true,
                // The replay reached no epoch (no local group): nothing could be
                // decrypted, so these must be retried once a group exists.
                (Some(_), None) => false,
            },
            // delete tombstones / unknown types are epoch-independent.
            _ => true,
        }
    };

    // The sent_at of the first envelope we must retry is an EXCLUSIVE ceiling on
    // the watermark: advancing to (or, via a sent_at tie, past) it would drop it
    // from the next `sent_at > watermark` fetch.
    let stop_at: Option<String> = envelopes
        .iter()
        .enumerate()
        .find(|(i, env)| !is_handled(*i, env.6.as_str()))
        .map(|(_, env)| env.5.clone());

    let mut candidate: Option<String> = None;
    for env in envelopes {
        let sent_at = &env.5;
        if let Some(stop) = stop_at.as_ref() {
            if sent_at >= stop {
                break;
            }
        }
        candidate = Some(sent_at.clone());
    }
    Ok(candidate)
}

/// Decrypt one `message` or `edit` envelope at the group's CURRENT epoch and
/// persist it. Invoked by [`ingest_envelopes_interleaved`]'s per-epoch hook, so
/// the caller has already positioned the local group at this envelope's epoch.
/// Infallible: a failed decrypt or a transient DB error simply leaves nothing
/// persisted (the envelope stays in `message_envelope` for a later retry), the
/// same outcome the watermark logic accounts for.
fn decrypt_and_persist_one(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    mls_group_id: &str,
    env: &EnvelopeRow,
) {
    let (id, sender_id, ciphertext, reply_to_id, target_id, sent_at, env_type) = env;
    match env_type.as_str() {
        "message" => {
            let exists: bool = conn
                .query_row(
                    "SELECT 1 FROM message WHERE id = ?1",
                    rusqlite::params![id],
                    |_| Ok(true),
                )
                .optional()
                .ok()
                .flatten()
                .unwrap_or(false);
            if exists {
                return;
            }
            let ct_bytes = ciphertext
                .strip_prefix("mls:")
                .and_then(|h| hex::decode(h).ok());
            let plaintext = ct_bytes
                .as_ref()
                .and_then(|b| crate::commands::mls::try_mls_decrypt(conn, mls_group_id, b))
                .and_then(|b| String::from_utf8(b).ok());
            if let (Some(text), Some(bytes)) = (plaintext, ct_bytes) {
                let _ = conn.execute(
                    "INSERT OR IGNORE INTO message
                     (id, conversation_id, sender_id, ciphertext, content, reply_to_id, sent_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    rusqlite::params![id, conversation_id, sender_id, bytes, text, reply_to_id, sent_at],
                );
            }
            // Decrypt failed — leave the envelope in message_envelope for a
            // future retry; the watermark is computed to not skip past it.
        }
        "edit" => {
            if let Some(tid) = target_id.as_ref() {
                let plaintext = ciphertext
                    .strip_prefix("mls:")
                    .and_then(|h| hex::decode(h).ok())
                    .and_then(|b| crate::commands::mls::try_mls_decrypt(conn, mls_group_id, &b))
                    .and_then(|b| String::from_utf8(b).ok());
                if let Some(text) = plaintext {
                    let now = chrono::Utc::now().to_rfc3339();
                    let _ = conn.execute(
                        "UPDATE message SET content = ?1, edited_at = ?2
                         WHERE id = ?3 AND deleted_at IS NULL",
                        rusqlite::params![text, now, tid],
                    );
                }
            }
        }
        _ => {}
    }
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
    let envelopes: Vec<EnvelopeRow> = {
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

    let watermark_ts: Option<String> = ingest_envelopes_interleaved(
        state,
        user_id,
        dm_channel_id,
        dm_channel_id,
        &envelopes,
    ).await?;

    // Advance watermark + run envelope GC through the DS — see
    // ingest_channel_envelopes_inner. DM cleanup uses the DM query.
    if let (Some(ts), Some(did)) = (watermark_ts, device_id.as_ref()) {
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

    let body = serde_json::json!({ "conversation_id": dm_channel_id, "is_dm": true });
    if let Err(e) = crate::commands::mls::ds_post_ok(state, "/v1/envelopes/gc", &body).await {
        eprintln!("[ingest] dm cleanup (DS) failed: {e}");
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
