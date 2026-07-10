-- Per-device commit-log catch-up high-water. Refs issue #539 (I4 retention
-- floor — "prune the MLS commit log: bound storage + stale-user catch-up").
--
-- IMPORTANT: COMMIT-LOG-DB migration (mls_commit_log / mls_welcome /
-- mls_group_info live on the separate log DB post-#420). Applied by
-- desktop-release.yml's second db-apply step (MIGRATIONS_DIR=
-- pollis-core/src/db/migrations-log). It must NOT go in the main-DB dir.
--
-- WHY THIS TABLE. The Delivery Service prunes `mls_commit_log` below a retention
-- FLOOR so storage stays bounded per conversation (it grows with
-- membership-churn × time otherwise). The floor's Tier-1 (zero-loss) bound is the
-- MIN applied-epoch across all CURRENT member devices — everyone still needs
-- commits `>= that epoch`, so nothing anyone is waiting on is deleted (Spec B
-- `NoLossForCurrentMember`, specs/tla/Delivery.tla). This table is the signal the
-- MIN is taken over: each device records, on its commit-catch-up, the epoch it is
-- caught up FROM (its `since`, i.e. its current local MLS epoch). It is DISTINCT
-- from `conversation_watermark` (main DB), which tracks message-envelope FETCH
-- progress, not applied MLS epoch — the two floors are computed independently.
--
-- The DS is the SOLE writer (device-signed reads report the high-water; the prune
-- DELETE runs in DS code) — consistent with invariants living in DS code, not DB
-- triggers.
--
-- Additive/backward-compatible (CLAUDE.md migration constraint): a new table +
-- index only. Old shipped clients never touch it; they simply never report, which
-- pins the Tier-1 floor conservatively low (Tier 2's hard cap still bounds
-- storage). No DROP, no column/nullability change to an existing table.

-- One high-water row per (conversation, user, device). `since_epoch` is monotone:
-- the DS upserts it as MAX(existing, reported), so a stale/reordered report can
-- never LOWER a device's recorded epoch and prune commits it still needs.
CREATE TABLE IF NOT EXISTS mls_commit_since (
    conversation_id  TEXT NOT NULL,
    user_id          TEXT NOT NULL,      -- FK to users(id) dropped (cross-DB, like the sibling log tables)
    device_id        TEXT NOT NULL,
    since_epoch      INTEGER NOT NULL,   -- the device's applied MLS epoch (it still needs commits >= this)
    updated_at       TEXT NOT NULL DEFAULT (datetime('now')),
    PRIMARY KEY (conversation_id, user_id, device_id)
);

-- The floor computation scans a conversation's reported high-waters to take their
-- MIN across current member devices.
CREATE INDEX IF NOT EXISTS idx_mls_commit_since_conv
    ON mls_commit_since (conversation_id);
