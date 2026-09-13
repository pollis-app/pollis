//! #1090: a push token belongs to the device that registered it.
//!
//! `push_token.token` is the primary key and the conflict branch reassigned
//! `user_id` unconditionally, so anyone holding a victim's Expo token string
//! could register it to their own account: the victim's phone then received the
//! attacker's notifications and stopped receiving its own.
//!
//! Refusing every reassignment would have broken the case the original design
//! served — switching accounts on one phone. The two are told apart by WHICH
//! DEVICE asks, using the server-verified `X-Pollis-Device`, never the body.

use pollis_api::devices::PushTokenBody;
use pollis_delivery::devices::{
    apply_register_push_token, is_expo_push_token, PUSH_TOKENS_PER_USER,
};
use pollis_delivery::writes::WriteOutcome;

mod common;

const VICTIM: &str = "victim-1";
const MALLORY: &str = "mallory-1";
const TOKEN: &str = "ExponentPushToken[abc123def456]";

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    conn.execute_batch(
        "INSERT INTO users (id, email, username) VALUES ('victim-1','v@x','victim');\
         INSERT INTO users (id, email, username) VALUES ('mallory-1','m@x','mallory');",
    )
    .await
    .expect("seed");
    db
}

fn body(token: &str) -> PushTokenBody {
    PushTokenBody {
        token: token.to_string(),
        platform: "ios".to_string(),
        updated_at: "2026-01-01T00:00:00.000000000+00:00".to_string(),
        user_id: None,
    }
}

async fn owner_of(conn: &libsql::Connection, token: &str) -> Option<String> {
    let mut rows = conn
        .query(
            "SELECT user_id FROM push_token WHERE token = ?1",
            libsql::params![token.to_string()],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().map(|row| row.get(0).unwrap())
}

/// The finding, in one test: a stolen token string is not enough.
#[tokio::test]
async fn another_device_cannot_steal_a_registered_token() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    apply_register_push_token(&conn, Some(VICTIM), Some("victim-phone"), &body(TOKEN))
        .await
        .unwrap();
    assert_eq!(owner_of(&conn, TOKEN).await.as_deref(), Some(VICTIM));

    let out =
        apply_register_push_token(&conn, Some(MALLORY), Some("mallory-phone"), &body(TOKEN))
            .await
            .unwrap();
    assert!(
        matches!(out, WriteOutcome::Forbidden),
        "a different device must not take the token over"
    );
    assert_eq!(
        owner_of(&conn, TOKEN).await.as_deref(),
        Some(VICTIM),
        "the victim keeps its own notifications"
    );
}

/// And the case the original design was serving still works: one phone, new
/// account. Same device, so it is an account switch, not a theft.
#[tokio::test]
async fn the_same_device_may_switch_accounts() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    apply_register_push_token(&conn, Some(VICTIM), Some("the-phone"), &body(TOKEN))
        .await
        .unwrap();
    let out = apply_register_push_token(&conn, Some(MALLORY), Some("the-phone"), &body(TOKEN))
        .await
        .unwrap();
    assert!(matches!(out, WriteOutcome::Ok), "an account switch is legitimate");
    assert_eq!(owner_of(&conn, TOKEN).await.as_deref(), Some(MALLORY));
}

/// A row written before this migration carries no binding; the first device to
/// re-register adopts it, rather than the row being permanently unclaimable.
#[tokio::test]
async fn a_legacy_unbound_row_is_adopted_then_defended() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT INTO push_token (token, user_id, platform, updated_at) VALUES (?1,?2,'ios','t')",
        libsql::params![TOKEN.to_string(), VICTIM.to_string()],
    )
    .await
    .unwrap();

    apply_register_push_token(&conn, Some(VICTIM), Some("victim-phone"), &body(TOKEN))
        .await
        .unwrap();
    // Now bound — so the thief is refused.
    let out =
        apply_register_push_token(&conn, Some(MALLORY), Some("mallory-phone"), &body(TOKEN))
            .await
            .unwrap();
    assert!(matches!(out, WriteOutcome::Forbidden));
}

#[tokio::test]
async fn junk_that_was_never_a_token_is_refused() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    for bad in ["", "not-a-token", "ExponentPushToken[]", "ExponentPushToken[abc", "<script>"] {
        let out = apply_register_push_token(&conn, Some(VICTIM), Some("p"), &body(bad))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Forbidden), "{bad:?} must be refused");
    }
    assert!(is_expo_push_token("ExpoPushToken[xyz_1-2.3]"), "the other prefix is valid");
}

/// A reinstall loop must not turn one message into unbounded Expo calls.
#[tokio::test]
async fn tokens_per_user_are_capped_oldest_first() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    let n = PUSH_TOKENS_PER_USER + 5;
    for i in 0..n {
        let mut b = body(&format!("ExponentPushToken[tok{i}]"));
        // Ascending stamps, so the low indices are the oldest.
        b.updated_at = format!("2026-01-01T00:00:{:02}.000000000+00:00", i);
        apply_register_push_token(&conn, Some(VICTIM), Some("the-phone"), &b)
            .await
            .unwrap();
    }

    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM push_token WHERE user_id = ?1",
            libsql::params![VICTIM.to_string()],
        )
        .await
        .unwrap();
    let kept: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(kept, PUSH_TOKENS_PER_USER, "the cap holds");
    drop(rows);

    // The oldest went, the newest stayed.
    assert!(owner_of(&conn, "ExponentPushToken[tok0]").await.is_none(), "oldest evicted");
    assert!(
        owner_of(&conn, &format!("ExponentPushToken[tok{}]", n - 1)).await.is_some(),
        "newest kept"
    );
}
