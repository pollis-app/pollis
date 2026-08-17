//! #875 — the DS must not declare its own tables.
//!
//! `pollis-delivery` deliberately does not depend on `pollis-core`, and for a
//! long time that independence was paid for by hand-rolling a partial
//! `CREATE TABLE` block per test module. Twenty-two of them accumulated, and
//! they drifted: eleven different `user_device` definitions, `group_member`
//! with and without its primary key, an `mls_key_package` with a `claimed_at`
//! column production does not have, an `mls_welcome` whose `id` was an INTEGER
//! where the shipped one is TEXT. A fixture without a primary key is not a
//! cosmetic difference — it lets a test insert the duplicate membership row
//! production rejects, so the property the test claims to prove was proved
//! against a schema no deploy has.
//!
//! `pollis-schema` removed the reason to hand-roll one: `apply::main_db`,
//! `apply::log_db` and `apply::single_db` stamp a connection with exactly the
//! scripts `scripts/db-apply.sh` runs. This test keeps it removed. It is a
//! grep, deliberately — the failure mode it guards is someone adding a
//! *convenient* fifteen-line table for a new test, which no schema assertion
//! elsewhere would ever notice.
//!
//! If you genuinely need a table the remote schema does not have, it belongs in
//! a migration, not in a test fixture.

use std::path::{Path, PathBuf};

/// Files allowed to contain the phrase, with the reason.
const ALLOWED: &[(&str, &str)] = &[
    // This file names the pattern in order to ban it.
    ("tests/schema_is_not_reinvented.rs", "the ban itself"),
];

fn rust_sources() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut out = Vec::new();
    for dir in ["src", "tests"] {
        let mut stack = vec![root.join(dir)];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).expect("read_dir") {
                let p = entry.expect("entry").path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.extension().is_some_and(|e| e == "rs") {
                    out.push(p);
                }
            }
        }
    }
    out.sort();
    out
}

#[test]
fn no_delivery_source_declares_a_table() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut offenders = Vec::new();
    for path in rust_sources() {
        let rel = path
            .strip_prefix(root)
            .expect("under the manifest dir")
            .to_string_lossy()
            .replace('\\', "/");
        if ALLOWED.iter().any(|(f, _)| *f == rel) {
            continue;
        }
        let body = std::fs::read_to_string(&path).expect("read");
        for (i, line) in body.lines().enumerate() {
            let upper = line.to_uppercase();
            if upper.contains("CREATE TABLE") {
                offenders.push(format!("{rel}:{}", i + 1));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "pollis-delivery must not declare tables — use \
         `pollis_schema::apply::{{main_db, log_db, single_db}}` so tests run against \
         the schema that ships. Offending lines:\n  {}",
        offenders.join("\n  ")
    );
}

/// The counterpart obligation: the helper the ban points at must actually
/// produce the tables the DS reads, and the #420 split must stay a split —
/// a main-DB read on a log connection has to fail rather than silently find
/// the table, because that is the bug the split exists to make visible.
#[tokio::test]
async fn the_two_halves_are_not_interchangeable() {
    async fn conn_with(
        f: impl for<'a> FnOnce(
            &'a libsql::Connection,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<(), libsql::Error>> + 'a>,
        >,
    ) -> (libsql::Database, libsql::Connection) {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let c = db.connect().unwrap();
        f(&c).await.expect("schema");
        // The connection borrows the in-memory database, so the handle is
        // returned for the caller to hold rather than leaked (#942).
        (db, c)
    }

    let (_main_db, main) = conn_with(|c| Box::pin(pollis_schema::apply::main_db(c))).await;
    let (_log_db, log) = conn_with(|c| Box::pin(pollis_schema::apply::log_db(c))).await;

    // The log DB has no membership tables — a misrouted authz read must fail.
    assert!(
        log.query("SELECT 1 FROM group_member", ()).await.is_err(),
        "the log DB must not carry group_member"
    );
    // …and the log DB is where `mls_commit_since` lives.
    assert!(log.query("SELECT 1 FROM mls_commit_since", ()).await.is_ok());
    assert!(
        main.query("SELECT 1 FROM mls_commit_since", ()).await.is_err(),
        "the main DB must not carry mls_commit_since"
    );
    // The generation column is a log-DB migration; the main DB still carries
    // the frozen pre-#420 copies of those tables WITHOUT it, which is exactly
    // why applying the log scripts second is load-bearing for single-DB deploys.
    assert!(
        log.query("SELECT generation FROM mls_commit_log", ())
            .await
            .is_ok(),
        "the log DB's commit log must have `generation`"
    );
    assert!(
        main.query("SELECT generation FROM mls_commit_log", ())
            .await
            .is_err(),
        "the main DB's frozen copy predates `generation` — if this ever passes, \
         the baseline was edited or a main-DB migration grew the column, and \
         `single_db_scripts()`'s ordering comment needs revisiting"
    );
}
