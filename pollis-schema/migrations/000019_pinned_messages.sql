-- #99 — pinned messages, encrypted under a per-MLS-group pin master key.
--
-- THE KEY MODEL (revised design on the issue). Each MLS conversation (a group
-- — whose channels all share one MLS group — or a DM) gets one random 32-byte
-- AES-256-GCM key, Kpin. Pin content is encrypted directly under Kpin and is
-- NEVER re-encrypted. Kpin itself is stored here WRAPPED under a KEK derived
-- from the current MLS epoch's exporter secret, and every commit that advances
-- the epoch re-wraps it (O(1) per commit — one ~60-byte blob — regardless of
-- pin count; see `publish_group_info` in pollis-core). A device external-joins
-- at epoch N, derives the current exporter, unwraps Kpin and can read every
-- pin, including ones created before it joined. A removed member cannot unwrap
-- any wrap produced after its removal, so it loses access to future pins; old
-- plaintexts it saw while a member are unrecoverable server-side, as with any
-- messenger — that is the honest guarantee, stated in the docs.
--
-- The server stores only ciphertext + nonces on both tables: it can neither
-- read pin content nor recover Kpin (the exporter secret never leaves the
-- members' devices).
--
-- Additive + backward-compatible (CLAUDE.md migration rule): two brand-new
-- tables and an index. A previously shipped client neither reads nor writes
-- either table.

-- One row per MLS conversation, mutable — the CURRENT wrap of Kpin. Keyed
-- exactly like `mls_group_info` (the group id, or the DM channel id), because
-- the re-wrap rides the same post-commit chokepoint and the same
-- strictly-greater-epoch compare-and-set: a stale committer must not clobber a
-- newer wrap, and a replay of an old wrap must not roll the row back to an
-- epoch a removed member could still derive.
CREATE TABLE IF NOT EXISTS pin_keystate (
    conversation_id      TEXT PRIMARY KEY,
    -- AES-256-GCM ciphertext of the 32-byte Kpin, under the KEK exported from
    -- the MLS (generation, epoch) named below. Opaque here.
    wrapped_kpin         BLOB NOT NULL,
    -- 12-byte AES-256-GCM nonce for the wrap, fresh per re-wrap.
    nonce                BLOB NOT NULL,
    -- The MLS suite GENERATION and epoch whose exporter produced this wrap.
    -- The CAS pair, compared lexicographically exactly like `mls_group_info`
    -- (see pollis-delivery/src/writes.rs `upsert_group_info`): a suite
    -- migration stands up a successor lineage restarting at epoch 0, so an
    -- epoch-only guard would strand the successor's wraps forever. An UPDATE
    -- lands only when its pair is strictly greater than the stored one.
    generation           INTEGER NOT NULL DEFAULT 0,
    epoch                INTEGER NOT NULL,
    updated_at           TEXT NOT NULL DEFAULT (datetime('now')),
    -- Which device produced the wrap. Diagnostic, not authz — the DS binds the
    -- WRITE to the authenticated device; this records it for operators.
    updated_by_device_id TEXT NOT NULL
);

-- One row per pinned message. `conversation_id` is the channel or DM the
-- message lives in (pins are listed per channel, like Slack), while the key
-- that opens `encrypted_content` is the MLS conversation's Kpin above.
CREATE TABLE IF NOT EXISTS pinned_message (
    id                TEXT PRIMARY KEY,
    conversation_id   TEXT NOT NULL,
    -- The pinned message's id. No FK to `message_envelope`: a pin is a content
    -- SNAPSHOT and deliberately outlives envelope GC — the envelope is
    -- transport, the pin is the record.
    message_id        TEXT NOT NULL,
    pinned_by         TEXT NOT NULL,
    -- AES-256-GCM ciphertext under the conversation's Kpin: a JSON snapshot of
    -- the message (author, sent_at, body), padded to the message-framing size
    -- buckets before encryption so pin length does not count words for the
    -- server. Written once at pin time, never re-encrypted.
    encrypted_content BLOB NOT NULL,
    -- 12-byte AES-256-GCM nonce, unique per pin.
    nonce             BLOB NOT NULL,
    pinned_at         TEXT NOT NULL DEFAULT (datetime('now')),
    -- Pinning is idempotent per message: the same message pinned twice is one
    -- pin, whoever got there first.
    UNIQUE (conversation_id, message_id)
);

-- The pinned-list read: one conversation's pins, newest first.
CREATE INDEX IF NOT EXISTS idx_pinned_message_conv
    ON pinned_message (conversation_id, pinned_at DESC);
