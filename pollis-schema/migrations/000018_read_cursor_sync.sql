-- #844 — the read cursor, synced across a user's devices as a blob the server
-- cannot read.
--
-- WHY THIS IS NOT `user_preferences`. Preferences sync as PLAINTEXT JSON in
-- `user_preferences.preferences`, and that is a defensible trade for a theme
-- name and a font size. It is not defensible here. A read cursor is
-- `(conversation_id, last_read_at)` per conversation, rewritten every time the
-- human opens a conversation — which is to say it is a precise, timestamped log
-- of WHICH conversations a user reads and WHEN. Stored in the clear it would
-- hand the operator (and anyone who breaches Turso, or subpoenas it) a
-- behavioural profile strictly richer than the envelope metadata the threat
-- model already concedes: envelopes say a message was delivered, this would say
-- a human sat down and read it. So the row carries ciphertext only.
--
-- WHAT THE CLIENT PUTS HERE. AES-256-GCM over the padded JSON cursor list,
-- under a key the client derives with HKDF-SHA256 from the ACCOUNT IDENTITY KEY
-- (`pollis_core::commands::messages::read_state`). That key represents the
-- human, not the device: every device of the account already holds a copy
-- (transferred at enrollment) and the DS has never held it in the clear, so
-- every device can decrypt this row and the server can decrypt none of it. Same
-- construction as the `account_recovery` wrap, different `info` string.
--
-- WHAT THE SERVER STILL LEARNS, stated plainly rather than left implied: that
-- this user's cursor blob changed, roughly how big it is (padded to the size
-- buckets in `commands::messages::framing`, so "how many conversations" is
-- blunted, not erased), and when. Timing is inherent to a sync that must
-- converge; the CONTENT — which conversations, read up to where — is not
-- recoverable from the row.
--
-- ONE ROW PER USER, last-writer-wins at the storage layer. That is safe because
-- the payload merge is a `max()` over per-conversation positions performed on
-- the CLIENT after decrypting (cursors only ever move forward), so a racing
-- overwrite loses at most one push, and the next push from either device
-- re-converges. The server cannot merge, and must not need to.
--
-- Additive + backward-compatible (CLAUDE.md migration rule): one brand-new
-- table, no change to any existing one. A previously shipped client neither
-- reads nor writes it.
CREATE TABLE IF NOT EXISTS read_cursor_sync (
    -- The owning account. Bound to the authenticated user by the DS on write —
    -- you may only read or write your OWN cursors.
    user_id    TEXT PRIMARY KEY,
    -- AES-256-GCM nonce, 12 bytes, fresh per push.
    nonce      BLOB NOT NULL,
    -- AES-256-GCM ciphertext + tag over the PADDED cursor JSON. Opaque here.
    blob       BLOB NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
