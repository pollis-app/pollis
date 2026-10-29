//! Delivery and read receipts — **DMs only** (#857).
//!
//! A receipt is an MLS-encrypted `0xF8` frame on the ordinary send path, so the DS learns nothing new.
//! `reader_id` is always the MLS credential, so no member can forge another's receipt.
//! "Read implies delivered" is enforced by a trigger in `local_schema.sql`.
//! The `send_read_receipts` opt-out is reciprocal: off means no emitting and no recording.

use std::sync::Arc;

use rusqlite::OptionalExtension;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::framing::ReceiptKind;

/// Preferences key for the reciprocal opt-out, synced so it applies on every device.
const PREF_KEY: &str = "send_read_receipts";

/// Every receipt this device holds for one message: who has received it, and who
/// has actually read it. Both lists are user ids, taken from MLS credentials.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct MessageReceipts {
    pub message_id: String,
    /// Readers whose device fetched and decrypted the message.
    pub delivered_by: Vec<String>,
    /// Readers who actually saw it on screen. Always a subset of
    /// `delivered_by` — the schema trigger guarantees it.
    pub read_by: Vec<String>,
}

/// Is this device allowed to emit and record receipts?
///
/// Missing, unparseable or absent all mean true: a cold cache must not disable receipts.
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

/// The gate on all receipt activity: DMs only (#857), and only when enabled.
pub(crate) fn receipts_permitted(is_dm: bool, enabled: bool) -> bool {
    is_dm && enabled
}

/// Record one inbound receipt locally.
///
/// `reader_id` MUST be the MLS-authenticated credential of the receipt's author,
/// so a member can only ever speak for themselves.
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
    // `INSERT OR IGNORE`: replays are idempotent and the first timestamp stands.
    let _ = conn.execute(
        "INSERT OR IGNORE INTO message_receipt (message_id, reader_id, kind, at)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![message_id, reader_id, kind.as_str(), at],
    );
}

/// True when `conversation_id` is a DM.
async fn is_dm(state: &Arc<AppState>, conversation_id: &str) -> Result<bool> {
    let resolved = crate::commands::ds_reads::catch_up(state, conversation_id, false, false).await?;
    Ok(resolved.kind == pollis_api::directory::ConversationKind::Dm)
}

/// The emit chokepoint for both receipt kinds: DMs only, and the opt-out honoured.
/// Seals at the current epoch without a catch-up: callers just finished one, and from ingest it would recurse.
pub(crate) async fn emit_receipt(
    state: &Arc<AppState>,
    conversation_id: &str,
    kind: ReceiptKind,
    message_ids: &[String],
) -> Result<()> {
    if message_ids.is_empty() {
        return Ok(());
    }
    // Scope gate, resolved against the DB rather than trusted from the caller.
    let dm = is_dm(state, conversation_id).await?;

    let now = super::envelope_sent_at();
    let envelope_id = ulid::Ulid::new().to_string();
    let sealed = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        // Both gates together, under the same lock as the encrypt so the
        // decision and the act cannot straddle a preference change.
        if !receipts_permitted(dm, receipts_enabled(db.conn())) {
            return Ok(());
        }
        let plaintext = super::framing::pad_receipt(kind, &now, message_ids);
        // A DM's MLS group is keyed by the conversation id directly.
        let Some(sealed) =
            super::seal::try_seal_unlocked(db.conn(), conversation_id, conversation_id, &plaintext)?
        else {
            // No local MLS group yet; a receipt is best-effort, so skip.
            return Ok(());
        };
        // It will come back through this device's own fetch, undecryptable
        // here (#1312).
        super::loss::record_own_envelope(db.conn(), &envelope_id);
        sealed
    };

    // Sealed sender (#607) and a delete capability (#1086), like every envelope.
    let delete_token_hash =
        super::delete_capability::envelope_delete_token_hash(state, &envelope_id).await;
    // The DS refuses an envelope with no capability (#1135); only a locked device lacks one, so skip.
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
        // A receipt is not a reply.
        reply_to_id: None,
        sent_at: sealed.sent_at.clone(),
        sealed: 1,
        generation: Some(sealed.generation),
        epoch: Some(sealed.epoch),
        // A receipt frame wakes nobody: it is an acknowledgement of
        // something the recipient already has (#987).
        push_to: None,
        push_mentions: None,
        delete_token_hash: Some(delete_token_hash),
    };
    // No re-seal loop: callers may hold the group lock; an epoch-behind receipt is re-emitted later.
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

    // Wake the peers; a receipt already looks like a message on the wire, so this leaks nothing extra.
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

/// Emit **delivered** receipts for messages this device just fetched and
/// decrypted. Called by the ingest path; see [`emit_receipt`] for the gates.
pub(crate) async fn emit_delivered(
    state: &Arc<AppState>,
    conversation_id: &str,
    message_ids: &[String],
) -> Result<()> {
    emit_receipt(state, conversation_id, ReceiptKind::Delivered, message_ids).await
}

/// Mark messages as **actually read by the human** and emit a read receipt.
///
/// Called only for messages genuinely on screen in a focused window.
///
/// Only others' messages this device holds are acknowledged.
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
            // Present, and not ours.
            if author.is_some_and(|a| a != user_id) {
                out.push(id.clone());
            }
        }
        out
    };

    emit_receipt(state, &conversation_id, ReceiptKind::Read, &to_ack).await
}

/// Every receipt this device holds for the messages of one conversation.
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

    // Preserve message order while grouping.
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

    /// DMs only: a group channel never produces or records a receipt, over the whole truth table.
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

        // Same inputs, in a DM: recorded.
        record_receipt(db.conn(), "m1", "bob", ReceiptKind::Delivered, "t0", true);
        assert_eq!(receipt_rows(db.conn(), "m1").len(), 1);
    }

    /// Default is on, and every missing/broken cache shape reads as on.
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

    /// The opt-out is the only thing that turns it off, and it turns it off for
    /// both directions.
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

        // Flipping it back on resumes collection from that moment.
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
        // Three peers received it; only two of them actually read it.
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

        // The sender's "N of M read" therefore falls straight out of the rows.
        let read_count = rows.iter().filter(|(_, k)| k == "read").count();
        let delivered_count = rows.iter().filter(|(_, k)| k == "delivered").count();
        assert_eq!((delivered_count, read_count), (3, 2));
    }

    /// "Read implies delivered" is a schema guarantee.
    #[test]
    fn read_without_delivered_is_unrepresentable() {
        let db = db();
        // Raw SQL, bypassing every Rust helper.
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

    /// The kind column is closed: nothing but the two real signals can be
    /// stored, so "delivered" can never drift into meaning "read".
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

    /// Re-ingesting the same envelope must not duplicate rows or move the recorded time.
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
