CREATE TABLE IF NOT EXISTS kv (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS identity_key (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    public_key BLOB NOT NULL,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- NOTE: signed_prekey, one_time_prekey, signal_session, group_sender_key were
-- Signal Protocol tables removed in favour of MLS. They are not created for new
-- local databases. Existing databases may still have these tables but nothing
-- reads from or writes to them.

CREATE TABLE IF NOT EXISTS message (
    id TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    sender_id TEXT NOT NULL,
    ciphertext BLOB NOT NULL,
    content TEXT,
    reply_to_id TEXT,
    sent_at TEXT NOT NULL,
    received_at TEXT NOT NULL DEFAULT (datetime('now')),
    delivered INTEGER NOT NULL DEFAULT 0,
    edited_at TEXT,
    deleted_at TEXT,
    -- ULID of the thread root this message replies into, NULL for an ordinary
    -- channel message (#825). Only ever populated from the decrypted MLS
    -- payload — it is never sent to, or read from, the DS.
    --
    -- This column is ALSO added to pre-existing databases by
    -- `add_missing_columns` in `local.rs`; CREATE TABLE IF NOT EXISTS is a
    -- no-op once the table exists, so this line only serves fresh DBs.
    thread_id TEXT
);

CREATE INDEX IF NOT EXISTS idx_message_conversation ON message(conversation_id, sent_at);

-- Thread lookup: fetching one thread's replies, and counting replies per root.
-- Created on every open like the eviction index above, so existing DBs gain it
-- without a schema-version bump (which would wipe history).
CREATE INDEX IF NOT EXISTS idx_message_thread ON message(thread_id, sent_at);

-- Eviction scan index: lookback/retention sweeps delete by received_at. Run on
-- every open (this schema is re-applied each open) so existing DBs gain it
-- without a schema-version bump (which would wipe history).
CREATE INDEX IF NOT EXISTS idx_message_received_at ON message(received_at);

CREATE TABLE IF NOT EXISTS dm_conversation (
    id TEXT PRIMARY KEY,
    peer_user_id TEXT NOT NULL UNIQUE,
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- User preferences (local-first mirror of remote user_preferences).
-- Single-row table — the DB file is already scoped to one user.
-- No seed row: a missing (or literal '{}') row tells get_preferences to pull
-- from remote and cache here. save_preferences upserts into this table.
CREATE TABLE IF NOT EXISTS preferences (
    preferences TEXT NOT NULL DEFAULT '{}',
    updated_at  TEXT NOT NULL DEFAULT (datetime('now'))
);

-- UI state (window geometry, etc.)
CREATE TABLE IF NOT EXISTS ui_state (
    key        TEXT PRIMARY KEY,
    value      TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);
INSERT OR IGNORE INTO ui_state (key, value) VALUES ('window_state', '{"width":1024,"height":768,"x":0,"y":0}');

-- MLS StorageProvider backend.
-- All openmls state is stored here via MlsStore (src/signal/mls_storage.rs).
-- scope = entity-type discriminator string (see MLSProgress.md for convention).
-- key   = serde_json-serialised lookup key (hash_ref, group_id, public_key, …).
-- value = serde_json-serialised entity value.
CREATE TABLE IF NOT EXISTS mls_kv (
    scope TEXT    NOT NULL,
    key   BLOB    NOT NULL,
    value BLOB    NOT NULL,
    PRIMARY KEY (scope, key)
);

-- When this device last rotated its OWN leaf in a group (issue #666).
-- openmls keeps `unmerged_leaves` crate-private, so a device cannot ask the
-- ratchet tree "is my leaf still unmerged?"; it tracks its own rotations here
-- instead. Absence of a row means "never rotated" — which is exactly the
-- post-join state whose unmerged leaf inflates every other member's commits.
CREATE TABLE IF NOT EXISTS mls_self_update (
    conversation_id TEXT PRIMARY KEY,
    last_epoch      INTEGER NOT NULL,
    last_at         TEXT NOT NULL
);

-- Which suite GENERATION of a conversation's MLS group this device is on
-- (#454 P4). MLS binds the ciphersuite into the group, so upgrading a live
-- conversation to the post-quantum hybrid suite means standing up a SUCCESSOR
-- group and moving the roster into it — a second lineage, restarting at epoch 0.
--
-- Absence of a row means generation 0, which is every group that exists today,
-- so this table is inert until a conversation actually migrates. The value is
-- the lineage this device currently READS AND WRITES; a successor group that has
-- been created locally but not yet adopted (its predecessor is still being
-- drained) is stored under its own openmls GroupId and is invisible until this
-- row advances. See `commands/mls/generation.rs`.
CREATE TABLE IF NOT EXISTS mls_generation (
    conversation_id TEXT PRIMARY KEY,
    generation      INTEGER NOT NULL,
    updated_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS user_cache (
    id         TEXT PRIMARY KEY,
    username   TEXT NOT NULL,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Delivery / read receipts for DM messages (#857).
--
-- PER MESSAGE, PER READER, PER KIND — never a boolean on the message. A DM can
-- have several members, so "who has received this" and "who has read this" are
-- sets, and the 1:1 case is just the N=1 row count. `reader_id` is always the
-- MLS credential recovered from inside the receipt's ciphertext, so a member can
-- only ever speak for themselves.
--
-- Device-local by design: a receipt is a fact this device has learned, and
-- keeping it off the server is the whole point — the DS never sees who read
-- what, or when. `at` is the reader's own timestamp, carried inside the
-- authenticated ciphertext.
CREATE TABLE IF NOT EXISTS message_receipt (
    message_id TEXT NOT NULL,
    reader_id  TEXT NOT NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('delivered', 'read')),
    at         TEXT NOT NULL,
    PRIMARY KEY (message_id, reader_id, kind)
);
CREATE INDEX IF NOT EXISTS idx_message_receipt_message ON message_receipt(message_id);

-- "Read implies delivered" enforced at the LOWEST layer (see
-- docs/backend-core-invariants.md): you cannot have read a message your device
-- never received. Recording a read materialises the matching delivered row, so
-- the state "read but not delivered" is physically unrepresentable rather than
-- something every caller has to remember to maintain.
CREATE TRIGGER IF NOT EXISTS message_receipt_read_implies_delivered
AFTER INSERT ON message_receipt
WHEN NEW.kind = 'read'
BEGIN
    INSERT OR IGNORE INTO message_receipt (message_id, reader_id, kind, at)
    VALUES (NEW.message_id, NEW.reader_id, 'delivered', NEW.at);
END;

-- Safety-number / contact verification pins (Signal-style).
-- TOFU: the first account_id_pub seen for a peer is stored here. A later
-- mismatch is surfaced as a "changed" status and clears `verified`. Lives
-- in the local (secrets) DB so a malicious Turso write to
-- users.account_id_pub is detectable client-side.
CREATE TABLE IF NOT EXISTS contact_verification (
    peer_user_id     TEXT PRIMARY KEY,
    account_id_pub   BLOB NOT NULL,
    identity_version INTEGER NOT NULL,
    verified         INTEGER NOT NULL DEFAULT 0,
    first_seen_at    TEXT NOT NULL DEFAULT (datetime('now')),
    updated_at       TEXT NOT NULL DEFAULT (datetime('now'))
);


-- Saved / bookmarked messages (#854).
--
-- DEVICE-LOCAL BY DESIGN. Bookmarks are never sent to the DS or Turso and there
-- is no sync endpoint for them. Which messages a user chose to save is exactly
-- the kind of per-message interest metadata the threat model says the untrusted
-- server must never learn — syncing it would hand the server a ranked list of
-- the conversations and moments a user cares most about, which is strictly more
-- than it learns from envelope delivery alone. The cost is that bookmarks do not
-- follow you to a new device, which is consistent with accepted loss (2) in
-- CLAUDE.md: a new device starts empty and has no history to bookmark anyway.
--
-- No foreign key to `message`: a bookmark is allowed to outlive local retention
-- eviction. The saved-items list resolves each row against `message` at read
-- time and renders an honest "you do not have this message" placeholder when the
-- join misses, rather than making the row silently disappear.
--
-- Additive CREATE TABLE IF NOT EXISTS — this file is re-applied on every open,
-- so existing databases gain the table with no LOCAL_SCHEMA_VERSION bump (a bump
-- would DELETE the user's message history; see the comment in local.rs).
-- Where the human has read up to, per conversation (#844).
--
-- A CURSOR, not a count. Unread state used to be an in-memory MobX field that
-- only the live-event dispatcher wrote, so a restart marked everything read and
-- anything that arrived while offline never badged at all — users silently
-- missed messages. A count cannot survive a restart honestly because it has no
-- way to say *which* messages it counted; a cursor can, and the count is then
-- derived from the `message` rows that ingest has already stored.
--
-- The position is the pair `(last_read_at, last_read_message_id)`, matching the
-- `ORDER BY sent_at DESC, id DESC` the message list itself uses — `sent_at`
-- alone is a sender-supplied timestamp and two messages can share one.
--
-- The cursor only ever moves FORWARD (see `advance_read_cursor`). That is what
-- makes multi-device merge a `max()` with no locking and no conflict
-- resolution: applying the same or an older position is a no-op, so devices
-- converge whatever order their updates arrive in.
CREATE TABLE IF NOT EXISTS read_cursor (
    conversation_id      TEXT PRIMARY KEY,
    last_read_at         TEXT NOT NULL,
    last_read_message_id TEXT NOT NULL,
    updated_at           TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS bookmark (
    message_id      TEXT PRIMARY KEY,
    conversation_id TEXT NOT NULL,
    created_at      TEXT NOT NULL DEFAULT (datetime('now'))
);

-- The saved-items surface lists newest-first across all conversations.
CREATE INDEX IF NOT EXISTS idx_bookmark_created_at ON bookmark(created_at DESC);

-- ── Full-text message search (#850) ──────────────────────────────────────────
--
-- ADDITIVE ONLY. Every statement below is `IF NOT EXISTS` and this file is
-- re-applied on every open, so an existing database gains the index with
-- nothing lost. Bumping LOCAL_SCHEMA_VERSION to add it would DELETE the user's
-- database including their MLS state — see the comment in `local.rs`.
--
-- `content=''` makes this a CONTENTLESS index: FTS5 stores the term dictionary
-- and nothing else, which is what keeps the cost at ~11 MB per 100k messages
-- and, more importantly, keeps a second copy of every plaintext message body
-- out of the file. Snippets are cut in Rust from `message.content`, not by
-- `snippet()`, which a contentless table cannot answer anyway.
--
-- `remove_diacritics 2` is what makes `cafe` find `café`, `resume` find
-- `résumé` and `nandu` find `Ñandú`. CJK is handled a layer up, by
-- `pollis_search_text` emitting overlapping bigrams — `unicode61` tokenises a
-- whole Han run as one token and `trigram` needs three characters, so neither
-- can find a two-character Chinese word.
CREATE VIRTUAL TABLE IF NOT EXISTS message_fts USING fts5(
    body,
    content='',
    tokenize="unicode61 remove_diacritics 2"
);

-- The index is maintained by TRIGGERS, not by the Rust write sites.
--
-- There are three INSERT sites, six UPDATE sites and one DELETE site on this
-- table today. Covering them by hand is the "code discipline" rung CLAUDE.md
-- ranks last, and a write path added next year would silently stop indexing.
-- A trigger is a DB-level guarantee: a row cannot be written without the index
-- learning about it. `db::local::tests::the_search_index_cannot_drift` is the
-- invariant test.
--
-- Both the delete and the insert halves call `pollis_search_text`, which is
-- registered on every connection in `local.rs`. A contentless FTS5 table is
-- told what to remove by being handed the body it indexed, which is why that
-- function must stay deterministic.
CREATE TRIGGER IF NOT EXISTS message_fts_ai AFTER INSERT ON message
WHEN new.content IS NOT NULL AND new.deleted_at IS NULL
BEGIN
    INSERT INTO message_fts (rowid, body) VALUES (new.rowid, pollis_search_text(new.content));
END;

-- Both delete-halves below additionally check `message_fts_docsize` — FTS5's
-- own per-row shadow table — so the 'delete' command is only ever issued for a
-- row the index actually holds. This is not an optimisation: until the
-- backfill reaches a pre-existing row, that row is unindexed, and a contentless
-- 'delete' against a rowid the index doesn't know HARD-ERRORS with
-- SQLITE_CORRUPT ("database disk image is malformed"), aborting whatever write
-- fired the trigger — the retention sweep on the very first post-upgrade open
-- being the deterministic case.
CREATE TRIGGER IF NOT EXISTS message_fts_ad AFTER DELETE ON message
WHEN old.content IS NOT NULL AND old.deleted_at IS NULL
BEGIN
    INSERT INTO message_fts (message_fts, rowid, body)
    SELECT 'delete', old.rowid, pollis_search_text(old.content)
    WHERE old.rowid IN (SELECT rowid FROM message_fts_docsize);
END;

-- Edits, soft deletes (content set to NULL) and moderator deletes all arrive
-- here. Remove whatever the old row contributed, then re-add the new row if it
-- still has indexable text — the two halves are guarded independently so a
-- delete does not try to un-index a row that was never indexed (NULL/tombstoned
-- old content, or a pre-backfill row the index has not reached yet).
CREATE TRIGGER IF NOT EXISTS message_fts_au AFTER UPDATE OF content, deleted_at ON message
BEGIN
    INSERT INTO message_fts (message_fts, rowid, body)
    SELECT 'delete', old.rowid, pollis_search_text(old.content)
    WHERE old.content IS NOT NULL AND old.deleted_at IS NULL
      AND old.rowid IN (SELECT rowid FROM message_fts_docsize);

    INSERT INTO message_fts (rowid, body)
    SELECT new.rowid, pollis_search_text(new.content)
    WHERE new.content IS NOT NULL AND new.deleted_at IS NULL;
END;

-- Local mirror of what a conversation id MEANS (#850).
--
-- Channel and group names are remote-only: `list_group_channels` and the
-- bootstrap read both go to the Delivery Service, and there is no embedded
-- replica. A local `message` row cannot even say whether its `conversation_id`
-- is a channel or a DM. Without this table every search result page would need
-- an N+1 network round trip to render a name, and search would be broken
-- offline — so the two places that already list channels and DMs write what
-- they learned here, exactly as `attach_sender_usernames_local` writes
-- `user_cache`.
--
-- Best-effort and disposable: a miss renders the id, which is what search did
-- for every result before this table existed.
CREATE TABLE IF NOT EXISTS conversation_cache (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL CHECK (kind IN ('channel', 'dm')),
    name       TEXT,
    group_id   TEXT,
    group_name TEXT,
    updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

-- `in:#channel` / `in:@person` resolve a typed name back to a conversation id.
CREATE INDEX IF NOT EXISTS idx_conversation_cache_name ON conversation_cache(name);

-- This device's plaintext copy of each conversation's pin master key (#99).
--
-- Kpin is the AES-256-GCM key every pin snapshot in a conversation is sealed
-- under. It is held HERE in the clear because this database is the same trust
-- domain that holds the MLS group state itself (SQLCipher at rest, key behind
-- the PIN) — wrapping it again locally would protect it from nobody the MLS
-- state is not already exposed to. The SERVER-side copy is always wrapped
-- under the current epoch's exporter KEK.
--
-- (wrapped_generation, wrapped_epoch) records the newest (generation, epoch)
-- pair THIS DEVICE has published a wrap at — the local-first staleness check:
-- after an epoch advance, `maybe_rewrap_pin_key` compares these two columns
-- against the local group with no network round trip, and only a device that
-- is actually behind re-wraps and posts. `max_past_epochs = 0` makes this
-- cache load-bearing, not a convenience: the previous epoch's exporter is
-- destroyed at merge, so a device that lost Kpin cannot recover it from a
-- stale server wrap — only from this row or from a peer's fresh re-wrap.
CREATE TABLE IF NOT EXISTS pin_key_cache (
    conversation_id    TEXT PRIMARY KEY,
    kpin               BLOB NOT NULL,
    wrapped_generation INTEGER NOT NULL DEFAULT 0,
    wrapped_epoch      INTEGER NOT NULL DEFAULT 0,
    updated_at         TEXT NOT NULL DEFAULT (datetime('now'))
);

-- Decrypted vault entries (#107) — the local cache of the remote
-- `vault_message` rows, which are ciphertext under a key derived from the
-- account identity key. Plaintext here for the same reason `message.content`
-- is: this database is the trusted store. Replaced wholesale by each sync
-- (the remote is the source of truth — the vault must behave like cloud
-- storage, surviving any single device), which is also what lets the vault
-- render offline from the last synced state.
--
-- Named `vault_entry_cache`, NOT `vault_message`: the local and remote
-- schemas keep DISJOINT table names by design, and the
-- `no_client_side_remote_writes` guard depends on that disjointness to tell
-- a local write from a banned remote one by table name alone.
CREATE TABLE IF NOT EXISTS vault_entry_cache (
    id         TEXT PRIMARY KEY,
    -- The entry's content string — the same `{"_att":[...],"_txt":"..."}`
    -- envelope shape chat messages use, so the renderer and the media grid
    -- parse it with the exact same code.
    content    TEXT NOT NULL,
    pinned     INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_vault_entry_cache_created ON vault_entry_cache(created_at DESC);
