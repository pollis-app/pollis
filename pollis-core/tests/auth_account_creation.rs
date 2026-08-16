//! #875 — resolve-or-create of the `users` row from an email address, the client
//! twin of the Delivery Service's `apply_verify_otp`.
//!
//! These live in an integration binary rather than pollis-core's `--lib` tests
//! for the same reason as `resign_stale_certs.rs`: libsql's local backend calls
//! `sqlite3_config` on first use, which fails once rusqlite/SQLCipher (which
//! `LocalDb` uses) has already initialised SQLite in the same process. An
//! integration test gets its own process, so the real query and the real
//! `NOT NULL UNIQUE` on `users.email` are what the assertions run against —
//! which matters here, because the bug being pinned is about a constraint the
//! two spellings of one address slip past.
//!
//! The exercised function is dev-only (`#[cfg(debug_assertions)]`), so this file
//! compiles away in a release test run.
#![cfg(debug_assertions)]

use pollis_core::commands::auth::resolve_or_create_user_by_email;

async fn conn() -> libsql::Connection {
    let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
    let conn = db.connect().unwrap();
    // The SHIPPED remote schema (#875): the old fixture's `users` had a
    // non-UNIQUE `username` and no `identity_version`, so "did this create a
    // second account?" was asked of a table that permits collisions the real
    // one rejects.
    for sql in pollis_schema::main_scripts() {
        conn.execute_batch(sql).await.unwrap();
    }
    conn
}

/// Count the `users` rows, so "did this create a second account?" is a direct
/// observation rather than an inference.
async fn user_count(conn: &libsql::Connection) -> i64 {
    let mut rows = conn.query("SELECT COUNT(*) FROM users", ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

/// The default username the DS mints: the email's local part, an underscore, and
/// the last four characters of the account's ULID.
#[tokio::test]
async fn a_new_address_creates_one_row_with_the_ds_default_username() {
    let c = conn().await;
    let r = resolve_or_create_user_by_email(&c, "alice@x.com")
        .await
        .unwrap();

    assert!(r.is_new);
    assert!(r.account_id_pub.is_none());
    assert_eq!(user_count(&c).await, 1);

    let suffix = &r.user_id[r.user_id.len() - 4..];
    assert_eq!(r.username, format!("alice_{suffix}"));
}

/// **The drift.** The DS trims before it touches `users`; the client copy did
/// not. Two spellings of one address therefore missed each other's row and each
/// INSERTed their own — two accounts for one person, which the UNIQUE constraint
/// cannot catch because the two strings genuinely differ.
#[tokio::test]
async fn the_same_address_spelled_two_ways_resolves_to_one_account() {
    let c = conn().await;
    let first = resolve_or_create_user_by_email(&c, "alice@x.com")
        .await
        .unwrap();
    let padded = resolve_or_create_user_by_email(&c, "  alice@x.com \n")
        .await
        .unwrap();

    assert!(
        !padded.is_new,
        "the padded spelling must find the existing row"
    );
    assert_eq!(padded.user_id, first.user_id);
    assert_eq!(
        user_count(&c).await,
        1,
        "one address must never own two accounts"
    );
}

/// A returning account reports its published identity key, which is what drives
/// the enrollment gate.
#[tokio::test]
async fn an_existing_row_reports_its_identity_key() {
    let c = conn().await;
    c.execute(
        "INSERT INTO users (id, email, username, account_id_pub) VALUES (?1, ?2, ?3, ?4)",
        libsql::params!["u-1", "bob@x.com", "bob", vec![7u8; 32]],
    )
    .await
    .unwrap();

    let r = resolve_or_create_user_by_email(&c, "bob@x.com").await.unwrap();
    assert!(!r.is_new);
    assert_eq!(r.user_id, "u-1");
    assert_eq!(r.username, "bob");
    assert_eq!(r.account_id_pub, Some(vec![7u8; 32]));
}

/// A NULL `account_id_pub` is "no identity yet", not an error. Regression guard:
/// the old `row.get::<Vec<u8>>(2).ok()` spelling happened to reach the same
/// answer, so this passes either way — it pins the behaviour the enrollment gate
/// depends on rather than proving a fix.
#[tokio::test]
async fn a_null_identity_key_reads_as_none() {
    let c = conn().await;
    c.execute(
        "INSERT INTO users (id, email, username) VALUES ('u-2', 'carol@x.com', 'carol')",
        (),
    )
    .await
    .unwrap();

    let r = resolve_or_create_user_by_email(&c, "carol@x.com")
        .await
        .unwrap();
    assert!(!r.is_new);
    assert!(r.account_id_pub.is_none());
}

/// An address with no local part still yields a username — the DS produces
/// `"_<suffix>"` here (`split('@')` returns an empty first segment, never
/// `None`), and the client must not "improve" on that.
#[tokio::test]
async fn an_empty_local_part_matches_the_ds_exactly() {
    let c = conn().await;
    let r = resolve_or_create_user_by_email(&c, "@x.com").await.unwrap();
    let suffix = &r.user_id[r.user_id.len() - 4..];
    assert_eq!(r.username, format!("_{suffix}"));
}
