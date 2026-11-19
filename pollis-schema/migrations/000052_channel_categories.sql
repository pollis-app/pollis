-- #2031: channel categories and channel order within a group.
--
-- A category is a group-wide, admin-managed section of the channel list. Every
-- member sees the same categories in the same order; only the collapsed state
-- is per device, and that never reaches the server.
--
-- Display order, which the DS returns channels in (pollis-delivery
-- `directory::CHANNEL_ORDER_SQL`): uncategorised channels first, then category
-- by category in `channel_category.position`; inside each block, channels with
-- a `position` by position, then any without one by creation time. Existing
-- channels have no position and no category, so they keep sorting by creation
-- time and no row is rewritten by this migration.
--
-- Positions are rewritten by the DS in one write transaction per change, every
-- block it touches renumbered from 0 (`channel_order::renumber_block` in pollis-delivery). The partial
-- unique indexes below make a duplicate position unrepresentable, so two admins
-- dragging at once can serialize but never interleave into a collision.
--
-- Deleting a category never deletes a channel: the DS moves its channels to
-- the end of the uncategorised block first, and the BEFORE DELETE trigger below
-- detaches any channel a writer left behind, clearing its position too
-- (production runs with foreign_keys=OFF, so the `ON DELETE SET NULL` clause
-- alone is inert there; and where it does act, it would keep a position that
-- can collide with one in the uncategorised block, so the trigger runs first).
-- A channel may only name a category of its own group (the two BEFORE
-- triggers), so a move into another group's category is refused by the
-- database as well as by the DS.
--
-- Category names are group metadata like channel names: read only by current
-- members (#917), through the same bootstrap and group reads.
--
-- Additive and backward-compatible: one new table, two nullable columns,
-- indexes and triggers. A shipped client reads neither column and keeps
-- rendering the flat list, which the DS now returns in display order.
CREATE TABLE IF NOT EXISTS channel_category (
    id         TEXT PRIMARY KEY CHECK (length(id) BETWEEN 1 AND 128),
    group_id   TEXT NOT NULL CHECK (length(group_id) BETWEEN 1 AND 128),
    name       TEXT NOT NULL CHECK (length(name) BETWEEN 1 AND 400),
    position   INTEGER NOT NULL CHECK (position >= 0),
    created_at TEXT NOT NULL DEFAULT (datetime('now'))
);
CREATE UNIQUE INDEX IF NOT EXISTS idx_channel_category_position
    ON channel_category(group_id, position);

ALTER TABLE channels ADD COLUMN category_id TEXT REFERENCES channel_category(id) ON DELETE SET NULL;
ALTER TABLE channels ADD COLUMN position INTEGER CHECK (position IS NULL OR position >= 0);

-- One position per block; '' stands for the uncategorised block.
CREATE UNIQUE INDEX IF NOT EXISTS idx_channels_block_position
    ON channels(group_id, COALESCE(category_id, ''), position)
    WHERE position IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_channels_category
    ON channels(category_id)
    WHERE category_id IS NOT NULL;

CREATE TRIGGER IF NOT EXISTS channel_category_delete_detaches
BEFORE DELETE ON channel_category
FOR EACH ROW
BEGIN
    UPDATE channels SET category_id = NULL, position = NULL WHERE category_id = OLD.id;
END;

CREATE TRIGGER IF NOT EXISTS channels_category_same_group_insert
BEFORE INSERT ON channels
FOR EACH ROW
WHEN NEW.category_id IS NOT NULL
    AND NOT EXISTS (SELECT 1 FROM channel_category
                    WHERE id = NEW.category_id AND group_id = NEW.group_id)
BEGIN
    SELECT RAISE(ABORT, 'channel category belongs to another group');
END;

CREATE TRIGGER IF NOT EXISTS channels_category_same_group_update
BEFORE UPDATE OF category_id, group_id ON channels
FOR EACH ROW
WHEN NEW.category_id IS NOT NULL
    AND NOT EXISTS (SELECT 1 FROM channel_category
                    WHERE id = NEW.category_id AND group_id = NEW.group_id)
BEGIN
    SELECT RAISE(ABORT, 'channel category belongs to another group');
END;
