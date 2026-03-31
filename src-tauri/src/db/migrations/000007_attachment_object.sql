-- Cross-user attachment deduplication registry.
-- Convergent encryption: SHA-256(plaintext) → deterministic key → identical ciphertext.
-- Same file uploaded by any user maps to the same R2 object.
CREATE TABLE IF NOT EXISTS attachment_object (
    content_hash  TEXT PRIMARY KEY,
    r2_key        TEXT NOT NULL,
    mime_type     TEXT NOT NULL,
    size_bytes    INTEGER NOT NULL,
    created_at    TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT INTO schema_migrations (version, description) VALUES
    (7, 'cross-user attachment deduplication registry');
