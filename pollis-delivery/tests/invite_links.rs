//! #847 — shareable group invite links: the security properties.

use pollis_delivery::db::Db;
use pollis_delivery::groups::{
    apply_create_invite_link, apply_redeem_invite_link, apply_revoke_invite_link,
    CreateInviteLinkBody, RedeemInviteLinkBody, RedeemOutcome, RevokeInviteLinkBody,
};
use pollis_delivery::invite_token;
use pollis_delivery::writes::WriteOutcome;

mod common;


const GROUP: &str = "grp-1";

/// `group_name` rides along since #987 so the confirmation needs no follow-up read.
fn joined() -> RedeemOutcome {
    RedeemOutcome::Joined {
        group_id: GROUP.to_string(),
        group_name: Some("Group One".to_string()),
    }
}
const ADMIN: &str = "admin-1";
const JOINER: &str = "joiner-1";

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    let conn = db.conn().await.unwrap();
    // Turso runs with foreign keys off; local libsql defaults them on.
    conn.execute_batch("PRAGMA foreign_keys=OFF;").await.expect("schema");
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    conn.execute_batch(
        "INSERT INTO users (id, email, username) VALUES \
           ('admin-1','admin@x','admin'),('joiner-1','joiner@x','joiner'),('other-1','other@x','other');\
         INSERT INTO conversation (id, kind) VALUES \
           ('grp-1','group'),('grp-2','group'),('chan-1','channel');\
         INSERT INTO groups (id, name, owner_id) VALUES \
           ('grp-1','Group One','admin-1'),('grp-2','Group Two','other-1');\
         INSERT INTO channels (id, group_id, name) VALUES ('chan-1','grp-1', 'chan');\
         INSERT INTO user_device (device_id, user_id) VALUES ('dev-joiner','joiner-1');\
         INSERT INTO group_member (group_id, user_id, role) VALUES ('grp-1','admin-1','admin');\
         INSERT INTO group_member (group_id, user_id, role) VALUES ('grp-2','other-1','admin');",
    )
    .await
    .expect("seed");
    db
}

struct Link {
    id: String,
    token: String,
}

/// Copied from `pollis-core`'s `invite_token::mint`: the DS does not depend on pollis-core.
fn mint() -> (String, String, String) {
    use base64::Engine as _;
    use rand_core::{OsRng, RngCore as _};
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let mut sel = [0u8; 12];
    let mut sec = [0u8; 32];
    OsRng.fill_bytes(&mut sel);
    OsRng.fill_bytes(&mut sec);
    let selector = b64.encode(sel);
    let secret = b64.encode(sec);
    let hash = invite_token::hash_secret(&secret);
    (format!("{selector}.{secret}"), selector, hash)
}

async fn create_link(
    db: &Db,
    id: &str,
    expires_at: Option<&str>,
    max_uses: Option<i64>,
) -> Link {
    let (token, selector, secret_hash) = mint();
    let out = apply_create_invite_link(
        &db.conn().await.unwrap(),
        Some(ADMIN),
        &CreateInviteLinkBody {
            id: id.to_string(),
            group_id: GROUP.to_string(),
            created_by: Some(ADMIN.to_string()),
            selector,
            secret_hash,
            expires_at: expires_at.map(str::to_string),
            max_uses,
        },
    )
    .await
    .expect("create");
    assert!(matches!(out, WriteOutcome::Ok), "link creation must succeed");
    Link {
        id: id.to_string(),
        token,
    }
}

fn redeem_body(token: &str, attempt: &str, now: &str) -> RedeemInviteLinkBody {
    RedeemInviteLinkBody {
        token: token.to_string(),
        user_id: Some(JOINER.to_string()),
        attempt_id: attempt.to_string(),
        now: now.to_string(),
    }
}

async fn redeem(db: &Db, token: &str, attempt: &str, now: &str) -> RedeemOutcome {
    apply_redeem_invite_link(
        &db.conn().await.unwrap(),
        Some(JOINER),
        &redeem_body(token, attempt, now),
    )
    .await
    .expect("redeem")
}

async fn is_member(db: &Db, group: &str, user: &str) -> bool {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT 1 FROM group_member WHERE group_id = ?1 AND user_id = ?2",
            libsql::params![group, user],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().is_some()
}

/// The server ignores this (H3); use [`server_time_offset`] for clock relationships.
const NOW: &str = "2026-08-15T12:00:00Z";

fn server_time_offset(seconds: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(seconds)).to_rfc3339()
}

/// The failure window is measured on `attempted_at`, so ageing rows is the only way out (H3).
async fn age_attempts(db: &Db, seconds: i64) {
    db.conn()
        .await
        .unwrap()
        .execute(
            "UPDATE group_invite_link_redemption \
             SET attempted_at = datetime(attempted_at, ?1)",
            libsql::params![format!("-{seconds} seconds")],
        )
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_database_never_stores_the_token() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", None, None).await;
    let (_, secret) = invite_token::parse(&link.token).unwrap();

    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT id, group_id, created_by, selector, secret_hash, \
                    COALESCE(expires_at,''), COALESCE(max_uses,-1), uses, COALESCE(revoked_at,'') \
             FROM group_invite_link WHERE id = ?1",
            libsql::params![link.id.clone()],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().expect("row");

    let stored_selector: String = row.get(3).unwrap();
    let stored_hash: String = row.get(4).unwrap();

    for i in 0..9 {
        let cell = match row.get_value(i).unwrap() {
            libsql::Value::Text(t) => t,
            other => format!("{other:?}"),
        };
        assert!(
            !cell.contains(&secret),
            "column {i} leaked the secret: {cell}"
        );
        assert!(
            !cell.contains(&link.token),
            "column {i} leaked the whole token: {cell}"
        );
    }

    assert_eq!(stored_hash.len(), 64);
    assert!(stored_hash.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(stored_hash, invite_token::hash_secret(&secret));
    assert_ne!(stored_hash, secret);

    // The selector is stored verbatim but useless alone.
    let forged = format!("{stored_selector}.{}", "A".repeat(43));
    assert_eq!(
        redeem(&db, &forged, "att-forged", NOW).await,
        RedeemOutcome::Rejected,
        "the public selector alone must not admit anyone"
    );
    assert!(!is_member(&db, GROUP, JOINER).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_token_is_rejected() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", None, None).await;

    let out = apply_revoke_invite_link(
        &db.conn().await.unwrap(),
        Some(ADMIN),
        &RevokeInviteLinkBody {
            link_id: link.id.clone(),
            actor_id: Some(ADMIN.to_string()),
            revoked_at: NOW.to_string(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Ok));

    assert_eq!(
        redeem(&db, &link.token, "att-1", NOW).await,
        RedeemOutcome::Rejected
    );
    assert!(
        !is_member(&db, GROUP, JOINER).await,
        "a revoked link must not admit anyone"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_expired_token_is_rejected() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", Some(&server_time_offset(-3600)), None).await;

    assert_eq!(
        redeem(&db, &link.token, "att-1", NOW).await,
        RedeemOutcome::Rejected
    );
    assert!(!is_member(&db, GROUP, JOINER).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_expiring_exactly_now_is_rejected() {
    let db = fresh().await;
    // One second back is "exactly now" at `datetime()`'s resolution.
    let link = create_link(&db, "link-1", Some(&server_time_offset(-1)), None).await;
    assert_eq!(
        redeem(&db, &link.token, "att-1", NOW).await,
        RedeemOutcome::Rejected
    );
    assert!(!is_member(&db, GROUP, JOINER).await);
}

/// Control: proves the expiry tests don't pass by rejecting everything.
#[tokio::test(flavor = "multi_thread")]
async fn a_live_token_with_a_future_expiry_is_accepted() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", Some(&server_time_offset(3600)), None).await;
    assert_eq!(redeem(&db, &link.token, "att-1", NOW).await, joined());
    assert!(is_member(&db, GROUP, JOINER).await);
}

/// H3: expiry must be compared against the server clock, not `body.now`.
#[tokio::test(flavor = "multi_thread")]
async fn a_caller_supplied_clock_cannot_revive_an_expired_link() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", Some(&server_time_offset(-3600)), None).await;

    assert_eq!(
        redeem(&db, &link.token, "att-backdated", "2020-01-01T00:00:00Z").await,
        RedeemOutcome::Rejected,
        "a backdated `now` must not revive an expired link"
    );
    assert!(!is_member(&db, GROUP, JOINER).await);
}

/// H3: a future `body.now` must not empty the failure-budget window.
#[tokio::test(flavor = "multi_thread")]
async fn a_future_clock_cannot_empty_the_failure_budget() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", None, None).await;

    let forged = format!("{}.{}", "A".repeat(16), "B".repeat(43));
    for i in 0..11 {
        redeem(&db, &forged, &format!("att-{i}"), NOW).await;
    }
    assert_eq!(
        redeem(&db, &forged, "att-limited", NOW).await,
        RedeemOutcome::RateLimited,
        "the budget must be spent"
    );

    assert_eq!(
        redeem(&db, &link.token, "att-future", "2126-01-01T00:00:00Z").await,
        RedeemOutcome::RateLimited
    );
    assert!(!is_member(&db, GROUP, JOINER).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_past_max_uses_is_rejected() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", None, Some(1)).await;

    assert_eq!(
        redeem(&db, &link.token, "att-1", NOW).await,
        joined()
    );
    assert!(is_member(&db, GROUP, JOINER).await);

    let out = apply_redeem_invite_link(
        &db.conn().await.unwrap(),
        Some("other-1"),
        &RedeemInviteLinkBody {
            token: link.token.clone(),
            user_id: Some("other-1".to_string()),
            attempt_id: "att-2".to_string(),
            now: NOW.to_string(),
        },
    )
    .await
    .unwrap();
    assert_eq!(out, RedeemOutcome::Rejected);
    assert!(
        !is_member(&db, GROUP, "other-1").await,
        "an exhausted link must not admit a second person"
    );

    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT uses FROM group_invite_link WHERE id = ?1",
            libsql::params![link.id],
        )
        .await
        .unwrap();
    let uses: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(uses, 1, "a refused redemption must not burn a use");
}

/// The DB, not the app check, enforces the cap against racing redemptions.
#[tokio::test(flavor = "multi_thread")]
async fn the_use_cap_holds_under_concurrent_redemption() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", None, Some(1)).await;
    let conn = db.conn().await.unwrap();

    let stmt = "UPDATE group_invite_link SET uses = uses + 1 \
                WHERE id = ?1 AND revoked_at IS NULL \
                  AND (max_uses IS NULL OR uses < max_uses)";
    let first = conn
        .execute(stmt, libsql::params![link.id.clone()])
        .await
        .unwrap();
    let second = conn
        .execute(stmt, libsql::params![link.id.clone()])
        .await
        .unwrap();
    assert_eq!(first, 1, "the first racer takes the last use");
    assert_eq!(second, 0, "the second racer must find nothing to update");

    let forced = conn
        .execute(
            "UPDATE group_invite_link SET uses = uses + 1 WHERE id = ?1",
            libsql::params![link.id.clone()],
        )
        .await;
    assert!(
        forced.is_err(),
        "the CHECK (uses <= max_uses) constraint must reject an over-count \
         even when the application guard is bypassed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn redemption_goes_through_the_normal_membership_path() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", None, None).await;

    assert_eq!(
        redeem(&db, &link.token, "att-1", NOW).await,
        joined()
    );

    let conn = db.conn().await.unwrap();

    let mut rows = conn
        .query(
            "SELECT role FROM group_member WHERE group_id = ?1 AND user_id = ?2",
            libsql::params![GROUP, JOINER],
        )
        .await
        .unwrap();
    let role: String = rows.next().await.unwrap().expect("member row").get(0).unwrap();
    assert_eq!(role, "member");
    drop(rows);

    // Without seeded watermarks envelope GC is stranded (I3).
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM conversation_watermark \
             WHERE user_id = ?1 AND conversation_id = 'chan-1'",
            libsql::params![JOINER],
        )
        .await
        .unwrap();
    let seeded: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(
        seeded, 1,
        "redemption must seed watermarks exactly as add_member_rows does"
    );
    drop(rows);

    // Redemption must not touch MLS state.
    let mut rows = conn
        .query(
            "SELECT succeeded, link_id FROM group_invite_link_redemption WHERE user_id = ?1",
            libsql::params![JOINER],
        )
        .await
        .unwrap();
    let row = rows.next().await.unwrap().expect("audit row");
    assert_eq!(row.get::<i64>(0).unwrap(), 1);
    assert_eq!(row.get::<String>(1).unwrap(), link.id);
}

#[tokio::test(flavor = "multi_thread")]
async fn redemption_cannot_add_a_member_without_a_valid_token() {
    let db = fresh().await;
    let live = create_link(&db, "link-live", None, None).await;
    let (selector, _) = invite_token::parse(&live.token).unwrap();

    let bogus = [
        format!("{}.{}", "A".repeat(16), "B".repeat(43)),
        format!("{selector}.{}", "B".repeat(43)),
        "not-a-token".to_string(),
        String::new(),
        format!("{}.{}", "A".repeat(16), invite_token::parse(&live.token).unwrap().1),
    ];

    for (i, token) in bogus.iter().enumerate() {
        let out = redeem(&db, token, &format!("att-{i}"), NOW).await;
        assert_eq!(out, RedeemOutcome::Rejected, "token {i} must be rejected");
        assert!(
            !is_member(&db, GROUP, JOINER).await,
            "token {i} must not create a membership row"
        );
    }
}

/// With auth on and no signer, `resolve_actor` has nobody to bind to.
#[tokio::test(flavor = "multi_thread")]
async fn redemption_requires_an_authenticated_actor() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", None, None).await;

    let out = apply_redeem_invite_link(
        &db.conn().await.unwrap(),
        Some(JOINER),
        &RedeemInviteLinkBody {
            token: link.token.clone(),
            user_id: Some("other-1".to_string()),
            attempt_id: "att-1".to_string(),
            now: NOW.to_string(),
        },
    )
    .await
    .unwrap();
    assert_eq!(out, RedeemOutcome::Rejected);
    assert!(!is_member(&db, GROUP, "other-1").await);
    assert!(!is_member(&db, GROUP, JOINER).await);
}

/// Distinguishing failures would reveal a token was right but stale.
#[tokio::test(flavor = "multi_thread")]
async fn every_failure_is_indistinguishable() {
    let db = fresh().await;

    let revoked = create_link(&db, "l-revoked", None, None).await;
    apply_revoke_invite_link(
        &db.conn().await.unwrap(),
        Some(ADMIN),
        &RevokeInviteLinkBody {
            link_id: revoked.id.clone(),
            actor_id: Some(ADMIN.to_string()),
            revoked_at: NOW.to_string(),
        },
    )
    .await
    .unwrap();

    let expired = create_link(&db, "l-expired", Some("2026-08-15T11:00:00Z"), None).await;

    let exhausted = create_link(&db, "l-exhausted", None, Some(1)).await;
    apply_redeem_invite_link(
        &db.conn().await.unwrap(),
        Some("other-1"),
        &RedeemInviteLinkBody {
            token: exhausted.token.clone(),
            user_id: Some("other-1".to_string()),
            attempt_id: "att-seed".to_string(),
            now: NOW.to_string(),
        },
    )
    .await
    .unwrap();

    let cases = [
        ("wrong", format!("{}.{}", "A".repeat(16), "B".repeat(43))),
        ("malformed", "garbage".to_string()),
        ("revoked", revoked.token.clone()),
        ("expired", expired.token.clone()),
        ("exhausted", exhausted.token.clone()),
    ];

    for (i, (name, token)) in cases.iter().enumerate() {
        let out = redeem(&db, token, &format!("att-ind-{i}"), NOW).await;
        assert_eq!(
            out,
            RedeemOutcome::Rejected,
            "{name} must be reported as the SAME opaque rejection"
        );
    }

    // Rejections must stay unit variants: no payload a handler could leak.
    assert_eq!(
        std::mem::size_of::<RedeemOutcome>(),
        std::mem::size_of::<(String, Option<String>)>(),
        "RedeemOutcome must carry a payload only on the Joined path"
    );
}

/// A valid token is refused too, or the limiter becomes an oracle.
#[tokio::test(flavor = "multi_thread")]
async fn brute_force_is_rate_limited() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", None, None).await;

    let mut limited_at = None;
    for i in 0..15 {
        let out = redeem(
            &db,
            &format!("{}.{}", "A".repeat(16), "B".repeat(43)),
            &format!("att-{i}"),
            NOW,
        )
        .await;
        if out == RedeemOutcome::RateLimited {
            limited_at = Some(i);
            break;
        }
    }
    let limited_at = limited_at.expect("brute force must eventually be rate limited");
    assert_eq!(
        limited_at, 10,
        "the bound is 10 failures per window; it must engage on the 11th attempt"
    );

    assert_eq!(
        redeem(&db, &link.token, "att-valid", NOW).await,
        RedeemOutcome::RateLimited
    );
    assert!(!is_member(&db, GROUP, JOINER).await);

    // The bound is a delay, not a ban.
    age_attempts(&db, 3600).await;
    assert_eq!(
        redeem(&db, &link.token, "att-later", NOW).await,
        joined()
    );
    assert!(is_member(&db, GROUP, JOINER).await);
}

/// Otherwise a busy group's members would lock each other out.
#[tokio::test(flavor = "multi_thread")]
async fn successful_redemptions_do_not_consume_the_failure_budget() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", None, None).await;

    assert_eq!(
        redeem(&db, &link.token, "att-1", NOW).await,
        joined()
    );
    for i in 0..5 {
        assert_eq!(
            redeem(&db, &link.token, &format!("att-re-{i}"), NOW).await,
            joined()
        );
    }
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM group_invite_link_redemption WHERE user_id = ?1 AND succeeded = 0",
            libsql::params![JOINER],
        )
        .await
        .unwrap();
    let failures: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(failures, 0);

    drop(rows);
    let mut rows = conn
        .query(
            "SELECT uses FROM group_invite_link WHERE id = ?1",
            libsql::params![link.id],
        )
        .await
        .unwrap();
    let uses: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(uses, 1, "an idempotent re-redemption must not burn a use");
}

/// A link is an invite with the addressee left open, so the bar is the same.
#[tokio::test(flavor = "multi_thread")]
async fn only_an_admin_can_create_a_link() {
    let db = fresh().await;
    let (_, selector, secret_hash) = mint();
    let out = apply_create_invite_link(
        &db.conn().await.unwrap(),
        Some(JOINER),
        &CreateInviteLinkBody {
            id: "link-x".to_string(),
            group_id: GROUP.to_string(),
            created_by: Some(JOINER.to_string()),
            selector,
            secret_hash,
            expires_at: None,
            max_uses: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Forbidden));
}

/// The group is resolved from the link row, never from the request.
#[tokio::test(flavor = "multi_thread")]
async fn an_admin_of_another_group_cannot_revoke_this_groups_link() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", None, None).await;

    let out = apply_revoke_invite_link(
        &db.conn().await.unwrap(),
        Some("other-1"),
        &RevokeInviteLinkBody {
            link_id: link.id.clone(),
            actor_id: Some("other-1".to_string()),
            revoked_at: NOW.to_string(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(out, WriteOutcome::Forbidden));

    assert_eq!(
        redeem(&db, &link.token, "att-1", NOW).await,
        joined()
    );
}

/// Otherwise a hostile client could plant a hash it can cheaply preimage.
#[tokio::test(flavor = "multi_thread")]
async fn a_low_entropy_secret_hash_is_refused() {
    let db = fresh().await;
    for bad in ["", "abc", &"z".repeat(64), &"a".repeat(63)] {
        let out = apply_create_invite_link(
            &db.conn().await.unwrap(),
            Some(ADMIN),
            &CreateInviteLinkBody {
                id: format!("link-{}", bad.len()),
                group_id: GROUP.to_string(),
                created_by: Some(ADMIN.to_string()),
                selector: format!("sel{}", bad.len()),
                secret_hash: bad.to_string(),
                expires_at: None,
                max_uses: None,
            },
        )
        .await
        .unwrap();
        assert!(
            matches!(out, WriteOutcome::Forbidden),
            "secret_hash {bad:?} must be refused"
        );
    }
}

/// Re-revoking must not rewrite when the link died.
#[tokio::test(flavor = "multi_thread")]
async fn revocation_is_permanent_and_keeps_its_first_timestamp() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", None, None).await;
    let conn = db.conn().await.unwrap();

    for at in [NOW, "2026-09-01T00:00:00Z"] {
        apply_revoke_invite_link(
            &conn,
            Some(ADMIN),
            &RevokeInviteLinkBody {
                link_id: link.id.clone(),
                actor_id: Some(ADMIN.to_string()),
                revoked_at: at.to_string(),
            },
        )
        .await
        .unwrap();
    }

    let mut rows = conn
        .query(
            "SELECT revoked_at FROM group_invite_link WHERE id = ?1",
            libsql::params![link.id],
        )
        .await
        .unwrap();
    let revoked_at: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(revoked_at, NOW, "the first revocation time must stand");
}
