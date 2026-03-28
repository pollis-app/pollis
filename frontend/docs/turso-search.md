# Turso Full-Text Search: Research Findings

## Summary

Turso has built a native full-text search (FTS) engine, but it lives in their **new in-process "turso" database** (a complete Rust rewrite of SQLite), not in the older **libSQL fork** that Pollis currently uses via the `libsql` crate. For the hosted Turso cloud product, traditional SQLite **FTS5** virtual tables are supported and work via the `libsql` crate. The new Tantivy-powered FTS is experimental and not yet available on hosted Turso cloud.

The bigger constraint for Pollis is architectural: message content is encrypted on the server. FTS on message bodies — whether FTS5 or the new Tantivy engine — is useless for E2E encrypted content stored in `message_envelope.ciphertext`. The existing `search_messages` command already does the right thing: it searches the local plaintext cache using a LIKE query. The only real FTS opportunity in Pollis is for searching metadata that lives in plaintext on Turso (usernames, group names, channel names).

---

## Two Distinct Products

Turso now ships two separate things that are easy to confuse:

**1. libSQL** (`libsql` crate, v0.6 in Pollis)
The original open-source fork of SQLite. This is what Pollis uses. It supports remote connections to hosted Turso cloud via HTTP/WebSockets. SQLite's built-in FTS5 extension is enabled on the hosted platform. A known bug in the TypeScript client prevents parameterized inserts into FTS5 tables (GitHub issue #1811 on tursodatabase/libsql), but this does not affect the Rust client.

**2. Turso Database** (`turso` crate, currently in public beta)
A ground-up rewrite of SQLite in Rust, announced late 2024. This is where the new native FTS lives. It is an **in-process** database — it runs embedded, not as a remote service. Turso Cloud currently runs on libSQL and will integrate the Turso engine in the future, but that migration has not happened yet.

---

## The New FTS Feature (Tantivy-based)

**Status:** Experimental, available in the `turso` in-process crate only. Shipped in v0.5.0 of the turso crate.

**Engine:** Built on Tantivy, a Rust-native Apache Lucene-style search library. Unlike FTS5's shadow tables, it stores inverted index segments directly inside the SQLite B-tree.

**SQL syntax:**

```sql
-- Create an FTS index
CREATE INDEX idx_groups_search ON groups USING fts (name, description);

-- Query with relevance ranking (BM25)
SELECT *, fts_score(name, description, 'rust chat') AS score
FROM groups
WHERE fts_match(name, description, 'rust chat')
ORDER BY score DESC;

-- Alternative MATCH syntax
SELECT * FROM groups WHERE (name, description) MATCH 'rust chat';

-- Result highlighting
SELECT fts_highlight(name, description, '<b>', '</b>', 'rust') FROM groups;

-- Per-field weighting
CREATE INDEX idx ON groups USING fts (name, description)
    WITH (tokenizer = 'simple', weights = 'name=3.0,description=1.0');
```

**Key limitations:**
- Automatic background segment merging is disabled to preserve transaction safety. Segments accumulate over time and query performance degrades without manual maintenance.
- Requires periodic `OPTIMIZE INDEX` commands.
- Storage overhead grows from unmerged deletions.
- Not available on hosted Turso cloud (yet).
- The `turso` crate is in beta with a "not ready for production" warning.

---

## FTS5 on Hosted Turso (via libsql crate)

SQLite FTS5 is enabled by default on hosted Turso databases. The `libsql` crate (which Pollis already uses) can execute FTS5 queries against hosted Turso without any changes to the crate version or feature flags.

**FTS5 syntax:**

```sql
-- Create virtual table
CREATE VIRTUAL TABLE users_fts USING fts5(username, content='users', content_rowid='rowid');

-- Populate it
INSERT INTO users_fts(users_fts) VALUES('rebuild');

-- Query
SELECT u.* FROM users u
JOIN users_fts f ON u.rowid = f.rowid
WHERE users_fts MATCH 'alice'
ORDER BY rank;
```

FTS5 is a content table approach: the virtual table mirrors data from the base table and must be kept in sync via triggers or manual rebuild. It provides BM25 ranking via the `rank` column, prefix queries, phrase queries, and snippet highlighting.

---

## The Encryption Constraint

This is the decisive factor for Pollis.

The `message_envelope` table on Turso stores `ciphertext TEXT NOT NULL` — the encrypted Signal Protocol payload. The plaintext is never sent to the server. FTS on message content at the Turso layer is therefore impossible by design. Any server-side index over `message_envelope.ciphertext` would index encrypted bytes, not words.

The local `message` table in the SQLite database (accessed via `rusqlite`) has a `content TEXT` column that holds the decrypted plaintext after the device has received and decrypted a message. The current `search_messages` Tauri command already searches this column with a LIKE query:

```sql
SELECT id, conversation_id, sender_id, content, sent_at
FROM message
WHERE content IS NOT NULL AND content LIKE ?1
ORDER BY sent_at DESC LIMIT ?2
```

This is correct behavior. Any FTS improvement for message content must be applied to this **local** SQLite database, not to Turso. The `rusqlite` crate used for the local DB has FTS5 bundled with it (SQLite ships FTS5 by default, and the `bundled-sqlcipher` feature in Pollis's Cargo.toml includes it).

---

## Where FTS Would Actually Help

There are three plaintext columns on the remote Turso database worth considering for FTS:

| Table | Column(s) | Current search |
|---|---|---|
| `users` | `username` | LIKE in `search_user_by_username` command |
| `groups` | `name`, `description` | No dedicated search command |
| `channels` | `name`, `description` | No dedicated search command |

These are stored in plaintext. A LIKE query on `users.username` (as currently used) performs a full table scan. For small user counts this is fine. For groups and channels the dataset is even smaller.

---

## Implementation Proposal

### Option A: FTS5 on local SQLite for message search (low-risk, concrete improvement)

Replace the LIKE table scan in `search_messages` with an FTS5 virtual table on the local `message` table. This requires:

1. Add an FTS5 virtual table to `local_schema.sql`:

```sql
CREATE VIRTUAL TABLE IF NOT EXISTS message_fts
    USING fts5(content, content='message', content_rowid='rowid');
```

2. Add triggers to keep it in sync when messages are inserted or deleted:

```sql
CREATE TRIGGER IF NOT EXISTS message_fts_insert AFTER INSERT ON message BEGIN
    INSERT INTO message_fts(rowid, content) VALUES (new.rowid, new.content);
END;

CREATE TRIGGER IF NOT EXISTS message_fts_delete AFTER DELETE ON message BEGIN
    INSERT INTO message_fts(message_fts, rowid, content) VALUES ('delete', old.rowid, old.content);
END;
```

3. Update `search_messages` in `src-tauri/src/commands/messages.rs` to use the FTS index:

```sql
SELECT m.id, m.conversation_id, m.sender_id, m.content, m.sent_at
FROM message m
JOIN message_fts f ON m.rowid = f.rowid
WHERE message_fts MATCH ?1
ORDER BY m.sent_at DESC LIMIT ?2
```

This works entirely within `rusqlite` (no Turso changes needed), uses the bundled SQLite FTS5, and eliminates the table scan. The `bundled-sqlcipher` feature in Cargo.toml already includes FTS5.

The `snippet()` function could also power the `snippet` field in `SearchResult` to return highlighted context rather than the full content.

### Option B: FTS5 on hosted Turso for username/group/channel search (moderate complexity)

Add FTS5 virtual tables to the remote Turso schema for `users.username`, `groups.name`, and `channels.name`. This would improve the `search_user_by_username` command and add new group/channel search.

Caveats:
- FTS5 virtual tables on Turso must be kept in sync manually (via triggers created in migration files, or via rebuild after writes).
- Turso does not support triggers natively (they are a libSQL limitation — Turso cloud runs without trigger support in most configurations). Rebuilding the index on every relevant write from the Rust backend is the safer path.
- For the current scale of Pollis (small teams), the existing LIKE query on `users.username` is unlikely to be a performance problem. The benefit does not obviously outweigh the sync complexity.

### Option C: Wait for native FTS on hosted Turso cloud

Turso has announced that the new Tantivy-based FTS will eventually come to the hosted cloud product (the GitHub issue tracking FTS in the turso rewrite was closed in March 2026 as implemented). The migration of Turso Cloud from libSQL to the new engine has not yet happened. When it does, the cleaner `USING fts` syntax and automatic B-tree integration become available without shadow table management.

There is no public timeline for this migration.

---

## Recommendation

**For message search (the existing `search_messages` command):** Option A is viable and worth doing. It is entirely local, requires no remote schema changes, uses already-available tooling (`rusqlite` with FTS5), and avoids the encryption constraint entirely. The main effort is adding the FTS5 virtual table and triggers to `local_schema.sql` and updating the query in `messages.rs`. This would also enable proper relevance ranking and snippet highlighting.

**For metadata search (users, groups, channels):** The current LIKE queries are adequate at Pollis's current scale. FTS5 on Turso remote is possible but requires careful trigger/sync management that adds complexity without obvious user-facing benefit until user counts are in the tens of thousands. Defer this until the new Turso engine lands on hosted cloud.

**Do not attempt to use the new Tantivy-based FTS (`turso` crate) for remote Turso access.** The `turso` crate is an in-process beta database, not a remote client. Pollis's architecture routes all remote access through the `libsql` crate, and that is correct.

---

## Versions and Links

- Pollis uses `libsql = { version = "0.6", features = ["remote"] }` — the remote-connection client for hosted Turso cloud.
- The new `turso` in-process crate is separate and in beta. It is not a drop-in replacement for the `libsql` remote crate.
- Turso blog post on native FTS: https://turso.tech/blog/beyond-fts5
- Turso v0.5.0 release: https://turso.tech/blog/turso-0.5.0
- libSQL FTS5 issue (TypeScript client): https://github.com/tursodatabase/libsql/issues/1811
- Turso FTS GitHub issue (closed, implemented): https://github.com/tursodatabase/turso/issues/997
- turso crate: https://crates.io/crates/turso
