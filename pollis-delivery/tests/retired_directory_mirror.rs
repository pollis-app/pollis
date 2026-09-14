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
//! Migration 000025 emptied them and the DS stopped naming them; migration
//! 000029 then DROPped them (#1143), once enough deploys had shipped that no
//! running instance still issued the removed DELETEs.
//!
//! Both halves are still guarded. `the_retired_tables_are_gone_from_the_schema`
//! proves the DROP actually happened rather than the migration being a silent
//! no-op, and `no_ds_sql_names_the_retired_directory_mirror` keeps the name from
//! coming back — a recreated table would be far worse than the emptied one,
//! since nothing would be maintaining it and the sidebar it fed is long gone.
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

/// The DROP in migration 000029 must actually remove the tables — a migration
/// that silently did nothing would leave exactly the inviting empty table #1143
/// set out to remove, and nothing else would notice.
#[tokio::test]
async fn the_retired_tables_are_gone_from_the_schema() {
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");

    let mut present: Vec<String> = Vec::new();
    for table in RETIRED {
        let mut rows = conn
            .query(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?1",
                libsql::params![table.to_string()],
            )
            .await
            .unwrap();
        if rows.next().await.unwrap().is_some() {
            present.push((*table).to_string());
        }
    }

    assert!(
        present.is_empty(),
        "migration 000029 must DROP the retired directory mirror, but {present:?} \
         still exist after applying the full migration set"
    );

    // Sanity: the probe can see tables that DO exist, or the assertion above is
    // vacuous and would pass against a schema that was never applied.
    let mut rows = conn
        .query(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'group_member'",
            (),
        )
        .await
        .unwrap();
    assert!(
        rows.next().await.unwrap().is_some(),
        "the schema probe found no `group_member`, so it would not have found the \
         retired tables either — the check above proves nothing"
    );
}
