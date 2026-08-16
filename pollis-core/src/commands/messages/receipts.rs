//! Delivery and read receipts — **DMs only** (#857).
//!
//! ## Shape
//!
//! A receipt is a **client-emitted, MLS-encrypted control message**, exactly like
//! the "delete for everyone" redaction next door in [`super::edit_delete`]. It is
//! a `0xF8` [`super::framing`] frame, zero-padded to the same size bucket as a
//! short text message, posted to `/v1/messages/send` as an ordinary
//! `type='message'` envelope.
//!
//! It is deliberately **not** a server-visible flag on the envelope, and
//! deliberately **not** its own `/v1/receipts/*` DS endpoint: either would hand
//! the untrusted Delivery Service exactly the metadata this feature must
//! withhold — who read what, and when. Riding the ordinary send path means the
//! DS sees one more indistinguishable `MIN_BUCKET`-sized envelope in a
//! conversation it already knows exists, and nothing else. It also means
//! receipts inherit MLS encryption, per-conversation watermarks, envelope GC and
//! offline delivery with **no remote schema change, no migration, and no new DS
//! endpoint**.
//!
//! ## Data model (multi-participant DMs)
//!
//! Receipts land in the device-local `message_receipt` table, keyed
//! `(message_id, reader_id, kind)` — **per message, per reader**, never a single
//! boolean on the message. A DM with five members produces up to five
//! `delivered` rows and five `read` rows for one message, and the sender's UI
//! renders "3 of 5 read" from exactly those rows. A 1:1 DM is just the N=1 case
//! of the same table, so nothing has to be extended later.
//!
//! `reader_id` is always the **MLS credential** recovered from inside the
//! ciphertext, never a server-supplied field, so one member cannot forge a
//! receipt in another member's name.
//!
//! Receipts are local-only: they are a fact about what *this* device has
//! learned, and there is no remote receipt table for a breach to read.
//!
//! ## Delivered vs read — two different signals, never conflated
//!
//! * **Delivered** is emitted by the ingest path in [`super::ingest`] when this
//!   device has actually fetched an envelope and successfully MLS-decrypted it.
//!   It is a statement about a *device*.
//! * **Read** is emitted only by [`mark_messages_read`], which the renderer calls
//!   when a message was genuinely on screen in a focused window in the
//!   conversation the user is looking at. It is a statement about a *human*.
//!   Rendering a conversation offscreen, or receiving it in a background window,
//!   never produces a read receipt.
//!
//! The ordering invariant "read implies delivered" is enforced by a trigger in
//! `local_schema.sql`, not by caller discipline.
//!
//! ## Opt-out (reciprocal)
//!
//! `send_read_receipts` in the synced preferences blob (default **true**) gates
//! BOTH kinds at the single [`emit_receipt`] chokepoint. A user who turns it off
//! emits nothing at all — no read receipts and no delivery receipts, because a
//! delivery receipt is itself a "my device was online and fetched at time T"
//! signal that a user declining receipts is declining.
//!
//! The switch is **reciprocal**: while it is off this device also refuses to
//! *record* inbound receipts (see [`record_receipt`]), so the UI showing you
//! nothing is a fact about the database rather than a promise the renderer makes.
//! Turning it back on resumes collection from that moment.

use std::sync::Arc;

use rusqlite::OptionalExtension;

use crate::error::{Error, Result};
use crate::state::AppState;

use super::framing::ReceiptKind;

/// Preferences key for the reciprocal receipt opt-out. Lives in the existing
/// synced `user_preferences` JSON blob, so it needs no migration and — unlike a
/// device-local setting — actually applies on every one of the user's devices. A
/// privacy switch that silently fails to apply on your other laptop is a broken
/// privacy switch, which is worth more than hiding this one static bit from a
/// server that already stores the rest of the blob.
const PREF_KEY: &str = "send_read_receipts";

/// Every receipt this device holds for one message: who has received it, and who
/// has actually read it. Both lists are user ids, taken from MLS credentials.
///
/// Per-reader by construction — this is what makes a multi-participant DM work,
/// and what a single `delivered: bool` on the message could never express.
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
/// Reads the locally cached preferences blob written by
/// `commands::user::get_preferences`. Missing / unparseable / absent key all
/// mean **true**: receipts are the default behaviour of a messenger, and a
/// cold cache must not silently disable a feature the user never turned off.
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

/// THE gate on all receipt activity, in one pure predicate — emitting and
/// recording, both kinds, every call site.
///
/// Both conditions are load-bearing and neither is a nicety:
/// * `is_dm` — #857 is DMs-only. A group channel emits nothing and records
///   nothing. Not behind a flag, not "while we're here".
/// * `enabled` — the user's `send_read_receipts` preference. Reciprocal, so it
///   gates the recording direction too.
///
/// Pure so the whole truth table is testable without a database or a network,
/// which is what makes "a receipt is never produced in a group channel" an
/// asserted invariant rather than a claim about two call sites.
pub(crate) fn receipts_permitted(is_dm: bool, enabled: bool) -> bool {
    is_dm && enabled
}

/// Record one inbound receipt locally.
///
/// `reader_id` MUST be the MLS-authenticated credential of the receipt's author,
/// so a member can only ever speak for themselves.
///
/// Refuses while the reciprocal opt-out is off: a user who does not send
/// receipts does not collect them either, so "you can't see theirs" is enforced
/// by the absence of rows rather than by the renderer choosing not to look.
/// Refuses outside a DM for the same reason the emit side does.
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
    // `INSERT OR IGNORE`: the (message_id, reader_id, kind) primary key makes a
    // replayed envelope idempotent, and the first timestamp for a given signal
    // is the true one — a later duplicate must not move it.
    let _ = conn.execute(
        "INSERT OR IGNORE INTO message_receipt (message_id, reader_id, kind, at)
         VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![message_id, reader_id, kind.as_str(), at],
    );
}

/// True when `conversation_id` is a DM.
///
/// Receipts are DMs-only, full stop — group channels never emit or record them.
/// This is checked inside [`emit_receipt`] rather than left to callers, so the
/// scope restriction is a property of the chokepoint and not of everyone
/// remembering to ask.
async fn is_dm(state: &Arc<AppState>, conversation_id: &str) -> Result<bool> {
    let conn = state.remote_db.conn().await?;
    let mut rows = conn
        .query(
            "SELECT 1 FROM dm_channel WHERE id = ?1 LIMIT 1",
            libsql::params![conversation_id.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
}

/// THE emit chokepoint for both receipt kinds. Every receipt this client ever
/// sends goes through here, so the two invariants that decide whether this
/// feature is correct or a metadata leak are enforced in exactly one place:
///
/// 1. **DMs only** — a group channel emits nothing, ever.
/// 2. **Opt-out honoured on the sending side** — a user with
///    `send_read_receipts = false` emits nothing, ever.
///
/// A no-op returns `Ok(())`: not sending a receipt is never an error worth
/// failing an ingest pass or a UI interaction over.
///
/// The frame is encrypted at the group's CURRENT epoch. Unlike the send / edit /
/// redaction paths this deliberately does **not** run a catch-up first: the only
/// two callers have just finished one (ingest) or are acknowledging messages
/// they already hold (read), and calling back into
/// `catch_up_mls_group_interleaved` from inside the ingest path would recurse.
pub(crate) async fn emit_receipt(
    state: &Arc<AppState>,
    conversation_id: &str,
    kind: ReceiptKind,
    message_ids: &[String],
) -> Result<()> {
    if message_ids.is_empty() {
        return Ok(());
    }
    // Scope gate, resolved against the database rather than trusted from the
    // caller: a group channel must never produce a receipt.
    let dm = is_dm(state, conversation_id).await?;

    let now = super::envelope_sent_at();
    let ciphertext_remote = {
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
        let Some(mls_bytes) =
            crate::commands::mls::try_mls_encrypt(db.conn(), conversation_id, &plaintext)
        else {
            // No local MLS group for this DM yet. A receipt is best-effort
            // telemetry about messages we already hold; failing the caller over
            // it would be worse than silently skipping.
            return Ok(());
        };
        format!("mls:{}", hex::encode(&mls_bytes))
    };

    // Blind the envelope sender exactly like every other send — sealing is
    // unconditional (#607). The reader's true identity is the MLS credential
    // inside the ciphertext, which is what the recipient records as `reader_id`.
    let body = pollis_api::messages::SendMessageBody {
        id: ulid::Ulid::new().to_string(),
        conversation_id: conversation_id.to_string(),
        sender_id: Some(super::send::SEALED_SENDER_SENTINEL.to_string()),
        ciphertext: ciphertext_remote,
        // A receipt is not a reply.
        reply_to_id: None,
        sent_at: now,
        sealed: 1,
    };
    crate::commands::mls::ds_post_ok(state, &body).await?;

    // Wake the peers so the sender sees the tick without polling. This is the
    // same routing-only hint an ordinary message publishes, which is also why it
    // leaks nothing extra: a receipt already looks like a message on the wire.
    // A receipt never becomes a local `message` row, so it cannot move an unread
    // count or raise a notification on arrival.
    if let Err(e) = crate::commands::livekit::publish_new_message_to_room(
        state,
        conversation_id,
        None,
        Some(conversation_id),
    )
    .await
    {
        eprintln!("[receipts] publish hint for {conversation_id}: {e}");
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
/// The renderer calls this only for messages that were genuinely on screen, in a
/// focused window, in the conversation the user is looking at — never merely
/// because a conversation was mounted or rendered offscreen.
///
/// Only messages authored by *someone else* are acknowledged; a user reading
/// their own message is not a signal. Ids the device does not hold are dropped
/// rather than trusted, so a buggy or hostile renderer cannot make this device
/// vouch for messages it never received.
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
///
/// Returns one entry per message that has at least one receipt; a message
/// nobody has acknowledged simply does not appear. Returns an empty list while
/// the reciprocal opt-out is off — belt-and-braces over [`record_receipt`],
/// which already declines to store anything in that state.
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
    //! The properties, not the happy path: what the opt-out means, that a read
    //! can never exist without a delivered, and that the table really is
    //! per-reader rather than a boolean wearing a disguise.

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

    /// **DMs only.** The single most important scope property of #857: a group
    /// channel must never produce or record a receipt, whatever the preference
    /// says. Exhaustive over the whole two-bit truth table, so the "not behind a
    /// flag, not while we're here" requirement is asserted, not asserted-about.
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

    /// The DM-only rule holds on the RECORDING side too, not just the emitting
    /// side — a receipt frame that somehow arrives in a group channel is
    /// dropped rather than honoured.
    #[test]
    fn a_group_channel_records_no_receipts() {
        let db = db();
        record_receipt(db.conn(), "m1", "bob", ReceiptKind::Delivered, "t0", false);
        record_receipt(db.conn(), "m1", "bob", ReceiptKind::Read, "t1", false);
        assert!(
            receipt_rows(db.conn(), "m1").is_empty(),
            "a group channel must not accumulate receipts",
        );

        // Same inputs, in a DM: recorded. Proves the test above is not passing
        // for some unrelated reason.
        record_receipt(db.conn(), "m1", "bob", ReceiptKind::Delivered, "t0", true);
        assert_eq!(receipt_rows(db.conn(), "m1").len(), 1);
    }

    /// Default is ON, and every shape of missing/broken cache must also read as
    /// ON — a cold or corrupt preferences row must never silently disable a
    /// feature the user did not turn off.
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

    /// A multi-participant DM records receipts PER READER — the property a
    /// single boolean on the message could not express. Four members, partial
    /// reads, and the counts must come out per-person.
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

    /// "Read implies delivered" is a SCHEMA guarantee, not caller discipline:
    /// inserting only a read row must materialise the delivered row too, so the
    /// state "read but never delivered" cannot exist in the table.
    #[test]
    fn read_without_delivered_is_unrepresentable() {
        let db = db();
        // Straight through the raw SQL, bypassing every Rust-side helper — the
        // invariant has to hold at the lowest layer.
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

    /// Re-ingesting the same envelope must not duplicate rows or move the
    /// recorded time — envelopes are re-fetched whenever a watermark cannot
    /// advance, so idempotency is load-bearing, not a nicety.
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
