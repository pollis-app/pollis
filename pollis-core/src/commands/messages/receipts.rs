//! Delivery and read receipts — **DMs only** (#857).
//!
//! A receipt is an MLS-encrypted `0xF8` [`super::framing`] frame on the ordinary
//! send path, so the DS cannot tell it from a short message. Stored locally
//! per message and per reader (`message_receipt`); `reader_id` is the MLS
//! credential. Delivered is about a device (ingest), Read about a human
//! ([`mark_messages_read`]). The `send_read_receipts` opt-out gates both kinds
//! and both directions. See `.codesight/wiki/mls.md` → "Receipts".

use std::sync::Arc;

use rusqlite::OptionalExtension;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::framing::ReceiptKind;

/// Preferences key for the reciprocal receipt opt-out. In the synced
/// preferences blob so it applies on every one of the user's devices.
const PREF_KEY: &str = "send_read_receipts";

/// Every receipt this device holds for one message, per reader. Both lists are
/// user ids from MLS credentials.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct MessageReceipts {
    pub message_id: String,
    /// Readers whose device fetched and decrypted the message.
    pub delivered_by: Vec<String>,
    /// Readers who saw it on screen; a subset of `delivered_by` (schema trigger).
    pub read_by: Vec<String>,
}

/// Is this device allowed to emit and record receipts? Missing, unparseable or
/// absent preference all mean true, so a cold cache never disables receipts.
pub(crate) fn receipts_enabled(conn: &rusqlite::Connection) -> bool {
    let raw: Option<String> = conn
        .query_row("SELECT preferences FROM preferences LIMIT 1", [], |row| {
            row.get(0)
        })
        .optional()
        .ok()
        .flatten();
    let Some(raw) = raw else {
        return true;
    };
    serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|v| v.get(PREF_KEY).and_then(serde_json::Value::as_bool))
        .unwrap_or(true)
}

/// The single gate on all receipt activity, emitting and recording: DMs only,
/// and the user's `send_read_receipts` preference. Pure so the truth table is
/// tested directly.
pub(crate) fn receipts_permitted(is_dm: bool, enabled: bool) -> bool {
    is_dm && enabled
}

/// Record one inbound receipt locally.
///
/// `reader_id` must be the MLS-authenticated author of the receipt. Refuses
/// outside a DM and while the reciprocal opt-out is off.
pub(crate) fn record_receipt(
    conn: &rusqlite::Connection,
    message_id: &str,
    reader_id: &str,
    kind: ReceiptKind,
    at: &str,
    is_dm: bool,
) {
    if !receipts_permitted(is_dm, receipts_enabled(conn)) {
        return;
    }
    // The primary key makes a replay idempotent, and the first timestamp wins.
    let _ = conn.execute(
        "INSERT OR IGNORE INTO message_receipt (message_id, reader_id, kind, at)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![message_id, reader_id, kind.as_str(), at],
    );
}

/// True when `conversation_id` is a DM, checked inside [`emit_receipt`] so the
/// scope rule belongs to the chokepoint.
async fn is_dm(state: &Arc<AppState>, conversation_id: &str) -> Result<bool> {
    let resolved = crate::commands::ds_reads::catch_up(state, conversation_id, false, false).await?;
    Ok(resolved.kind == pollis_api::directory::ConversationKind::Dm)
}

/// The emit chokepoint for both receipt kinds: DMs only, opt-out honoured. A
/// skipped receipt is `Ok(())`; it never fails an ingest or a UI action.
///
/// No catch-up first: callers just ran one (ingest) or hold the messages
/// already (read), and catching up from inside ingest would recurse.
pub(crate) async fn emit_receipt(
    state: &Arc<AppState>,
    conversation_id: &str,
    kind: ReceiptKind,
    message_ids: &[String],
) -> Result<()> {
    if message_ids.is_empty() {
        return Ok(());
    }
    // Resolved, not trusted from the caller.
    let dm = is_dm(state, conversation_id).await?;

    let now = super::envelope_sent_at();
    let envelope_id = ulid::Ulid::new().to_string();
    let sealed = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        // Under the same lock as the encrypt, so the decision cannot straddle a
        // preference change.
        if !receipts_permitted(dm, receipts_enabled(db.conn())) {
            return Ok(());
        }
        let plaintext = super::framing::pad_receipt(kind, &now, message_ids);
        let Some(sealed) =
            super::seal::try_seal_unlocked(db.conn(), conversation_id, conversation_id, &plaintext)?
        else {
            // No local MLS group for this DM yet: skip, best effort.
            return Ok(());
        };
        // It comes back through this device's own fetch, undecryptable (#1312).
        super::loss::record_own_envelope(db.conn(), &envelope_id);
        sealed
    };

    // Sealed like every send (#607), with a capability like every envelope
    // (#1086).
    let delete_token_hash =
        super::delete_capability::envelope_delete_token_hash(state, &envelope_id).await;
    // The DS refuses an envelope without a capability (#1135). That only
    // happens on a locked device, so skip it and let the next ingest re-emit.
    let Some(delete_token_hash) = delete_token_hash else {
        crate::diag!(
            "[receipts] {kind:?} receipt for {conversation_id} skipped — this device \
             cannot compute a delete capability (locked); the next ingest re-emits it"
        );
        return Ok(());
    };
    let body = pollis_api::messages::SendMessageBody {
        id: envelope_id,
        conversation_id: conversation_id.to_string(),
        sender_id: Some(super::send::SEALED_SENDER_SENTINEL.to_string()),
        ciphertext: sealed.wire(),
        reply_to_id: None,
        sent_at: sealed.sent_at.clone(),
        sealed: 1,
        generation: Some(sealed.generation),
        epoch: Some(sealed.epoch),
        // Wakes nobody (#987).
        push_to: None,
        push_mentions: None,
        delete_token_hash: Some(delete_token_hash),
    };
    // No re-seal loop: a caller may hold the group lock (the committer's
    // post-merge sweep) and the re-seal's catch-up would deadlock. An
    // epoch-behind receipt (#1041) is re-emitted by the next read or ingest.
    match crate::commands::mls::ds_post_envelope(state, &body).await? {
        crate::commands::mls::EnvelopePost::Landed => {}
        crate::commands::mls::EnvelopePost::EpochBehind {
            head_generation,
            head_epoch,
        } => {
            crate::diag!(
                "[receipts] {kind:?} receipt for {conversation_id} sealed at ({}, {}) but the \
                 log head is ({head_generation}, {head_epoch}) — skipped",
                sealed.generation, sealed.epoch
            );
            return Ok(());
        }
    }

    // Routing-only wake-up, as for a message. A receipt never becomes a
    // `message` row, so it cannot move an unread count.
    if let Err(e) = crate::commands::livekit::publish_new_message_to_room(
        state,
        conversation_id,
        None,
        Some(conversation_id),
    )
    .await
    {
        crate::diag!("[receipts] publish hint for {conversation_id}: {e}");
    }

    Ok(())
}

/// Emit delivered receipts for messages this device just decrypted (ingest).
pub(crate) async fn emit_delivered(
    state: &Arc<AppState>,
    conversation_id: &str,
    message_ids: &[String],
) -> Result<()> {
    emit_receipt(state, conversation_id, ReceiptKind::Delivered, message_ids).await
}

/// Mark messages as **actually read by the human** and emit a read receipt.
///
/// The renderer calls this only for messages genuinely on screen, focused, in
/// the open conversation. Only other people's messages that this device holds
/// are acknowledged, so a buggy renderer cannot vouch for unseen messages.
pub async fn mark_messages_read(
    conversation_id: String,
    user_id: String,
    message_ids: Vec<String>,
    state: &Arc<AppState>,
) -> Result<()> {
    if message_ids.is_empty() {
        return Ok(());
    }
    let to_ack: Vec<String> = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        let mut out = Vec::new();
        for id in &message_ids {
            let author: Option<String> = db
                .conn()
                .query_row(
                    "SELECT sender_id FROM message
                     WHERE id = ?1 AND conversation_id = ?2",
                    rusqlite::params![id, conversation_id],
                    |row| row.get(0),
                )
                .optional()
                .ok()
                .flatten();
            if author.is_some_and(|a| a != user_id) {
                out.push(id.clone());
            }
        }
        out
    };

    emit_receipt(state, &conversation_id, ReceiptKind::Read, &to_ack).await
}

/// Every receipt this device holds for the messages of one conversation.
///
/// One entry per message with at least one receipt. Empty while the opt-out is
/// off, on top of [`record_receipt`] storing nothing then.
pub async fn get_conversation_receipts(
    conversation_id: String,
    state: &Arc<AppState>,
) -> Result<Vec<MessageReceipts>> {
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    let conn = db.conn();
    if !receipts_enabled(conn) {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        "SELECT r.message_id, r.reader_id, r.kind
         FROM message_receipt r
         JOIN message m ON m.id = r.message_id
         WHERE m.conversation_id = ?1
         ORDER BY r.message_id ASC, r.at ASC",
    )?;
    let rows = stmt.query_map(rusqlite::params![conversation_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;

    let mut out: Vec<MessageReceipts> = Vec::new();
    for row in rows {
        let (message_id, reader_id, kind) = row?;
        if out.last().map(|r| r.message_id.as_str()) != Some(message_id.as_str()) {
            out.push(MessageReceipts {
                message_id: message_id.clone(),
                delivered_by: Vec::new(),
                read_by: Vec::new(),
            });
        }
        let entry = out
            .last_mut()
            .expect("just pushed when the group changed");
        match kind.as_str() {
            "delivered" => entry.delivered_by.push(reader_id),
            "read" => entry.read_by.push(reader_id),
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    //! The opt-out, read-implies-delivered, and per-reader storage.

    use super::*;
    use crate::db::local::LocalDb;

    fn db() -> LocalDb {
        LocalDb::open_in_memory().expect("in-memory local db")
    }

    fn set_pref(conn: &rusqlite::Connection, json: &str) {
        conn.execute("DELETE FROM preferences", []).unwrap();
        conn.execute(
            "INSERT INTO preferences (preferences) VALUES (?1)",
            rusqlite::params![json],
        )
        .unwrap();
    }

    fn receipt_rows(conn: &rusqlite::Connection, message_id: &str) -> Vec<(String, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT reader_id, kind FROM message_receipt
                 WHERE message_id = ?1 ORDER BY reader_id, kind",
            )
            .unwrap();
        let rows = stmt
            .query_map(rusqlite::params![message_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
            })
            .unwrap();
        rows.map(|r| r.unwrap()).collect()
    }

    /// DMs only (#857), over the whole truth table.
    #[test]
    fn receipts_are_permitted_only_in_dms() {
        assert!(receipts_permitted(true, true), "a DM with receipts on");
        assert!(
            !receipts_permitted(false, true),
            "a GROUP CHANNEL must never produce a receipt, even with the preference on",
        );
        assert!(
            !receipts_permitted(true, false),
            "a DM must produce nothing while the user has opted out",
        );
        assert!(!receipts_permitted(false, false), "neither");
    }

    /// The DM-only rule holds on the recording side too.
    #[test]
    fn a_group_channel_records_no_receipts() {
        let db = db();
        record_receipt(db.conn(), "m1", "bob", ReceiptKind::Delivered, "t0", false);
        record_receipt(db.conn(), "m1", "bob", ReceiptKind::Read, "t1", false);
        assert!(
            receipt_rows(db.conn(), "m1").is_empty(),
            "a group channel must not accumulate receipts",
        );

        // Same inputs in a DM are recorded, so the check above is meaningful.
        record_receipt(db.conn(), "m1", "bob", ReceiptKind::Delivered, "t0", true);
        assert_eq!(receipt_rows(db.conn(), "m1").len(), 1);
    }

    /// Default on; every missing or broken cache also reads as on.
    #[test]
    fn receipts_default_to_enabled() {
        let db = db();
        assert!(receipts_enabled(db.conn()), "no preferences row at all");
        for json in ["{}", r#"{"other":1}"#, "not json", r#"{"send_read_receipts":null}"#] {
            set_pref(db.conn(), json);
            assert!(receipts_enabled(db.conn()), "prefs = {json}");
        }
        set_pref(db.conn(), r#"{"send_read_receipts":true}"#);
        assert!(receipts_enabled(db.conn()));
    }

    /// The opt-out turns off recording too.
    #[test]
    fn opt_out_disables_recording_in_both_directions() {
        let db = db();
        set_pref(db.conn(), r#"{"send_read_receipts":false}"#);
        assert!(!receipts_enabled(db.conn()));

        record_receipt(db.conn(), "m1", "bob", ReceiptKind::Delivered, "t0", true);
        record_receipt(db.conn(), "m1", "bob", ReceiptKind::Read, "t1", true);
        assert!(
            receipt_rows(db.conn(), "m1").is_empty(),
            "an opted-out device must not collect other people's receipts either",
        );

        set_pref(db.conn(), r#"{"send_read_receipts":true}"#);
        record_receipt(db.conn(), "m1", "bob", ReceiptKind::Delivered, "t2", true);
        assert_eq!(
            receipt_rows(db.conn(), "m1"),
            vec![("bob".to_string(), "delivered".to_string())],
        );
    }

    /// A multi-participant DM records receipts per reader.
    #[test]
    fn multi_participant_dm_records_per_reader() {
        let db = db();
        for reader in ["bob", "carol", "dave"] {
            record_receipt(db.conn(), "m1", reader, ReceiptKind::Delivered, "t0", true);
        }
        for reader in ["bob", "dave"] {
            record_receipt(db.conn(), "m1", reader, ReceiptKind::Read, "t1", true);
        }

        let rows = receipt_rows(db.conn(), "m1");
        assert_eq!(
            rows,
            vec![
                ("bob".to_string(), "delivered".to_string()),
                ("bob".to_string(), "read".to_string()),
                ("carol".to_string(), "delivered".to_string()),
                ("dave".to_string(), "delivered".to_string()),
                ("dave".to_string(), "read".to_string()),
            ],
            "each reader must be tracked independently",
        );

        let read_count = rows.iter().filter(|(_, k)| k == "read").count();
        let delivered_count = rows.iter().filter(|(_, k)| k == "delivered").count();
        assert_eq!((delivered_count, read_count), (3, 2));
    }

    /// "Read implies delivered" is a schema guarantee.
    #[test]
    fn read_without_delivered_is_unrepresentable() {
        let db = db();
        // Raw SQL: the invariant must hold at the lowest layer.
        db.conn()
            .execute(
                "INSERT INTO message_receipt (message_id, reader_id, kind, at)
                 VALUES ('m9', 'erin', 'read', 't5')",
                [],
            )
            .unwrap();
        assert_eq!(
            receipt_rows(db.conn(), "m9"),
            vec![
                ("erin".to_string(), "delivered".to_string()),
                ("erin".to_string(), "read".to_string()),
            ],
            "a read receipt must imply a delivered receipt",
        );
    }

    /// The kind column is closed to the two real signals.
    #[test]
    fn unknown_receipt_kinds_are_rejected() {
        let db = db();
        let err = db.conn().execute(
            "INSERT INTO message_receipt (message_id, reader_id, kind, at)
             VALUES ('m1', 'bob', 'seen-ish', 't0')",
            [],
        );
        assert!(err.is_err(), "an unknown receipt kind must not be storable");
    }

    /// Envelopes are re-fetched while a watermark cannot advance, so recording
    /// must be idempotent and keep the first timestamp.
    #[test]
    fn recording_is_idempotent_and_keeps_the_first_timestamp() {
        let db = db();
        record_receipt(db.conn(), "m1", "bob", ReceiptKind::Delivered, "t-first", true);
        record_receipt(db.conn(), "m1", "bob", ReceiptKind::Delivered, "t-second", true);
        assert_eq!(receipt_rows(db.conn(), "m1").len(), 1);
        let at: String = db
            .conn()
            .query_row(
                "SELECT at FROM message_receipt WHERE message_id = 'm1' AND kind = 'delivered'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(at, "t-first");
    }
}
