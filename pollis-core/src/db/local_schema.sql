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
    thread_id TEXT,
    -- Set on a PLACEHOLDER row (#1312): an envelope this device was entitled to
    -- read but never decrypted, written when the ingest cursor moves past it so
    -- the loss shows in the conversation instead of vanishing. The value is the
    -- reason (see `messages::loss::Reason`); a placeholder never holds content.
    -- Also added to existing DBs by `add_missing_columns`.
    undecryptable TEXT CHECK (
        undecryptable IS NULL
        OR (undecryptable IN ('stranded', 'failed', 'lineage') AND content IS NULL)
    ),
    -- The durable outbox (#1335). NULL for every delivered or received row.
    -- 'pending' = this device's own message, stored BEFORE any network work and
    -- not yet confirmed by the DS; 'failed' = its last delivery attempt failed
    -- in a way a later attempt may not; 'rejected' = the DS refused it in a way
    -- no retry fixes (not a member, conversation gone, malformed). The outbox
    -- drain (after unlock, on reconnect, on resume) retries pending and failed,
    -- never rejected; an explicit Retry tries any of them; delivery clears it.
    -- The renderers show failed and rejected alike. An unsent row is
    -- always a real message — never a placeholder. Also added to existing DBs
    -- by `add_missing_columns`.
    send_state TEXT CHECK (
        send_state IS NULL
        OR (send_state IN ('pending', 'failed', 'rejected') AND undecryptable IS NULL AND content IS NOT NULL)
    ),
    -- 'transfer' on a row copied from another of this account's devices
    -- (#1735), NULL on every row this device received or sent itself. A
    -- notification never previews a transferred row (`notify::fetch`). Also
    -- added to existing DBs by `add_missing_columns`; must stay identical to
    -- its `ADDITIVE_COLUMNS` entry.
    origin TEXT CHECK (origin IS NULL OR origin = 'transfer')
);

-- Envelopes this device has already accounted for, that leave no `message`
-- row of their own (#1312): ones it POSTED ('own') — under sealed sender
-- nothing on an envelope says who sent it, and a device cannot decrypt its own
-- MLS messages — and control frames (receipts, deletes, edits) it already
-- OPENED ('opened'), since an MLS message cannot be decrypted twice and an
-- envelope can be fetched again. Without this list each would look like a lost
-- message when it comes back through the fetch. Pruned past the DS's longest
-- envelope retention.
CREATE TABLE IF NOT EXISTS known_envelope (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('own', 'opened')),
    recorded_at TEXT NOT NULL DEFAULT (datetime('now'))
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

-- Attachment bytes an unsent message still has to upload (#1335). A send with
-- attachments stores them here BEFORE its `message` row and before any network
-- work — one row per attachment, each in its own short insert so a large
-- message never holds the database lock across all its files — and then writes
-- the message row, which is the commit point. So a process killed mid-upload
-- loses nothing: the outbox drain uploads them, rewrites the row's content to
-- the final `_att` envelope, deletes these rows, and only then posts. Bytes
-- whose message row was never written (killed between the two) are swept when
-- the database opens. No foreign key, for that reason: the bytes exist before
-- the row. The bytes are plaintext only inside this SQLCipher database
-- (encrypted under the session's db_key) — never a file on disk (#1000).
-- Deleted once uploaded and with their message on a discard; kept across a
-- keep-data sign-out like the rest of the outbox.
CREATE TABLE IF NOT EXISTS outbox_attachment (
    message_id   TEXT NOT NULL,
    position     INTEGER NOT NULL CHECK (position >= 0),
    filename     TEXT NOT NULL,
    content_type TEXT NOT NULL,
    data         BLOB NOT NULL,
    -- Renderer-computed metadata (a video's poster blurhash and size); the
    -- upload's own values win when it produces them, as on the renderer path.
    blurhash     TEXT,
    width        INTEGER,
    height       INTEGER,
    -- Marked sensitive in the composer (#1568); written into the envelope as
    -- `sens`. Must stay identical to its `ADDITIVE_COLUMNS` entry in local.rs.
    sensitive    INTEGER NOT NULL DEFAULT 0 CHECK (sensitive IN (0, 1)),
    PRIMARY KEY (message_id, position)
);

-- An edit of one of this device's own messages that the DS has not confirmed
-- yet (#1555) — the outbox for edits. `edit_message` stores it here BEFORE any
-- network work and leaves the `message` row alone; only once the DS holds the
-- edit envelope does one transaction write the new text to the row and delete
-- this one. So a process killed in between never leaves the sender's copy
-- showing text no other member has: the row still holds what everyone holds,
-- and the drain (after unlock, on reconnect, on resume) posts the edit again.
-- The read paths show the pending text, with `edit_state`, over the row.
--
-- One per target: a newer edit replaces an unconfirmed older one, as the DS
-- replaces a target's pending edit envelope. `envelope_id` is kept across
-- every attempt at the same edit, so a retry after a post that landed unseen
-- replaces that envelope with one carrying the same text. Same states as
-- `message.send_state`; the drain retries pending and failed, never rejected.
-- Kept across a keep-data sign-out like the rest of the outbox.
CREATE TABLE IF NOT EXISTS outbox_edit (
    target_message_id TEXT PRIMARY KEY,
    envelope_id       TEXT NOT NULL UNIQUE,
    conversation_id   TEXT NOT NULL,
    sender_id         TEXT NOT NULL,
    content           TEXT NOT NULL,
    send_state        TEXT NOT NULL DEFAULT 'pending'
                      CHECK (send_state IN ('pending', 'failed', 'rejected')),
    created_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

-- The outbox drain's scan (#1335): only unsent rows, which are few.
CREATE INDEX IF NOT EXISTS idx_message_send_state ON message(sent_at) WHERE send_state IS NOT NULL;
-- A conversation's unsent line, in order (#1636): the outbox's ahead pass and re-stamp.
CREATE INDEX IF NOT EXISTS idx_message_unsent_line ON message(conversation_id, sent_at, id) WHERE send_state IS NOT NULL;

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

-- Safety-number change notices (#1605): one row per actual change of a pinned
-- key, durable, so a change found by ANY ingest path (the app, the iOS
-- Notification Service Extension, the Android FCM service) reaches the user.
-- The live `KeyChanged` event only has a listener while the desktop app runs;
-- a headless fetch has none, so the event alone lost those changes.
--
-- Written by the TRIGGER below, never by Rust: every path that re-pins a key
-- (batch TOFU on ingest and reconcile, the single-peer helper, Mark verified
-- re-pinning the current key) goes through `contact_verification`, so the
-- notice cannot be forgotten by a new call site. The WHEN clause compares the
-- row's OLD value at write time, under SQLite's write lock, so two processes
-- that both saw the old pin (the extension, then the app) write one notice:
-- the second UPDATE sets the key it already has and the trigger does not fire.
-- A first pin is an INSERT and is not a change.
--
-- Per PEER, not per conversation: the change is to the person's key, and the
-- path that noticed it may be a group reconcile. Every DM timeline with that
-- peer shows it at `detected_at` (Signal does the same).
CREATE TABLE IF NOT EXISTS key_change_notice (
    id                 INTEGER PRIMARY KEY AUTOINCREMENT,
    peer_user_id       TEXT NOT NULL,
    old_account_id_pub BLOB NOT NULL,
    new_account_id_pub BLOB NOT NULL CHECK (new_account_id_pub != old_account_id_pub),
    detected_at        TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
);

CREATE INDEX IF NOT EXISTS idx_key_change_notice_peer
    ON key_change_notice(peer_user_id, detected_at);

CREATE TRIGGER IF NOT EXISTS contact_verification_key_change
AFTER UPDATE OF account_id_pub ON contact_verification
WHEN OLD.account_id_pub IS NOT NEW.account_id_pub
BEGIN
    INSERT INTO key_change_notice (peer_user_id, old_account_id_pub, new_account_id_pub)
    VALUES (NEW.peer_user_id, OLD.account_id_pub, NEW.account_id_pub);
END;


-- Saved / bookmarked messages (#854).
--
-- DEVICE-LOCAL BY DESIGN. Bookmarks are never sent to the DS or Turso and there
-- is no sync endpoint for them. Which messages a user chose to save is exactly
-- the kind of per-message interest metadata the threat model says the untrusted
-- server must never learn — syncing it would hand the server a ranked list of
-- the conversations and moments a user cares most about, which is strictly more
-- than it learns from envelope delivery alone. The cost is that bookmarks do not
-- follow you to a new device, which is consistent with accepted loss (2) in
-- CLAUDE.md: a new device starts empty (unless the user copies history from another
-- of their enrolled devices, #1735 — which copies messages, never bookmarks).
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
-- and, more importantly, keeps a second copy of every decrypted message body
-- out of the file (still SQLCipher ciphertext at rest, but one copy is enough). Snippets are cut in Rust from `message.content`, not by
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
-- they learned here, exactly as `hydrate_rows` writes
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

-- This device's unwrapped copy of each conversation's pin master key (#99).
--
-- Kpin is the AES-256-GCM key every pin snapshot in a conversation is sealed
-- under. It is held HERE unwrapped because this database is the same trust
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

-- The attachment key ring (#1382) for each MLS conversation this device has
-- held it for: `keyring` is the unwrapped ring (48-byte entries: a 32-byte
-- key and the big-endian (generation, epoch) it was added at). Load-bearing
-- for the same reason as `pin_key_cache` — the previous epoch's exporter is
-- gone after a merge, so re-wrapping needs the plaintext ring. The wrapped_*
-- columns say which epoch this device last wrapped it at.
-- `rotation_owed_*` is set by the device that committed a removal and cleared
-- once the ring holds a key added at or after that point; it survives a crash
-- between the commit and the rotation.
CREATE TABLE IF NOT EXISTS file_key_cache (
    conversation_id          TEXT PRIMARY KEY,
    keyring                  BLOB NOT NULL,
    wrapped_generation       INTEGER NOT NULL DEFAULT 0,
    wrapped_epoch            INTEGER NOT NULL DEFAULT 0,
    rotation_owed_generation INTEGER,
    rotation_owed_epoch      INTEGER,
    updated_at               TEXT NOT NULL DEFAULT (datetime('now'))
);

-- The attachment ring's KEK (`export_secret("pollis-file-kek")`) of each epoch
-- this device has LEFT, kept per MLS lineage (`mls_group_id` is the openmls
-- group id: the conversation id, plus `#g<n>` on a migrated generation). The
-- ring is wrapped under one epoch's KEK and openmls keeps no exporter for a
-- past epoch, so without this a device that was not holding the ring could
-- open the stored wrap only while sitting at exactly that epoch — one commit
-- of its own before a key holder re-wrapped and it was locked out until that
-- holder came back online. Written right before every merge
-- (`file_keys::keep_epoch_kek`), bounded per lineage. A KEK opens only a wrap
-- of keys this device was entitled to as a member at that epoch.
CREATE TABLE IF NOT EXISTS file_kek_history (
    mls_group_id TEXT NOT NULL,
    epoch        INTEGER NOT NULL,
    kek          BLOB NOT NULL CHECK (length(kek) = 32),
    PRIMARY KEY (mls_group_id, epoch)
);

-- Decrypted vault entries (#107) — the local cache of the remote
-- `vault_message` rows, which are ciphertext under a key derived from the
-- account identity key. Unwrapped here for the same reason `message.content`
-- is decrypted: this database is the trusted store (SQLCipher at rest). Replaced wholesale by each sync
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

-- An edit, self-redaction or admin tombstone that reached this device before
-- the message it targets (#1735) — the target is absent (sent before this
-- device joined, so only a history transfer can bring it) or a loss
-- placeholder (#1312, whose author is unknown, so the author check cannot run
-- yet). Applied by `messages::pending_change::apply` the moment a real row for
-- the target is created or healed — by a transfer import or by ingest — and
-- deleted then. Without it, a deleted or edited message copied in later from
-- a sibling device would come back in its old state.
--   * `actor_id` is the MLS-authenticated author of the change (NULL only for
--     a tombstone, which the DS authorised);
--   * an edit carries its new text, nothing else does;
--   * `seq` is the change's DS position, so the latest edit wins.
-- Bounded: at most 5000 per conversation (oldest dropped), pruned at 400
-- days and by the retention window.
CREATE TABLE IF NOT EXISTS pending_message_change (
    envelope_id     TEXT PRIMARY KEY,
    target_id       TEXT NOT NULL,
    conversation_id TEXT NOT NULL,
    kind            TEXT NOT NULL CHECK (kind IN ('edit', 'redact', 'tombstone')),
    actor_id        TEXT,
    content         TEXT,
    seq             INTEGER NOT NULL DEFAULT 0,
    recorded_at     TEXT NOT NULL DEFAULT (datetime('now')),
    CHECK ((kind = 'edit') = (content IS NOT NULL)),
    CHECK ((kind = 'tombstone') = (actor_id IS NULL))
);
CREATE INDEX IF NOT EXISTS idx_pending_message_change_target
    ON pending_message_change(target_id);
CREATE INDEX IF NOT EXISTS idx_pending_message_change_conversation
    ON pending_message_change(conversation_id, recorded_at);

-- A history transfer this device takes part in (#1735), as the source ('src')
-- or the receiver ('dst'). Its key material lives only here, inside the
-- encrypted local DB, and is gone once the transfer ends:
--   * `kem_sk` (receiver only) — the key-agreement secret, until the source's
--     acceptance is verified and the transfer key derived;
--   * `transfer_key` — the 32-byte chunk key, while transferring.
-- `next_index` only moves forward (trigger), and an ended transfer never
-- reopens (trigger). One open transfer as receiver at a time (index). Ended
-- rows double as the record of handled requests, so a replayed request is
-- refused.
CREATE TABLE IF NOT EXISTS history_transfer_local (
    transfer_id      TEXT PRIMARY KEY,
    role             TEXT NOT NULL CHECK (role IN ('src', 'dst')),
    peer_device_id   TEXT NOT NULL,
    peer_device_name TEXT,
    via              TEXT NOT NULL CHECK (via IN ('link', 'sibling')),
    state            TEXT NOT NULL CHECK (state IN (
                         'awaiting_approval', 'transferring', 'paused',
                         'completed', 'declined', 'cancelled', 'expired', 'failed')),
    kem_sk           BLOB,
    transfer_key     BLOB CHECK (transfer_key IS NULL OR length(transfer_key) = 32),
    req              BLOB NOT NULL,
    acc              BLOB,
    -- Source: the plan fixed at the start (eligible + deferred conversations), JSON.
    plan             TEXT,
    -- Source: the last (conversation_id, sent_at, id) sent, JSON.
    cursor           TEXT,
    next_index       INTEGER NOT NULL DEFAULT 0 CHECK (next_index >= 0),
    prev_hash        BLOB NOT NULL DEFAULT (zeroblob(32)) CHECK (length(prev_hash) = 32),
    -- Source: the index of the final chunk once it is sealed.
    final_index      INTEGER,
    chunks_done      INTEGER NOT NULL DEFAULT 0 CHECK (chunks_done >= 0),
    rows_done        INTEGER NOT NULL DEFAULT 0 CHECK (rows_done >= 0),
    rows_skipped     INTEGER NOT NULL DEFAULT 0 CHECK (rows_skipped >= 0),
    deferred_count   INTEGER NOT NULL DEFAULT 0 CHECK (deferred_count >= 0),
    -- Rows the source expects to send (counted when it plans; the receiver
    -- learns it from chunk 0). A progress hint, never checked against.
    estimated_total  INTEGER CHECK (estimated_total IS NULL OR estimated_total >= 0),
    error            TEXT,
    created_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    updated_at       TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now')),
    CHECK (role = 'dst' OR kem_sk IS NULL),
    CHECK (state IN ('awaiting_approval', 'transferring', 'paused')
           OR (kem_sk IS NULL AND transfer_key IS NULL))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_history_transfer_local_one_receiving
    ON history_transfer_local(role)
    WHERE role = 'dst' AND state IN ('awaiting_approval', 'transferring', 'paused');

CREATE TRIGGER IF NOT EXISTS history_transfer_local_index_forward
BEFORE UPDATE OF next_index ON history_transfer_local
WHEN NEW.next_index < OLD.next_index
BEGIN
    SELECT RAISE(ABORT, 'history_transfer_local: next_index only moves forward');
END;

CREATE TRIGGER IF NOT EXISTS history_transfer_local_ended_stays_ended
BEFORE UPDATE OF state ON history_transfer_local
WHEN OLD.state IN ('completed', 'declined', 'cancelled', 'expired', 'failed')
 AND NEW.state != OLD.state
BEGIN
    SELECT RAISE(ABORT, 'history_transfer_local: an ended transfer never reopens');
END;

-- The source's sealed chunks between sealing and the receiver's ack (#1735).
-- Written in the same transaction that advances `next_index`, BEFORE the
-- upload, so a restart re-sends the very same bytes (the DS refuses
-- different bytes at an index) and never re-seals an index.
CREATE TABLE IF NOT EXISTS history_transfer_outgoing (
    transfer_id TEXT NOT NULL,
    idx         INTEGER NOT NULL CHECK (idx >= 0),
    sealed      BLOB NOT NULL,
    PRIMARY KEY (transfer_id, idx)
);
