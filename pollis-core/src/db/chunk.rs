//! Bounded `IN (…)` lists (#916).
//!
//! A generated placeholder list is only valid under SQLite's bound-parameter
//! limit (999 on older builds); over it the statement fails to prepare, so a
//! large enough list stops loading rather than slowing down.
//!
//! ```ignore
//! for chunk in bind_chunks(&ids, 0) {
//!     let sql = format!("SELECT … WHERE id IN ({})", placeholders(chunk.len(), 1));
//!     // … bind `chunk`, run, accumulate …
//! }
//! ```
//! ```ignore
//! for chunk in bind_chunks(&ids, 0) {
//!     let sql = format!("SELECT … WHERE id IN ({})", placeholders(chunk.len(), 1));
//!     // … bind `chunk`, run, accumulate …
//! }
//! ```
//!
//! `reserved` is how many slots the statement spends on non-list parameters
//! and `first` is the index the list starts at; keep the two adjacent at the
//! call site. `pollis-delivery`'s `util.rs` has its own copy; keep the bounds
//! in step.

/// Maximum number of values bound into a single statement, far below even the
/// old 999 limit so it holds on any SQLite.
pub const MAX_BIND_PARAMS: usize = 500;

/// Split `items` into chunks that fit within [`MAX_BIND_PARAMS`], leaving
/// `reserved` slots for the statement's fixed (non-list) parameters.
///
/// Chunks are never empty and cover the input exactly once.
pub fn bind_chunks<T>(items: &[T], reserved: usize) -> std::slice::Chunks<'_, T> {
    // `max(1)`: `slice::chunks(0)` panics.
    let per_chunk = MAX_BIND_PARAMS.saturating_sub(reserved).max(1);
    items.chunks(per_chunk)
}

/// `?first,?first+1,…` for `count` values, numbered so list parameters can mix
/// with fixed ones.
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
        assert_eq!(seen, ids);
    }

    #[test]
    fn reserved_slots_come_out_of_the_chunk_budget() {
        let ids: Vec<usize> = (0..1000).collect();
        for chunk in bind_chunks(&ids, 2) {
            assert!(chunk.len() + 2 <= MAX_BIND_PARAMS);
        }
        assert_eq!(bind_chunks(&ids, MAX_BIND_PARAMS + 10).next().unwrap().len(), 1);
    }

    /// An unchunked `IN (…)` over a large list fails to prepare; chunked, it runs.
    #[test]
    fn a_list_too_large_to_bind_fails_unchunked_and_succeeds_chunked() {
        let conn = rusqlite::Connection::open_in_memory().expect("in-memory db");
        conn.execute_batch("CREATE TABLE t (id TEXT PRIMARY KEY);").unwrap();
        conn.execute("INSERT INTO t (id) VALUES ('needle')", []).unwrap();

        // Past the bundled build's 32766 ceiling.
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
