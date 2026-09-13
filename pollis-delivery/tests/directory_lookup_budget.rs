//! #1089: the directory identifier lookup is an email→identity oracle, bounded
//! per user per day.
//!
//! `/v1/directory/users` resolves an `identifier` — a username or an EMAIL — to
//! an account id, username and avatar for any authenticated caller. Without a
//! bound, one throwaway account turns a mailing list into a list of which
//! addresses have Pollis accounts and under what name.
//!
//! The per-IP middleware tier sheds floods, but an attacker rotates IPs, so the
//! bound that binds is keyed on the authenticated user and read from the
//! database — restart-proof and shared across container instances. Same
//! reasoning the invite-redemption limit already writes down.

use pollis_delivery::directory::{charge_identifier_lookup, IDENTIFIER_LOOKUPS_PER_DAY};

mod common;

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    db
}

#[tokio::test]
async fn lookups_are_allowed_up_to_the_cap_then_refused() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    for i in 1..=IDENTIFIER_LOOKUPS_PER_DAY {
        assert!(
            charge_identifier_lookup(&conn, "alice").await.unwrap(),
            "lookup {i} of {IDENTIFIER_LOOKUPS_PER_DAY} must be allowed"
        );
    }
    assert!(
        !charge_identifier_lookup(&conn, "alice").await.unwrap(),
        "the lookup past the cap must be refused"
    );
}

/// A refused attempt still costs. Otherwise the cap is a speed bump: an attacker
/// that ignores the 429 gets an unlimited oracle at one lookup per request.
#[tokio::test]
async fn refused_attempts_still_consume_budget() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    for _ in 0..IDENTIFIER_LOOKUPS_PER_DAY {
        charge_identifier_lookup(&conn, "mallory").await.unwrap();
    }
    for _ in 0..5 {
        assert!(!charge_identifier_lookup(&conn, "mallory").await.unwrap());
    }
    let mut rows = conn
        .query(
            "SELECT lookups FROM directory_lookup_budget WHERE user_id = 'mallory'",
            (),
        )
        .await
        .unwrap();
    let used: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(
        used,
        IDENTIFIER_LOOKUPS_PER_DAY + 5,
        "every attempt is charged, refused ones included"
    );
}

/// The budget is per user, so one account burning its allowance cannot deny the
/// directory to everyone else.
#[tokio::test]
async fn the_budget_is_per_user() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    for _ in 0..=IDENTIFIER_LOOKUPS_PER_DAY {
        charge_identifier_lookup(&conn, "mallory").await.unwrap();
    }
    assert!(
        !charge_identifier_lookup(&conn, "mallory").await.unwrap(),
        "mallory is out"
    );
    assert!(
        charge_identifier_lookup(&conn, "alice").await.unwrap(),
        "alice's allowance is her own"
    );
}

/// And it is per UTC day: yesterday's exhaustion does not carry over.
#[tokio::test]
async fn the_budget_resets_on_a_new_day() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    // Pre-load yesterday at the cap, the way an exhausted day looks on disk.
    conn.execute(
        "INSERT INTO directory_lookup_budget (user_id, day, lookups) \
         VALUES ('alice', strftime('%Y-%m-%d','now','-1 day'), ?1)",
        libsql::params![IDENTIFIER_LOOKUPS_PER_DAY + 10],
    )
    .await
    .unwrap();

    assert!(
        charge_identifier_lookup(&conn, "alice").await.unwrap(),
        "today is a fresh allowance"
    );
}
