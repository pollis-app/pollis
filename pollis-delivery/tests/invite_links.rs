//! #847 — shareable group invite links: the security properties.
//!
//! These tests exist to prove the things that could go WRONG, not that the happy
//! path works. Each one below fails if a specific guarantee is removed:
//!
//!   * `a_revoked_token_is_rejected`
//!   * `an_expired_token_is_rejected`
//!   * `a_token_past_max_uses_is_rejected`
//!   * `the_database_never_stores_the_token` — the property the issue names
//!     first: dumping `group_invite_link` must not yield a working invite.
//!   * `redemption_goes_through_the_normal_membership_path` — a redeemer lands
//!     as an ordinary `group_member` row, byte-identical to one produced by
//!     accepting an addressed invite, with watermarks seeded the same way.
//!   * `redemption_cannot_add_a_member_without_a_valid_token` — the negative of
//!     the above: no `group_member` row appears for any rejected attempt.
//!   * `every_failure_is_indistinguishable` — wrong / malformed / revoked /
//!     expired / exhausted all produce the identical status AND body.
//!   * `brute_force_is_rate_limited` — the durable per-user bound.
//!   * `the_use_cap_holds_under_concurrent_redemption` — the DB CHECK, not the
//!     application check, is the arbiter.
//!   * `an_admin_of_another_group_cannot_revoke_this_groups_link`.
//!
//! Driven against a local libsql DB exactly as the DS runs, at the `apply_*`
//! level (where the authorization decision lives), mirroring
//! `envelope_retention.rs`. The router-level auth path is covered by `auth.rs`
//! and is not re-proved here.

use pollis_delivery::db::Db;
use pollis_delivery::groups::{
    apply_create_invite_link, apply_redeem_invite_link, apply_revoke_invite_link,
    CreateInviteLinkBody, RedeemInviteLinkBody, RedeemOutcome, RevokeInviteLinkBody,
};
use pollis_delivery::invite_token;
use pollis_delivery::writes::WriteOutcome;

mod common;


const GROUP: &str = "grp-1";

/// The success outcome for [`GROUP`].
///
/// `group_name` rides along since #987 so the redeem confirmation needs no
/// follow-up read; asserting it here keeps that promise honest rather than
/// letting the field quietly go `None`.
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
    // Production is Turso, where foreign-key enforcement is off; libsql's LOCAL
    // backend turns it ON by default, so say so explicitly rather than inherit a
    // constraint no deploy has (`Db::connect_local` makes the same call).
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

/// A minted token plus the link id it was stored under. Mirrors what the client
/// does: mint locally, send only the hash.
struct Link {
    id: String,
    token: String,
}

/// Mint a token the way `pollis-core`'s `invite_token::mint` does. Duplicated
/// here rather than imported because the DS deliberately does not depend on
/// pollis-core — the DS never mints in production.
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

const NOW: &str = "2026-08-15T12:00:00Z";

// ── The token is never stored ────────────────────────────────────────────────

/// The property #847 names first: a database read must not yield working
/// invites. Every column of the stored row is scanned for any substring of the
/// live token.
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

    // The secret half must appear NOWHERE.
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

    // What IS stored is a sha256 digest of the secret — one-way.
    assert_eq!(stored_hash.len(), 64);
    assert!(stored_hash.chars().all(|c| c.is_ascii_hexdigit()));
    assert_eq!(stored_hash, invite_token::hash_secret(&secret));
    assert_ne!(stored_hash, secret);

    // The selector IS stored verbatim — and is useless alone. Presenting it with
    // any other secret must fail.
    let forged = format!("{stored_selector}.{}", "A".repeat(43));
    assert_eq!(
        redeem(&db, &forged, "att-forged", NOW).await,
        RedeemOutcome::Rejected,
        "the public selector alone must not admit anyone"
    );
    assert!(!is_member(&db, GROUP, JOINER).await);
}

// ── Revoked / expired / exhausted ────────────────────────────────────────────

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
    // Expired an hour before `NOW`.
    let link = create_link(&db, "link-1", Some("2026-08-15T11:00:00Z"), None).await;

    assert_eq!(
        redeem(&db, &link.token, "att-1", NOW).await,
        RedeemOutcome::Rejected
    );
    assert!(!is_member(&db, GROUP, JOINER).await);
}

/// The boundary: a link that expires exactly at `now` is dead, not alive.
#[tokio::test(flavor = "multi_thread")]
async fn a_token_expiring_exactly_now_is_rejected() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", Some(NOW), None).await;
    assert_eq!(
        redeem(&db, &link.token, "att-1", NOW).await,
        RedeemOutcome::Rejected
    );
    assert!(!is_member(&db, GROUP, JOINER).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_token_past_max_uses_is_rejected() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", None, Some(1)).await;

    // First redemption consumes the single use.
    assert_eq!(
        redeem(&db, &link.token, "att-1", NOW).await,
        joined()
    );
    assert!(is_member(&db, GROUP, JOINER).await);

    // A DIFFERENT user presenting the same, now-exhausted token is refused.
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

/// The cap is enforced by the DATABASE, not merely by the application check.
/// This drives the guarded UPDATE directly, simulating two redemptions that both
/// passed the read-side check before either committed.
#[tokio::test(flavor = "multi_thread")]
async fn the_use_cap_holds_under_concurrent_redemption() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", None, Some(1)).await;
    let conn = db.conn().await.unwrap();

    // Two racers issue the same guarded UPDATE the redeem path issues.
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

    // And even if the guard were removed, the CHECK constraint refuses.
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

// ── Redemption reuses the normal membership path ─────────────────────────────

/// A redeemer must land as an ORDINARY member — same role, same seeded
/// watermarks — indistinguishable from someone added by an addressed invite.
/// If redemption ever grew its own insert, this test would catch the drift.
#[tokio::test(flavor = "multi_thread")]
async fn redemption_goes_through_the_normal_membership_path() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", None, None).await;

    assert_eq!(
        redeem(&db, &link.token, "att-1", NOW).await,
        joined()
    );

    let conn = db.conn().await.unwrap();

    // 1. A plain 'member' row — never 'admin', never a bespoke role.
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

    // 2. Watermarks seeded for every (channel, live device) pair — the part of
    //    `add_member_rows` that a hand-rolled INSERT would silently omit,
    //    stranding envelope GC (invariant I3).
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

    // 3. Redemption must NOT touch MLS state or stage anything. The only rows it
    //    wrote are the membership row, the watermark, the use count, and the
    //    audit row — nothing that could place a device in the tree.
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

/// The negative: no rejected attempt, of any kind, may produce a membership row.
#[tokio::test(flavor = "multi_thread")]
async fn redemption_cannot_add_a_member_without_a_valid_token() {
    let db = fresh().await;
    let live = create_link(&db, "link-live", None, None).await;
    let (selector, _) = invite_token::parse(&live.token).unwrap();

    let bogus = [
        // Entirely wrong token.
        format!("{}.{}", "A".repeat(16), "B".repeat(43)),
        // Right selector, wrong secret — the interesting one.
        format!("{selector}.{}", "B".repeat(43)),
        // Malformed.
        "not-a-token".to_string(),
        String::new(),
        // Right secret, wrong selector.
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

/// An unauthenticated caller cannot redeem: with auth ON and no signer,
/// `resolve_actor` has nobody to bind to. A link removes the need for an admin
/// to know your username — it does not remove the need to BE someone.
#[tokio::test(flavor = "multi_thread")]
async fn redemption_requires_an_authenticated_actor() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", None, None).await;

    // authed = Some(...) is the signed path. Here the body claims to be someone
    // else — `resolve_actor` must refuse to act as a different user.
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

// ── Failures are indistinguishable ───────────────────────────────────────────

/// Wrong, malformed, revoked, expired and exhausted must be one response.
/// Leaking which is which tells an attacker a token was RIGHT but stale, turning
/// a hopeless search into a hunt for a fresher link.
#[tokio::test(flavor = "multi_thread")]
async fn every_failure_is_indistinguishable() {
    let db = fresh().await;

    // Revoked.
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

    // Expired.
    let expired = create_link(&db, "l-expired", Some("2026-08-15T11:00:00Z"), None).await;

    // Exhausted — consumed by someone else first.
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

    // The outcome enum itself carries no discriminating payload — `Rejected` and
    // `RateLimited` are unit variants, so there is nothing for a handler to
    // accidentally surface. Asserted as a size equality with the `Joined`
    // payload alone: if either rejection variant grew a field, the enum would
    // have to grow past it. This is what makes the property structural rather
    // than a matter of care.
    //
    // The payload is `(group_id, group_name)` since #987 — the confirmation UI's
    // name rides along with the join so it needs no follow-up read. Widening it
    // is fine; what must never happen is a REJECTION carrying anything.
    assert_eq!(
        std::mem::size_of::<RedeemOutcome>(),
        std::mem::size_of::<(String, Option<String>)>(),
        "RedeemOutcome must carry a payload only on the Joined path"
    );
}

// ── Rate limiting ────────────────────────────────────────────────────────────

/// The durable per-user bound: after enough failures the actor is cut off, and
/// crucially a VALID token presented afterwards is also refused — otherwise the
/// limiter would be a free oracle confirming when a guess finally landed.
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

    // A VALID token now also fails — the actor is cut off, not the token.
    assert_eq!(
        redeem(&db, &link.token, "att-valid", NOW).await,
        RedeemOutcome::RateLimited
    );
    assert!(!is_member(&db, GROUP, JOINER).await);

    // Outside the window the actor recovers: the bound is a delay, not a ban.
    assert_eq!(
        redeem(&db, &link.token, "att-later", "2026-08-15T13:00:00Z").await,
        joined()
    );
    assert!(is_member(&db, GROUP, JOINER).await);
}

/// Successful redemptions must NOT count toward the failure budget — a busy
/// group's members would otherwise lock each other out.
#[tokio::test(flavor = "multi_thread")]
async fn successful_redemptions_do_not_consume_the_failure_budget() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", None, None).await;

    assert_eq!(
        redeem(&db, &link.token, "att-1", NOW).await,
        joined()
    );
    // Re-redeeming as an existing member is idempotent and still not a failure.
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

    // And the use count moved exactly once, despite six calls.
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

// ── Creation and revocation authorization ────────────────────────────────────

/// Only an admin of the group may mint a link — the same bar as an addressed
/// invite, because a link IS an invite with the addressee left open.
#[tokio::test(flavor = "multi_thread")]
async fn only_an_admin_can_create_a_link() {
    let db = fresh().await;
    let (_, selector, secret_hash) = mint();
    let out = apply_create_invite_link(
        &db.conn().await.unwrap(),
        // A non-member signing as themselves.
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

/// An admin of a DIFFERENT group must not be able to revoke this group's link.
/// The group is resolved from the link row, never from the request.
#[tokio::test(flavor = "multi_thread")]
async fn an_admin_of_another_group_cannot_revoke_this_groups_link() {
    let db = fresh().await;
    let link = create_link(&db, "link-1", None, None).await;

    // `other-1` is an admin — of grp-2.
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

    // The link is still live.
    assert_eq!(
        redeem(&db, &link.token, "att-1", NOW).await,
        joined()
    );
}

/// A client that posts something other than a sha256 digest is refused — that is
/// how a hostile client would plant a hash it can cheaply preimage.
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

/// Revocation is one-way and keeps the original timestamp — re-revoking must not
/// rewrite when the link actually died.
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
