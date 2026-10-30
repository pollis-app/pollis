//! Where the human has read up to, and the unread counts derived from it (#844).
//!
//! The position `(last_read_at, last_read_message_id)` matches the message list's `sent_at DESC, id DESC`.
//! [`advance_read_cursor`] never moves backwards, so merging devices is a lock-free `max()`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aes_gcm::aead::generic_array::GenericArray;
use aes_gcm::aead::Aead;
use aes_gcm::{Aes256Gcm, KeyInit};
use crate::util::b64;
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
/// The one ordering rule; mirrors `ORDER BY sent_at DESC, id DESC`.
pub(crate) fn is_newer(candidate: (&str, &str), current: (&str, &str)) -> bool {
    candidate > current
}

/// Move a conversation's cursor to `(at, message_id)`, or leave it alone if
/// that is not strictly newer than where it already is.
///
/// Returns whether the cursor moved.
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
/// Returns the conversations whose cursor actually moved this device forward.
pub(crate) fn merge_read_cursors(conn: &Connection, incoming: &[ReadCursor]) -> Result<Vec<String>> {
    let mut moved = Vec::new();
    for c in incoming {
        if advance_read_cursor(
            conn,
            &c.conversation_id,
            &c.last_read_at,
            &c.last_read_message_id,
        )? {
            moved.push(c.conversation_id.clone());
        }
    }
    Ok(moved)
}

/// Does `local` hold a position `remote` lacks? Only then is a push worth a DS write (#1628):
/// pushing a set the account already has would rewrite it for nothing and nudge every device.
pub(crate) fn is_ahead_of(local: &[ReadCursor], remote: &[ReadCursor]) -> bool {
    let theirs: std::collections::HashMap<&str, (&str, &str)> = remote
        .iter()
        .map(|c| {
            (
                c.conversation_id.as_str(),
                (c.last_read_at.as_str(), c.last_read_message_id.as_str()),
            )
        })
        .collect();
    local.iter().any(|c| match theirs.get(c.conversation_id.as_str()) {
        None => true,
        Some(&at) => is_newer((&c.last_read_at, &c.last_read_message_id), at),
    })
}

/// Unread counts for every conversation this device holds messages for.
///
/// Conversations with nothing unread are omitted.
///
/// Excludes own and deleted messages; with no cursor yet, everything from others counts.
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

/// Of the messages an ingest just stored, the ones a wake may announce (#1628): per
/// conversation, the newest that is someone else's, not deleted, and past the read cursor.
///
/// Anything else a wake carries — a receipt, an edit, a replay of what is held, a message
/// already read on another device — announces nothing. Returns message ids, newest first.
pub(crate) fn newest_unread_of(
    conn: &Connection,
    user_id: &str,
    stored: &[(String, Vec<String>)],
) -> Result<Vec<String>> {
    let mut out: Vec<(String, String)> = Vec::new();
    for (conversation_id, ids) in stored {
        let mut best: Option<(String, String)> = None;
        // `?1` conversation, `?2` user, then the id list.
        for chunk in crate::db::chunk::bind_chunks(ids, 2) {
            let sql = format!(
                "SELECT m.sent_at, m.id
                 FROM message m
                 LEFT JOIN read_cursor c ON c.conversation_id = m.conversation_id
                 WHERE m.conversation_id = ?1
                   AND m.sender_id != ?2
                   AND m.deleted_at IS NULL
                   AND m.undecryptable IS NULL
                   AND m.id IN ({})
                   AND (
                         c.conversation_id IS NULL
                         OR (m.sent_at, m.id) > (c.last_read_at, c.last_read_message_id)
                       )
                 ORDER BY m.sent_at DESC, m.id DESC
                 LIMIT 1",
                crate::db::chunk::placeholders(chunk.len(), 3)
            );
            let mut params: Vec<&dyn rusqlite::ToSql> = vec![conversation_id, &user_id];
            for id in chunk {
                params.push(id);
            }
            let found: Option<(String, String)> = conn
                .query_row(&sql, params.as_slice(), |row| Ok((row.get(0)?, row.get(1)?)))
                .optional()
                .map_err(|e| Error::Other(anyhow::anyhow!("fresh unread lookup: {e}")))?;
            if let Some(f) = found {
                if best.as_ref().is_none_or(|b| is_newer((&f.0, &f.1), (&b.0, &b.1))) {
                    best = Some(f);
                }
            }
        }
        if let Some(b) = best {
            out.push(b);
        }
    }
    out.sort_by(|a, b| b.cmp(a));
    Ok(out.into_iter().map(|(_, id)| id).collect())
}

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
/// The caller names the conversation, never a position: only a cursor at a real stored message can be written.
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
    // Only a cursor that moved is worth syncing.
    if moved {
        schedule_read_cursor_push(state, &user_id);
        // #1508: and clear its notifications from the account's phones. Only
        // on a move — re-opening a read conversation reports nothing.
        crate::commands::push::schedule_push_dismiss(state, &user_id, &conversation_id);
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
    // Newest message from somebody else, excluding own/deleted as `unread_counts` does.
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

// Cross-device sync is encrypted: a plaintext cursor log would show which conversations a human reads, and when.
// Key: HKDF-SHA256 over the account identity key (every device has it, the DS never), then AES-256-GCM.
// The HKDF salt is the user id: a random salt would need agreeing first, and the seed is already uniform.
// The server still learns when and roughly how big; padding blunts size, debouncing blunts timing.

/// HKDF info string — binds this key to read-cursor sync so the account identity
/// key cannot be repurposed between this and the `account_recovery` wrap.
const READ_CURSOR_HKDF_INFO: &[u8] = b"pollis-read-cursor-sync-v1";

const READ_CURSOR_NONCE_LEN: usize = 12;

/// How long a burst of cursor advances is collapsed into one DS write.
/// Without it, reading a busy channel is one signed write per message: amplification and a timing signal.
const PUSH_DEBOUNCE: Duration = Duration::from_secs(5);

/// Is a debounced push already scheduled?
/// Process-global: one account per process, and the push reads the table when it fires. One-shot, not a poll.
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
/// Padded with [`super::framing::pad`] before encryption, so there is one bucket ladder.
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

/// Upload `cursors`, encrypted. The whole set each time: the merge is a `max()`, and a delta
/// would need server-side ordering. Only [`converge_read_cursors`] calls it, with the set it
/// has just merged the account's blob into, so it never drops a position another device wrote.
async fn push_read_cursors(
    state: &Arc<AppState>,
    user_id: &str,
    cursors: &[ReadCursor],
) -> Result<()> {
    // Nothing read yet: don't overwrite another device's blob with an empty list.
    if cursors.is_empty() {
        return Ok(());
    }
    let seed = account_seed(state, user_id).await?;
    let (nonce, blob) = seal_cursors(&seed, user_id, cursors)?;
    let body = pollis_api::profile::SaveReadCursorsBody {
        user_id: user_id.to_string(),
        nonce: b64(&nonce),
        blob: b64(&blob),
    };
    crate::commands::mls::ds_post_ok(state, &body).await
}

/// The account's cursor set as the DS holds it; empty when there is none yet.
///
/// A blob this device cannot open is treated as empty, so the next push replaces it rather
/// than wedging sync for good (it was sealed under this account's key, so only corruption
/// gets here).
async fn fetch_read_cursors(state: &Arc<AppState>, user_id: &str) -> Result<Vec<ReadCursor>> {
    let resp = crate::commands::ds_reads::read_cursors(state, user_id).await?;
    let Some(stored) = resp.cursors else {
        return Ok(Vec::new());
    };
    let nonce = crate::commands::ds_reads::decode_b64("read-cursor nonce", &stored.nonce)?;
    let blob = crate::commands::ds_reads::decode_b64("read-cursor blob", &stored.blob)?;
    let seed = account_seed(state, user_id).await?;
    match open_cursors(&seed, user_id, &nonce, &blob) {
        Ok(c) => Ok(c),
        Err(e) => {
            crate::diag!("[read-cursor] the account's blob would not open; replacing it: {e}");
            Ok(Vec::new())
        }
    }
}

/// One device converges at a time: a launch sync, a nudge and a debounced push may overlap.
static CONVERGE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Pull the account's cursors, merge them in, and push only if this device then holds a
/// position the account lacks (#1628).
///
/// Pull before push is what stops a device that has been running for days from rewinding
/// reads made since on the account's other devices: the blob is last-writer-wins, so a push
/// of this device's set alone would replace theirs.
///
/// Returns the conversations whose cursor moved here.
pub(crate) async fn converge_read_cursors(
    state: &Arc<AppState>,
    user_id: &str,
) -> Result<Vec<String>> {
    let _one = CONVERGE.lock().await;
    let remote = fetch_read_cursors(state, user_id).await?;
    let (moved, local) = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| Error::Other(anyhow::anyhow!("Not signed in")))?;
        let moved = merge_read_cursors(db.conn(), &remote)?;
        (moved, all_read_cursors(db.conn())?)
    };
    if is_ahead_of(&local, &remote) {
        push_read_cursors(state, user_id, &local).await?;
    }
    Ok(moved)
}

/// Queue a debounced converge, unless one is already queued.
/// Fire-and-forget: a failed one only clears a badge late elsewhere; the next advance retries.
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
        if let Err(e) = converge_read_cursors(&state, &user_id).await {
            crate::diag!("[read-cursor] push failed (will retry on the next advance): {e}");
        }
    });
}

/// Pull the other devices' read positions, merge them, and push the result if it adds anything.
/// Returns how many local cursors moved (the launch path's shape).
pub async fn sync_read_cursors(user_id: String, state: &Arc<AppState>) -> Result<usize> {
    Ok(converge_read_cursors(state, &user_id).await?.len())
}

/// Re-sync on focus, resume, a realtime reconnect, or the DS's `read_cursors_changed` nudge
/// (#1628): the same converge, returning the conversations read elsewhere so the caller can
/// refetch unread counts, clear an alert for them and dismiss their notifications.
pub async fn refresh_read_cursors(user_id: String, state: &Arc<AppState>) -> Result<Vec<String>> {
    converge_read_cursors(state, &user_id).await
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

    /// A cursor never moves backwards, so stale replays can't re-badge and merge is order-independent.
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

    #[test]
    fn opening_a_conversation_clears_it() {
        let conn = db();
        msg(&conn, "m1", THEM, "2026-01-01T00:00:01Z");
        msg(&conn, "m2", THEM, "2026-01-01T00:00:02Z");
        assert_eq!(count(&conn), 2);

        assert!(mark_conversation_read_in(&conn, CONV, ME).unwrap());
        assert_eq!(count(&conn), 0);
    }

    /// The cursor lands on the newest message from someone else.
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

    fn cursor(at: &str, id: &str) -> ReadCursor {
        ReadCursor {
            conversation_id: CONV.to_string(),
            last_read_at: at.to_string(),
            last_read_message_id: id.to_string(),
        }
    }

    /// Merge is `max()`, so arrival order cannot change the result.
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
        assert_eq!(merge_read_cursors(&conn, &c).unwrap().len(), 1);
        assert_eq!(
            merge_read_cursors(&conn, &c).unwrap().len(),
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
        assert_eq!(merge_read_cursors(&conn, &incoming).unwrap(), vec!["c2".to_string()]);
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
        assert_eq!(merge_read_cursors(&fresh, &back).unwrap().len(), 2);
        assert_eq!(all_read_cursors(&fresh).unwrap(), all);
    }

    const SEED: &[u8; 32] = b"0123456789abcdef0123456789abcdef";

    // 8-byte ids so the ciphertext search can't match by chance (#1244), yet within the smallest bucket.
    fn sample() -> Vec<ReadCursor> {
        vec![
            ReadCursor {
                conversation_id: "01JCONVERSATIONALPHA0001".into(),
                last_read_at: "2026-01-01T00:00:05Z".into(),
                last_read_message_id: "msgA0005".into(),
            },
            ReadCursor {
                conversation_id: "01JCONVERSATIONBRAVO0002".into(),
                last_read_at: "2026-01-02T00:00:00Z".into(),
                last_read_message_id: "msgB0007".into(),
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

    /// What the DS stores must not name a conversation.
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

    /// Control: the same probe must see a conversation id when one is present.
    #[test]
    fn the_plaintext_probe_can_see_a_conversation_id() {
        let cursors = sample();
        let json = serde_json::to_vec(&cursors).unwrap();
        assert!(contains(&json, cursors[0].conversation_id.as_bytes()));
        assert!(contains(&json, b"conversation_id"));
    }

    /// Another account's key or a tampered blob must not decrypt; a forged cursor would mark messages read.
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

    /// One cursor and several must land in the same padding bucket.
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

    /// Two pushes of the same cursors must not produce the same ciphertext (fresh nonce).
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
        assert_eq!(merge_read_cursors(&b, &incoming).unwrap().len(), 2);
        assert_eq!(all_read_cursors(&b).unwrap(), all_read_cursors(&a).unwrap());

        // And it is idempotent: replaying the same blob moves nothing.
        assert_eq!(merge_read_cursors(&b, &incoming).unwrap().len(), 0);
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
        assert_eq!(merge_read_cursors(&here, &incoming).unwrap().len(), 0);
        assert_eq!(
            all_read_cursors(&here).unwrap()[0].last_read_message_id,
            "m9"
        );
    }

    fn stored(ids: &[&str]) -> Vec<(String, Vec<String>)> {
        vec![(CONV.to_string(), ids.iter().map(|s| s.to_string()).collect())]
    }

    // #1628: a wake whose ingest stored nothing (a receipt, a replay) announces nothing.
    #[test]
    fn a_wake_that_stored_nothing_announces_nothing() {
        let conn = db();
        msg(&conn, "m1", THEM, "2026-01-01T00:00:01Z");
        assert!(newest_unread_of(&conn, ME, &[]).unwrap().is_empty());
        assert!(newest_unread_of(&conn, ME, &stored(&[])).unwrap().is_empty());
    }

    #[test]
    fn a_wake_announces_the_newest_message_it_stored() {
        let conn = db();
        msg(&conn, "m1", THEM, "2026-01-01T00:00:01Z");
        msg(&conn, "m2", THEM, "2026-01-01T00:00:02Z");
        msg(&conn, "m3", THEM, "2026-01-01T00:00:03Z");
        assert_eq!(newest_unread_of(&conn, ME, &stored(&["m1", "m2"])).unwrap(), vec!["m2"]);
    }

    // The reported bug: a message the phone already read must not alert the desktop.
    #[test]
    fn a_stored_message_already_read_elsewhere_announces_nothing() {
        let conn = db();
        msg(&conn, "m1", THEM, "2026-01-01T00:00:01Z");
        msg(&conn, "m2", THEM, "2026-01-01T00:00:02Z");
        advance_read_cursor(&conn, CONV, "2026-01-01T00:00:02Z", "m2").unwrap();
        assert!(newest_unread_of(&conn, ME, &stored(&["m1", "m2"])).unwrap().is_empty());
    }

    #[test]
    fn own_and_deleted_messages_are_never_news() {
        let conn = db();
        msg(&conn, "mine", ME, "2026-01-01T00:00:01Z");
        msg(&conn, "gone", THEM, "2026-01-01T00:00:02Z");
        conn.execute("UPDATE message SET deleted_at = 'x' WHERE id = 'gone'", [])
            .unwrap();
        assert!(newest_unread_of(&conn, ME, &stored(&["mine", "gone"])).unwrap().is_empty());
    }

    #[test]
    fn news_is_reported_per_conversation_newest_first() {
        let conn = db();
        msg(&conn, "m1", THEM, "2026-01-01T00:00:01Z");
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at)
             VALUES ('n1', 'c2', ?1, X'00', 'body', '2026-01-01T00:00:05Z')",
            rusqlite::params![THEM],
        )
        .unwrap();
        let both = vec![
            (CONV.to_string(), vec!["m1".to_string()]),
            ("c2".to_string(), vec!["n1".to_string()]),
        ];
        assert_eq!(newest_unread_of(&conn, ME, &both).unwrap(), vec!["n1", "m1"]);
    }

    #[test]
    fn a_device_is_ahead_only_when_it_holds_a_position_the_account_lacks() {
        let older = cursor("2026-01-01T00:00:01Z", "m1");
        let newer = cursor("2026-01-01T00:00:09Z", "m9");
        assert!(!is_ahead_of(&[], &[newer.clone()]));
        assert!(!is_ahead_of(&[newer.clone()], &[newer.clone()]));
        assert!(!is_ahead_of(&[older.clone()], &[newer.clone()]), "a stale device must not push");
        assert!(is_ahead_of(&[newer.clone()], &[older.clone()]));
        let elsewhere = ReadCursor {
            conversation_id: "c2".into(),
            ..older.clone()
        };
        assert!(is_ahead_of(&[newer.clone(), elsewhere], &[newer]));
    }

    /// Naive substring search over bytes — no `str` conversion, because the
    /// ciphertext is not valid UTF-8 and a lossy conversion could hide a match.
    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|w| w == needle)
    }
}
