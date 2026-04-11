-- Add metadata columns to mls_commit_log so receivers can verify the
-- cross-signing certs of any devices being added by a commit BEFORE
-- calling openmls `process_message` — closing the step-3b gap in
-- MULTI_DEVICE_ENROLLMENT.md.
--
-- The alternative (inspecting the staged commit after process_message)
-- was rejected because process_message may advance per-epoch decryption
-- state for encrypted commits, so re-processing after an async cert
-- lookup is not safe.
--
-- Run against Turso manually.

-- user_id of the account whose devices are being added by this commit.
-- NULL for commits that do not add any devices (removes, self-updates).
ALTER TABLE mls_commit_log ADD COLUMN added_user_id TEXT;

-- Comma-separated device_id list for the user identified by
-- `added_user_id`. Populated in lock-step with `added_user_id`. The
-- receiver splits on ',' and verifies each device's cert.
ALTER TABLE mls_commit_log ADD COLUMN added_device_ids TEXT;

INSERT INTO schema_migrations (version, description) VALUES
    (14, 'mls_commit_log: metadata columns for inbound device cert verification');
