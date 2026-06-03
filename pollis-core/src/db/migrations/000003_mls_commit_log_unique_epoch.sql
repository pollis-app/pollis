-- Collapse any historical commit forks. Where two or more commits share the
-- same (conversation_id, epoch), keep the lowest seq -- the one every member
-- that processed the log in (epoch ASC, seq ASC) order already merged -- and
-- drop the rest. The dropped rows are the "losing branch" that no member on
-- the canonical branch ever applied, so removing them is a no-op for those
-- members and harmless for the forked author (whose divergent crypto state
-- lives in their own local mls_kv, not here). Required before the unique
-- index below, which would otherwise fail on the duplicates.
DELETE FROM mls_commit_log
WHERE seq NOT IN (
    SELECT MIN(seq) FROM mls_commit_log GROUP BY conversation_id, epoch
);

-- One commit per epoch per conversation, enforced from here on. Two members
-- racing to commit at the same epoch used to both land (no constraint),
-- forking the group: each member applied the lower-seq commit while the
-- higher-seq author merged its own and diverged permanently. With this index
-- the second INSERT conflicts; the new client code treats the conflict as
-- "lost the race", rolls back its local merge, and re-processes the winner.
-- Backward-compatible: the previously shipped app's plain INSERT now fails
-- fast on the conflict (and rolls back its pending commit) instead of forking.
CREATE UNIQUE INDEX IF NOT EXISTS idx_mls_commit_conv_epoch
    ON mls_commit_log (conversation_id, epoch);
