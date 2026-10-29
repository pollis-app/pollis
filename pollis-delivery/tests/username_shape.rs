//! Email-shaped username squatting — the invariant, at every layer it lives.

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

/// Setting one's username to someone's email address is a 400; the row is untouched.
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

/// A legacy unchanged username (`Legacy_7XK2`) may be re-sent; the carve-out never covers `@`.
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

    // A legacy `@` row gets no pass, and the refusal is a 400, not the trigger's 500.
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

/// With no DS code in the path, the schema refuses an `@` on INSERT and UPDATE (`000021`).
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

    // The trigger validates the value; it is not a lock on the column.
    conn.execute(
        "UPDATE users SET username = COALESCE(NULL, username), preferred_name = 'A' WHERE id = 'admin-1'",
        (),
    )
    .await
    .expect("an unchanged conforming username must still save");
}

/// Drop the triggers so a pre-`000021` squatter row can exist.
async fn drop_username_triggers(conn: &libsql::Connection) {
    conn.execute_batch(
        "DROP TRIGGER users_username_no_at_insert; DROP TRIGGER users_username_no_at_update;",
    )
    .await
    .expect("drop triggers");
}

/// Victim's row first, then a squatter whose username is her email: the invite resolves to her.
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

/// A blocked invitee answers exactly as a missing user, so the block is not revealed.
#[tokio::test]
async fn a_block_is_indistinguishable_from_no_such_user() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    conn.execute(
        "INSERT INTO user_block (blocker_id, blocked_id) VALUES (?1, ?2)",
        libsql::params![VICTIM, ADMIN],
    )
    .await
    .expect("seed block");

    let invite = |identifier: &str, id: &str| CreateInviteBody {
        id: id.to_string(),
        group_id: GROUP.to_string(),
        inviter_id: Some(ADMIN.to_string()),
        invitee_identifier: identifier.to_string(),
    };

    let blocked = apply_create_invite(&conn, Some(ADMIN), &invite(VICTIM_EMAIL, "inv-b"))
        .await
        .expect("apply");
    let absent = apply_create_invite(&conn, Some(ADMIN), &invite("nobody@nowhere.test", "inv-n"))
        .await
        .expect("apply");

    assert!(
        matches!(blocked, InviteOutcome::NoSuchUser),
        "a block must answer as a missing user, got {blocked:?}"
    );
    assert!(matches!(absent, InviteOutcome::NoSuchUser), "got {absent:?}");

    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM group_invite WHERE invitee_id = ?1",
            libsql::params![VICTIM],
        )
        .await
        .expect("query");
    let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(n, 0);
}

#[tokio::test]
async fn an_unblocked_invite_is_still_created() {
    let db = fresh().await;
    let conn = db.conn().await.unwrap();
    let outcome = apply_create_invite(
        &conn,
        Some(ADMIN),
        &CreateInviteBody {
            id: "inv-ok".to_string(),
            group_id: GROUP.to_string(),
            inviter_id: Some(ADMIN.to_string()),
            invitee_identifier: VICTIM_EMAIL.to_string(),
        },
    )
    .await
    .expect("apply");
    assert!(matches!(outcome, InviteOutcome::Created { .. }), "got {outcome:?}");
}

/// The directory lookup dispatches on `@`: email matches `email` only, else `username` only.
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

    let squatter_by_own_name = user_by_identifier(&conn, "victim@corp.com").await.expect("lookup");
    assert_ne!(squatter_by_own_name.as_ref().map(|u| u.id.as_str()), Some(MALLORY));

    let squatter_by_email = user_by_identifier(&conn, "mallory@x").await.expect("lookup");
    assert_eq!(squatter_by_email.as_ref().map(|u| u.id.as_str()), Some(MALLORY));

    assert!(user_by_identifier(&conn, "nobody").await.expect("lookup").is_none());
    assert!(user_by_identifier(&conn, "nobody@x").await.expect("lookup").is_none());
}
