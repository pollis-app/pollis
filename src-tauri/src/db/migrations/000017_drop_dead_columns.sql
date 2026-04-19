-- Drop the `users.identity_key` column.
--
-- This column was part of the pre-MLS Signal Protocol schema. Nothing in
-- the production codebase reads or writes it — the only reference is an
-- isolated test fixture in src/db/remote.rs. All identity-key semantics
-- have since moved to `users.account_id_pub` (Ed25519 account identity)
-- and the MLS credential / device certificate in `user_device`.
--
-- SQLite 3.35+ supports ALTER TABLE ... DROP COLUMN natively; libSQL/Turso
-- honour the same syntax.
ALTER TABLE users DROP COLUMN identity_key;

INSERT INTO schema_migrations (version, description) VALUES
    (17, 'drop users.identity_key (dead pre-MLS column)');
