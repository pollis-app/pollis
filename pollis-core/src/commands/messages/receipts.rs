//! Delivery and read receipts — **DMs only** (#857).
//!
//! A receipt is an MLS-encrypted `0xF8` frame on the ordinary send path, so the DS learns nothing new.
//! `reader_id` is always the MLS credential, so no member can forge another's receipt.
//! "Read implies delivered" is enforced by a trigger in `local_schema.sql`.
//! The `send_read_receipts` opt-out is reciprocal: off means no emitting and no recording.
//!
//! **Owed until sent.** A receipt this device owes is a `receipt_owed` row, written with the
//! message it acknowledges (delivered) or when it is read, and cleared only once the DS took the
//! envelope carrying it. [`drain_owed`] is the one way out: sealed under the DM's MLS lock and
//! posted through `seal::post_resealing` like every message, so a staged commit, a commit that
//! lands between seal and post, or a locked device delays a receipt but never drops it.

use std::collections::{BTreeMap, HashMap};
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
    /// When each reader in `read_by` read it (#2140): the reader's own timestamp, carried inside
    /// the encrypted receipt. Keyed by reader id; exactly the readers in `read_by`.
    pub read_at: BTreeMap<String, String>,
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

/// Store a receipt copied from a sibling device (#1735), through the same gate
/// as a received one: DMs only, and only while this device has receipts on.
pub(crate) fn record_transferred_receipt(
    conn: &rusqlite::Connection,
    message_id: &str,
    reader_id: &str,
    kind: &str,
    at: &str,
    is_dm: bool,
) {
    let kind = match kind {
        "delivered" => ReceiptKind::Delivered,
        "read" => ReceiptKind::Read,
        _ => return,
    };
    record_receipt(conn, message_id, reader_id, kind, at, is_dm);
}

/// The most message ids one receipt frame carries; a longer backlog goes as several.
const MAX_IDS_PER_RECEIPT: usize = 256;

/// The receipts this device owes, kept until the DS has taken the envelope that carries them.
///
/// One row per `(conversation, kind, message)`. A delivered receipt is owed in the same
/// savepoint that stores the message it acknowledges ([`owe_delivered_on`]), and a read one
/// when the reader marks it read ([`mark_messages_read`]); only [`drain_owed`] clears a row,
/// once its envelope landed, or when no receipt may be sent for it (not a DM any more, not a
/// member, the opt-out). A read row carries the reader's own time (#2140); a delivered one is
/// stamped when it is sealed. Created by the app's own open only (`LocalDb::open_at`), never by
/// the extensions' opens; a file the app has not opened since the upgrade has no table, and
/// then the pass that stores a message sends its receipt once, as before ([`drain_owed`]).
pub(crate) const RECEIPT_OWED_DDL: &str = "CREATE TABLE IF NOT EXISTS receipt_owed (
    conversation_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('delivered', 'read')),
    read_at TEXT,
    CHECK ((kind = 'read') = (read_at IS NOT NULL)),
    PRIMARY KEY (conversation_id, kind, message_id)
) WITHOUT ROWID";

/// Whether this database has [`RECEIPT_OWED_DDL`]'s table.
fn owed_table_exists(conn: &rusqlite::Connection) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'receipt_owed'",
        [],
        |_| Ok(()),
    )
    .optional()
    .ok()
    .flatten()
    .is_some()
}

/// Owe the delivered receipt for `message_id`, just stored in DM `conversation_id`. Called in
/// the savepoint that stores the message, so a stored inbound DM message always has its receipt
/// sent or owed. Nothing is owed while the opt-out is on. Never fails the store: a file the app
/// has not opened since the upgrade has no table yet (see [`RECEIPT_OWED_DDL`]).
pub(crate) fn owe_delivered_on(conn: &rusqlite::Connection, conversation_id: &str, message_id: &str) {
    if !receipts_enabled(conn) {
        return;
    }
    if let Err(e) = conn.execute(
        "INSERT OR IGNORE INTO receipt_owed (conversation_id, message_id, kind, read_at)
         VALUES (?1, ?2, 'delivered', NULL)",
        rusqlite::params![conversation_id, message_id],
    ) {
        crate::diag!("[receipts] delivered receipt for {conversation_id} not recorded: {e}");
    }
}

/// Owe read receipts for `message_ids` in `conversation_id`, read at `read_at`.
fn owe_read_on(
    conn: &rusqlite::Connection,
    conversation_id: &str,
    message_ids: &[String],
    read_at: &str,
) -> Result<()> {
    if !receipts_enabled(conn) {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    for id in message_ids {
        tx.execute(
            "INSERT OR IGNORE INTO receipt_owed (conversation_id, message_id, kind, read_at)
             VALUES (?1, ?2, 'read', ?3)",
            rusqlite::params![conversation_id, id, read_at],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// One receipt frame's worth of owed rows: a kind, the reader's time for a read, and the ids.
#[derive(Debug, PartialEq, Eq)]
struct OwedBatch {
    kind: ReceiptKind,
    /// `Some` for a read receipt (the reader's time), `None` for a delivered one.
    read_at: Option<String>,
    message_ids: Vec<String>,
}

/// What `conversation_id` owes, as frames of at most [`MAX_IDS_PER_RECEIPT`] ids: delivered
/// first, then each read time on its own.
fn owed_batches_on(conn: &rusqlite::Connection, conversation_id: &str) -> Result<Vec<OwedBatch>> {
    let mut stmt = conn.prepare(
        "SELECT kind, read_at, message_id FROM receipt_owed WHERE conversation_id = ?1
         ORDER BY kind, read_at, message_id",
    )?;
    let rows = stmt.query_map(rusqlite::params![conversation_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out: Vec<OwedBatch> = Vec::new();
    for row in rows {
        let (kind, read_at, id) = row?;
        let kind = match kind.as_str() {
            "delivered" => ReceiptKind::Delivered,
            "read" => ReceiptKind::Read,
            _ => continue,
        };
        match out.last_mut() {
            Some(last)
                if last.kind == kind
                    && last.read_at == read_at
                    && last.message_ids.len() < MAX_IDS_PER_RECEIPT =>
            {
                last.message_ids.push(id);
            }
            _ => out.push(OwedBatch {
                kind,
                read_at,
                message_ids: vec![id],
            }),
        }
    }
    Ok(out)
}

/// Clear the owed rows a landed receipt carried.
fn settle_owed_on(conn: &rusqlite::Connection, conversation_id: &str, batch: &OwedBatch) -> Result<()> {
    let tx = conn.unchecked_transaction()?;
    for id in &batch.message_ids {
        tx.execute(
            "DELETE FROM receipt_owed WHERE conversation_id = ?1 AND kind = ?2 AND message_id = ?3",
            rusqlite::params![conversation_id, batch.kind.as_str(), id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Clear everything `conversation_id` owes: no receipt may be sent for any of it.
fn forget_owed_on(conn: &rusqlite::Connection, conversation_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM receipt_owed WHERE conversation_id = ?1",
        rusqlite::params![conversation_id],
    )?;
    Ok(())
}

/// Which conversations have a [`drain_owed`] running in this process, and whether it must go
/// round once more: a drain asked for while one runs never waits (it may be inside that very
/// drain's re-seal catch-up); the running one picks its rows up.
#[derive(Default)]
pub struct ReceiptDrains(std::sync::Mutex<HashMap<String, bool>>);

/// One conversation's running drain. Dropped (finished, failed or cancelled), it lets the next
/// one start.
struct DrainTurn<'a> {
    drains: &'a ReceiptDrains,
    conversation_id: String,
}

impl ReceiptDrains {
    fn begin(&self, conversation_id: &str) -> Option<DrainTurn<'_>> {
        let mut running = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(again) = running.get_mut(conversation_id) {
            *again = true;
            return None;
        }
        running.insert(conversation_id.to_string(), false);
        Some(DrainTurn {
            drains: self,
            conversation_id: conversation_id.to_string(),
        })
    }
}

impl DrainTurn<'_> {
    /// Whether a drain was asked for since this one began; clears the request.
    fn again(&self) -> bool {
        let mut running = self.drains.0.lock().unwrap_or_else(|e| e.into_inner());
        running
            .get_mut(&self.conversation_id)
            .map(std::mem::take)
            .unwrap_or(false)
    }
}

impl Drop for DrainTurn<'_> {
    fn drop(&mut self) {
        let mut running = self.drains.0.lock().unwrap_or_else(|e| e.into_inner());
        running.remove(&self.conversation_id);
    }
}

/// THE way a receipt leaves this device: send what `conversation_id` owes.
///
/// Every caller goes through here: the catch-up pass that stored the messages (sweep, realtime
/// wake, push fetch, mobile's concurrent ingest, a send's own catch-up), the committer once its
/// commit is merged, `mark_messages_read`, and the outbox drain (unlock, reconnect, resume). Each
/// frame is sealed under the DM's MLS lock and posted through `post_resealing`, which catches
/// up and re-seals at the log head when a commit landed first (#1041), exactly as a message is;
/// its rows are cleared only once the DS took it. A locked device (no delete capability, #1135)
/// seals nothing and keeps what it owes for the next drain.
///
/// `unrecorded`: what the caller's pass just stored, sent from memory only when this database
/// has no owed-receipt table yet (an extension's open before the app's first open since the
/// upgrade, see [`RECEIPT_OWED_DDL`]). Ignored otherwise: those rows are already owed.
pub(crate) async fn drain_owed(
    state: &Arc<AppState>,
    user_id: &str,
    conversation_id: &str,
    unrecorded: &[String],
) -> Result<()> {
    let Some(turn) = state.receipt_drains.begin(conversation_id) else {
        // A drain is running here; it goes round again for whatever this one would send.
        return Ok(());
    };
    let mut unrecorded = unrecorded;
    loop {
        drain_once(state, user_id, conversation_id, unrecorded).await?;
        unrecorded = &[];
        if !turn.again() {
            return Ok(());
        }
    }
}

/// Send what every conversation owes: the outbox drain's share (unlock, reconnect, resume).
pub(crate) async fn drain_all_owed(state: &Arc<AppState>) -> Result<()> {
    let conversations: Vec<String> = {
        let guard = state.local_db.lock().await;
        let Some(db) = guard.as_ref() else {
            return Ok(());
        };
        if !owed_table_exists(db.conn()) {
            return Ok(());
        }
        super::strings_on(db.conn(), "SELECT DISTINCT conversation_id FROM receipt_owed")?
    };
    if conversations.is_empty() {
        return Ok(());
    }
    let user_id = crate::commands::mls::ds_client::current_user_id(state).await?;
    for conversation_id in conversations {
        if let Err(e) = drain_owed(state, &user_id, &conversation_id, &[]).await {
            crate::diag!("[receipts] owed receipts for {conversation_id} not sent yet: {e}");
        }
    }
    Ok(())
}

/// What one frame's attempt came to.
enum Sent {
    /// The DS took it.
    Landed,
    /// The opt-out is on: nothing may be sent.
    OptedOut,
}

async fn drain_once(
    state: &Arc<AppState>,
    user_id: &str,
    conversation_id: &str,
    unrecorded: &[String],
) -> Result<()> {
    let (recorded, batches) = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        if owed_table_exists(db.conn()) {
            (true, owed_batches_on(db.conn(), conversation_id)?)
        } else {
            let batches = unrecorded
                .chunks(MAX_IDS_PER_RECEIPT)
                .map(|ids| OwedBatch {
                    kind: ReceiptKind::Delivered,
                    read_at: None,
                    message_ids: ids.to_vec(),
                })
                .collect();
            (false, batches)
        }
    };
    if batches.is_empty() {
        return Ok(());
    }
    // The DS refuses an envelope with no delete capability (#1135); only a locked device (a
    // notification extension) lacks one. Nothing is sealed, and the rows wait for the app.
    if !super::delete_capability::has_delete_capability(state).await {
        return Ok(());
    }
    // Scope gate, resolved against the DS rather than trusted from the caller: DMs only (#857).
    let resolved = crate::commands::ds_reads::catch_up(state, conversation_id, false, false).await?;
    let dm = resolved.kind == pollis_api::directory::ConversationKind::Dm;
    if !resolved.authorized || !dm {
        if recorded {
            super::with_local_conn(state, |conn| forget_owed_on(conn, conversation_id)).await?;
        }
        return Ok(());
    }
    for batch in batches {
        let sent = match deliver_batch(state, user_id, conversation_id, &batch).await {
            Ok(sent) => sent,
            // A refusal no retry fixes (#1335's rule): these can never go.
            Err(e) if super::outbox::is_permanent(&e) => {
                crate::diag!("[receipts] receipts for {conversation_id} refused for good: {e}");
                Sent::OptedOut
            }
            Err(e) => return Err(e),
        };
        if !recorded {
            continue;
        }
        match sent {
            Sent::Landed => {
                super::with_local_conn(state, |conn| settle_owed_on(conn, conversation_id, &batch))
                    .await?
            }
            Sent::OptedOut => {
                super::with_local_conn(state, |conn| forget_owed_on(conn, conversation_id)).await?;
                return Ok(());
            }
        }
    }
    Ok(())
}

/// Seal one receipt frame under the DM's MLS lock and post it like any envelope.
async fn deliver_batch(
    state: &Arc<AppState>,
    user_id: &str,
    conversation_id: &str,
    batch: &OwedBatch,
) -> Result<Sent> {
    let envelope_id = ulid::Ulid::new().to_string();
    let Some(delete_token_hash) =
        super::delete_capability::envelope_delete_token_hash(state, &envelope_id).await
    else {
        return Err(Error::Other(anyhow::anyhow!(
            "locked: no delete capability for the receipt envelope"
        )));
    };
    // A delivered receipt is stamped now; a read one carries the reader's own time (#2140).
    let at = batch
        .read_at
        .clone()
        .unwrap_or_else(super::envelope_sent_at);
    let plaintext = super::framing::pad_receipt(batch.kind, &at, &batch.message_ids);
    // A DM's MLS group is keyed by the conversation id directly. The opt-out is read under the
    // same lock as the seal, so the decision and the act cannot straddle a preference change.
    let sealed = super::seal::with_sealing_lock(state, conversation_id, |conn, held| {
        if !receipts_enabled(conn) {
            return Ok(None);
        }
        super::seal::seal_under_lock(conn, conversation_id, conversation_id, &plaintext, held).map(Some)
    })
    .await?;
    let Some(sealed) = sealed else {
        return Ok(Sent::OptedOut);
    };
    // Sealed sender (#607) and a delete capability (#1086), like every envelope. It will come
    // back through this device's own fetch, undecryptable here (#1312): `post_resealing`
    // records it as this device's own first.
    super::seal::post_resealing(
        state,
        super::seal::Target {
            mls_group_id: conversation_id,
            conversation_id,
            user_id,
            plaintext: &plaintext,
            envelope_id: &envelope_id,
        },
        sealed,
        // A receipt wakes nobody: it acknowledges something the recipient already has (#987).
        |sealed| {
            super::seal::control_envelope(
                &envelope_id,
                conversation_id,
                sealed,
                Some(delete_token_hash.clone()),
            )
        },
        |_conn, _sealed| Ok(()),
    )
    .await?;

    // Wake the peers. Deliberately the same `new_message` wake as a message (#1628): realtime
    // data packets are not end-to-end encrypted (RoomOptions::default()), so a distinct type
    // would tell the LiveKit server which wakes are receipts. A receiver's ingest reports that
    // it stored no news, so the wake neither alerts nor counts.
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
    Ok(Sent::Landed)
}

/// Send the delivered receipts a catch-up pass owes, for each of `conversations` (with what it
/// just stored there, see [`drain_owed`]'s `unrecorded`). Failures are logged: what is owed
/// stays owed.
///
/// Type-erased: a receipt's re-seal runs a catch-up whose tail calls this again, and the
/// compiler can only prove that recursive future `Send` across a boundary like this one.
pub(crate) fn send_owed<'a>(
    state: &'a Arc<AppState>,
    user_id: &'a str,
    conversations: &'a [(String, Vec<String>)],
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        for (conversation_id, stored) in conversations {
            if let Err(e) = drain_owed(state, user_id, conversation_id, stored).await {
                crate::diag!("[receipts] receipts for {conversation_id} not sent yet: {e}");
            }
        }
    })
}

/// Mark messages as **actually read by the human** and send a read receipt.
///
/// Called only for messages genuinely on screen in a focused window.
///
/// Only others' messages this device holds are acknowledged. The receipt is owed before this
/// returns, then sent through [`drain_owed`] in the background; a failure to send leaves it
/// owed, so this never fails for it.
pub async fn mark_messages_read(
    conversation_id: String,
    user_id: String,
    message_ids: Vec<String>,
    state: &Arc<AppState>,
) -> Result<()> {
    if message_ids.is_empty() {
        return Ok(());
    }
    let read_at = super::envelope_sent_at();
    {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        let mut to_ack = Vec::new();
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
                to_ack.push(id.clone());
            }
        }
        if to_ack.is_empty() {
            return Ok(());
        }
        owe_read_on(db.conn(), &conversation_id, &to_ack, &read_at)?;
    }

    // Owed now; sent in its own task, so marking never waits behind a committer holding the
    // DM's MLS lock (the seal needs it). A drain that never runs leaves it owed for the next.
    let task_state = Arc::clone(state);
    state.spawn_background(async move {
        if let Err(e) = drain_owed(&task_state, &user_id, &conversation_id, &[]).await {
            crate::diag!("[receipts] read receipt for {conversation_id} not sent yet: {e}");
        }
    });
    Ok(())
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
    conversation_receipts(db.conn(), &conversation_id)
}

/// The query behind [`get_conversation_receipts`]: nothing while receipts are off, else every
/// receipt grouped by message, with each reader's read time.
fn conversation_receipts(
    conn: &rusqlite::Connection,
    conversation_id: &str,
) -> Result<Vec<MessageReceipts>> {
    if !receipts_enabled(conn) {
        return Ok(Vec::new());
    }

    let mut stmt = conn.prepare(
        "SELECT r.message_id, r.reader_id, r.kind, r.at
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
            row.get::<_, String>(3)?,
        ))
    })?;

    // Preserve message order while grouping.
    let mut out: Vec<MessageReceipts> = Vec::new();
    for row in rows {
        let (message_id, reader_id, kind, at) = row?;
        if out.last().map(|r| r.message_id.as_str()) != Some(message_id.as_str()) {
            out.push(MessageReceipts {
                message_id: message_id.clone(),
                delivered_by: Vec::new(),
                read_by: Vec::new(),
                read_at: BTreeMap::new(),
            });
        }
        let entry = out
            .last_mut()
            .expect("just pushed when the group changed");
        match kind.as_str() {
            "delivered" => entry.delivered_by.push(reader_id),
            "read" => {
                entry.read_at.insert(reader_id.clone(), at);
                entry.read_by.push(reader_id);
            }
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

    fn owed_rows(conn: &rusqlite::Connection) -> Vec<(String, String, String, Option<String>)> {
        let mut stmt = conn
            .prepare(
                "SELECT conversation_id, message_id, kind, read_at FROM receipt_owed
                 ORDER BY conversation_id, kind, message_id",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    /// An owed read receipt always has the reader's time and a delivered one never does: the
    /// table refuses either shape, so a frame can never go out with no time or a wrong one.
    #[test]
    fn an_owed_receipt_without_its_time_is_unrepresentable() {
        let db = db();
        for (kind, read_at) in [("read", None), ("delivered", Some("t0")), ("seen-ish", None)] {
            let err = db.conn().execute(
                "INSERT INTO receipt_owed (conversation_id, message_id, kind, read_at)
                 VALUES ('dm1', 'm1', ?1, ?2)",
                rusqlite::params![kind, read_at],
            );
            assert!(err.is_err(), "{kind} with read_at {read_at:?} must not be storable");
        }
    }

    /// A stored message owes its delivered receipt once, whatever re-stores it; the opt-out owes
    /// nothing.
    #[test]
    fn a_delivered_receipt_is_owed_once_and_not_while_opted_out() {
        let db = db();
        owe_delivered_on(db.conn(), "dm1", "m1");
        owe_delivered_on(db.conn(), "dm1", "m1");
        assert_eq!(
            owed_rows(db.conn()),
            vec![("dm1".into(), "m1".into(), "delivered".into(), None)]
        );
        set_pref(db.conn(), r#"{"send_read_receipts":false}"#);
        owe_delivered_on(db.conn(), "dm1", "m2");
        owe_read_on(db.conn(), "dm1", &["m1".to_string()], "t1").unwrap();
        assert_eq!(owed_rows(db.conn()).len(), 1, "nothing is owed while opted out");
    }

    /// What is owed goes out as frames: delivered together, each read time on its own, and no
    /// frame over the cap. Clearing one frame's rows leaves the rest owed.
    #[test]
    fn owed_receipts_are_sent_as_bounded_frames_and_cleared_per_frame() {
        let db = db();
        let many: Vec<String> = (0..MAX_IDS_PER_RECEIPT + 3).map(|i| format!("m{i:04}")).collect();
        for id in &many {
            owe_delivered_on(db.conn(), "dm1", id);
        }
        owe_read_on(db.conn(), "dm1", &["r1".to_string(), "r2".to_string()], "t-early").unwrap();
        owe_read_on(db.conn(), "dm1", &["r3".to_string()], "t-late").unwrap();
        owe_delivered_on(db.conn(), "dm2", "other");

        let batches = owed_batches_on(db.conn(), "dm1").unwrap();
        let shape: Vec<(ReceiptKind, Option<&str>, usize)> = batches
            .iter()
            .map(|b| (b.kind, b.read_at.as_deref(), b.message_ids.len()))
            .collect();
        assert_eq!(
            shape,
            vec![
                (ReceiptKind::Delivered, None, MAX_IDS_PER_RECEIPT),
                (ReceiptKind::Delivered, None, 3),
                (ReceiptKind::Read, Some("t-early"), 2),
                (ReceiptKind::Read, Some("t-late"), 1),
            ]
        );

        settle_owed_on(db.conn(), "dm1", &batches[2]).unwrap();
        let left = owed_batches_on(db.conn(), "dm1").unwrap();
        assert_eq!(left.len(), 3, "only the landed frame's rows are cleared");
        assert!(left.iter().all(|b| b.read_at.as_deref() != Some("t-early")));

        forget_owed_on(db.conn(), "dm1").unwrap();
        assert!(owed_batches_on(db.conn(), "dm1").unwrap().is_empty());
        assert_eq!(owed_batches_on(db.conn(), "dm2").unwrap().len(), 1, "other DMs untouched");
    }

    /// A database the app has not opened since the upgrade (an extension's open) has no table:
    /// storing a message still works, and nothing claims to be owed.
    #[test]
    fn a_database_without_the_table_still_stores_and_owes_nothing() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::local::apply_local_schema(&conn).unwrap();
        assert!(!owed_table_exists(&conn));
        owe_delivered_on(&conn, "dm1", "m1");
        assert!(!owed_table_exists(&conn));
    }

    /// A drain asked for while one runs does not start a second; the running one goes round
    /// again, and the turn ends when it is dropped, however it ended.
    #[test]
    fn a_drain_asked_for_while_one_runs_makes_it_go_round_again() {
        let drains = ReceiptDrains::default();
        let turn = drains.begin("dm1").expect("the first drain runs");
        assert!(drains.begin("dm1").is_none(), "a second drain never runs beside it");
        assert!(drains.begin("dm2").is_some(), "other conversations drain freely");
        assert!(turn.again(), "the request made meanwhile is not lost");
        assert!(!turn.again(), "and is taken once");
        drop(turn);
        assert!(drains.begin("dm1").is_some(), "a dropped turn frees the conversation");
    }

    /// The read time is the read receipt's own timestamp, one per reader, and only for readers
    /// who read it (#2140).
    #[test]
    fn listed_receipts_carry_each_readers_read_time() {
        let db = db();
        for id in ["m1", "m2"] {
            db.conn()
                .execute(
                    "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at)
                     VALUES (?1, 'dm1', 'alice', x'00', 'hi', '2026-11-01T10:00:00Z')",
                    rusqlite::params![id],
                )
                .unwrap();
        }
        let dm = |message_id, reader_id, kind, at| {
            record_receipt(db.conn(), message_id, reader_id, kind, at, true);
        };
        dm("m1", "bob", ReceiptKind::Delivered, "2026-11-01T10:00:05Z");
        dm("m1", "bob", ReceiptKind::Read, "2026-11-01T10:03:00Z");
        dm("m1", "carol", ReceiptKind::Delivered, "2026-11-01T10:00:06Z");
        dm("m2", "bob", ReceiptKind::Delivered, "2026-11-01T10:04:00Z");

        let listed = conversation_receipts(db.conn(), "dm1").unwrap();
        assert_eq!(listed.len(), 2);
        let m1 = listed.iter().find(|r| r.message_id == "m1").unwrap();
        assert_eq!(m1.read_by, vec!["bob".to_string()]);
        assert_eq!(
            m1.read_at,
            BTreeMap::from([("bob".to_string(), "2026-11-01T10:03:00Z".to_string())]),
            "the read time is the read receipt's own timestamp, not the delivery's",
        );
        let m2 = listed.iter().find(|r| r.message_id == "m2").unwrap();
        assert!(m2.read_by.is_empty());
        assert!(m2.read_at.is_empty(), "a delivered-only message has no time");

        // On the wire to the renderer, a reader id maps to its time.
        let json = serde_json::to_value(m1).unwrap();
        assert_eq!(json["read_at"]["bob"], "2026-11-01T10:03:00Z");

        // The opt-out lists nothing at all.
        set_pref(db.conn(), r#"{"send_read_receipts":false}"#);
        assert!(conversation_receipts(db.conn(), "dm1").unwrap().is_empty());
    }
}
