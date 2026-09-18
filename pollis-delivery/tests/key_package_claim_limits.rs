//! `POST /v1/key-packages/claim` used to be unlimited on purpose, and the
//! comment said so: "KP exhaustion — a hostile peer draining a target's pool by
//! claiming repeatedly — is a known concern tracked for #419, but is
//! deliberately NOT rate-limited here."
//!
//! It is the whole attack. A claim is a ONE-WAY flip of `claimed`: the package
//! is spent whether or not the claimer ever builds an Add. So one authenticated
//! account could loop until a target's pool was empty, and a device with no
//! unclaimed package cannot be added to a group at all — a stranger holds an
//! arbitrary user out of every conversation they are invited to, and the
//! packages do not come back.
//!
//! Three things now cost a claim: the claimer must be allowed to reach the
//! target at all, it is counted against a durable per-pair and per-target budget
//! read from the database (so a rolling deploy does not hand out a fresh
//! allowance), and only SUCCESSFUL claims are counted (charging for an empty pool
//! would let a target's own exhaustion lock out the honest adders retrying behind
//! it).
//!
//! Every test below fails against the pre-fix handler.

use pollis_delivery::devices::{apply_claim_key_package, ClaimKeyPackageBody, ClaimOutcome};

mod common;

/// MLS code point of the current suite, as the DS defaults an untagged claim.
use pollis_delivery::devices::CIPHERSUITE_PQ;

/// A fixture with `alice`, `bob` and `mallory` all in one group.
///
/// A claim now requires a SHARED CONVERSATION, not merely the absence of a block
/// (see `devices::may_claim_from`) — every real claim path writes the roster row
/// before reconciling the MLS tree to it, so this is the state every honest
/// claim is made in. The budget tests below are about what a co-member can do;
/// `a_stranger_cannot_touch_a_pool_at_all` covers the other side.
async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("kp-limits.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap())
        .await
        .expect("schema");
    co_members(&db, "shared-grp", &["alice", "bob", "mallory"]).await;
    db
}

/// Put every one of `users` in the group `group_id`.
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

/// Publish `count` unclaimed packages for `(user, device)`.
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

/// How many claims are on record for a pair.
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

/// THE DRAIN. Claiming in a loop stops paying out long before a real pool is
/// gone, and the refusal is distinguishable from an empty pool.
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

    // The pool still has packages for an honest adder — the point of the bound.
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

/// A normal add is nowhere near the bound: one claim per device, a handful of
/// devices, repeated for every conversation.
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

/// Each pair has its OWN budget, so one exhausted claimer cannot lock everybody
/// else out of a popular account.
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

/// A claim against an EMPTY pool is not charged. Otherwise a target whose pool
/// has run dry would see the honest adders retrying behind it burn their budgets
/// and be locked out once it replenishes.
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

/// Blocked in either direction is the one relationship state where draining a
/// pool is pure harassment and there is no legitimate add to protect.
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

/// **L3.** The budget was 60 per pair per hour against a pool of FIVE, so it
/// bound nothing: any unblocked stranger could empty a device's pool in five
/// requests and hold that device out of every new conversation until it next
/// came online. The gate is now a relationship — a shared conversation — and a
/// stranger cannot take even the first package.
#[tokio::test]
async fn a_stranger_cannot_touch_a_pool_at_all() {
    let db = fresh().await;
    publish(&db, "bob", "b1", 5).await;
    let conn = db.conn().await.unwrap();

    // `stranger` is in no conversation with bob, and has blocked nobody.
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

    // The pool is untouched, so bob is still addable. Scoped so the read
    // statement is finalized before the write below — an open cursor pins this
    // connection's snapshot and would hide it.
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

    // The moment they share a conversation — which every real add path writes
    // BEFORE reconciling the MLS tree — the claim goes through.
    co_members(&db, "new-grp", &["stranger", "bob"]).await;
    let out = apply_claim_key_package(&conn, Some("stranger"), &claim("bob", "b1"))
        .await
        .unwrap();
    assert!(matches!(out, ClaimOutcome::Claimed { .. }), "got {out:?}");
}

/// The gate is the DESIRED roster, so a PENDING INVITEE is claimable. This is
/// the shape `send_group_invite` produces: `/v1/invites/create` writes the
/// pending `group_invite` row and the inviter reconciles immediately, claiming
/// the invitee's KeyPackage so their Welcome is staged before they accept. A
/// gate that demanded `group_member` would 403 there and break every invite.
#[tokio::test]
async fn a_pending_invitee_is_claimable_by_the_inviter() {
    let db = fresh().await;
    publish(&db, "newcomer", "n1", 5).await;
    let conn = db.conn().await.unwrap();

    // Alice is an admin of a group; `newcomer` has been invited but has not
    // accepted, so there is no `group_member` row for them.
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

/// A DM is the other shape membership takes, and is what the "anybody may start
/// a conversation with anybody" path produces: `/v1/dm/create` writes both
/// `dm_channel_member` rows in one transaction before the client reconciles, so
/// a first-contact DM still claims.
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

/// Adding your own second device claims from your own pool, and must never be
/// gated by a relationship check.
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

/// The budget is DURABLE — counted from the table, not from a process-local
/// map — so a DS restart does not hand an attacker a fresh allowance. Modelled
/// by re-reading through a brand-new connection, which is all a restarted
/// instance has.
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

    // A "new instance": a fresh connection with no memory of the spree.
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
