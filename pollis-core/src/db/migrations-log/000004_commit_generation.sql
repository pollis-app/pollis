-- Suite-generation lineage for the MLS commit log (issue #454 P4 — migrate an
-- existing classic group to the PQ-hybrid suite at an epoch boundary).
--
-- IMPORTANT: COMMIT-LOG-DB migration (mls_commit_log / mls_group_info /
-- mls_welcome / mls_commit_since live on the separate log DB post-#420). Applied
-- by desktop-release.yml's second db-apply step (MIGRATIONS_DIR=
-- pollis-core/src/db/migrations-log). It must NOT go in the main-DB dir.
--
-- WHY. MLS binds the ciphersuite into the group, so a classic group cannot be
-- switched to hybrid in place: the migration stands up a *successor* group for
-- the same conversation and moves the roster into it by Welcome. A successor
-- restarts at MLS epoch 0, and the rest of the system enforces per-conversation
-- epoch monotonicity — the DS head rule, the transparency log's commit-log
-- invariant, and `docs/mls-reconcile-hardening.md`'s explicit ban on "re-create
-- the group at epoch 0". So the successor is NOT an exception to monotonicity; it
-- is a lineage. The monotone key widens from `(conversation_id, epoch)` to
-- `(conversation_id, generation, epoch)`, ordered lexicographically. Within a
-- generation the rule is byte-for-byte today's head+1 rule; epoch 0 is accepted
-- only as the OPENING commit of generation N+1, and only when it references the
-- closed head of generation N (see `pollis_delivery::commit::submit_commit`).
--
-- Every existing row is generation 0 — correct by default, so no backfill and no
-- reinterpretation of already-written history.
--
-- BACKWARD COMPATIBILITY (CLAUDE.md migration rule). The four ADD COLUMNs are
-- additive with a NOT NULL DEFAULT, which a shipped pre-hybrid app simply never
-- reads or writes: it keeps submitting commits with no generation field, the DS
-- defaults it to 0, and generation 0 is exactly the lineage that app is in.
--
-- The one non-additive statement is the DROP of the old
-- `(conversation_id, epoch)` unique index, which MUST go: it would reject
-- generation 1's epoch 0 as a duplicate of generation 0's. That is safe here in a
-- way a main-DB index drop would not be, because no shipped client writes this
-- table — post-#420 clients hold a READ-ONLY token on the log DB and every write
-- goes through the Delivery Service, which is deployed in lockstep with this
-- migration. Dropping it also LOOSENS rather than tightens, so nothing that was
-- previously accepted starts failing; the replacement index below re-imposes the
-- same one-commit-per-epoch guarantee, one key wider.

-- The lineage column on the log itself. `generation` 0 is the original classic
-- lineage of every conversation that exists today.
ALTER TABLE mls_commit_log ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;

-- GroupInfo is what an external-joining device reads to learn the current tree
-- and suite. After a migration the successor's epoch (0, then 1…) is LOWER than
-- the classic group's last epoch, so the existing `excluded.epoch >
-- mls_group_info.epoch` upsert guard would reject the successor's GroupInfo and
-- strand every recovering device on the retired suite. The guard becomes
-- lexicographic on (generation, epoch); this column is what it orders on.
ALTER TABLE mls_group_info ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;

-- A Welcome is how a member is moved into the successor group. The recipient must
-- know which lineage the Welcome belongs to BEFORE applying it, so it can finish
-- draining its current generation first (`max_past_epochs = 0` means a message
-- must be decrypted while the group is still at its epoch). Without this the
-- recipient would have to guess, and a guess is how messages get dropped.
ALTER TABLE mls_welcome ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;

-- The retention floor (#539, I4) is computed from each device's reported
-- catch-up high-water. A high-water is only meaningful within its lineage — epoch
-- 3 of generation 1 is not "behind" epoch 12 of generation 0, it is AHEAD of it.
-- The reported pair is therefore (generation, since_epoch), compared
-- lexicographically, and it is also the signal that says when a closed
-- generation's commits are safe to delete outright (every current member device
-- has reported a strictly greater generation).
ALTER TABLE mls_commit_since ADD COLUMN generation INTEGER NOT NULL DEFAULT 0;

-- Replace the one-commit-per-epoch index with its one-key-wider form. Same
-- guarantee (a racing second INSERT conflicts instead of forking the group), now
-- scoped to a lineage so a successor generation may legitimately start at epoch 0.
DROP INDEX IF EXISTS idx_mls_commit_conv_epoch;

-- It also serves the catch-up read, which scans one lineage in epoch order
-- (`WHERE conversation_id = ? AND generation = ? AND epoch >= ? ORDER BY epoch`),
-- so no separate covering index is needed.
CREATE UNIQUE INDEX IF NOT EXISTS idx_mls_commit_conv_gen_epoch
    ON mls_commit_log (conversation_id, generation, epoch);
