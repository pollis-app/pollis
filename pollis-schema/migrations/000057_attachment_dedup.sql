-- #2109: each distinct attachment is stored once, across every account.
--
-- A shared object's id is `v2-` + the hex SHA-256 of its stored bytes (the
-- ciphertext). Its bytes are sealed under a key derived from the plaintext by
-- a separate one-way derivation, so neither the id nor anything stored here
-- yields the key. A v1 object's id is 64 bare hex characters (a random file
-- id, #1382), so the two id spaces never meet.
--
-- `attachment_lease`: a sender's hold on a shared object between learning it
-- is stored (and skipping the upload) and their reference landing. The
-- reference count (`live_ref_exists` in pollis-delivery) counts a lease
-- younger than 30 days, so another account's delete cannot reclaim the bytes
-- in that window. `pending` counts the sender's sends of the object still
-- outstanding; each reference that lands counts it down, and the last drops
-- the row. A lease whose reference never comes lapses after 30 days.
--
-- The trigger: a shared object's row may name only its own canonical key, so
-- the DS's reclaim of it deletes exactly that object.
--
-- `attachment_removed`: operator removal of a stored file. Because a file is
-- stored once, one removal takes it from every message and Vault entry that
-- held it. The row outlives the object and refuses its id from then on (no
-- claim, upload, registration or download), so sending the identical file
-- again cannot bring it back. It holds the id and when, nothing else.
--
-- Additive and backward-compatible: new tables, an index and two triggers
-- that only constrain ids no shipped client or DS writes.
CREATE TABLE IF NOT EXISTS attachment_removed (
    content_hash TEXT PRIMARY KEY,
    removed_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS attachment_lease (
    content_hash TEXT NOT NULL
        CHECK (substr(content_hash, 1, 3) = 'v2-' AND length(content_hash) = 67),
    user_id      TEXT NOT NULL,
    pending      INTEGER NOT NULL DEFAULT 1 CHECK (pending BETWEEN 1 AND 1000),
    updated_at   TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (content_hash, user_id)
);
CREATE INDEX IF NOT EXISTS idx_attachment_lease_updated ON attachment_lease(updated_at);

CREATE TRIGGER IF NOT EXISTS attachment_object_shared_key_insert
BEFORE INSERT ON attachment_object
WHEN substr(NEW.content_hash, 1, 3) = 'v2-'
 AND (length(NEW.content_hash) != 67 OR NEW.r2_key IS NOT ('media/' || NEW.content_hash || '.enc'))
BEGIN
    SELECT RAISE(ABORT, 'a shared attachment object names only media/<its id>.enc');
END;

CREATE TRIGGER IF NOT EXISTS attachment_object_shared_key_update
BEFORE UPDATE OF content_hash, r2_key ON attachment_object
WHEN substr(NEW.content_hash, 1, 3) = 'v2-'
 AND (length(NEW.content_hash) != 67 OR NEW.r2_key IS NOT ('media/' || NEW.content_hash || '.enc'))
BEGIN
    SELECT RAISE(ABORT, 'a shared attachment object names only media/<its id>.enc');
END;
