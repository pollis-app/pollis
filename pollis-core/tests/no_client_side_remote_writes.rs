//! `pollis-core` must not write to the shared database (#910).
//!
//! Remote and local table names are disjoint by design, so a write is classified
//! by table name alone. Reads source text: a lower bound, not a proof.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// Every table the REMOTE schema defines, read from `pollis-schema`'s migrations.
fn remote_tables() -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let mut sql = String::new();
    sql.push_str(pollis_schema::BASELINE_SQL);
    sql.push('\n');
    sql.push_str(pollis_schema::LOG_DB_SCHEMA);
    for (_, _, m) in pollis_schema::POST_BASELINE_MIGRATIONS {
        sql.push('\n');
        sql.push_str(m);
    }
    for (_, _, m) in pollis_schema::POST_BASELINE_LOG_MIGRATIONS {
        sql.push('\n');
        sql.push_str(m);
    }

    let lower = sql.to_lowercase();
    let mut rest = lower.as_str();
    while let Some(i) = rest.find("create table") {
        rest = &rest[i + "create table".len()..];
        let mut tail = rest.trim_start();
        for prefix in ["if not exists"] {
            if let Some(t) = tail.strip_prefix(prefix) {
                tail = t.trim_start();
            }
        }
        // The baseline quotes some names (`CREATE TABLE "users"`).
        let name = read_identifier(tail);
        if !name.is_empty() {
            out.insert(name);
        }
    }
    assert!(
        out.contains("users") && out.contains("message_envelope"),
        "the remote-table list did not parse; it is the whole basis of this test"
    );
    out
}

/// The identifier at the start of `s`, with SQLite's optional quoting removed.
fn read_identifier(s: &str) -> String {
    let s = s.trim_start();
    let (delim, body) = match s.chars().next() {
        Some('"') => (Some('"'), &s[1..]),
        Some('`') => (Some('`'), &s[1..]),
        Some('[') => (Some(']'), &s[1..]),
        _ => (None, s),
    };
    match delim {
        Some(d) => body.chars().take_while(|c| *c != d).collect(),
        None => body
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect(),
    }
}

/// Source with every `#[cfg(test)]` module removed, by brace matching.
fn without_test_modules(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(i) = rest.find("#[cfg(test)]") {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        match after.find('{') {
            Some(open) => {
                let mut depth = 0usize;
                let mut end = None;
                for (off, ch) in after[open..].char_indices() {
                    match ch {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                end = Some(open + off + 1);
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                match end {
                    Some(e) => rest = &after[e..],
                    None => return out,
                }
            }
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            // `tests.rs`'s `#[cfg(test)]` is in the parent file, unseen by brace-stripping.
            if path.file_name().is_some_and(|n| n == "tests.rs") {
                continue;
            }
            out.push(path);
        }
    }
}

/// The remote table a SQL statement writes, if it writes one.
fn written_table(literal: &str) -> Option<String> {
    let sql = literal.trim_start().to_lowercase();
    let rest = if let Some(r) = sql.strip_prefix("insert") {
        let r = r.trim_start();
        let r = r.strip_prefix("or ignore").or_else(|| r.strip_prefix("or replace")).unwrap_or(r);
        if !sql.contains("values") && !sql.contains("select") {
            return None;
        }
        r.trim_start().strip_prefix("into")?
    } else if let Some(r) = sql.strip_prefix("delete") {
        r.trim_start().strip_prefix("from")?
    } else if let Some(r) = sql.strip_prefix("update") {
        // Without SET, `require_admin(…, "update channels")` would read as SQL.
        if !sql.contains(" set ") {
            return None;
        }
        r
    } else {
        return None;
    };
    let table = read_identifier(rest);
    if table.is_empty() {
        None
    } else {
        Some(table)
    }
}

/// The contents of every double-quoted string literal on a line.
fn string_literals(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let chars: Vec<char> = line.chars().collect();
    let mut i = 0usize;
    while i < chars.len() {
        if chars[i] != '"' {
            i += 1;
            continue;
        }
        let mut buf = String::new();
        let mut j = i + 1;
        while j < chars.len() {
            if chars[j] == '\\' {
                j += 2;
                continue;
            }
            if chars[j] == '"' {
                break;
            }
            buf.push(chars[j]);
            j += 1;
        }
        if !buf.is_empty() {
            out.push(buf);
        }
        i = j + 1;
    }
    out
}

#[test]
fn no_production_code_in_pollis_core_writes_a_remote_table() {
    let remote = remote_tables();
    let src_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src_root, &mut files);
    assert!(!files.is_empty(), "found no sources to scan");

    let mut offenders: Vec<String> = Vec::new();

    for file in files {
        let text = std::fs::read_to_string(&file).expect("read source");
        let production = without_test_modules(&text);

        for (n, line) in production.lines().enumerate() {
            for literal in string_literals(line) {
                let Some(table) = written_table(&literal) else { continue };
                if !remote.contains(&table) {
                    continue;
                }
                offenders.push(format!(
                    "{}:{} writes remote table `{}`\n      {}",
                    file.strip_prefix(env!("CARGO_MANIFEST_DIR")).unwrap_or(&file).display(),
                    n + 1,
                    table,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "pollis-core must not write the shared database — extend a DS endpoint instead \
         (CLAUDE.md, and #910 which removed the last one):\n  {}",
        offenders.join("\n  ")
    );
}

/// The local tables the client DOES own must not be swept up by the rule above.
#[test]
fn the_local_schema_does_not_collide_with_remote_table_names() {
    let remote = remote_tables();
    for local in ["message", "user_cache", "mls_kv", "contact_verification"] {
        assert!(
            !remote.contains(local),
            "`{local}` is both a local and a remote table name; the two schemas are \
             supposed to be disjoint, and this test cannot tell them apart if they are not"
        );
    }
}
