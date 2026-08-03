-- Server-side attachment reference counting (issue #690).
--
-- Attachments are convergently encrypted and deduped by `content_hash`: one
-- `attachment_object` row (and one R2 blob) is shared by every message that
-- carries the same file, across conversations and users. Before this migration a
-- message-delete issued an UNCONDITIONAL `DELETE FROM attachment_object WHERE
-- content_hash = ?` plus an R2 delete, which stranded any OTHER message still
-- referencing that hash — its attachment 404'd. The DS cannot see message content
-- (bodies are E2EE ciphertext in `message_envelope`), so it cannot count
-- references itself; the client must DECLARE each reference through the DS. This
-- table holds those declarations.
--
-- A reference is one `(content_hash, message_id)` pair: "message M carries the
-- file with this hash". The sender registers it on send (`/v1/attachments/register`
-- now also carries a `message_id`); it is released when M is deleted
-- (`/v1/attachments/delete`), and the shared `attachment_object` row — and its R2
-- object — is collected only once NO reference remains. See
-- `docs/metadata-retention-policy.md` §2/§5 and `.codesight/wiki/database.md`.
--
-- Metadata-exposure trade-off, stated plainly (this repo is actively REDUCING
-- server-visible linkage — #701/#662, `docs/metadata-minimization-design.md`):
-- storing `message_id` in the clear moves the attachment↔message linkage that
-- previously lived ONLY inside the MLS-encrypted payload out to the operator. The
-- operator can now join this table to `message_envelope.conversation_id` and learn
-- which messages — and therefore which conversations — share a given convergent
-- file. It does NOT reveal the file (plaintext is never seen) or the sender
-- (sealed, #607); it reveals a co-reference graph over hashes the operator already
-- stores (`attachment_object`, `r2_key`). The opaque-`ref_token` alternative would
-- hide that graph but leaks references forever when a device loses local state and
-- cannot release a token an admin never held — i.e. it fails deletion, the exact
-- thing this ticket exists to make correct. We chose the counted, robust option.
--
-- Additive + backward-compatible (CLAUDE.md migration rule): a brand-new table
-- only, no change to `attachment_object`. A previously-shipped client that never
-- calls the extended endpoints simply writes no rows here. Existing
-- `attachment_object` rows have no references recorded and are NOT backfilled:
-- they retain exactly today's best-effort semantics (the single delete predicate
-- `NOT EXISTS (refs)` collects a zero-reference legacy row, matching the old
-- unconditional delete — no worse than today, not "instantly deletable by anyone"
-- beyond what they already were, and not permanently pinned). Any send from an
-- updated client re-references such a hash and thereby PROMOTES it to full counted
-- protection.
--
-- PK `(content_hash, message_id)` makes registration idempotent (a retried send
-- cannot double-count) and indexes `content_hash` left-most, so the delete gate's
-- `WHERE content_hash = ?` / `NOT EXISTS` count is a single index probe.
CREATE TABLE IF NOT EXISTS attachment_ref (
    content_hash TEXT NOT NULL,
    message_id   TEXT NOT NULL,
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (content_hash, message_id)
);
