//! #1085: `user_groups` / `user_dms` are retired, and nothing may quietly start
//! using them again.
//!
//! Keyed on SQL keyword adjacency so prose mentions in `teardown.rs` don't trip it.

use std::fs;
use std::path::Path;

const RETIRED: &[&str] = &["user_groups", "user_dms"];
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
    // Prose about the tables must not be detected.
    let prose = "Retired (#1085). user_groups was the #532 sidebar index; user_dms too.";
    let prose_upper = prose.to_uppercase();
    assert!(
        !RETIRED.iter().any(|t| SQL_CONTEXTS
            .iter()
            .any(|kw| prose_upper.contains(&format!("{kw} {}", t.to_uppercase())))),
        "prose naming the tables must not trip the guard"
    );
}

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

    // Sanity: the probe sees tables that do exist.
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
