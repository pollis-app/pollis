-- #107 — the Vault: a personal encrypted space (Note to Self), synced across
-- the account's devices as ciphertext the server cannot read.
--
-- SAME CONSTRUCTION AS `read_cursor_sync` (migration 000018), PER MESSAGE
-- INSTEAD OF PER ACCOUNT. Each row's `blob` is AES-256-GCM over the padded
-- JSON of one vault entry (body text, pinned flag, edit timestamp), under a
-- key the client derives with HKDF-SHA256 from the ACCOUNT IDENTITY KEY with a
-- vault-specific `info` string (`pollis_core::commands::vault`). That key
-- represents the human, not a device: every enrolled device already holds the
-- account key, so every device decrypts every entry with no coordination, and
-- the DS — which has never held the key — decrypts none of it. No MLS: the
-- vault is single-user, so there is no group to agree on an epoch with, and
-- deriving from the account key is exactly what lets a brand-new device read
-- the whole vault the moment it enrolls (the "new device starts empty" loss
-- does NOT apply here, by design).
--
-- WHAT THE SERVER LEARNS, stated plainly: that this user keeps N vault rows,
-- roughly how big each is (blunted by padding to the message-framing size
-- buckets), and when each was created or last rewritten. Not their content,
-- not which are pinned, not what was searched — the pinned flag lives INSIDE
-- the ciphertext (pin/unpin is an ordinary re-encrypt-and-save), and search
-- runs client-side over the decrypted local cache.
--
-- Deletes are hard DELETEs: a vault entry the user removed is gone from the
-- server, not tombstoned — there is no other member who still needs it.
--
-- Additive + backward-compatible (CLAUDE.md migration rule): one brand-new
-- table and an index. A previously shipped client neither reads nor writes it.
CREATE TABLE IF NOT EXISTS vault_message (
    id         TEXT PRIMARY KEY,
    -- The owner. Bound to the authenticated user by the DS on every write and
    -- read — you can only ever touch your OWN vault.
    user_id    TEXT NOT NULL,
    -- 12-byte AES-256-GCM nonce, fresh per save (a re-save of an edited entry
    -- gets a new nonce, never reuses one).
    nonce      BLOB NOT NULL,
    -- AES-256-GCM ciphertext + tag over the padded entry JSON. Opaque here.
    blob       BLOB NOT NULL,
    -- Client-supplied creation time (the list orders by it, like `sent_at` on
    -- messages). Server-stamped `updated_at` records the last rewrite —
    -- last-write-wins per entry, which is safe because entries are keyed by
    -- ULID (two devices never mint the same id) and an edit rewrites the whole
    -- entry.
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- The vault list read: one user's entries, newest first.
CREATE INDEX IF NOT EXISTS idx_vault_message_user
    ON vault_message (user_id, created_at DESC);

-- Which attachment objects a vault entry holds alive — the vault's leg of the
-- attachment GC liveness predicate (see `pollis-delivery/src/messages.rs`,
-- `LIVE_REF_EXISTS_SQL`). Without this, an object referenced only by a vault
-- entry counts as unreferenced the moment any client's post-delete cleanup or
-- upload dedup pass looks at its hash, and the user's file evaporates —
-- exactly the storage-service behavior the vault must not have.
--
-- Sibling of `attachment_ref` (migration 000012), which maps hash →
-- message_id; this maps hash → vault entry. Maintained by `/v1/vault/save`
-- (replace-on-save, so an edit that drops a file releases it) and
-- `/v1/vault/delete`.
--
-- WHAT THIS ADMITS TO THE SERVER, stated plainly: that this vault entry
-- references these content hashes. That pairing is already conceded — the
-- upload presign (`/v1/r2/presign`) is device-signed and names both the user
-- and the object key, so the DS learns "this user touched this object" at
-- upload time regardless. This table makes the same fact durable in exchange
-- for the file reliably existing tomorrow.
CREATE TABLE IF NOT EXISTS vault_attachment_ref (
    content_hash     TEXT NOT NULL,
    vault_message_id TEXT NOT NULL,
    created_at       TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (content_hash, vault_message_id)
);

-- The replace-on-save and on-delete legs address refs by entry, not by hash.
CREATE INDEX IF NOT EXISTS idx_vault_attachment_ref_entry
    ON vault_attachment_ref (vault_message_id);
