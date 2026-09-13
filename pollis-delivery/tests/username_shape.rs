//! Email-shaped username squatting — the invariant, at every layer it lives.
//!
//! The DS used to write any string as a `username` (only `UNIQUE` applied) and
//! resolve "who is `alice@corp.com`" with `WHERE username = ?1 OR email = ?1`,
//! first row wins. SQLite scans the `username` index term first, so an account
//! that had set its username to a victim's email address was what every invite
//! and DM-start for that address resolved to, whether or not the victim had an
//! account yet — and the MLS Add faithfully admitted the squatter.
//!
//! Three layers now refuse that, and each has a test here that fails if that
//! layer is removed:
//!
//!   * the profile endpoint — `apply_update_profile` refuses a username outside
//!     `^[a-z0-9_.-]{3,32}$` with a 400, `@` included;
//!   * the database — migration `000021`'s triggers refuse an `@` in a username
//!     on INSERT and UPDATE with no pollis-delivery code in the path;
//!   * the resolvers — `directory::user_by_identifier` (behind
//!     `/v1/directory/users` and `apply_create_invite`) matches an identifier
//!     that contains `@` against `email` ONLY, so even a legacy squatter row
//!     that predates the trigger can never be what an email resolves to.
//!
//! Driven at the `apply_*` level against a local libsql DB, mirroring
//! `join_requests.rs`.

use pollis_delivery::db::Db;
use pollis_delivery::directory::user_by_identifier;
use pollis_delivery::groups::{apply_create_invite, CreateInviteBody, InviteOutcome};
use pollis_delivery::profile::{
    apply_update_profile, is_valid_username, ProfileOutcome, UpdateProfileBody, USERNAME_INVALID,
};

mod common;

const GROUP: &str = "grp-1";
const ADMIN: &str = "admin-1";
const VICTIM: &str = "victim-1";
const MALLORY: &str = "mallory-1";
const VICTIM_EMAIL: &str = "victim@corp.com";

async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    let conn = db.conn().await.unwrap();
    pollis_schema::apply::single_db(&conn).await.expect("schema");
    conn.execute_batch(
        "INSERT INTO users (id, email, username) VALUES \
           ('admin-1','admin@x','admin'),\
           ('victim-1','victim@corp.com','victim_1234');\
         INSERT INTO conversation (id, kind) VALUES ('grp-1','group');\
         INSERT INTO groups (id, name, owner_id) VALUES ('grp-1','Group One','admin-1');\
         INSERT INTO group_member (group_id, user_id, role) VALUES ('grp-1','admin-1','admin');",
    )
    .await
    .expect("seed");
    db
}

async fn username_of(db: &Db, user_id: &str) -> String {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT username FROM users WHERE id = ?1",
            libsql::params![user_id.to_string()],
        )
        .await
        .expect("query");
    rows.next().await.expect("row").expect("row").get(0).expect("username")
}

fn profile(user_id: &str, username: Option<&str>, preferred_name: Option<&str>) -> UpdateProfileBody {
    UpdateProfileBody {
        user_id: user_id.to_string(),
        username: username.map(str::to_string),
        preferred_name: preferred_name.map(str::to_string),
        phone: None,
        avatar_url: None,
    }
}

// ── Layer 1: the profile endpoint ────────────────────────────────────────────

/// The attack's first move, refused at the door: setting one's username to
/// someone's email address is a 400, and the row is untouched.
#[tokio::test]
async fn a_username_containing_an_at_sign_is_refused_by_the_profile_endpoint() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    let outcome = apply_update_profile(&conn, Some(ADMIN), &profile(ADMIN, Some(VICTIM_EMAIL), None))
        .await
        .expect("apply");

    assert!(
        matches!(outcome, ProfileOutcome::Invalid(why) if why == USERNAME_INVALID),
        "an email-shaped username must be refused as invalid, got {outcome:?}"
    );
    assert_eq!(username_of(&db, ADMIN).await, "admin", "the row must be untouched");
}

/// The whole character rule, not just `@`: anything outside
/// `^[a-z0-9_.-]{3,32}$` is refused, and a conforming rename lands.
#[tokio::test]
async fn a_username_outside_the_character_rule_is_refused_and_a_conforming_one_lands() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    for bad in ["Admin", "ab", "has space", "tab\tname", "ünïcode", "", "x@y", "with/slash"] {
        let outcome = apply_update_profile(&conn, Some(ADMIN), &profile(ADMIN, Some(bad), None))
            .await
            .expect("apply");
        assert!(
            matches!(outcome, ProfileOutcome::Invalid(_)),
            "{bad:?} must be refused, got {outcome:?}"
        );
    }
    let too_long = "a".repeat(33);
    let outcome = apply_update_profile(&conn, Some(ADMIN), &profile(ADMIN, Some(&too_long), None))
        .await
        .expect("apply");
    assert!(matches!(outcome, ProfileOutcome::Invalid(_)), "33 chars must be refused");
    assert_eq!(username_of(&db, ADMIN).await, "admin", "no refused value may land");

    let outcome = apply_update_profile(&conn, Some(ADMIN), &profile(ADMIN, Some("admin.new-1_x"), None))
        .await
        .expect("apply");
    assert!(matches!(outcome, ProfileOutcome::Ok), "a conforming rename must land, got {outcome:?}");
    assert_eq!(username_of(&db, ADMIN).await, "admin.new-1_x");
}

/// Both clients re-send the current username on every save. An account whose
/// default name predates the rule (`Legacy_7XK2`: upper-case ULID suffix) must
/// still be able to save its display name — an unchanged username introduces no
/// new state. The carve-out never covers `@`.
#[tokio::test]
async fn an_unchanged_legacy_username_still_saves_but_never_one_with_an_at_sign() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT INTO users (id, email, username) VALUES ('legacy-1','legacy@x','Legacy_7XK2')",
        (),
    )
    .await
    .expect("seed legacy");
    assert!(!is_valid_username("Legacy_7XK2"), "the fixture must be a name the rule refuses");

    let outcome = apply_update_profile(
        &conn,
        Some("legacy-1"),
        &profile("legacy-1", Some("Legacy_7XK2"), Some("Legacy Person")),
    )
    .await
    .expect("apply");
    assert!(matches!(outcome, ProfileOutcome::Ok), "an unchanged legacy name must pass, got {outcome:?}");

    // A legacy row holding an `@` (written before 000021) gets no such pass —
    // and the refusal is a 400 from the DS, not the trigger's abort as a 500.
    drop_username_triggers(&conn).await;
    conn.execute(
        "INSERT INTO users (id, email, username) VALUES ('squat-1','squat@x','someone@corp.com')",
        (),
    )
    .await
    .expect("seed pre-migration squatter");
    let outcome = apply_update_profile(
        &conn,
        Some("squat-1"),
        &profile("squat-1", Some("someone@corp.com"), Some("Still Here")),
    )
    .await
    .expect("apply");
    assert!(
        matches!(outcome, ProfileOutcome::Invalid(_)),
        "an unchanged `@` username is never grandfathered, got {outcome:?}"
    );
}

// ── Layer 2: the database ────────────────────────────────────────────────────

/// With no pollis-delivery code in the path, the schema itself refuses an `@`
/// in a username on both INSERT and UPDATE (migration `000021`).
#[tokio::test]
async fn the_database_refuses_an_at_sign_in_a_username_without_the_ds() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();

    let inserted = conn
        .execute(
            "INSERT INTO users (id, email, username) VALUES ('mallory-1','mallory@x','victim@corp.com')",
            (),
        )
        .await;
    assert!(inserted.is_err(), "INSERT of an email-shaped username must be refused by the trigger");

    let updated = conn
        .execute(
            "UPDATE users SET username = 'victim@corp.com' WHERE id = 'admin-1'",
            (),
        )
        .await;
    assert!(updated.is_err(), "UPDATE to an email-shaped username must be refused by the trigger");
    assert_eq!(username_of(&db, ADMIN).await, "admin");

    // The same COALESCE the profile endpoint issues, leaving a conforming
    // username in place, is not caught by the trigger — it is a validator on
    // the value, not a lock on the column.
    conn.execute(
        "UPDATE users SET username = COALESCE(NULL, username), preferred_name = 'A' WHERE id = 'admin-1'",
        (),
    )
    .await
    .expect("an unchanged conforming username must still save");
}

// ── Layer 3: the resolvers ───────────────────────────────────────────────────

/// Legacy data: rows written before `000021` existed. The triggers are dropped
/// so the squatter row can exist at all, which is exactly the state the
/// resolvers must be safe against.
async fn drop_username_triggers(conn: &libsql::Connection) {
    conn.execute_batch(
        "DROP TRIGGER users_username_no_at_insert; DROP TRIGGER users_username_no_at_update;",
    )
    .await
    .expect("drop triggers");
}

/// The finding's exact reproduction: the victim's row exists FIRST, then a
/// squatter whose username equals the victim's email. The old
/// `username = ?1 OR email = ?1` returned the squatter; an invite for the
/// email must resolve to the account whose EMAIL it is.
#[tokio::test]
async fn an_invite_for_an_email_resolves_to_the_account_with_that_email_not_a_squatted_username() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    drop_username_triggers(&conn).await;
    conn.execute(
        "INSERT INTO users (id, email, username) VALUES ('mallory-1','mallory@x','victim@corp.com')",
        (),
    )
    .await
    .expect("seed squatter");

    let outcome = apply_create_invite(
        &conn,
        Some(ADMIN),
        &CreateInviteBody {
            id: "inv-1".to_string(),
            group_id: GROUP.to_string(),
            inviter_id: Some(ADMIN.to_string()),
            invitee_identifier: VICTIM_EMAIL.to_string(),
        },
    )
    .await
    .expect("apply");

    match outcome {
        InviteOutcome::Created { invitee_id, .. } => {
            assert_eq!(invitee_id, VICTIM, "the invite must go to the account whose email this is");
            assert_ne!(invitee_id, MALLORY, "the squatter must never be resolved for an email");
        }
        other => panic!("expected Created, got {other:?}"),
    }

    let mut rows = conn
        .query("SELECT invitee_id FROM group_invite WHERE id = 'inv-1'", ())
        .await
        .expect("query");
    let stored: String = rows.next().await.expect("row").expect("row").get(0).expect("id");
    assert_eq!(stored, VICTIM, "the stored invite row must name the victim, not the squatter");
}

/// The directory lookup (behind `/v1/directory/users`, hence the client's
/// DM-start `search_user_by_username`) dispatches on `@`: an email matches
/// `email` only, a username matches `username` only.
#[tokio::test]
async fn the_directory_lookup_matches_an_email_against_email_only_and_a_username_against_username_only() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    drop_username_triggers(&conn).await;
    conn.execute(
        "INSERT INTO users (id, email, username) VALUES ('mallory-1','mallory@x','victim@corp.com')",
        (),
    )
    .await
    .expect("seed squatter");

    let by_email = user_by_identifier(&conn, VICTIM_EMAIL).await.expect("lookup");
    assert_eq!(
        by_email.as_ref().map(|u| u.id.as_str()),
        Some(VICTIM),
        "an email-shaped identifier must resolve by email, never by a squatted username"
    );

    let by_username = user_by_identifier(&conn, "victim_1234").await.expect("lookup");
    assert_eq!(by_username.as_ref().map(|u| u.id.as_str()), Some(VICTIM));

    // The squatter's own (legacy) username is email-shaped, so it is now
    // unreachable as a username — the identifier goes to the email column,
    // which nobody's row satisfies but the victim's.
    let squatter_by_own_name = user_by_identifier(&conn, "victim@corp.com").await.expect("lookup");
    assert_ne!(squatter_by_own_name.as_ref().map(|u| u.id.as_str()), Some(MALLORY));

    // Their email still resolves them — the squatter is reachable, just not
    // as the victim.
    let squatter_by_email = user_by_identifier(&conn, "mallory@x").await.expect("lookup");
    assert_eq!(squatter_by_email.as_ref().map(|u| u.id.as_str()), Some(MALLORY));

    assert!(user_by_identifier(&conn, "nobody").await.expect("lookup").is_none());
    assert!(user_by_identifier(&conn, "nobody@x").await.expect("lookup").is_none());
}
