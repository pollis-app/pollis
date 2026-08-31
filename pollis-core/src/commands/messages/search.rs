//! On-device message search (#850).
//!
//! **Server-side search is impossible here by construction and must never be
//! proposed.** Message bodies are E2EE; the only plaintext that exists anywhere
//! is the `content` column of this device's own SQLCipher database. Everything
//! in this module runs against that, locally, offline-capable.
//!
//! What replaced the old `content LIKE '%q%'` full table scan:
//!
//! * an FTS5 contentless index kept in sync by triggers (`local_schema.sql`),
//! * a **parsed** query — `from:`, `in:`, `before:`/`after:`/`on:`,
//!   `has:attachment`, `has:link` — re-emitted as a well-formed FTS5
//!   expression. Raw user text NEVER reaches `MATCH`: a stray quote, `*`, `AND`
//!   or `NEAR` would either error or silently mean something else,
//! * two orderings — bm25 relevance rescored in Rust with a recency decay, and
//!   plain recency — defaulting to relevance globally and recency inside one
//!   conversation, because that is where each is the right answer,
//! * cursor pagination and a total hit count, replacing a hardcoded `limit: 50`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::db::search_text::{
    has_attachment, has_cjk, plain_text, segment_cjk, strip_sentinels, ATTACHMENT_SENTINEL,
    LINK_SENTINEL,
};
use crate::error::Result;
use crate::state::AppState;

use super::types::{
    SearchCorpus, SearchCursor, SearchPage, SearchResult, SearchSnippet, SearchSort,
};

/// Results per page when the caller does not say.
const DEFAULT_LIMIT: i64 = 25;

/// Hard ceiling on a page, so a caller cannot ask for the whole corpus in one
/// IPC message.
const MAX_LIMIT: i64 = 200;

/// How many bm25-ranked candidates the Rust rescorer considers.
///
/// Relevance ordering is not expressible in SQL alone — the recency decay is —
/// so the top slice is pulled out and re-sorted here. 500 is far past what
/// anyone pages through, and it is what keeps a pathological term (present in
/// half the corpus) from materialising 50k rows.
const RESCORE_CANDIDATES: i64 = 500;

/// Half-life-ish constant for the relevance recency decay, in days.
///
/// `score = bm25 / (1 + age_days / 90)`. bm25 is negative and lower is better,
/// so dividing by a number that grows with age pulls an old hit TOWARDS zero,
/// i.e. down the list. A good hit from last week outranks an equally good hit
/// from two years ago; a much better old hit still wins.
const RECENCY_DECAY_DAYS: f64 = 90.0;

/// Characters of context either side of the first match in a snippet.
const SNIPPET_RADIUS: usize = 60;

// ── Query parsing ────────────────────────────────────────────────────────────

/// A search query after parsing: free text separated from filters.
///
/// The whole point of this type is that nothing downstream ever sees the raw
/// string again.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct ParsedQuery {
    /// Bare words, in order. Sentinels are already stripped.
    pub terms: Vec<String>,
    /// `"quoted phrases"`, matched as adjacent tokens.
    pub phrases: Vec<String>,
    /// `from:@alice` — a username, not yet resolved to an id.
    pub from: Option<String>,
    /// `in:#general` or `in:@bob` — a conversation name, not yet resolved.
    pub in_conversation: Option<String>,
    /// `before:2026-01-31` — an ISO date.
    pub before: Option<String>,
    /// `after:2026-01-01`.
    pub after: Option<String>,
    /// `on:2026-01-15`.
    pub on: Option<String>,
    pub has_attachment: bool,
    pub has_link: bool,
}

impl ParsedQuery {
    /// Does this query say anything the index can answer?
    fn has_match_terms(&self) -> bool {
        !self.terms.is_empty()
            || !self.phrases.is_empty()
            || self.has_attachment
            || self.has_link
    }

    /// Is there anything to search for at all?
    fn is_empty(&self) -> bool {
        !self.has_match_terms()
            && self.from.is_none()
            && self.in_conversation.is_none()
            && self.before.is_none()
            && self.after.is_none()
            && self.on.is_none()
    }
}

/// Split a query into words and `"quoted phrases"`, tolerating an unbalanced
/// trailing quote (which is what a half-typed query always looks like).
fn tokenize(query: &str) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for c in query.chars() {
        match c {
            '"' => {
                if in_quotes {
                    if !current.trim().is_empty() {
                        out.push((current.trim().to_string(), true));
                    }
                    current.clear();
                    in_quotes = false;
                } else {
                    if !current.trim().is_empty() {
                        out.push((current.trim().to_string(), false));
                    }
                    current.clear();
                    in_quotes = true;
                }
            }
            c if c.is_whitespace() && !in_quotes => {
                if !current.trim().is_empty() {
                    out.push((current.trim().to_string(), false));
                }
                current.clear();
            }
            c => current.push(c),
        }
    }
    if !current.trim().is_empty() {
        // An unterminated quote degrades to a phrase rather than an error.
        out.push((current.trim().to_string(), in_quotes));
    }
    out
}

/// Parse Slack-style operators out of a raw query string.
///
/// Unknown `word:value` shapes are left as ordinary search terms — a message
/// really can contain `http://x` or `note:` and pretending otherwise would make
/// text unfindable.
pub fn parse_query(query: &str) -> ParsedQuery {
    let mut parsed = ParsedQuery::default();

    for (token, is_phrase) in tokenize(query) {
        if is_phrase {
            let cleaned = strip_sentinels(&token);
            if !cleaned.trim().is_empty() {
                parsed.phrases.push(cleaned.trim().to_string());
            }
            continue;
        }

        let lower = token.to_lowercase();
        if let Some(value) = lower.strip_prefix("has:") {
            match value {
                "attachment" | "attachments" | "file" | "files" => {
                    parsed.has_attachment = true;
                    continue;
                }
                "link" | "links" | "url" => {
                    parsed.has_link = true;
                    continue;
                }
                _ => {}
            }
        }

        let split = token.split_once(':');
        if let Some((key, value)) = split {
            let value = value.trim();
            if !value.is_empty() {
                match key.to_lowercase().as_str() {
                    "from" => {
                        parsed.from = Some(value.trim_start_matches('@').to_string());
                        continue;
                    }
                    "in" => {
                        parsed.in_conversation = Some(value.to_string());
                        continue;
                    }
                    "before" => {
                        if let Some(d) = normalize_date(value) {
                            parsed.before = Some(d);
                            continue;
                        }
                    }
                    "after" => {
                        if let Some(d) = normalize_date(value) {
                            parsed.after = Some(d);
                            continue;
                        }
                    }
                    "on" => {
                        if let Some(d) = normalize_date(value) {
                            parsed.on = Some(d);
                            continue;
                        }
                    }
                    _ => {}
                }
            }
        }

        let cleaned = strip_sentinels(&token);
        let cleaned = cleaned.trim();
        if !cleaned.is_empty() {
            parsed.terms.push(cleaned.to_string());
        }
    }

    parsed
}

/// Accept `YYYY-MM-DD` and reject everything else, so a malformed date stays a
/// search term instead of silently filtering the corpus to nothing.
fn normalize_date(value: &str) -> Option<String> {
    chrono::NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .map(|d| d.format("%Y-%m-%d").to_string())
}

/// The day after `date`, for the exclusive upper bound of `on:` and the
/// inclusive lower bound of `after:`.
fn next_day(date: &str) -> Option<String> {
    let parsed = chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").ok()?;
    parsed
        .succ_opt()
        .map(|d| d.format("%Y-%m-%d").to_string())
}

/// Turn one search term into a well-formed FTS5 expression fragment.
///
/// Latin text becomes a quoted string (so `AND`, `NEAR`, `*` and friends are
/// literal). CJK becomes the same overlapping-bigram sequence the index holds,
/// issued as a phrase, which is what makes a two-character Chinese word
/// findable at all.
fn term_expression(term: &str, allow_prefix: bool) -> Option<String> {
    if has_cjk(term) {
        let segmented = segment_cjk(term);
        let segmented = segmented.trim();
        if segmented.is_empty() {
            return None;
        }
        return Some(format!("\"{}\"", escape_fts(segmented)));
    }
    let cleaned: String = term
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == '\'' || *c == '-' || *c == '_' || *c == '.')
        .collect();
    if cleaned.is_empty() {
        return None;
    }
    if allow_prefix {
        // As-you-type search should behave like a prefix search on the word
        // still being typed, and only on that one.
        Some(format!("\"{}\"*", escape_fts(&cleaned)))
    } else {
        Some(format!("\"{}\"", escape_fts(&cleaned)))
    }
}

/// A double quote is the only character with meaning inside an FTS5 string
/// literal, and it is escaped by doubling.
fn escape_fts(s: &str) -> String {
    s.replace('"', "\"\"")
}

/// Build the FTS5 MATCH expression, or `None` when the query has no text side
/// (filters only).
pub fn match_expression(parsed: &ParsedQuery) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();

    for phrase in &parsed.phrases {
        let body = if has_cjk(phrase) {
            segment_cjk(phrase).trim().to_string()
        } else {
            phrase.to_string()
        };
        if !body.is_empty() {
            parts.push(format!("\"{}\"", escape_fts(&body)));
        }
    }

    let last = parsed.terms.len().saturating_sub(1);
    for (i, term) in parsed.terms.iter().enumerate() {
        // Only the final term is a prefix match: the earlier ones are words the
        // user finished typing, and prefixing them makes `the cat` match
        // `theatre catalogue`.
        if let Some(expr) = term_expression(term, i == last) {
            parts.push(expr);
        }
    }

    if parsed.has_attachment {
        parts.push(format!("\"{ATTACHMENT_SENTINEL}\""));
    }
    if parsed.has_link {
        parts.push(format!("\"{LINK_SENTINEL}\""));
    }

    if parts.is_empty() {
        return None;
    }
    Some(parts.join(" AND "))
}

// ── Filter resolution ────────────────────────────────────────────────────────

/// The filters, with names resolved to ids.
#[derive(Debug, Default)]
struct ResolvedFilters {
    sender_id: Option<String>,
    conversation_id: Option<String>,
    /// A `from:`/`in:` that matched nothing. The correct answer is then zero
    /// results, NOT "ignore the filter and show everything".
    unresolved: bool,
}

/// Resolve `from:` against `user_cache`, falling back to one DS lookup.
async fn resolve_sender(state: &Arc<AppState>, username: &str) -> Result<Option<String>> {
    let cached: Option<String> = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        db.conn()
            .query_row(
                "SELECT id FROM user_cache WHERE lower(username) = lower(?1)",
                rusqlite::params![username],
                |row| row.get(0),
            )
            .ok()
    };
    if cached.is_some() {
        return Ok(cached);
    }

    // Offline is not an error here: an unresolvable `from:` yields no results,
    // which is the honest answer for "messages from a person I cannot name".
    match crate::commands::ds_reads::users(state, Vec::new(), Some(username.to_string())).await {
        Ok(users) => Ok(users.into_iter().next().map(|u| u.id)),
        Err(e) => {
            eprintln!("[search] from: lookup fell back to remote and failed: {e}");
            Ok(None)
        }
    }
}

/// Resolve `in:#channel` / `in:@person` against `conversation_cache`.
///
/// Local only, deliberately: the cache is written by the two reads that already
/// list channels and DMs, so anything the user can see in their sidebar is
/// resolvable here without a round trip.
fn resolve_conversation(conn: &rusqlite::Connection, raw: &str) -> Option<String> {
    let (kind, name) = match raw.chars().next() {
        Some('#') => (Some("channel"), &raw[1..]),
        Some('@') => (Some("dm"), &raw[1..]),
        _ => (None, raw),
    };
    if name.is_empty() {
        return None;
    }

    let sql = match kind {
        Some(_) => {
            "SELECT id FROM conversation_cache WHERE kind = ?2 AND lower(name) = lower(?1) LIMIT 1"
        }
        None => "SELECT id FROM conversation_cache WHERE lower(name) = lower(?1) LIMIT 1",
    };
    let found: Option<String> = match kind {
        Some(k) => conn
            .query_row(sql, rusqlite::params![name, k], |row| row.get(0))
            .ok(),
        None => conn
            .query_row(sql, rusqlite::params![name], |row| row.get(0))
            .ok(),
    };
    // A raw id pasted into `in:` is a legitimate spelling too.
    found.or_else(|| {
        conn.query_row(
            "SELECT id FROM conversation_cache WHERE id = ?1",
            rusqlite::params![raw],
            |row| row.get(0),
        )
        .ok()
    })
}

// ── The command ──────────────────────────────────────────────────────────────

/// One row as it comes back from SQLite, before enrichment.
struct RawHit {
    message_id: String,
    conversation_id: String,
    sender_id: String,
    content: String,
    sent_at: String,
    thread_id: Option<String>,
    rank: f64,
}

/// Search this device's decrypted message history.
///
/// `conversation_id` scopes the search to one conversation, which is also what
/// flips the default ordering to recency — inside a conversation people are
/// looking for "that thing from last week", globally they are looking for the
/// best match.
pub async fn search_messages(
    query: String,
    conversation_id: Option<String>,
    sort: Option<SearchSort>,
    limit: Option<i64>,
    cursor: Option<SearchCursor>,
    state: &Arc<AppState>,
) -> Result<SearchPage> {
    let limit = limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let offset = cursor.as_ref().map(|c| c.offset).unwrap_or(0).max(0);
    let parsed = parse_query(&query);

    let scoped = conversation_id.is_some() || parsed.in_conversation.is_some();
    let sort = sort.unwrap_or(if scoped {
        SearchSort::Recent
    } else {
        SearchSort::Relevant
    });

    if parsed.is_empty() {
        let corpus = read_corpus(state).await?;
        return Ok(SearchPage {
            results: Vec::new(),
            total: 0,
            next_cursor: None,
            sort,
            corpus,
        });
    }

    // Filters first: an unresolvable `from:`/`in:` means zero results, and
    // finding that out before touching the index saves the query entirely.
    let mut filters = ResolvedFilters {
        conversation_id: conversation_id.clone(),
        ..Default::default()
    };
    if let Some(username) = parsed.from.as_deref() {
        match resolve_sender(state, username).await? {
            Some(id) => filters.sender_id = Some(id),
            None => filters.unresolved = true,
        }
    }
    if let Some(raw) = parsed.in_conversation.as_deref() {
        let resolved = {
            let guard = state.local_db.lock().await;
            let db = guard
                .as_ref()
                .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
            resolve_conversation(db.conn(), raw)
        };
        match resolved {
            Some(id) => filters.conversation_id = Some(id),
            None => filters.unresolved = true,
        }
    }

    if filters.unresolved {
        let corpus = read_corpus(state).await?;
        return Ok(SearchPage {
            results: Vec::new(),
            total: 0,
            next_cursor: None,
            sort,
            corpus,
        });
    }

    let match_expr = match_expression(&parsed);

    // Terms were typed but none survived tokenisation (`???`, `!!!`): nothing
    // the index can answer. Zero results is the honest answer — falling
    // through would run the filter-only scan and present the entire corpus
    // as "matches" for a query that matched nothing.
    if parsed.has_match_terms() && match_expr.is_none() {
        let corpus = read_corpus(state).await?;
        return Ok(SearchPage {
            results: Vec::new(),
            total: 0,
            next_cursor: None,
            sort,
            corpus,
        });
    }

    // The rusqlite connection and its statements are not `Send`, so all of them
    // have to be dropped before the awaits below — scoping the whole DB read in
    // a block is what keeps this command `Send`.
    let (hits, total) = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
        run_query(db.conn(), &parsed, &filters, match_expr.as_deref(), sort, limit, offset)?
    };

    let mut results = enrich(state, hits, &parsed).await?;
    // Highlighting is the last step so it sees the final snippet text.
    for result in results.iter_mut() {
        result.snippet.highlights = highlight_ranges(&result.snippet.text, &parsed);
    }

    // Relevance mode rescores the best RESCORE_CANDIDATES bm25 hits and can
    // page no deeper — `total` stays the honest match count, but the cursor
    // must stop at the pageable ceiling instead of handing out one empty page.
    let pageable = if sort == SearchSort::Relevant && match_expr.is_some() {
        total.min(RESCORE_CANDIDATES)
    } else {
        total
    };
    let returned = offset + results.len() as i64;
    let next_cursor = if results.len() as i64 == limit && returned < pageable {
        Some(SearchCursor { offset: returned })
    } else {
        None
    };

    // Only the first page pays for the corpus stats — the footer that renders
    // them does not change while paging.
    let corpus = if offset == 0 {
        read_corpus(state).await?
    } else {
        SearchCorpus::default()
    };

    Ok(SearchPage {
        results,
        total,
        next_cursor,
        sort,
        corpus,
    })
}

/// Build and run the ranked query. Everything here is synchronous SQLite work.
fn run_query(
    conn: &rusqlite::Connection,
    parsed: &ParsedQuery,
    filters: &ResolvedFilters,
    match_expr: Option<&str>,
    sort: SearchSort,
    limit: i64,
    offset: i64,
) -> Result<(Vec<RawHit>, i64)> {
    let mut wheres: Vec<String> = vec![
        "m.content IS NOT NULL".to_string(),
        "m.deleted_at IS NULL".to_string(),
    ];
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = Vec::new();

    let from_clause = if let Some(expr) = match_expr {
        wheres.insert(0, "message_fts MATCH ?".to_string());
        params.push(Box::new(expr.to_string()));
        "FROM message_fts JOIN message m ON m.rowid = message_fts.rowid"
    } else {
        // Filters with no text side: an ordinary indexed scan, honestly
        // ordered by recency because there is nothing to rank.
        "FROM message m"
    };

    if let Some(id) = &filters.sender_id {
        wheres.push("m.sender_id = ?".to_string());
        params.push(Box::new(id.clone()));
    }
    if let Some(id) = &filters.conversation_id {
        wheres.push("m.conversation_id = ?".to_string());
        params.push(Box::new(id.clone()));
    }
    // ISO-8601 sorts lexicographically, so a date compare is a string compare.
    if let Some(date) = &parsed.before {
        wheres.push("m.sent_at < ?".to_string());
        params.push(Box::new(date.clone()));
    }
    if let Some(date) = &parsed.after {
        if let Some(next) = next_day(date) {
            wheres.push("m.sent_at >= ?".to_string());
            params.push(Box::new(next));
        }
    }
    if let Some(date) = &parsed.on {
        wheres.push("m.sent_at >= ?".to_string());
        params.push(Box::new(date.clone()));
        if let Some(next) = next_day(date) {
            wheres.push("m.sent_at < ?".to_string());
            params.push(Box::new(next));
        }
    }

    let where_sql = wheres.join(" AND ");

    let count_sql = format!("SELECT count(*) {from_clause} WHERE {where_sql}");
    let total: i64 = conn.query_row(
        &count_sql,
        rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
        |row| row.get(0),
    )?;
    if total == 0 {
        return Ok((Vec::new(), 0));
    }

    let use_relevance = sort == SearchSort::Relevant && match_expr.is_some();
    let rank_select = if match_expr.is_some() {
        "bm25(message_fts)"
    } else {
        "0.0"
    };
    let (order_sql, row_limit, row_offset) = if use_relevance {
        // Pull the best candidates, then rescore in Rust: the recency decay is
        // not expressible in the SQL ordering without ugly date arithmetic in
        // every query, and keeping the formula in Rust keeps it testable.
        ("ORDER BY bm25(message_fts)", RESCORE_CANDIDATES, 0)
    } else {
        ("ORDER BY m.sent_at DESC, m.id DESC", limit, offset)
    };

    let sql = format!(
        "SELECT m.id, m.conversation_id, m.sender_id, m.content, m.sent_at, m.thread_id, {rank_select} \
         {from_clause} WHERE {where_sql} {order_sql} LIMIT ? OFFSET ?"
    );
    params.push(Box::new(row_limit));
    params.push(Box::new(row_offset));

    let mut stmt = conn.prepare(&sql)?;
    let mapped = stmt.query_map(
        rusqlite::params_from_iter(params.iter().map(|p| p.as_ref())),
        |row| {
            Ok(RawHit {
                message_id: row.get(0)?,
                conversation_id: row.get(1)?,
                sender_id: row.get(2)?,
                content: row.get(3)?,
                sent_at: row.get(4)?,
                thread_id: row.get(5)?,
                rank: row.get::<_, f64>(6)?,
            })
        },
    )?;
    let mut hits: Vec<RawHit> = mapped.flatten().collect();

    if use_relevance {
        rescore_by_recency(&mut hits);
        let start = (offset as usize).min(hits.len());
        let end = (start + limit as usize).min(hits.len());
        hits = hits.drain(start..end).collect();
    }

    Ok((hits, total))
}

/// Re-sort bm25 candidates so recency counts for something.
///
/// bm25 is negative and lower is better. Dividing by `1 + age/90` moves an old
/// hit towards zero, i.e. down the list, without ever letting age alone beat a
/// substantially better match.
fn rescore_by_recency(hits: &mut [RawHit]) {
    let now = chrono::Utc::now();
    let mut scored: Vec<(usize, f64)> = hits
        .iter()
        .enumerate()
        .map(|(i, hit)| {
            let age_days = chrono::DateTime::parse_from_rfc3339(&hit.sent_at)
                .map(|t| (now - t.to_utc()).num_days().max(0) as f64)
                .unwrap_or(0.0);
            (i, hit.rank / (1.0 + age_days / RECENCY_DECAY_DAYS))
        })
        .collect();
    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    let order: Vec<usize> = scored.into_iter().map(|(i, _)| i).collect();
    apply_permutation(hits, &order);
}

/// Reorder `items` so that `items[i]` ends up holding what was at `order[i]`.
fn apply_permutation<T>(items: &mut [T], order: &[usize]) {
    let mut position: Vec<usize> = vec![0; items.len()];
    for (target, &source) in order.iter().enumerate() {
        position[source] = target;
    }
    for i in 0..items.len() {
        while position[i] != i {
            let target = position[i];
            items.swap(i, target);
            position.swap(i, target);
        }
    }
}

/// Attach the names, kinds and snippets a result needs to render.
async fn enrich(
    state: &Arc<AppState>,
    hits: Vec<RawHit>,
    parsed: &ParsedQuery,
) -> Result<Vec<SearchResult>> {
    if hits.is_empty() {
        return Ok(Vec::new());
    }

    let conversation_ids: HashSet<String> =
        hits.iter().map(|h| h.conversation_id.clone()).collect();
    let sender_ids: HashSet<String> = hits.iter().map(|h| h.sender_id.clone()).collect();

    struct CachedRow {
        kind: String,
        name: Option<String>,
        group_id: Option<String>,
        group_name: Option<String>,
    }

    let (conversations, mut usernames) = {
        let guard = state.local_db.lock().await;
        let db = guard
            .as_ref()
            .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;

        let mut conversations: HashMap<String, CachedRow> = HashMap::new();
        let ids: Vec<String> = conversation_ids.into_iter().collect();
        for chunk in crate::db::chunk::bind_chunks(&ids, 0) {
            let sql = format!(
                "SELECT id, kind, name, group_id, group_name FROM conversation_cache WHERE id IN ({})",
                crate::db::chunk::placeholders(chunk.len(), 1)
            );
            let mut stmt = db.conn().prepare(&sql)?;
            let mapped = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    CachedRow {
                        kind: row.get(1)?,
                        name: row.get(2)?,
                        group_id: row.get(3)?,
                        group_name: row.get(4)?,
                    },
                ))
            })?;
            for (id, conv) in mapped.flatten() {
                conversations.insert(id, conv);
            }
        }

        let mut usernames: HashMap<String, String> = HashMap::new();
        let ids: Vec<String> = sender_ids.iter().cloned().collect();
        for chunk in crate::db::chunk::bind_chunks(&ids, 0) {
            let sql = format!(
                "SELECT id, username FROM user_cache WHERE id IN ({})",
                crate::db::chunk::placeholders(chunk.len(), 1)
            );
            let mut stmt = db.conn().prepare(&sql)?;
            let mapped = stmt.query_map(rusqlite::params_from_iter(chunk.iter()), |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for (id, name) in mapped.flatten() {
                usernames.insert(id, name);
            }
        }
        (conversations, usernames)
    };

    // One request for whatever the cache could not name. Offline just means the
    // id renders, exactly as it did before this ticket.
    let missing: Vec<String> = sender_ids
        .iter()
        .filter(|id| !usernames.contains_key(*id))
        .cloned()
        .collect();
    if !missing.is_empty() {
        match crate::commands::ds_reads::users(state, missing, None).await {
            Ok(users) => {
                let fetched: Vec<(String, String)> =
                    users.into_iter().map(|u| (u.id, u.username)).collect();
                let guard = state.local_db.lock().await;
                if let Some(db) = guard.as_ref() {
                    for (id, name) in &fetched {
                        let _ = db.conn().execute(
                            "INSERT INTO user_cache (id, username, updated_at) VALUES (?1, ?2, datetime('now')) \
                             ON CONFLICT(id) DO UPDATE SET username = ?2, updated_at = datetime('now')",
                            rusqlite::params![id, name],
                        );
                    }
                }
                for (id, name) in fetched {
                    usernames.insert(id, name);
                }
            }
            Err(e) => {
                eprintln!("[search] sender name lookup failed (non-fatal): {e}");
            }
        }
    }

    Ok(hits
        .into_iter()
        .map(|hit| {
            let conv = conversations.get(&hit.conversation_id);
            SearchResult {
                snippet: build_snippet(&hit.content, parsed),
                has_attachment: has_attachment(&hit.content),
                has_link: crate::db::search_text::has_link(&hit.content),
                conversation_kind: conv.map(|c| c.kind.clone()),
                conversation_name: conv.and_then(|c| c.name.clone()),
                group_id: conv.and_then(|c| c.group_id.clone()),
                group_name: conv.and_then(|c| c.group_name.clone()),
                sender_username: usernames.get(&hit.sender_id).cloned(),
                message_id: hit.message_id,
                conversation_id: hit.conversation_id,
                sender_id: hit.sender_id,
                content: hit.content,
                sent_at: hit.sent_at,
                thread_id: hit.thread_id,
            }
        })
        .collect())
}

/// What this device actually holds, for the footer that says so (§6 of #850).
async fn read_corpus(state: &Arc<AppState>) -> Result<SearchCorpus> {
    if let Some(cached) = cached_corpus() {
        return Ok(cached);
    }

    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
    let (message_count, earliest_sent_at): (i64, Option<String>) = db.conn().query_row(
        "SELECT count(*), min(sent_at) FROM message WHERE content IS NOT NULL AND deleted_at IS NULL",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let retention_days = crate::db::local::get_message_retention_days(db.conn()).unwrap_or(0);
    let indexing = crate::db::local::search_backfill_pending(db.conn()).unwrap_or(false);
    let corpus = SearchCorpus {
        message_count,
        earliest_sent_at,
        retention_days,
        indexing,
    };
    store_corpus(&corpus);
    Ok(corpus)
}

/// How long a corpus reading stays good enough to reuse.
///
/// `count(*) … WHERE content IS NOT NULL` cannot use an index — it is a full
/// scan of the `message` table, ~10–20 ms at 100k rows. Cheap once; charged on
/// EVERY debounced keystroke it would dominate a 2–3 ms query and undo the
/// point of the whole ticket. The number it produces is a footer describing how
/// much history this device holds, which moves by single messages over minutes,
/// so a bounded staleness is not a correctness compromise — it is the honest
/// shape of the quantity.
const CORPUS_TTL: std::time::Duration = std::time::Duration::from_secs(60);

static CORPUS_CACHE: std::sync::Mutex<Option<(std::time::Instant, SearchCorpus)>> =
    std::sync::Mutex::new(None);

fn cached_corpus() -> Option<SearchCorpus> {
    let guard = CORPUS_CACHE.lock().ok()?;
    let (at, corpus) = guard.as_ref()?;
    if at.elapsed() > CORPUS_TTL {
        return None;
    }
    Some(SearchCorpus {
        message_count: corpus.message_count,
        earliest_sent_at: corpus.earliest_sent_at.clone(),
        retention_days: corpus.retention_days,
        indexing: corpus.indexing,
    })
}

fn store_corpus(corpus: &SearchCorpus) {
    if let Ok(mut guard) = CORPUS_CACHE.lock() {
        *guard = Some((
            std::time::Instant::now(),
            SearchCorpus {
                message_count: corpus.message_count,
                earliest_sent_at: corpus.earliest_sent_at.clone(),
                retention_days: corpus.retention_days,
                indexing: corpus.indexing,
            },
        ));
    }
}

/// Drop the cached corpus reading.
///
/// Called on sign-out, because the next user's history is a different corpus and
/// showing them the previous account's message count would be both wrong and a
/// small leak.
pub fn invalidate_corpus_cache() {
    if let Ok(mut guard) = CORPUS_CACHE.lock() {
        *guard = None;
    }
}

// ── The conversation-name cache ──────────────────────────────────────────────

/// What a conversation id means, as the caller learned it from a remote list.
#[derive(Debug, Clone)]
pub struct CachedConversation {
    pub id: String,
    /// `channel` or `dm` — the CHECK constraint on the table rejects anything
    /// else, which is the point: an unknown kind cannot be routed.
    pub kind: &'static str,
    pub name: Option<String>,
    pub group_id: Option<String>,
    pub group_name: Option<String>,
}

/// Remember what these conversation ids mean, locally.
///
/// Called from the two reads that already list channels and DMs, mirroring the
/// way `attach_sender_usernames_local` writes `user_cache`. Without it a search
/// result can only render a UUID, `pages/Search.tsx` cannot tell a channel from
/// a DM (and so routed EVERY hit to a DM route), and search is useless offline.
///
/// Best-effort: a failed write costs a name, never a read.
pub async fn cache_conversations(state: &Arc<AppState>, entries: &[CachedConversation]) {
    if entries.is_empty() {
        return;
    }
    let guard = state.local_db.lock().await;
    let Some(db) = guard.as_ref() else {
        return;
    };
    for entry in entries {
        let _ = db.conn().execute(
            "INSERT INTO conversation_cache (id, kind, name, group_id, group_name, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, datetime('now')) \
             ON CONFLICT(id) DO UPDATE SET kind = ?2, name = ?3, group_id = ?4, group_name = ?5, \
             updated_at = datetime('now')",
            rusqlite::params![
                entry.id,
                entry.kind,
                entry.name,
                entry.group_id,
                entry.group_name
            ],
        );
    }
}

/// Throw the search index away and build it again from `message`.
///
/// The Settings escape hatch for the one failure a contentless FTS5 index can
/// have that nothing else repairs: if `pollis_search_text` ever produces
/// different bytes for a body it has already indexed, the delete side stops
/// matching and the index drifts. Startup repairs that silently when
/// `integrity-check` catches it; this is the button for when it does not.
pub async fn rebuild_search_index(state: &Arc<AppState>) -> Result<()> {
    let guard = state.local_db.lock().await;
    let db = guard
        .as_ref()
        .ok_or_else(|| crate::error::Error::Other(anyhow::anyhow!("Not signed in")))?;
    crate::db::local::rebuild_search_index(db.conn())
}

// ── Snippets ─────────────────────────────────────────────────────────────────

/// Cut a window of context around the first match.
///
/// Built in Rust from `message.content`, never by FTS5's `snippet()`: a
/// contentless table has no column text to return, and the indexed body is
/// JSON-stripped and bigram-segmented anyway — a CJK snippet straight from the
/// index would read `今天 天天 天气 气很 很好`.
pub fn build_snippet(content: &str, parsed: &ParsedQuery) -> SearchSnippet {
    let text = plain_text(content);
    let lower = text.to_lowercase();

    let first_match = needles(parsed)
        .iter()
        .filter_map(|needle| lower.find(needle.as_str()))
        .min();

    let Some(hit_at) = first_match else {
        return SearchSnippet {
            text: truncate_to(&text, SNIPPET_RADIUS * 3),
            highlights: Vec::new(),
        };
    };

    let start = floor_char_boundary(&text, hit_at.saturating_sub(SNIPPET_RADIUS));
    let end = ceil_char_boundary(&text, (hit_at + SNIPPET_RADIUS * 2).min(text.len()));

    let mut window = String::new();
    if start > 0 {
        window.push('…');
    }
    window.push_str(&text[start..end]);
    if end < text.len() {
        window.push('…');
    }

    SearchSnippet {
        text: window,
        highlights: Vec::new(),
    }
}

/// The lowercased literals a snippet should highlight.
fn needles(parsed: &ParsedQuery) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for phrase in &parsed.phrases {
        out.push(phrase.to_lowercase());
    }
    for term in &parsed.terms {
        out.push(term.to_lowercase());
    }
    out.retain(|n| !n.is_empty());
    out
}

/// Where to draw `<mark>`, as **UTF-16 code-unit** ranges.
///
/// UTF-16 rather than Unicode scalars because the only consumer is JavaScript,
/// whose string indices are code units: an emoji earlier in the snippet would
/// otherwise shift every subsequent highlight by one. Returning ranges rather
/// than HTML also means React renders `<mark>` without
/// `dangerouslySetInnerHTML`, and retires the reused-`lastIndex` regex bug the
/// old client-side highlighter had.
pub fn highlight_ranges(text: &str, parsed: &ParsedQuery) -> Vec<(usize, usize)> {
    let lower = text.to_lowercase();
    let mut spans: Vec<(usize, usize)> = Vec::new();

    for needle in needles(parsed) {
        let mut from = 0usize;
        while let Some(at) = lower[from..].find(&needle) {
            let start = from + at;
            let end = start + needle.len();
            if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
                from = start + 1;
                continue;
            }
            spans.push((utf16_len(&text[..start]), utf16_len(&text[..end])));
            from = end;
        }
    }

    spans.sort_unstable();
    // Overlapping spans (`cat` and `cats` both matching) would render nested
    // marks, so merge them into one range.
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in spans {
        match merged.last_mut() {
            Some(last) if start <= last.1 => last.1 = last.1.max(end),
            _ => merged.push((start, end)),
        }
    }
    merged
}

fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

fn truncate_to(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_string();
    }
    let end = floor_char_boundary(text, max_bytes);
    format!("{}…", &text[..end])
}

fn floor_char_boundary(s: &str, mut at: usize) -> usize {
    at = at.min(s.len());
    while at > 0 && !s.is_char_boundary(at) {
        at -= 1;
    }
    at
}

fn ceil_char_boundary(s: &str, mut at: usize) -> usize {
    at = at.min(s.len());
    while at < s.len() && !s.is_char_boundary(at) {
        at += 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_are_lifted_out_of_the_free_text() {
        let q = parse_query("from:@alice in:#general has:attachment budget report");
        assert_eq!(q.from.as_deref(), Some("alice"));
        assert_eq!(q.in_conversation.as_deref(), Some("#general"));
        assert!(q.has_attachment);
        assert_eq!(q.terms, vec!["budget", "report"]);
    }

    #[test]
    fn dates_parse_and_a_malformed_one_stays_a_search_term() {
        let q = parse_query("before:2026-01-31 after:2026-01-01 on:2026-01-15");
        assert_eq!(q.before.as_deref(), Some("2026-01-31"));
        assert_eq!(q.after.as_deref(), Some("2026-01-01"));
        assert_eq!(q.on.as_deref(), Some("2026-01-15"));

        let junk = parse_query("before:yesterday");
        assert!(junk.before.is_none());
        assert_eq!(junk.terms, vec!["before:yesterday"]);
    }

    #[test]
    fn quoted_phrases_survive_and_an_unbalanced_quote_does_not_error() {
        let q = parse_query("\"quarterly review\" notes");
        assert_eq!(q.phrases, vec!["quarterly review"]);
        assert_eq!(q.terms, vec!["notes"]);

        let half = parse_query("\"still typing");
        assert_eq!(half.phrases, vec!["still typing"]);
    }

    /// The security-shaped half of the parser: NOTHING the user types reaches
    /// `MATCH` unquoted. `AND`, `NEAR`, `*` and a stray quote are all either
    /// literals or gone.
    #[test]
    fn fts_syntax_in_user_input_is_neutralised() {
        let expr = match_expression(&parse_query("cat AND NEAR* \"quo\"te")).expect("expr");
        assert!(!expr.contains(" NEAR "), "{expr}");
        for fragment in ["\"cat\"", "\"AND\"", "\"NEAR\""] {
            assert!(expr.contains(fragment), "{fragment} not quoted in {expr}");
        }
        // Every quote in the expression is either a delimiter or doubled.
        assert_eq!(expr.matches('"').count() % 2, 0, "{expr}");
    }

    #[test]
    fn only_the_last_term_is_a_prefix_match() {
        let expr = match_expression(&parse_query("the cat")).expect("expr");
        assert_eq!(expr, "\"the\" AND \"cat\"*");
    }

    #[test]
    fn a_sentinel_typed_by_a_user_cannot_forge_a_filter() {
        let q = parse_query("zzpollisatt");
        assert!(!q.has_attachment);
        assert!(q.terms.is_empty() || q.terms.iter().all(|t| !t.contains(ATTACHMENT_SENTINEL)));
    }

    #[test]
    fn has_filters_become_sentinel_terms() {
        let expr = match_expression(&parse_query("has:attachment has:link")).expect("expr");
        assert!(expr.contains(ATTACHMENT_SENTINEL));
        assert!(expr.contains(LINK_SENTINEL));
    }

    #[test]
    fn cjk_queries_become_the_same_bigrams_the_index_holds() {
        let expr = match_expression(&parse_query("今天")).expect("expr");
        assert_eq!(expr, "\"今天\"");
        let long = match_expression(&parse_query("会议明天")).expect("expr");
        assert_eq!(long, "\"会议 议明 明天\"");
    }

    #[test]
    fn a_filter_only_query_has_no_match_expression() {
        assert!(match_expression(&parse_query("from:@alice")).is_none());
    }

    #[test]
    fn snippet_is_a_window_not_the_whole_body() {
        let body = format!("{} needle {}", "x".repeat(400), "y".repeat(400));
        let q = parse_query("needle");
        let snippet = build_snippet(&body, &q);
        assert!(snippet.text.len() < body.len(), "{}", snippet.text);
        assert!(snippet.text.contains("needle"));
        assert!(snippet.text.starts_with('…'));
        assert!(snippet.text.ends_with('…'));
    }

    #[test]
    fn snippet_of_an_attachment_message_reads_as_text_not_json() {
        let body = r#"{"_att":[{"key":"media/abc","url":"https://r2/abc","name":"deck.pdf"}],"_txt":"slides attached"}"#;
        let snippet = build_snippet(body, &parse_query("deck"));
        assert!(snippet.text.contains("slides attached"));
        assert!(snippet.text.contains("deck.pdf"));
        assert!(!snippet.text.contains("media/abc"));
    }

    #[test]
    fn highlights_are_utf16_ranges_and_never_overlap() {
        let text = "cat cats catalogue";
        let ranges = highlight_ranges(text, &parse_query("cat cats"));
        for pair in ranges.windows(2) {
            assert!(pair[0].1 <= pair[1].0, "overlapping ranges: {ranges:?}");
        }
        assert_eq!(ranges.first(), Some(&(0usize, 3usize)));
    }

    /// An emoji before the match is two UTF-16 units but one `char`. JS slices
    /// by code unit, so this is the offset that renders correctly.
    #[test]
    fn highlight_offsets_survive_an_astral_character() {
        let text = "🎉 party";
        let ranges = highlight_ranges(text, &parse_query("party"));
        assert_eq!(ranges, vec![(3, 8)]);
    }

    // ── The SQL, against a real index ────────────────────────────────────────
    //
    // `search_messages` itself needs an `AppState`; `run_query` is the half that
    // actually talks to SQLite, and running it for real is what stops a
    // malformed `MATCH` join or a bad bind order from shipping.

    fn indexed_db() -> rusqlite::Connection {
        let conn = rusqlite::Connection::open_in_memory().expect("open");
        crate::db::local::apply_local_schema(&conn).expect("schema");
        conn
    }

    fn seed(conn: &rusqlite::Connection, id: &str, conv: &str, sender: &str, content: &str, sent_at: &str) {
        conn.execute(
            "INSERT INTO message (id, conversation_id, sender_id, ciphertext, content, sent_at, received_at)
             VALUES (?1, ?2, ?3, X'00', ?4, ?5, datetime('now'))",
            rusqlite::params![id, conv, sender, content, sent_at],
        )
        .expect("seed");
    }

    fn search(
        conn: &rusqlite::Connection,
        query: &str,
        filters: ResolvedFilters,
        sort: SearchSort,
    ) -> (Vec<String>, i64) {
        let parsed = parse_query(query);
        let expr = match_expression(&parsed);
        let (hits, total) =
            run_query(conn, &parsed, &filters, expr.as_deref(), sort, 25, 0).expect("run_query");
        (hits.into_iter().map(|h| h.message_id).collect(), total)
    }

    #[test]
    fn the_ranked_query_runs_and_counts() {
        let conn = indexed_db();
        seed(&conn, "m1", "c1", "alice", "the quarterly budget review", "2026-08-01T10:00:00Z");
        seed(&conn, "m2", "c1", "bob", "budget", "2026-08-02T10:00:00Z");
        seed(&conn, "m3", "c2", "alice", "unrelated chatter", "2026-08-03T10:00:00Z");

        let (ids, total) = search(&conn, "budget", ResolvedFilters::default(), SearchSort::Relevant);
        assert_eq!(total, 2);
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"m1".to_string()) && ids.contains(&"m2".to_string()));
    }

    #[test]
    fn recency_ordering_is_newest_first() {
        let conn = indexed_db();
        seed(&conn, "old", "c1", "alice", "budget", "2026-08-01T10:00:00Z");
        seed(&conn, "new", "c1", "alice", "budget", "2026-08-09T10:00:00Z");
        let (ids, _) = search(&conn, "budget", ResolvedFilters::default(), SearchSort::Recent);
        assert_eq!(ids, vec!["new", "old"]);
    }

    #[test]
    fn a_deleted_message_leaves_the_results() {
        let conn = indexed_db();
        seed(&conn, "m1", "c1", "alice", "budget", "2026-08-01T10:00:00Z");
        conn.execute(
            "UPDATE message SET content = NULL, deleted_at = '2026-08-02T00:00:00Z' WHERE id = 'm1'",
            [],
        )
        .expect("soft delete");
        let (ids, total) = search(&conn, "budget", ResolvedFilters::default(), SearchSort::Recent);
        assert!(ids.is_empty());
        assert_eq!(total, 0);
    }

    #[test]
    fn filters_narrow_the_result_set() {
        let conn = indexed_db();
        seed(&conn, "m1", "c1", "alice", "budget", "2026-08-01T10:00:00Z");
        seed(&conn, "m2", "c2", "bob", "budget", "2026-08-05T10:00:00Z");

        let by_sender = ResolvedFilters {
            sender_id: Some("bob".into()),
            ..Default::default()
        };
        assert_eq!(search(&conn, "budget", by_sender, SearchSort::Recent).0, vec!["m2"]);

        let by_conversation = ResolvedFilters {
            conversation_id: Some("c1".into()),
            ..Default::default()
        };
        assert_eq!(
            search(&conn, "budget", by_conversation, SearchSort::Recent).0,
            vec!["m1"]
        );

        assert_eq!(
            search(&conn, "budget before:2026-08-03", ResolvedFilters::default(), SearchSort::Recent).0,
            vec!["m1"]
        );
        assert_eq!(
            search(&conn, "budget after:2026-08-01", ResolvedFilters::default(), SearchSort::Recent).0,
            vec!["m2"]
        );
        assert_eq!(
            search(&conn, "budget on:2026-08-05", ResolvedFilters::default(), SearchSort::Recent).0,
            vec!["m2"]
        );
    }

    /// `has:attachment` with no free text has to work: the sentinel IS the
    /// query. Attachment metadata must still be unfindable.
    #[test]
    fn has_filters_and_attachment_hygiene_hold_end_to_end() {
        let conn = indexed_db();
        seed(
            &conn,
            "m-att",
            "c1",
            "alice",
            r#"{"_att":[{"key":"media/9f3c1a2b","url":"https://r2/9f3c1a2b","name":"Q3-budget.xlsx"}],"_txt":"here"}"#,
            "2026-08-01T10:00:00Z",
        );
        seed(&conn, "m-link", "c1", "alice", "see https://example.com", "2026-08-02T10:00:00Z");
        seed(&conn, "m-plain", "c1", "alice", "just words", "2026-08-03T10:00:00Z");

        assert_eq!(
            search(&conn, "has:attachment", ResolvedFilters::default(), SearchSort::Recent).0,
            vec!["m-att"]
        );
        assert_eq!(
            search(&conn, "has:link", ResolvedFilters::default(), SearchSort::Recent).0,
            vec!["m-link"]
        );
        assert_eq!(
            search(&conn, "Q3-budget.xlsx", ResolvedFilters::default(), SearchSort::Recent).0,
            vec!["m-att"],
            "an attachment filename must be findable"
        );
        assert!(
            search(&conn, "9f3c1a2b", ResolvedFilters::default(), SearchSort::Recent).0.is_empty(),
            "the R2 object key must not be"
        );
    }

    /// Diacritics fold (the tokeniser's job) and two-character Mandarin words
    /// are findable (the bigram transform's job). Both are localisation
    /// requirements from #855, and both are properties of the SHIPPED pipeline
    /// rather than of either half alone.
    #[test]
    fn accents_fold_and_cjk_is_findable() {
        let conn = indexed_db();
        seed(&conn, "m-fr", "c1", "alice", "on va au café", "2026-08-01T10:00:00Z");
        seed(&conn, "m-cn", "c1", "alice", "今天天气很好", "2026-08-02T10:00:00Z");

        assert_eq!(
            search(&conn, "cafe", ResolvedFilters::default(), SearchSort::Recent).0,
            vec!["m-fr"]
        );
        assert_eq!(
            search(&conn, "今天", ResolvedFilters::default(), SearchSort::Recent).0,
            vec!["m-cn"]
        );
        assert_eq!(
            search(&conn, "天气", ResolvedFilters::default(), SearchSort::Recent).0,
            vec!["m-cn"]
        );
    }

    /// The security property, executed rather than asserted about: a query full
    /// of FTS5 operators must return an answer, not an error.
    #[test]
    fn hostile_query_syntax_does_not_reach_match() {
        let conn = indexed_db();
        seed(&conn, "m1", "c1", "alice", "ordinary text", "2026-08-01T10:00:00Z");
        for hostile in [
            "\"unbalanced",
            "* ",
            "NEAR(a b)",
            "a AND OR b",
            "ordinary AND \"",
            "^start",
            "col:value",
            "-minus",
        ] {
            let parsed = parse_query(hostile);
            let expr = match_expression(&parsed);
            run_query(
                &conn,
                &parsed,
                &ResolvedFilters::default(),
                expr.as_deref(),
                SearchSort::Relevant,
                25,
                0,
            )
            .unwrap_or_else(|e| panic!("query {hostile:?} must not error: {e}"));
        }
    }

    #[test]
    fn recency_decay_demotes_an_equally_good_old_hit() {
        let now = chrono::Utc::now();
        let mut hits = vec![
            RawHit {
                message_id: "old".into(),
                conversation_id: "c".into(),
                sender_id: "s".into(),
                content: "x".into(),
                sent_at: (now - chrono::Duration::days(720)).to_rfc3339(),
                thread_id: None,
                rank: -5.0,
            },
            RawHit {
                message_id: "new".into(),
                conversation_id: "c".into(),
                sender_id: "s".into(),
                content: "x".into(),
                sent_at: (now - chrono::Duration::days(7)).to_rfc3339(),
                thread_id: None,
                rank: -5.0,
            },
        ];
        rescore_by_recency(&mut hits);
        assert_eq!(hits[0].message_id, "new");

        // ...but a substantially better old hit still wins.
        assert_eq!(hits[1].message_id, "old");
        hits[1].rank = -50.0;
        rescore_by_recency(&mut hits);
        assert_eq!(hits[0].message_id, "old");
    }
}
