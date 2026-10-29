//! `POST /v1/key-packages/claim` limits: a claim spends a package, so unlimited claims drain a pool.

use pollis_delivery::devices::{apply_claim_key_package, ClaimKeyPackageBody, ClaimOutcome};

mod common;

use pollis_delivery::devices::CIPHERSUITE_PQ;

/// `alice`, `bob` and `mallory` in one group: a claim needs a shared conversation.
async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("kp-limits.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap())
        .await
        .expect("schema");
    co_members(&db, "shared-grp", &["alice", "bob", "mallory"]).await;
    db
}

async fn co_members(db: &common::TempDb, group_id: &str, users: &[&str]) {
    let conn = db.conn().await.unwrap();
    for u in users {
        conn.execute(
            "INSERT OR IGNORE INTO group_member (group_id, user_id) VALUES (?1, ?2)",
            libsql::params![group_id, *u],
        )
        .await
        .unwrap();
    }
}

async fn publish(db: &common::TempDb, user: &str, device: &str, count: usize) {
    let conn = db.conn().await.unwrap();
    for i in 0..count {
        conn.execute(
            "INSERT INTO mls_key_package \
                 (ref_hash, user_id, key_package, device_id, ciphersuite, claimed) \
             VALUES (?1, ?2, X'00', ?3, ?4, 0)",
            libsql::params![
                format!("{user}-{device}-{i}"),
                user,
                device,
                CIPHERSUITE_PQ
            ],
        )
        .await
        .unwrap();
    }
}

fn claim(target: &str, device: &str) -> ClaimKeyPackageBody {
    ClaimKeyPackageBody {
        target_user_id: target.to_string(),
        target_device_id: Some(device.to_string()),
        ciphersuite: None,
    }
}

async fn block(db: &common::TempDb, blocker: &str, blocked: &str) {
    db.conn()
        .await
        .unwrap()
        .execute(
            "INSERT INTO user_block (blocker_id, blocked_id) VALUES (?1, ?2)",
            libsql::params![blocker, blocked],
        )
        .await
        .unwrap();
}

async fn recorded(db: &common::TempDb, claimer: &str, target: &str) -> i64 {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM mls_key_package_claim \
             WHERE claimer_id = ?1 AND target_user_id = ?2",
            libsql::params![claimer, target],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get(0).unwrap()
}

/// Claiming in a loop stops paying out long before a real pool is gone.
#[tokio::test]
async fn one_account_cannot_drain_a_targets_pool() {
    let db = fresh().await;
    publish(&db, "bob", "b1", 200).await;
    let conn = db.conn().await.unwrap();

    let mut claimed = 0;
    let mut limited = false;
    for _ in 0..200 {
        match apply_claim_key_package(&conn, Some("mallory"), &claim("bob", "b1"))
            .await
            .unwrap()
        {
            ClaimOutcome::Claimed { .. } => claimed += 1,
            ClaimOutcome::RateLimited => {
                limited = true;
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    assert!(limited, "an unbounded claim loop must eventually be refused");
    assert!(
        claimed < 200,
        "the loop drained the whole pool: {claimed} packages taken"
    );

    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM mls_key_package WHERE user_id = 'bob' AND claimed = 0",
            (),
        )
        .await
        .unwrap();
    let left: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert!(left > 0, "bob must still be addable");
}

#[tokio::test]
async fn an_ordinary_run_of_adds_is_unaffected() {
    let db = fresh().await;
    publish(&db, "bob", "b1", 20).await;
    let conn = db.conn().await.unwrap();

    for i in 0..20 {
        let out = apply_claim_key_package(&conn, Some("alice"), &claim("bob", "b1"))
            .await
            .unwrap();
        assert!(
            matches!(out, ClaimOutcome::Claimed { .. }),
            "claim {i} of an ordinary run must succeed, got {out:?}"
        );
    }
}

/// Each pair has its own budget.
#[tokio::test]
async fn one_claimers_budget_is_not_another_claimers() {
    let db = fresh().await;
    publish(&db, "bob", "b1", 200).await;
    let conn = db.conn().await.unwrap();

    while !matches!(
        apply_claim_key_package(&conn, Some("mallory"), &claim("bob", "b1"))
            .await
            .unwrap(),
        ClaimOutcome::RateLimited
    ) {}

    let out = apply_claim_key_package(&conn, Some("alice"), &claim("bob", "b1"))
        .await
        .unwrap();
    assert!(
        matches!(out, ClaimOutcome::Claimed { .. }),
        "alice's first claim must not be charged to mallory's spree"
    );
}

/// A claim against an empty pool is not charged, or retrying adders burn their budgets.
#[tokio::test]
async fn an_empty_pool_costs_the_claimer_nothing() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    for _ in 0..100 {
        assert!(matches!(
            apply_claim_key_package(&conn, Some("alice"), &claim("bob", "b1"))
                .await
                .unwrap(),
            ClaimOutcome::NoKeyPackage
        ));
    }
    assert_eq!(recorded(&db, "alice", "bob").await, 0);

    publish(&db, "bob", "b1", 1).await;
    assert!(matches!(
        apply_claim_key_package(&conn, Some("alice"), &claim("bob", "b1"))
            .await
            .unwrap(),
        ClaimOutcome::Claimed { .. }
    ));
}

#[tokio::test]
async fn a_blocked_pair_cannot_claim_in_either_direction() {
    let db = fresh().await;
    publish(&db, "bob", "b1", 5).await;
    publish(&db, "mallory", "m1", 5).await;
    block(&db, "bob", "mallory").await;
    let conn = db.conn().await.unwrap();

    assert!(matches!(
        apply_claim_key_package(&conn, Some("mallory"), &claim("bob", "b1"))
            .await
            .unwrap(),
        ClaimOutcome::Forbidden
    ));
    assert!(
        matches!(
            apply_claim_key_package(&conn, Some("bob"), &claim("mallory", "m1"))
                .await
                .unwrap(),
            ClaimOutcome::Forbidden
        ),
        "the block is symmetric — the blocker must not drain the blocked user either"
    );
    assert_eq!(recorded(&db, "mallory", "bob").await, 0);
}

/// L3: the gate is a shared conversation; a stranger cannot take even the first package.
#[tokio::test]
async fn a_stranger_cannot_touch_a_pool_at_all() {
    let db = fresh().await;
    publish(&db, "bob", "b1", 5).await;
    let conn = db.conn().await.unwrap();

    assert!(
        matches!(
            apply_claim_key_package(&conn, Some("stranger"), &claim("bob", "b1"))
                .await
                .unwrap(),
            ClaimOutcome::Forbidden
        ),
        "an account sharing no conversation with the target must not claim at all"
    );
    assert_eq!(recorded(&db, "stranger", "bob").await, 0);

    // Scoped so the read is finalized before the write: an open cursor pins the snapshot.
    {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM mls_key_package WHERE user_id = 'bob' AND claimed = 0",
                (),
            )
            .await
            .unwrap();
        let left: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(left, 5, "a refused claim must spend nothing");
    }

    co_members(&db, "new-grp", &["stranger", "bob"]).await;
    let out = apply_claim_key_package(&conn, Some("stranger"), &claim("bob", "b1"))
        .await
        .unwrap();
    assert!(matches!(out, ClaimOutcome::Claimed { .. }), "got {out:?}");
}

/// The gate is the desired roster, so a pending invitee is claimable (`send_group_invite`).
#[tokio::test]
async fn a_pending_invitee_is_claimable_by_the_inviter() {
    let db = fresh().await;
    publish(&db, "newcomer", "n1", 5).await;
    let conn = db.conn().await.unwrap();

    co_members(&db, "alice-grp", &["alice"]).await;
    conn.execute(
        "INSERT INTO group_invite (id, group_id, inviter_id, invitee_id) \
         VALUES ('inv-1', 'alice-grp', 'alice', 'newcomer')",
        (),
    )
    .await
    .unwrap();

    let out = apply_claim_key_package(&conn, Some("alice"), &claim("newcomer", "n1"))
        .await
        .unwrap();
    assert!(matches!(out, ClaimOutcome::Claimed { .. }), "got {out:?}");
}

/// `/v1/dm/create` writes both member rows before reconcile, so a first-contact DM claims.
#[tokio::test]
async fn a_dm_counts_as_a_shared_conversation() {
    let db = fresh().await;
    publish(&db, "bob", "b1", 5).await;
    let conn = db.conn().await.unwrap();

    for u in ["newcomer", "bob"] {
        conn.execute(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) \
             VALUES ('dm-1', ?1, 'newcomer')",
            libsql::params![u],
        )
        .await
        .unwrap();
    }

    assert!(matches!(
        apply_claim_key_package(&conn, Some("newcomer"), &claim("bob", "b1"))
            .await
            .unwrap(),
        ClaimOutcome::Claimed { .. }
    ));
}

#[tokio::test]
async fn a_user_may_always_claim_from_its_own_pool() {
    let db = fresh().await;
    publish(&db, "alice", "a2", 1).await;
    let conn = db.conn().await.unwrap();

    assert!(matches!(
        apply_claim_key_package(&conn, Some("alice"), &claim("alice", "a2"))
            .await
            .unwrap(),
        ClaimOutcome::Claimed { .. }
    ));
}

/// The budget is durable (read from the table), so a restart grants no fresh allowance.
#[tokio::test]
async fn the_budget_survives_a_restart() {
    let db = fresh().await;
    publish(&db, "bob", "b1", 200).await;

    {
        let conn = db.conn().await.unwrap();
        while !matches!(
            apply_claim_key_package(&conn, Some("mallory"), &claim("bob", "b1"))
                .await
                .unwrap(),
            ClaimOutcome::RateLimited
        ) {}
    }

    let restarted = db.conn().await.unwrap();
    assert!(
        matches!(
            apply_claim_key_package(&restarted, Some("mallory"), &claim("bob", "b1"))
                .await
                .unwrap(),
            ClaimOutcome::RateLimited
        ),
        "a bound a rolling restart clears is not a bound"
    );
}
