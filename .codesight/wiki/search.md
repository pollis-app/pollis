# Message Search

On-device full-text search over this device's decrypted message history (#850).

**Server-side search is impossible here by construction, and must never be
proposed.** Message bodies are E2EE; the only plaintext that exists anywhere is
the `content` column of the device's own SQLCipher database. A search box that
worked on the server would be a search box proving the encryption did not. The
rationale is also written up for users at `website/learn.html#on-device-search`,
and the older engineering note is `frontend/docs/turso-search.md`.

## What it replaced

`search_messages` used to be one statement:

```sql
SELECT id, conversation_id, sender_id, content, sent_at
FROM message WHERE content IS NOT NULL AND content LIKE ?1
ORDER BY sent_at DESC LIMIT ?2
```

No index, no ranking, no filters, `limit` hardcoded to 50 by its only caller, and
`snippet` was the whole message body. Because attachment metadata is stored as
JSON inside `content`, it also matched R2 object keys and content hashes — a
search for `media` hit every message with an attachment. Measured at 100k local
rows it was a **40–57 ms full table scan on every keystroke-debounce**, degrading
linearly.

## The index

`pollis-core/src/db/local_schema.sql`, appended as **additive `IF NOT EXISTS`
DDL**. `LOCAL_SCHEMA_VERSION` was NOT bumped and must never be bumped for this:
a bump DELETES the user's local database including their MLS state
(`db/local.rs`). The schema file is re-applied on every open, so existing
databases gain the index with nothing lost.

```sql
CREATE VIRTUAL TABLE IF NOT EXISTS message_fts USING fts5(
    body, content='', tokenize="unicode61 remove_diacritics 2");
```

* **Contentless** (`content=''`): FTS5 stores the term dictionary and nothing
  else. That is what keeps the cost at ~11 MB per 100k messages (+41% on a 28 MB
  DB) and, more importantly, keeps a second copy of every plaintext body out of
  the file. Snippets are cut in Rust from `message.content`; a contentless table
  cannot answer `snippet()` anyway.
* **`remove_diacritics 2`** is what makes `cafe` find `café`, `resume` find
  `résumé`, `nandu` find `Ñandú`. Cyrillic and Arabic tokenise as ordinary words
  with no help.
* FTS5 is **already compiled into the shipped binary** — `libsqlite3-sys`'s
  bundled branch sets `-DSQLITE_ENABLE_FTS5` unconditionally. No feature flag, no
  binary-size change. The one Cargo change was adding rusqlite's `functions`
  feature (pure Rust) for the scalar function below.

### Triggers, not Rust call sites

Three INSERT sites, six UPDATE sites and one DELETE site write to the local
`message` table. Covering them by hand is the "code discipline" rung `CLAUDE.md`
ranks last, and a write path added next year would silently stop indexing. The
three triggers (`message_fts_ai` / `_ad` / `_au`) put the invariant at the DB
layer, where a future write path cannot forget it.

Both the delete and insert halves call `pollis_search_text`, so **a contentless
index is told what to remove by being handed the body it indexed**. That makes
determinism a correctness requirement — see below.

### `pollis_search_text`

`pollis-core/src/db/search_text.rs`, a deterministic rusqlite scalar function
registered on every local connection. Three jobs:

1. **Unwraps the attachment envelope.** A body is raw text or
   `{"_att":[{"key":…,"url":…,"name":…,"hash":…}],"_txt":…}`. Only `_txt` and
   each attachment's `name` are indexed; `key`, `url`, `ct`, `hash`, `bh` and
   `size` are dropped. `Q3-budget-final.xlsx` becomes findable and the R2 key and
   content hash stop being — the filename still never leaves the device
   (`commands/r2.rs`).
2. **Segments CJK into overlapping bigrams.** `unicode61` tokenises `今天天气很好`
   as ONE token, so `今天` finds nothing; `trigram` needs three characters and most
   Chinese words are two. Bigrams at index time plus the identical transform on
   the query is what makes Mandarin searchable at all (#855).
3. **Emits sentinel tokens** — `zzpollisatt`, `zzpollislnk` — so
   `has:attachment` / `has:link` are ordinary MATCH terms rather than an extra
   column, an extra index, or a schema change. Sentinels are stripped from both
   message text and user input, so nobody can forge one by typing it.

Registration lives in `db::local::apply_local_schema`, which registers the
function **and** applies the schema as one step. That pairing is deliberate: a
connection with the schema but not the function accepts every read and fails
every INSERT into `message` with `no such function`. Two hand-rolled test
connections were doing exactly that before the pairing existed.

### Backfill and repair

`db/local.rs`:

| Function | Purpose |
|---|---|
| `search_backfill_pending` | `kv` flag `search_index_backfilled`; the schema batch is re-applied on every open, so a flag is what stops a full re-scan each start |
| `backfill_search_index_chunk` | Indexes 2,000 rows not present in `message_fts_docsize` — **resumable by construction**, from FTS5's own shadow table rather than a stored cursor |
| `finish_search_backfill` | `'optimize'`, then sets the flag |
| `search_index_is_healthy` | `'integrity-check'` |
| `rebuild_search_index` | `'delete-all'`, clear the flag, re-backfill |

`spawn_search_index_maintenance` (`state.rs`) runs both off the sign-in path:
integrity-check first (rebuild silently if it fails — the user did nothing wrong
and has nothing to decide), then the chunked backfill with a `yield_now().await`
between chunks so the local-DB mutex is never held long enough to sit in front of
a send. Measured backfill: **265 ms for 100k messages**, which is 265 ms nobody
should wait to sign in for.

The manual escape hatch is **Preferences → "Rebuild search index"**
(`pref-rebuild-search-index`), for when the automatic repair is itself wrong.

## Querying

`pollis-core/src/commands/messages/search.rs`.

**Raw user text NEVER reaches `MATCH`.** A stray quote, `*`, `AND` or `NEAR`
either errors or silently changes meaning. The query is parsed into a
`ParsedQuery` and re-emitted as a well-formed FTS5 expression: every bare term
quoted, a trailing `*` on the FINAL term only (as-you-type prefix search on the
word still being typed, not on words the user finished).

| Filter | Resolution |
|---|---|
| `from:@alice` | `user_cache`, falling back to one DS lookup; `m.sender_id = ?` |
| `in:#channel` / `in:@person` | `conversation_cache` (local only); `m.conversation_id = ?` |
| `before:` / `after:` / `on:` | `m.sent_at` string compare — ISO-8601 sorts lexicographically |
| `has:attachment` / `has:link` | the sentinel tokens, as ordinary MATCH terms |

An **unresolvable** `from:`/`in:` yields zero results, not "ignore the filter and
show everything". A malformed date stays a search term rather than silently
filtering the corpus to nothing.

### Ranking

* **Relevance** — `bm25(message_fts)` for the top 500 candidates, then rescored
  in Rust: `score = bm25 / (1 + age_days / 90)`. bm25 is negative and lower is
  better, so dividing by a number that grows with age pulls an old hit towards
  zero. A good hit from last week outranks an equally good hit from two years
  ago; a substantially better old hit still wins. Keeping the formula in Rust
  keeps it testable and tunable without SQL gymnastics.
* **Recency** — `ORDER BY m.sent_at DESC, m.id DESC`.

**Default: relevance for a global search, recency when scoped to one
conversation** (Slack defaults to relevance, Discord to recency; scoping the
default to the query type gets both right). The page reports the ordering it
actually applied, and the UI shows a "Most recent / Most relevant" toggle.

Measured at 100k rows: selective term **2–3 ms ranked / 0.1–0.3 ms recency**,
conversation-scoped **1.1 ms**, a term present in 51% of the corpus **54 ms
ranked**. `count(*) … MATCH` is **1.4 ms** even for that pathological term, so
"About N results" is free. The LIKE baseline paid 40–57 ms for *every* query.

### Results

`SearchPage { results, total, next_cursor, sort, corpus }`. `SearchCursor` is an
**offset**, because relevance ordering is decided after the rows leave SQLite and
a keyset cursor cannot describe a position in an order the database never
produced. `corpus` is populated on the first page only. In relevance mode the
cursor stops at the 500-candidate rescore ceiling — `total` still reports the
full match count, but paging past the ceiling would serve rows bm25 never
ranked, so `next_cursor` goes `None` there; switching to Most recent pages the
whole corpus. A query whose terms all die in tokenisation (`???`) returns zero
results rather than falling through to a filter-only scan of everything.

`SearchResult` carries `conversation_kind`, `conversation_name`, `group_id`,
`group_name`, `sender_username`, `thread_id`, `has_attachment`, `has_link`, and a
structured `snippet { text, highlights: [[start, end], …] }`.

**Highlight offsets are UTF-16 code units**, not Unicode scalars: the only
consumer is JavaScript, whose string indices are code units, so an emoji earlier
in a snippet would otherwise shift every subsequent highlight. Returning ranges
rather than HTML also lets React render `<mark>` with no
`dangerouslySetInnerHTML`, and retired a reused-`lastIndex` regex bug in the old
client-side highlighter.

## `conversation_cache`

Channel and group names are **remote-only** — `list_group_channels` and the
bootstrap read both go to the DS, and there is no embedded replica. A local
`message` row cannot even say whether its `conversation_id` is a channel or a
DM. Without a local mirror, every result page needs an N+1 network round trip to
render a name and search is broken offline.

```sql
CREATE TABLE IF NOT EXISTS conversation_cache (
    id TEXT PRIMARY KEY, kind TEXT NOT NULL CHECK (kind IN ('channel','dm')),
    name TEXT, group_id TEXT, group_name TEXT, updated_at TEXT NOT NULL);
```

Written by `list_user_groups_with_channels` (channels) and `list_dm_channels`
(DMs, named after the peer) — the two reads that already list them — mirroring
how `attach_sender_usernames_local` writes `user_cache`. Best-effort and
disposable: a miss renders the id, which is what search did for every result
before this table existed.

## Commands

| Command | Notes |
|---|---|
| `search_messages(query, conversationId?, sort?, limit?, cursor?)` | Returns `SearchPage` |
| `rebuild_search_index()` | The Preferences escape hatch |
| `read_messages_around(conversationId, messageId, limit?)` | A page centred on one message, anchored on `(sent_at, id)` — the jump-to-message read path |
| `read_messages_after(conversationId, cursor, limit?)` | The oldest `limit` messages newer than `cursor` — how a jump's window pages forward to the live tail (#1039) |

Registered in `pollis-core/src/bridge.rs`, `src-tauri/src/commands/messages.rs`,
`src-tauri/src/lib.rs`'s `invoke_handler!` and `src-tauri/src/test_harness.rs`.

## The renderer

* `frontend/src/components/Search/SearchView.tsx` — input, sort toggle, results,
  "About N results", the corpus footer, the empty state.
* `frontend/src/pages/Search.tsx` — routes a hit by `conversation_kind` /
  `group_id`, falling back to `routeForConversation`. It previously navigated
  **every** hit to `/dms/$conversationId`, so every channel result landed on a
  broken DM route.
* `frontend/src/hooks/queries/useSearchMessages.ts` — `useInfiniteQuery` plus
  `useRebuildSearchIndex`.
* `frontend/src/components/SearchPanel.tsx` — Cmd+K stays a **navigator**. A
  "Search messages for …" row is appended (last, not first: Enter has always
  meant "go to the best-matching place", and pinning this to the top would
  redefine the default action of the app's most-used shortcut) and hands the
  query to `/search` via `?q=`. Message hits never render inline.
* Jump-to-message reuses #854's machinery — `messageJumpStore` →
  `MessageList.revealRow` → `pollis-message-flash`. `?message=` was added to
  `validatePanelSearch` (`types/panel.ts`); the root route's `validateSearch`
  **silently drops every key it does not return**, so a param that is not
  declared there does not exist. `MainContent` consumes it once, calls
  `read_messages_around` when the target is outside the loaded window, and
  strips the param so a back-navigation does not re-flash. The page that comes
  back opens an **anchored window** rather than being merged into the live one
  (#1039) — see [UI → The windowed log](./ui.md#the-windowed-log-874).

## Honesty (§6 of the ticket)

Search covers what THIS device holds, which is narrower than users assume. The
UI says so rather than letting "No results" imply "no such message":

* a persistent footer — *"Searching N messages stored on this device, back to
  &lt;date&gt;"* — plus a link to the retention setting when a window is set, and a
  link to the `/learn` entry;
* an empty state listing the five real reasons: not ingested on this device,
  before you joined (MLS, accepted loss 1), before this device was added
  (accepted loss 2), outside the retention window, or deleted.

Background ingest would fix the "never opened here" case and is deliberately a
separate, larger ticket.

## Tests

* `pollis-core/src/db/local.rs::tests::the_search_index_cannot_drift` — **the
  mandated invariant.** After every write shape the `message` table admits —
  insert, insert-already-deleted, edit, soft delete, moderator delete (including
  a replayed one), retention eviction — `INSERT INTO
  message_fts(message_fts) VALUES('integrity-check')` passes AND the FTS row
  count equals `SELECT count(*) FROM message WHERE content IS NOT NULL AND
  deleted_at IS NULL`. "Search finds a word I just sent" is explicitly not
  coverage. Siblings: `the_backfill_reaches_the_same_invariant`,
  `rebuilding_the_index_is_idempotent`, `the_indexed_body_is_the_transformed_one`.
* `commands/messages/search.rs::tests` — the parser, and `run_query` executed
  against a real FTS index: ranking, both orderings, every filter, accent
  folding, CJK, attachment hygiene, and a hostile-syntax sweep proving no query
  can error.
* `db/search_text.rs::tests` — the transform, including determinism.
* `e2e/search.spec.ts` — the user-visible flow in both skins: type a query, see
  ranked results with real names, click a channel hit, land on the channel route,
  see the message flash. Backed by the `search_messages` case in
  `frontend/src/__mocks__/tauri-core.ts`.

## Rejected

| Option | Why not |
|---|---|
| Server-side / Turso FTS | Bodies are E2EE; the server has ciphertext. Non-starter by design. |
| Keep `LIKE`, fix only the UI | 40–57 ms full scan at 100k and growing, no ranking, and it matches R2 keys and content hashes. |
| `trigram` tokenizer | Two-character CJK queries return nothing — fatal for Mandarin. |
| ICU tokenizer | `SQLITE_ENABLE_ICU` is not compiled in; linking ICU on five platforms costs MB for what bigrams get free. |
| Custom FTS5 tokenizer | rusqlite 0.37 exposes no FTS5 tokenizer API — raw unsafe FFI. |
| FTS writes at each Rust call site | Ten write sites; a missed one loses index rows silently. |
| Tantivy / a separate index | A second store to keep in sync, and a plaintext index on disk **outside** SQLCipher. |
| Bumping `LOCAL_SCHEMA_VERSION` | Wipes every user's local DB including MLS state. |
