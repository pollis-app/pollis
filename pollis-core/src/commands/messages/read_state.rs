//! Where the human has read up to, and the unread counts derived from it (#844).
//!
//! # Why a cursor and not a count
//!
//! Unread state used to live in one in-memory MobX field whose only writer was
//! the live-event dispatcher. Two consequences, both of which made users miss
//! messages — the failure mode the product promise is written against:
//!
//!   * a restart marked every conversation read, because the field started
//!     empty and nothing rehydrated it;
//!   * anything that arrived while the app was closed never badged, because no
//!     live event fired for it.
//!
//! A count cannot be persisted honestly: it records *how many* without
//! recording *which*, so it cannot be reconciled against the messages actually
//! on disk, nor merged with another device's idea of the same number. A cursor
//! can. Ingest already writes every message it accepts to the local `message`
//! table, so once the read position is durable the count is a query — and a
//! message that arrived while the app was shut is counted for free.
//!
//! # The position
//!
//! `(last_read_at, last_read_message_id)`, compared as a row value so it
//! matches the `ORDER BY sent_at DESC, id DESC` the message list itself uses.
//! `sent_at` alone is sender-supplied and two messages can carry the same one.
//!
//! # Why the cursor only moves forward
//!
//! [`advance_read_cursor`] refuses to move a cursor backwards, and that single
//! rule is what makes the multi-device story tractable. Merging two devices'
//! cursors is `max()`: commutative, idempotent, and order-independent, so there
//! is no lock, no last-writer-wins race and no conflict to resolve. Replaying a
//! stale update is a no-op rather than a regression that would re-badge
//! messages the human already read.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit};
use base64::Engine as _;
use hkdf::Hkdf;
use rand::rngs::OsRng;
use rand::RngCore;
use rusqlite::{Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::error::{Error, Result};
use crate::state::AppState;

/// One conversation's unread tally.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnreadCount {
    pub conversation_id: String,
    pub count: i64,
}

/// A read position, as stored and as synced.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReadCursor {
    pub conversation_id: String,
    pub last_read_at: String,
    pub last_read_message_id: String,
}

/// Is `candidate` strictly newer than `current`?
///
/// The whole ordering rule in one place, so the SQL, the merge and the tests
/// cannot drift apart. Mirrors `ORDER BY sent_at DESC, id DESC`.
pub(crate) fn is_newer(candidate: (&str, &str), current: (&str, &str)) -> bool {
    candidate > current
}

/// Move a conversation's cursor to `(at, message_id)`, or leave it alone if
/// that is not strictly newer than where it already is.
///
/// Returns whether the cursor actually moved, which is what lets a caller skip
/// a pointless sync push.
pub(crate) fn advance_read_cursor(
    conn: &Connection,
    conversation_id: &str,
    at: &str,
    message_id: &str,
) -> Result<bool> {
    let current: Option<(String, String)> = conn
        .query_row(
            "SELECT last_read_at, last_read_message_id FROM read_cursor
             WHERE conversation_id = ?1",
            rusqlite::params![conversation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| Error::Other(anyhow::anyhow!("read cursor lookup: {e}")))?;

    if let Some((cur_at, cur_id)) = &current {
        if !is_newer((at, message_id), (cur_at.as_str(), cur_id.as_str())) {
            return Ok(false);
        }
    }

    conn.execute(
        "INSERT INTO read_cursor (conversation_id, last_read_at, last_read_message_id, updated_at)
         VALUES (?1, ?2, ?3, datetime('now'))
         ON CONFLICT(conversation_id) DO UPDATE SET
             last_read_at         = excluded.last_read_at,
             last_read_message_id = excluded.last_read_message_id,
             updated_at           = excluded.updated_at",
        rusqlite::params![conversation_id, at, message_id],
    )
    .map_err(|e| Error::Other(anyhow::anyhow!("advance read cursor: {e}")))?;
    Ok(true)
}

/// Every cursor this device holds — the payload the cross-device sync encrypts.
pub(crate) fn all_read_cursors(conn: &Connection) -> Result<Vec<ReadCursor>> {
    let mut stmt = conn
        .prepare(
            "SELECT conversation_id, last_read_at, last_read_message_id
             FROM read_cursor ORDER BY conversation_id",
        )
        .map_err(|e| Error::Other(anyhow::anyhow!("prepare cursors: {e}")))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ReadCursor {
                conversation_id: row.get(0)?,
                last_read_at: row.get(1)?,
                last_read_message_id: row.get(2)?,
            })
        })
        .map_err(|e| Error::Other(anyhow::anyhow!("query cursors: {e}")))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| Error::Other(anyhow::anyhow!("row: {e}")))?);
    }
    Ok(out)
}

/// Apply another device's cursors, keeping whichever position is newer.
///
/// Returns how many actually moved this device forward.
pub(crate) fn merge_read_cursors(conn: &Connection, incoming: &[ReadCursor]) -> Result<usize> {
    let mut moved = 0;
    for c in incoming {
        if advance_read_cursor(
            conn,
            &c.conversation_id,
            &c.last_read_at,
            &c.last_read_message_id,
        )? {
            moved += 1;
        }
    }
    Ok(moved)
}

/// Unread counts for every conversation this device holds messages for.
///
/// A conversation with nothing unread is omitted rather than reported as zero,
/// so the caller can treat the result as the complete set of badges to show.
///
/// Three exclusions, all deliberate:
///   * **own messages** — reading your own is not a signal, and counting them
///     would badge every conversation you just spoke in;
///   * **deleted messages** — a redacted message must not hold a badge open
///     that the user can never clear by reading;
///   * **no cursor yet** — everything from someone else counts, which is what
///     makes a conversation that arrived entirely while offline badge correctly
///     on first launch.
pub(crate) fn unread_counts(conn: &Connection, user_id: &str) -> Result<Vec<UnreadCount>> {
    let mut stmt = conn
        .prepare(
            "SELECT m.conversation_id, COUNT(*)
             FROM message m
             LEFT JOIN read_cursor c ON c.conversation_id = m.conversation_id
             WHERE m.sender_id != ?1
               AND m.deleted_at IS NULL
               AND (
                     c.conversation_id IS NULL
                     OR (m.sent_at, m.id) > (c.last_read_at, c.last_read_message_id)
                   )
             GROUP BY m.conversation_id
             HAVING COUNT(*) > 0
             ORDER BY m.conversation_id",
        )
        .map_err(|e| Error::Other(anyhow::anyhow!("prepare unread: {e}")))?;
    let rows = stmt
        .query_map(rusqlite::params![user_id], |row| {
            Ok(UnreadCount {
                conversation_id: row.get(0)?,
                count: row.get(1)?,
            })
        })
        .map_err(|e| Error::Other(anyhow::anyhow!("query unread: {e}")))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| Error::Other(anyhow::anyhow!("row: {e}")))?);
    }
    Ok(out)
}

// ── Commands ─────────────────────────────────────────────────────────────────

/// Unread counts to hydrate the UI with at startup.
pub async fn get_unread_counts(user_id: String, state: &Arc<AppState>) -> Result<Vec<UnreadCount>> {
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    unread_counts(db.conn(), &user_id)
}

/// Mark a conversation read up to the newest message this device holds for it.
///
/// The caller names the CONVERSATION, never a position. The renderer knows when
/// the human opened a conversation; it has no business asserting which message
/// that corresponds to, and letting it try would make a whole class of bugs
/// representable — a cursor past a message that does not exist, a cursor
/// derived from a partially-loaded page, a fast local clock marking unseen
/// messages read. Resolving the position here from the rows ingest already
/// stored means the only cursor that can ever be written is one this device can
/// point at an actual message for.
///
/// Returns whether the cursor moved. A conversation with nothing from anyone
/// else is a no-op rather than an error.
pub async fn mark_conversation_read(
    conversation_id: String,
    user_id: String,
    state: &Arc<AppState>,
) -> Result<bool> {
    let moved = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        mark_conversation_read_in(db.conn(), &conversation_id, &user_id)?
    };
    // Only a cursor that actually moved is worth telling the other devices
    // about. Re-opening a conversation you have already read all of is the
    // common case and produces no write at all.
    if moved {
        schedule_read_cursor_push(state, &user_id);
    }
    Ok(moved)
}

/// The body of [`mark_conversation_read`], against a bare connection so it is
/// testable without an [`AppState`].
pub(crate) fn mark_conversation_read_in(
    conn: &Connection,
    conversation_id: &str,
    user_id: &str,
) -> Result<bool> {
    // The newest message from somebody else, by the same ordering the message
    // list uses. Own and deleted messages are excluded for the same reason
    // `unread_counts` excludes them: a cursor parked on one would be a position
    // the unread query can never agree with.
    let newest: Option<(String, String)> = conn
        .query_row(
            "SELECT sent_at, id FROM message
             WHERE conversation_id = ?1 AND sender_id != ?2 AND deleted_at IS NULL
             ORDER BY sent_at DESC, id DESC LIMIT 1",
            rusqlite::params![conversation_id, user_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| Error::Other(anyhow::anyhow!("newest message lookup: {e}")))?;

    match newest {
        Some((at, id)) => advance_read_cursor(conn, conversation_id, &at, &id),
        None => Ok(false),
    }
}

// ── Cross-device sync ────────────────────────────────────────────────────────
//
// # Why this is encrypted when preferences are not
//
// `commands::user` syncs preferences as PLAINTEXT JSON in Turso's
// `user_preferences.preferences`, and that is a defensible trade: the operator
// learns a theme name and a font size. A read cursor is not that. It is
// `(conversation_id, last_read_at)` per conversation, rewritten every time a
// human opens a conversation — a precise, timestamped log of WHICH conversations
// this person reads and WHEN. Stored in the clear it would be a behavioural
// profile strictly richer than the metadata the threat model already concedes:
// an envelope row says a message was delivered; a plaintext cursor would say a
// human sat down and read it, at 02:14, in that conversation and not the other
// one. `docs/metadata-minimization-design.md` is the whole argument for not
// handing the server that; syncing it as JSON would undo it in one column.
//
// # The key
//
// HKDF-SHA256 over the **account identity key**, which represents the human
// rather than any one device: every device of the account already holds a copy
// (transferred at enrollment) and the DS has never held it in the clear. So
// every device derives the same key with no coordination, and the server derives
// nothing. Then AES-256-GCM — the same construction as the `account_recovery`
// wrap in `commands::account_identity`, with a different `info` string so the one
// key cannot be repurposed across the two uses.
//
// The HKDF salt is the user id, not the random per-blob salt `account_recovery`
// uses. That is the one deliberate divergence: a random salt would have to be
// fetched and agreed before any device could decrypt, and the reason
// `account_recovery` needs one — stretching a user-typed Secret Key — does not
// apply to a 32-byte uniformly-random seed. A per-account non-secret salt still
// gives domain separation between accounts.
//
// # What the server still learns, stated plainly
//
// That this user's blob CHANGED, roughly HOW BIG it is, and WHEN. Size is blunted
// (not erased) by padding the plaintext to the same buckets message framing uses,
// so a burst of conversations does not step the length one triple at a time.
// Timing is inherent: a sync that must converge has to write when something
// changes. Debouncing collapses a reading session into one write rather than one
// per message, which is why it is a correctness-adjacent feature and not a
// nicety.

/// HKDF info string — binds this key to read-cursor sync so the account identity
/// key cannot be repurposed between this and the `account_recovery` wrap.
const READ_CURSOR_HKDF_INFO: &[u8] = b"pollis-read-cursor-sync-v1";

const READ_CURSOR_NONCE_LEN: usize = 12;

/// How long a burst of cursor advances is collapsed into one DS write.
///
/// Reading a busy channel advances the cursor once per message rendered; without
/// this, opening it would be one signed HTTP write per message — a write
/// amplification the DS sees, an obvious per-message timing signal, and a
/// pointless battery cost. Five seconds is long enough to swallow a scroll and
/// short enough that a second device picks the position up while the human is
/// still in front of it.
const PUSH_DEBOUNCE: Duration = Duration::from_secs(5);

/// Is a debounced push already scheduled?
///
/// Process-global rather than per-`AppState` because the desktop app has exactly
/// one signed-in account per process, and the push reads whatever is in the
/// cursor table at the moment it fires — so a second advance arriving during the
/// wait is carried by the push already scheduled and needs no second one. This is
/// a **one-shot timer per burst**, not a poll: nothing is scheduled when nothing
/// changes, which is what `frontend/tests/no-periodic-polling.test.ts` is about.
static PUSH_SCHEDULED: AtomicBool = AtomicBool::new(false);

/// Derive this account's cursor-sync key.
fn derive_cursor_key(account_seed: &[u8], user_id: &str) -> Zeroizing<[u8; 32]> {
    let hk = Hkdf::<Sha256>::new(Some(user_id.as_bytes()), account_seed);
    let mut out = Zeroizing::new([0u8; 32]);
    hk.expand(READ_CURSOR_HKDF_INFO, out.as_mut())
        .expect("HKDF-SHA256 expand 32 bytes is always valid");
    out
}

/// Encrypt a cursor list into the `(nonce, ciphertext)` the DS stores.
///
/// The plaintext is padded to a size bucket BEFORE encryption — reusing
/// [`super::framing::pad`] rather than a second padding scheme, so there is one
/// bucket ladder in the codebase and one place to reason about it.
pub(crate) fn seal_cursors(
    account_seed: &[u8],
    user_id: &str,
    cursors: &[ReadCursor],
) -> Result<(Vec<u8>, Vec<u8>)> {
    let json = serde_json::to_vec(cursors)
        .map_err(|e| Error::Other(anyhow::anyhow!("serialize read cursors: {e}")))?;
    let padded = super::framing::pad(&json);

    let key = derive_cursor_key(account_seed, user_id);
    let mut nonce = [0u8; READ_CURSOR_NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);

    let cipher = Aes256Gcm::new(GenericArray::from_slice(key.as_ref()));
    let ciphertext = cipher
        .encrypt(GenericArray::from_slice(&nonce), padded.as_slice())
        .map_err(|e| Error::Crypto(format!("seal read cursors: {e}")))?;
    Ok((nonce.to_vec(), ciphertext))
}

/// Recover a cursor list from what the DS handed back.
pub(crate) fn open_cursors(
    account_seed: &[u8],
    user_id: &str,
    nonce: &[u8],
    ciphertext: &[u8],
) -> Result<Vec<ReadCursor>> {
    if nonce.len() != READ_CURSOR_NONCE_LEN {
        return Err(Error::Crypto(format!(
            "read-cursor nonce has wrong length: {} (expected {READ_CURSOR_NONCE_LEN})",
            nonce.len()
        )));
    }
    let key = derive_cursor_key(account_seed, user_id);
    let cipher = Aes256Gcm::new(GenericArray::from_slice(key.as_ref()));
    let padded = cipher
        .decrypt(GenericArray::from_slice(nonce), ciphertext)
        .map_err(|e| Error::Crypto(format!("open read cursors: {e}")))?;
    let json = super::framing::strip(&padded);
    serde_json::from_slice(&json)
        .map_err(|e| Error::Other(anyhow::anyhow!("parse read cursors: {e}")))
}

/// This device's copy of the account identity key seed, as HKDF input material.
async fn account_seed(state: &Arc<AppState>, user_id: &str) -> Result<Zeroizing<Vec<u8>>> {
    let key = crate::commands::account_identity::load_account_id_key(state, user_id).await?;
    Ok(Zeroizing::new(key.to_seed().to_vec()))
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// Upload every cursor this device holds, encrypted.
///
/// The whole set every time rather than a delta: the payload is small, the merge
/// on the other side is a `max()` that does not care whether it has seen an entry
/// before, and a delta protocol would need an ordering the server cannot supply
/// without reading the blob.
pub(crate) async fn push_read_cursors(state: &Arc<AppState>, user_id: &str) -> Result<()> {
    let cursors = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        all_read_cursors(db.conn())?
    };
    // Nothing read yet on this device. Pushing an empty list would be harmless
    // (the merge is a max) but would overwrite a blob another device wrote with
    // real positions in it, costing a round trip to get them back.
    if cursors.is_empty() {
        return Ok(());
    }

    let seed = account_seed(state, user_id).await?;
    let (nonce, blob) = seal_cursors(&seed, user_id, &cursors)?;
    let body = pollis_api::profile::SaveReadCursorsBody {
        user_id: user_id.to_string(),
        nonce: b64(&nonce),
        blob: b64(&blob),
    };
    crate::commands::mls::ds_post_ok(state, &body).await
}

/// Fetch the account's blob and merge the positions it carries into this device.
///
/// Returns how many local cursors actually moved.
pub(crate) async fn pull_read_cursors(state: &Arc<AppState>, user_id: &str) -> Result<usize> {
    let resp = crate::commands::ds_reads::read_cursors(state, user_id).await?;
    let Some(stored) = resp.cursors else {
        return Ok(0);
    };
    let nonce = crate::commands::ds_reads::decode_b64("read-cursor nonce", &stored.nonce)?;
    let blob = crate::commands::ds_reads::decode_b64("read-cursor blob", &stored.blob)?;

    let seed = account_seed(state, user_id).await?;
    let incoming = open_cursors(&seed, user_id, &nonce, &blob)?;

    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
    merge_read_cursors(db.conn(), &incoming)
}

/// Queue a debounced push, unless one is already queued.
///
/// Fire-and-forget by design: a cursor that failed to reach the DS is a badge
/// that clears late on the *other* device, never a message lost or a local state
/// that is wrong, so it must not fail the call that marked the conversation read.
/// The next advance schedules another attempt — lazy recovery, no retry helper,
/// no timer that runs when nothing happened.
fn schedule_read_cursor_push(state: &Arc<AppState>, user_id: &str) {
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    if PUSH_SCHEDULED.swap(true, Ordering::SeqCst) {
        return;
    }
    let state = Arc::clone(state);
    let user_id = user_id.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(PUSH_DEBOUNCE).await;
        // Cleared BEFORE the push reads the table, so an advance that lands
        // during the upload schedules the next one instead of being swallowed.
        PUSH_SCHEDULED.store(false, Ordering::SeqCst);
        if let Err(e) = push_read_cursors(&state, &user_id).await {
            eprintln!("[read-cursor] push failed (will retry on the next advance): {e}");
        }
    });
}

/// Pull the other devices' read positions, merge them, and push the result.
///
/// Called once when the app comes up and whenever the client resyncs. Both
/// directions in one command because they are two halves of one convergence: a
/// device that only pulled would never publish what it read while it was the only
/// one running.
pub async fn sync_read_cursors(user_id: String, state: &Arc<AppState>) -> Result<usize> {
    let merged = pull_read_cursors(state, &user_id).await?;
    push_read_cursors(state, &user_id).await?;
    Ok(merged)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ME: &str = "u-me";
    const THEM: &str = "u-them";
    const CONV: &str = "c1";

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::db::local::apply_local_schema(&conn).unwrap();
        conn
    }

    /// `sent_at` values are ordered strings, so the fixtures read in order.
    fn msg(conn: &Connection, id: &str, sender: &str, sent_at: &str) {
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at)
             VALUES (?1, ?2, ?3, X'00', 'body', ?4)",
            rusqlite::params![id, CONV, sender, sent_at],
        )
        .unwrap();
    }

    fn count(conn: &Connection) -> i64 {
        unread_counts(conn, ME)
            .unwrap()
            .iter()
            .find(|u| u.conversation_id == CONV)
            .map(|u| u.count)
            .unwrap_or(0)
    }

    // The reported bug, from the other side: messages that arrived while the
    // app was shut have no cursor, and must all badge on first launch.
    #[test]
    fn with_no_cursor_every_message_from_someone_else_is_unread() {
        let conn = db();
        msg(&conn, "m1", THEM, "2026-01-01T00:00:01Z");
        msg(&conn, "m2", THEM, "2026-01-01T00:00:02Z");
        assert_eq!(count(&conn), 2);
    }

    #[test]
    fn reading_your_own_messages_is_not_a_signal() {
        let conn = db();
        msg(&conn, "m1", ME, "2026-01-01T00:00:01Z");
        msg(&conn, "m2", ME, "2026-01-01T00:00:02Z");
        assert_eq!(count(&conn), 0, "your own messages badged the conversation");
    }

    // A redacted message must not hold a badge open that reading cannot clear.
    #[test]
    fn deleted_messages_do_not_hold_a_badge_open() {
        let conn = db();
        msg(&conn, "m1", THEM, "2026-01-01T00:00:01Z");
        conn.execute(
            "UPDATE message SET deleted_at = '2026-01-01T00:00:05Z' WHERE id = 'm1'",
            [],
        )
        .unwrap();
        assert_eq!(count(&conn), 0);
    }

    #[test]
    fn advancing_the_cursor_clears_what_it_passes() {
        let conn = db();
        msg(&conn, "m1", THEM, "2026-01-01T00:00:01Z");
        msg(&conn, "m2", THEM, "2026-01-01T00:00:02Z");
        msg(&conn, "m3", THEM, "2026-01-01T00:00:03Z");
        assert_eq!(count(&conn), 3);

        assert!(advance_read_cursor(&conn, CONV, "2026-01-01T00:00:02Z", "m2").unwrap());
        assert_eq!(count(&conn), 1, "only the message past the cursor is unread");
    }

    /// The invariant the whole design rests on. If a cursor could move
    /// backwards, a stale device replaying an old position would re-badge
    /// messages the human has already read — and multi-device merge would stop
    /// being order-independent.
    #[test]
    fn a_cursor_never_moves_backwards() {
        let conn = db();
        msg(&conn, "m1", THEM, "2026-01-01T00:00:01Z");
        msg(&conn, "m2", THEM, "2026-01-01T00:00:02Z");

        assert!(advance_read_cursor(&conn, CONV, "2026-01-01T00:00:02Z", "m2").unwrap());
        assert_eq!(count(&conn), 0);

        // An older position, and the same position again.
        assert!(!advance_read_cursor(&conn, CONV, "2026-01-01T00:00:01Z", "m1").unwrap());
        assert!(!advance_read_cursor(&conn, CONV, "2026-01-01T00:00:02Z", "m2").unwrap());
        assert_eq!(count(&conn), 0, "a stale update re-badged read messages");
    }

    /// Two messages can share a `sent_at` — it is sender-supplied. The cursor
    /// compares the id as well, exactly as the message list sorts.
    #[test]
    fn messages_sharing_a_timestamp_are_ordered_by_id() {
        let conn = db();
        msg(&conn, "m-a", THEM, "2026-01-01T00:00:01Z");
        msg(&conn, "m-b", THEM, "2026-01-01T00:00:01Z");
        assert_eq!(count(&conn), 2);

        advance_read_cursor(&conn, CONV, "2026-01-01T00:00:01Z", "m-a").unwrap();
        assert_eq!(count(&conn), 1, "the tie was not broken by id");
    }

    #[test]
    fn a_conversation_with_nothing_unread_is_absent_rather_than_zero() {
        let conn = db();
        msg(&conn, "m1", ME, "2026-01-01T00:00:01Z");
        assert!(unread_counts(&conn, ME).unwrap().is_empty());
    }

    // ── Marking a conversation read ──────────────────────────────────────────

    #[test]
    fn opening_a_conversation_clears_it() {
        let conn = db();
        msg(&conn, "m1", THEM, "2026-01-01T00:00:01Z");
        msg(&conn, "m2", THEM, "2026-01-01T00:00:02Z");
        assert_eq!(count(&conn), 2);

        assert!(mark_conversation_read_in(&conn, CONV, ME).unwrap());
        assert_eq!(count(&conn), 0);
    }

    /// The cursor lands on the newest message from someone ELSE. Parking it on
    /// your own newer message would put it somewhere `unread_counts` — which
    /// skips own messages — can never agree with.
    #[test]
    fn the_cursor_lands_on_a_message_the_unread_query_also_counts() {
        let conn = db();
        msg(&conn, "m1", THEM, "2026-01-01T00:00:01Z");
        msg(&conn, "m2", ME, "2026-01-01T00:00:02Z");

        mark_conversation_read_in(&conn, CONV, ME).unwrap();
        let cursors = all_read_cursors(&conn).unwrap();
        assert_eq!(cursors[0].last_read_message_id, "m1");
        assert_eq!(count(&conn), 0);
    }

    #[test]
    fn a_conversation_with_nothing_from_anyone_else_is_a_no_op() {
        let conn = db();
        msg(&conn, "m1", ME, "2026-01-01T00:00:01Z");
        assert!(!mark_conversation_read_in(&conn, CONV, ME).unwrap());
        assert!(all_read_cursors(&conn).unwrap().is_empty());
    }

    /// Re-opening a conversation nothing has arrived in must not churn the
    /// cursor — that would push a pointless sync to every other device.
    #[test]
    fn reopening_an_already_read_conversation_does_not_move_the_cursor() {
        let conn = db();
        msg(&conn, "m1", THEM, "2026-01-01T00:00:01Z");
        assert!(mark_conversation_read_in(&conn, CONV, ME).unwrap());
        assert!(!mark_conversation_read_in(&conn, CONV, ME).unwrap());
    }

    /// A message that arrives after the conversation was read must badge again.
    #[test]
    fn a_new_arrival_after_reading_badges_again() {
        let conn = db();
        msg(&conn, "m1", THEM, "2026-01-01T00:00:01Z");
        mark_conversation_read_in(&conn, CONV, ME).unwrap();
        assert_eq!(count(&conn), 0);

        msg(&conn, "m2", THEM, "2026-01-01T00:00:02Z");
        assert_eq!(count(&conn), 1);
    }

    // ── Multi-device merge ───────────────────────────────────────────────────

    fn cursor(at: &str, id: &str) -> ReadCursor {
        ReadCursor {
            conversation_id: CONV.to_string(),
            last_read_at: at.to_string(),
            last_read_message_id: id.to_string(),
        }
    }

    /// Merge is `max()`, so the order two devices' updates arrive in cannot
    /// change where the cursor ends up. This is what removes the need for any
    /// locking or last-writer-wins tie-break in the sync path.
    #[test]
    fn merging_cursors_is_order_independent() {
        let older = cursor("2026-01-01T00:00:01Z", "m1");
        let newer = cursor("2026-01-01T00:00:09Z", "m9");

        let forward = db();
        merge_read_cursors(&forward, &[older.clone(), newer.clone()]).unwrap();

        let backward = db();
        merge_read_cursors(&backward, &[newer.clone(), older.clone()]).unwrap();

        assert_eq!(
            all_read_cursors(&forward).unwrap(),
            all_read_cursors(&backward).unwrap()
        );
        assert_eq!(all_read_cursors(&forward).unwrap(), vec![newer]);
    }

    #[test]
    fn merging_the_same_cursors_twice_changes_nothing() {
        let conn = db();
        let c = vec![cursor("2026-01-01T00:00:05Z", "m5")];
        assert_eq!(merge_read_cursors(&conn, &c).unwrap(), 1);
        assert_eq!(
            merge_read_cursors(&conn, &c).unwrap(),
            0,
            "a replayed sync moved the cursor again"
        );
    }

    #[test]
    fn a_merge_reports_only_the_cursors_that_actually_moved() {
        let conn = db();
        advance_read_cursor(&conn, CONV, "2026-01-01T00:00:05Z", "m5").unwrap();
        let incoming = vec![
            cursor("2026-01-01T00:00:01Z", "m1"),
            ReadCursor {
                conversation_id: "c2".into(),
                last_read_at: "2026-01-01T00:00:01Z".into(),
                last_read_message_id: "n1".into(),
            },
        ];
        assert_eq!(merge_read_cursors(&conn, &incoming).unwrap(), 1);
    }

    #[test]
    fn cursors_round_trip_through_the_sync_payload_shape() {
        let conn = db();
        advance_read_cursor(&conn, CONV, "2026-01-01T00:00:05Z", "m5").unwrap();
        advance_read_cursor(&conn, "c2", "2026-01-02T00:00:00Z", "n7").unwrap();

        let all = all_read_cursors(&conn).unwrap();
        let json = serde_json::to_string(&all).unwrap();
        let back: Vec<ReadCursor> = serde_json::from_str(&json).unwrap();

        let fresh = db();
        assert_eq!(merge_read_cursors(&fresh, &back).unwrap(), 2);
        assert_eq!(all_read_cursors(&fresh).unwrap(), all);
    }

    // ── Cross-device sync crypto ─────────────────────────────────────────────

    const SEED: &[u8; 32] = b"0123456789abcdef0123456789abcdef";

    fn sample() -> Vec<ReadCursor> {
        vec![
            ReadCursor {
                conversation_id: "01JCONVERSATIONALPHA0001".into(),
                last_read_at: "2026-01-01T00:00:05Z".into(),
                last_read_message_id: "m5".into(),
            },
            ReadCursor {
                conversation_id: "01JCONVERSATIONBRAVO0002".into(),
                last_read_at: "2026-01-02T00:00:00Z".into(),
                last_read_message_id: "n7".into(),
            },
        ]
    }

    #[test]
    fn a_sealed_cursor_list_opens_back_to_itself() {
        let cursors = sample();
        let (nonce, ct) = seal_cursors(SEED, ME, &cursors).unwrap();
        assert_eq!(nonce.len(), READ_CURSOR_NONCE_LEN);
        assert_eq!(open_cursors(SEED, ME, &nonce, &ct).unwrap(), cursors);
    }

    /// THE POINT OF THE WHOLE PHASE: what the DS stores must not name a
    /// conversation. Preferences sync as readable JSON; if this ever regressed
    /// to that, the server would hold a log of which conversations this human
    /// reads and when.
    #[test]
    fn the_bytes_the_server_stores_do_not_contain_a_conversation_id() {
        let cursors = sample();
        let (nonce, ct) = seal_cursors(SEED, ME, &cursors).unwrap();

        for c in &cursors {
            assert!(
                !contains(&ct, c.conversation_id.as_bytes()),
                "conversation id {} appears verbatim in the blob the DS stores",
                c.conversation_id
            );
            assert!(
                !contains(&ct, c.last_read_at.as_bytes()),
                "read timestamp {} appears verbatim in the blob the DS stores",
                c.last_read_at
            );
            assert!(
                !contains(&ct, c.last_read_message_id.as_bytes()),
                "message id {} appears verbatim in the blob the DS stores",
                c.last_read_message_id
            );
        }
        // Nor may the JSON framing leak — that would mean the payload was not
        // encrypted at all.
        assert!(!contains(&ct, b"conversation_id"));
        assert!(!contains(&nonce, b"conversation_id"));
    }

    /// The control for the test above: the same probe MUST be able to see a
    /// conversation id when one really is present. Without this,
    /// `the_bytes_the_server_stores_do_not_contain_a_conversation_id` would pass
    /// against a broken `contains` that always returned false.
    #[test]
    fn the_plaintext_probe_can_see_a_conversation_id() {
        let cursors = sample();
        let json = serde_json::to_vec(&cursors).unwrap();
        assert!(contains(&json, cursors[0].conversation_id.as_bytes()));
        assert!(contains(&json, b"conversation_id"));
    }

    /// Another account's key — or a tampered blob — must not decrypt. AES-GCM's
    /// tag is what makes "the DS cannot read it" also mean "the DS cannot
    /// forge it", which matters because a forged cursor would silently mark
    /// unread messages read.
    #[test]
    fn a_different_account_key_cannot_open_the_blob() {
        let (nonce, ct) = seal_cursors(SEED, ME, &sample()).unwrap();
        let other = b"ffffffffffffffffffffffffffffffff";
        assert!(open_cursors(other, ME, &nonce, &ct).is_err());
    }

    #[test]
    fn the_key_is_bound_to_the_user_and_to_this_use() {
        let (nonce, ct) = seal_cursors(SEED, ME, &sample()).unwrap();
        assert!(
            open_cursors(SEED, THEM, &nonce, &ct).is_err(),
            "the same seed under a different user id must not open the blob"
        );
        // The `info` string is what separates this key from the
        // `account_recovery` wrap that uses the SAME account seed.
        let mine = derive_cursor_key(SEED, ME);
        let hk = Hkdf::<Sha256>::new(Some(ME.as_bytes()), SEED.as_slice());
        let mut other = [0u8; 32];
        hk.expand(b"pollis-account-key-wrap-v1", &mut other).unwrap();
        assert_ne!(mine.as_ref(), &other);
    }

    #[test]
    fn a_tampered_blob_is_rejected_rather_than_silently_truncated() {
        let (nonce, mut ct) = seal_cursors(SEED, ME, &sample()).unwrap();
        ct[0] ^= 0x01;
        assert!(open_cursors(SEED, ME, &nonce, &ct).is_err());
    }

    #[test]
    fn a_wrong_length_nonce_is_refused_before_any_decrypt() {
        let (_, ct) = seal_cursors(SEED, ME, &sample()).unwrap();
        assert!(open_cursors(SEED, ME, &[0u8; 11], &ct).is_err());
    }

    /// Padding is what stops the blob's LENGTH from counting the user's
    /// conversations for the server. One cursor and several must land in the
    /// same bucket.
    #[test]
    fn small_cursor_lists_all_share_one_size_bucket() {
        let one = seal_cursors(SEED, ME, &sample()[..1]).unwrap().1;
        let two = seal_cursors(SEED, ME, &sample()).unwrap().1;
        let none = seal_cursors(SEED, ME, &[]).unwrap().1;
        assert_eq!(one.len(), two.len(), "cursor count leaked through the length");
        assert_eq!(none.len(), two.len(), "cursor count leaked through the length");
    }

    /// The control for the padding test: without padding these lengths DO
    /// differ, so the assertion above is testing something real.
    #[test]
    fn unpadded_cursor_lists_have_different_lengths() {
        let one = serde_json::to_vec(&sample()[..1]).unwrap();
        let two = serde_json::to_vec(&sample()).unwrap();
        assert_ne!(one.len(), two.len());
    }

    /// Two pushes of the SAME cursors must not produce the same ciphertext —
    /// a fixed nonce under a fixed key is the classic AES-GCM catastrophe, and
    /// it would also tell the server "nothing changed" for free.
    #[test]
    fn two_seals_of_the_same_cursors_differ() {
        let (n1, c1) = seal_cursors(SEED, ME, &sample()).unwrap();
        let (n2, c2) = seal_cursors(SEED, ME, &sample()).unwrap();
        assert_ne!(n1, n2);
        assert_ne!(c1, c2);
    }

    /// The end-to-end shape of a sync: device A seals, device B (same account,
    /// therefore same seed) opens and merges, and B's positions move forward.
    #[test]
    fn a_second_device_merges_what_the_first_one_sealed() {
        let a = db();
        advance_read_cursor(&a, CONV, "2026-01-01T00:00:05Z", "m5").unwrap();
        advance_read_cursor(&a, "c2", "2026-01-02T00:00:00Z", "n7").unwrap();
        let (nonce, ct) = seal_cursors(SEED, ME, &all_read_cursors(&a).unwrap()).unwrap();

        let b = db();
        advance_read_cursor(&b, CONV, "2026-01-01T00:00:01Z", "m1").unwrap();
        let incoming = open_cursors(SEED, ME, &nonce, &ct).unwrap();
        assert_eq!(merge_read_cursors(&b, &incoming).unwrap(), 2);
        assert_eq!(all_read_cursors(&b).unwrap(), all_read_cursors(&a).unwrap());

        // And it is idempotent: replaying the same blob moves nothing.
        assert_eq!(merge_read_cursors(&b, &incoming).unwrap(), 0);
    }

    /// A stale blob from a device that is behind must not rewind this device.
    #[test]
    fn a_stale_blob_from_another_device_cannot_rewind_this_one() {
        let stale = vec![ReadCursor {
            conversation_id: CONV.into(),
            last_read_at: "2026-01-01T00:00:01Z".into(),
            last_read_message_id: "m1".into(),
        }];
        let (nonce, ct) = seal_cursors(SEED, ME, &stale).unwrap();

        let here = db();
        advance_read_cursor(&here, CONV, "2026-01-01T00:00:09Z", "m9").unwrap();
        let incoming = open_cursors(SEED, ME, &nonce, &ct).unwrap();
        assert_eq!(merge_read_cursors(&here, &incoming).unwrap(), 0);
        assert_eq!(
            all_read_cursors(&here).unwrap()[0].last_read_message_id,
            "m9"
        );
    }

    /// Naive substring search over bytes — no `str` conversion, because the
    /// ciphertext is not valid UTF-8 and a lossy conversion could hide a match.
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|w| w == needle)
    }
}
