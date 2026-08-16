-- Custom per-group emoji (issue #848).
--
-- Two tables, deliberately separated along the same seam as
-- `attachment_object` / `attachment_ref` (#690): one row per STORED OBJECT,
-- content-addressed and shared by everyone, and one row per USE of that object
-- in a group. That separation is what makes "no fifteen party-parrots" true by
-- construction rather than by a cleanup job — fifteen groups adding the same
-- image produce fifteen `group_emoji` rows and exactly ONE
-- `custom_emoji_object` row (and one R2 blob).
--
-- ## These objects are NOT encrypted, on purpose
--
-- Every other blob Pollis puts in R2 is convergently encrypted; these are not.
-- The product decision (#848) is that a custom emoji is a public-ish asset, and
-- the reason is structural, not lazy: Discord's rule — which this implements —
-- is that a member of group A may use A's emoji when talking in group B, and
-- everyone in B must RENDER it even though they have no access to A. There is no
-- key any B-only member could hold that would decrypt an A-scoped secret, so an
-- encrypted emoji is an emoji that cannot cross a group boundary. Message
-- CONTENT stays E2EE exactly as before; what leaves the envelope is a shortcode
-- and a content hash, and the operator can already see that an object exists.
--
-- The cost is stated plainly: the operator can see every custom emoji image, and
-- which group registered it. It cannot see who sent one, in which conversation,
-- or in what message — that stays inside the MLS payload.
--
-- ## Deduplication
--
-- `content_hash` is SHA-256 of the RE-ENCODED bytes (see
-- `pollis-core::commands::emoji::encode_emoji`), never of what the user picked
-- off disk. The re-encoder is deterministic, so the same source image always
-- lands on the same hash whoever uploads it, and the second uploader's R2 PUT is
-- skipped entirely.
--
-- ## Garbage collection
--
-- A `custom_emoji_object` is collectable exactly when NO `group_emoji` row names
-- its hash. That count is DERIVED from row existence, not maintained as a
-- counter, so it cannot drift: deleting a group emoji, deleting the group, or
-- any future teardown path releases the reference by removing the row, and
-- nothing has to remember to decrement anything. `pollis-delivery::emoji`
-- collects on remove and in bulk via `POST /v1/emoji/gc`; the R2
-- `delete`-presign is gated by the same predicate (`broker.rs`), so the blob and
-- the row are never collected out from under a live reference.
--
-- Additive + backward-compatible (CLAUDE.md migration rule): two brand-new
-- tables and their indexes, no change to any existing table. A previously
-- shipped client that knows nothing about emoji writes no rows here and reads
-- none.
CREATE TABLE IF NOT EXISTS custom_emoji_object (
    -- SHA-256 (lowercase hex) of the re-encoded bytes. The dedup anchor, the R2
    -- key stem, and the integrity check every downloader re-verifies.
    content_hash TEXT PRIMARY KEY,
    -- `emoji/<content_hash>.<ext>`. Derived server-side from the hash and the
    -- content type — never taken from the client — so a client cannot point a
    -- hash at somebody else's object.
    r2_key       TEXT NOT NULL,
    -- `image/webp` (static) or `image/gif` (animated). The only two the
    -- re-encoder emits; the DS rejects anything else.
    content_type TEXT NOT NULL,
    -- Size of the stored object. Bounded by the DS at registration
    -- (`EMOJI_MAX_BYTES`) and by the encoder as a postcondition.
    size_bytes   INTEGER NOT NULL,
    animated     INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL DEFAULT (datetime('now'))
);

-- One row per (group, shortcode). The PK is what makes a shortcode unique within
-- its group — two members racing to add `:parrot:` cannot both win — while the
-- SAME image in another group is a second row pointing at the ONE object.
CREATE TABLE IF NOT EXISTS group_emoji (
    group_id     TEXT NOT NULL,
    -- `[a-z0-9_]{2,32}`, validated at the DS chokepoint. The narrow charset is a
    -- security control, not cosmetics: shortcodes are echoed into rendered
    -- message text, and a charset with no `<`, `&`, `:` or whitespace in it
    -- cannot carry markup or break the `<:name:hash>` token grammar.
    shortcode    TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    created_by   TEXT NOT NULL,
    created_at   TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (group_id, shortcode)
);

-- The GC predicate probes by hash ("is any group still using this object?").
CREATE INDEX IF NOT EXISTS idx_group_emoji_content_hash ON group_emoji(content_hash);

-- The per-user quota counts a creator's rows across every group, and account
-- teardown deletes by creator.
CREATE INDEX IF NOT EXISTS idx_group_emoji_created_by ON group_emoji(created_by);
