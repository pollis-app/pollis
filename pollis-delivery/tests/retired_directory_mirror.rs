//! #1085: `user_groups` / `user_dms` are retired, and nothing may quietly start
//! using them again.
//!
//! Migration 000009 created them as a denormalized sidebar index (#532) and
//! backfilled them once. #540 reverted the feature. From then on the DS never
//! INSERTed and never READ them — it only DELETEd from them on teardown — so
//! what remained was a one-time backfill minus deletions: a table that could
//! only drift further from `group_member` / `dm_channel_member`, and would have
//! served a confidently wrong sidebar to whatever started reading it.
//!
//! Migration 000025 empties them and the DS no longer names them at all. The
//! tables themselves are still there (a rolling deploy has older instances
//! running the removed DELETEs, so the DROP waits for a later release), which is
//! exactly why this guard exists: an empty, un-referenced table is an inviting
//! place to put something, and the next person to reach for it should be told
//! that its contents are not maintained.
//!
//! Keyed on SQL keyword adjacency rather than a bare name search, so the
//! exemption entries in `teardown.rs` — which name the tables in prose to
//! explain the decision — do not trip it.

use std::fs;
use std::path::Path;

const RETIRED: &[&str] = &["user_groups", "user_dms"];
/// The keywords that make a mention a USE rather than a note about one.
const SQL_CONTEXTS: &[&str] = &["FROM", "INTO", "UPDATE", "JOIN", "TABLE"];

fn rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_ds_sql_names_the_retired_directory_mirror() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rs_files(&src, &mut files);
    assert!(files.len() > 10, "expected to scan the DS sources, found {}", files.len());

    let mut offenders: Vec<String> = Vec::new();
    for file in &files {
        let text = fs::read_to_string(file).expect("read");
        // Normalise so `from user_groups` and `FROM user_groups` both match; SQL
        // in this crate is written inline in string literals either way.
        let upper = text.to_uppercase();
        for table in RETIRED {
            for kw in SQL_CONTEXTS {
                let needle = format!("{kw} {}", table.to_uppercase());
                if upper.contains(&needle) {
                    offenders.push(format!(
                        "{}: `{needle}`",
                        file.strip_prefix(&src).unwrap_or(file).display()
                    ));
                }
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "the directory mirror is retired (#1085) and its rows are not maintained — \
         nothing may read or write it. Derive from `group_member` / \
         `dm_channel_member` instead. Offending SQL: {offenders:?}"
    );
}

/// The guard is only worth having if it would actually catch a reintroduction.
#[test]
fn the_guard_would_catch_a_reintroduction() {
    let sample = r#"conn.query("SELECT group_id FROM user_groups WHERE user_id = ?1", p)"#;
    let upper = sample.to_uppercase();
    assert!(
        RETIRED
            .iter()
            .any(|t| SQL_CONTEXTS
                .iter()
                .any(|kw| upper.contains(&format!("{kw} {}", t.to_uppercase())))),
        "a SELECT ... FROM user_groups must be detected"
    );
    // And prose about the tables must NOT be detected, or the guard is unusable
    // alongside the exemption entries that explain the decision.
    let prose = "Retired (#1085). user_groups was the #532 sidebar index; user_dms too.";
    let prose_upper = prose.to_uppercase();
    assert!(
        !RETIRED.iter().any(|t| SQL_CONTEXTS
            .iter()
            .any(|kw| prose_upper.contains(&format!("{kw} {}", t.to_uppercase())))),
        "prose naming the tables must not trip the guard"
    );
}
