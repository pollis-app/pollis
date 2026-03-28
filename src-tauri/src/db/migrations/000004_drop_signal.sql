-- Drop legacy Signal sender-key protocol tables.
-- These are replaced entirely by MLS (RFC 9420).
DROP TABLE IF EXISTS sender_key_dist;
DROP TABLE IF EXISTS x3dh_init;
DROP TABLE IF EXISTS signed_prekey;
DROP TABLE IF EXISTS one_time_prekey;

INSERT INTO schema_migrations (version, description) VALUES
    (4, 'drop legacy Signal tables in favor of MLS');
