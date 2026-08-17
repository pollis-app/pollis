//! Bounded `IN (…)` lists (#916).
//!
//! Every batched read in this crate builds its `IN (…)` clause by generating one
//! placeholder per element of a caller-supplied list. That collapses N+1 query
//! storms into one round trip, which is why #874/#875 introduced them — but a
//! generated placeholder list is only correct while it stays under SQLite's
//! **bound-parameter limit**, and none of them checked.
//!
//! `SQLITE_MAX_VARIABLE_NUMBER` is 32766 on modern builds and **999** on older
//! ones, and it is a property of whichever SQLite is underneath at runtime — the
//! bundled one here, an arbitrarily old one behind Turso. Over the limit the
//! statement does not degrade, it fails to prepare: `too many SQL variables`.
//! So a group large enough to exceed it would not have loaded slowly, it would
//! have stopped loading — and the failure would first appear on whoever built
//! the biggest group, which is the worst possible discovery path.
//!
//! Nothing reaches these sizes today. That is exactly why the ceiling is worth
//! writing down as code rather than leaving to be found.
//!
//! ## Using it
//!
//! Two pieces, because the row-accumulation half genuinely differs per call site
//! (async `libsql` vs sync `rusqlite`, `SELECT` vs `UPDATE`) and forcing those
//! through one generic would be a worse abstraction than the duplication it
//! removes:
//!
//! ```ignore
//! for chunk in bind_chunks(&ids, 0) {
//!     let sql = format!("SELECT … WHERE id IN ({})", placeholders(chunk.len(), 1));
//!     // … bind `chunk`, run, accumulate …
//! }
//! ```
//!
//! `reserved` is how many parameter slots the statement spends on things that
//! are NOT list elements (`?1` for a user id, say), and `first` is the index the
//! list starts at. Getting those two out of step is the one mistake this module
//! cannot catch for you, so keep them adjacent at the call site.
//!
//! `pollis-delivery` has its own copy in `util.rs` — it must not depend on
//! `pollis-core`, and one shared constant is not worth a crate. Keep the two
//! bounds in step.

/// Maximum number of values bound into a single statement.
///
/// Deliberately far below even the OLD 999 limit rather than near the modern
/// 32766: the point is a bound that holds on whatever SQLite is actually
/// underneath, and the cost of a smaller chunk is one extra round trip on lists
/// that are already rare. A conservative constant that is never wrong beats a
/// tuned one that depends on the deployment.
pub const MAX_BIND_PARAMS: usize = 500;

/// Split `items` into chunks that fit within [`MAX_BIND_PARAMS`], leaving
/// `reserved` slots for the statement's fixed (non-list) parameters.
///
/// Chunks are never empty and always cover the input exactly once, so a caller
/// that accumulates rows across chunks gets the same result set the single
/// oversized query would have produced — had it been able to run.
pub fn bind_chunks<T>(items: &[T], reserved: usize) -> std::slice::Chunks<'_, T> {
    // `max(1)` so a pathological `reserved` cannot produce a zero-size chunk,
    // which `slice::chunks` panics on. One-at-a-time is degenerate but correct;
    // panicking inside a read path is not.
    let per_chunk = MAX_BIND_PARAMS.saturating_sub(reserved).max(1);
    items.chunks(per_chunk)
}

/// `?first,?first+1,…` for `count` values — the `IN (…)` body.
///
/// Numbered rather than anonymous `?` so a statement can mix list parameters
/// with fixed ones at known indices, which several call sites do.
pub fn placeholders(count: usize, first: usize) -> String {
    let mut out = String::with_capacity(count * 5);
    for i in 0..count {
        if i > 0 {
            out.push(',');
        }
        out.push('?');
        out.push_str(&(first + i).to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_numbered_from_the_requested_start() {
        assert_eq!(placeholders(3, 1), "?1,?2,?3");
        // The offset forms real call sites use: `?1` is a user id, the list
        // starts at `?2`; or `?1`/`?2` are a user+device, the list starts at `?3`.
        assert_eq!(placeholders(3, 2), "?2,?3,?4");
        assert_eq!(placeholders(2, 3), "?3,?4");
        assert_eq!(placeholders(0, 1), "");
    }

    #[test]
    fn chunks_cover_the_input_exactly_once() {
        let ids: Vec<usize> = (0..1234).collect();
        let mut seen: Vec<usize> = Vec::new();
        for chunk in bind_chunks(&ids, 0) {
            assert!(!chunk.is_empty());
            assert!(chunk.len() <= MAX_BIND_PARAMS);
            seen.extend_from_slice(chunk);
        }
        // The invariant a batched read depends on: chunking must not drop or
        // duplicate a single id, or the caller silently returns a partial answer
        // — which is worse than the error it replaced.
        assert_eq!(seen, ids);
    }

    #[test]
    fn reserved_slots_come_out_of_the_chunk_budget() {
        let ids: Vec<usize> = (0..1000).collect();
        for chunk in bind_chunks(&ids, 2) {
            assert!(chunk.len() + 2 <= MAX_BIND_PARAMS);
        }
        // Degenerate `reserved` must not panic (`chunks(0)` does).
        assert_eq!(bind_chunks(&ids, MAX_BIND_PARAMS + 10).next().unwrap().len(), 1);
    }

    /// The bug, demonstrated: an unchunked `IN (…)` over a large list does not
    /// run slowly, it fails to prepare.
    ///
    /// Uses rusqlite in-memory for the same reason `db/remote.rs`'s tests do —
    /// libsql-sys bundles SQLite with `SQLITE_THREADSAFE=0` and clashes with
    /// rusqlite in one test binary. The parameter limit is a property of SQLite
    /// itself, so either engine demonstrates it.
    #[test]
    fn a_list_too_large_to_bind_fails_unchunked_and_succeeds_chunked() {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch("CREATE TABLE t (id TEXT PRIMARY KEY);").unwrap();
        conn.execute("INSERT INTO t (id) VALUES ('needle')", []).unwrap();

        // Comfortably past the bundled build's 32766 ceiling, so this test does
        // not quietly stop testing anything if the limit is raised again.
        let ids: Vec<String> = (0..40_000).map(|i| format!("id-{i}")).collect();

        let unchunked = format!(
            "SELECT id FROM t WHERE id IN ({})",
            placeholders(ids.len(), 1)
        );
        assert!(
            conn.prepare(&unchunked).is_err(),
            "SQLite accepted {} bound parameters — if the ceiling really has gone \
             away, this test (not the chunking) is what should change",
            ids.len()
        );

        // The same logical query, chunked: every statement prepares, runs, and
        // the accumulated result is the one the caller wanted.
        let mut needle = ids.clone();
        needle.push("needle".to_string());
        let mut found: Vec<String> = Vec::new();
        for chunk in bind_chunks(&needle, 0) {
            let sql = format!(
                "SELECT id FROM t WHERE id IN ({})",
                placeholders(chunk.len(), 1)
            );
            let mut stmt = conn.prepare(&sql).expect("chunked statement prepares");
            let rows = stmt
                .query_map(rusqlite::params_from_iter(chunk.iter()), |r| {
                    r.get::<_, String>(0)
                })
                .expect("chunked query runs");
            found.extend(rows.flatten());
        }
        assert_eq!(found, vec!["needle".to_string()]);
    }
}
