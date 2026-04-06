-- Drop dead Signal Protocol tables. These were replaced by MLS in migration 000003
-- and have not been written to since. Any rows remaining are stale.
DROP TABLE IF EXISTS x3dh_init;
DROP TABLE IF EXISTS sender_key_dist;
DROP TABLE IF EXISTS one_time_prekey;
DROP TABLE IF EXISTS signed_prekey;

-- Enforce one join request per (group_id, requester_id) so re-applications
-- can upsert rather than accumulate rows. Keep the most-recent row per pair
-- before adding the constraint in case any duplicates exist.
DELETE FROM group_join_request
WHERE rowid NOT IN (
    SELECT MAX(rowid)
    FROM group_join_request
    GROUP BY group_id, requester_id
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_join_request_unique
    ON group_join_request(group_id, requester_id);

INSERT INTO schema_migrations (version, description) VALUES
    (9, 'drop dead signal tables; unique join request per user per group');
