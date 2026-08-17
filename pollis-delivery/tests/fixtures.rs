//! #942 — the shared test fixture actually cleans up after itself.
//!
//! Every fixture in this directory used to keep its temp directory alive with
//! `std::mem::forget(dir)`, which is permanent: twenty-one test binaries each
//! left a directory in `/tmp` on every run. `common::TempDb` replaces that with
//! ownership, and this is the assertion that the replacement is real rather than
//! cosmetic — without it, the fix is indistinguishable from renaming the leak.

mod common;

/// The directory exists while the fixture is held, and is gone once it is not.
#[tokio::test]
async fn the_fixture_removes_its_temp_dir_when_dropped() {
    let path = {
        let db = common::TempDb::open("db.db").await;
        // The file the DB was opened at — its parent is the temp dir.
        pollis_schema::apply::single_db(&db.conn().await.expect("checkout"))
            .await
            .expect("schema");

        let path = temp_dir_of(&db).await;
        assert!(path.is_dir(), "the fixture's directory must exist while it is held");
        path
    };

    assert!(
        !path.exists(),
        "the fixture's directory must be removed when it drops — this is the \
         whole of #942; a `std::mem::forget` here would leave it behind forever"
    );
}

/// The fixture stays usable for as long as it is held, which is why the naive
/// "just drop the TempDir at the end of the builder" fix does not work and the
/// leak existed in the first place.
#[tokio::test]
async fn the_fixture_outlives_the_function_that_built_it() {
    async fn build() -> common::TempDb {
        let db = common::TempDb::open("db.db").await;
        pollis_schema::apply::single_db(&db.conn().await.expect("checkout"))
            .await
            .expect("schema");
        db
    }

    let db = build().await;
    // A real write, after the builder has returned and any local guard would
    // have dropped.
    db.conn()
        .await
        .expect("checkout")
        .execute(
            "INSERT INTO users (id, email, username) VALUES ('u1','u1@x','u1')",
            (),
        )
        .await
        .expect("the database must still be usable after its builder returned");
}

/// Ask sqlite where its main database file is, so the assertion is about the
/// directory the fixture really used rather than one this test guessed.
async fn temp_dir_of(db: &common::TempDb) -> std::path::PathBuf {
    let conn = db.conn().await.expect("checkout");
    let mut rows = conn.query("PRAGMA database_list", ()).await.expect("pragma");
    let row = rows.next().await.expect("row").expect("main database");
    let file: String = row.get(2).expect("file column");
    std::path::PathBuf::from(file)
        .parent()
        .expect("the db file has a parent directory")
        .to_path_buf()
}
