-- #1382 — the per-conversation attachment key ring.
--
-- Attachments used to be encrypted under HKDF(SHA-256(plaintext)) and stored
-- at `media/{that hash}.enc`, with the hash also recorded in
-- `attachment_object` / `attachment_ref`: the server held everything needed to
-- decrypt every file. Attachments are now sealed under a key derived from a
-- random per-conversation FILE KEY RING that only MLS members can open.
--
-- Same construction as `pin_keystate` (000019): one mutable row per MLS
-- conversation (group id or DM channel id) holding the ring WRAPPED under a
-- KEK exported from the current epoch's MLS exporter secret, re-wrapped by any
-- key-holding device whenever the epoch advances, guarded by the same
-- lexicographic `(generation, epoch)` CAS. So any current member — a late
-- joiner, an external joiner, a freshly enrolled device — opens every file the
-- conversation ever shared, and the DS stores ciphertext it can never open.
--
-- The one difference from `pin_keystate`: the ring ROTATES. When a member is
-- removed, the committing device appends a fresh 32-byte key and new uploads
-- use it, so the removed member (who may have kept the old keys) cannot open
-- files posted after its removal. `key_version` is the ring's length; it is
-- plaintext here precisely so the DS can make a key fork unrepresentable:
--   mint    → insert-only, key_version = 1;
--   re-wrap → key_version must EQUAL the stored one (same ring, newer epoch);
--   rotate  → key_version must be EXACTLY stored + 1 (one new key, ever).
-- Two devices rotating concurrently therefore cannot both land; the loser
-- refetches and adopts the winner's ring before encrypting anything.
--
-- Additive + backward-compatible (CLAUDE.md migration rule): one new table. A
-- previously shipped client neither reads nor writes it.
CREATE TABLE IF NOT EXISTS file_keystate (
    conversation_id      TEXT PRIMARY KEY,
    -- AES-256-GCM ciphertext of the serialized ring (key_version × 32 bytes),
    -- under the KEK exported from the MLS (generation, epoch) below. Opaque.
    wrapped_keyring      BLOB NOT NULL,
    -- 12-byte AES-256-GCM nonce for the wrap, fresh per (re-)wrap.
    nonce                BLOB NOT NULL,
    -- Number of keys in the ring; the newest is the one uploads use.
    key_version          INTEGER NOT NULL CHECK (key_version >= 1),
    -- The MLS suite generation and epoch whose exporter produced this wrap;
    -- the CAS pair, exactly as `pin_keystate`.
    generation           INTEGER NOT NULL DEFAULT 0,
    epoch                INTEGER NOT NULL,
    updated_at           TEXT NOT NULL DEFAULT (datetime('now')),
    -- Which device produced the wrap. Diagnostic, not authz.
    updated_by_device_id TEXT NOT NULL
);
