//! Custom per-group emoji (#848).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt as _;
use pollis_delivery::db::Db;
use pollis_delivery::emoji::{
    apply_create_emoji, apply_remove_emoji, emoji_r2_key, object_is_referenced,
    release_group_emoji, sweep_unreferenced_emoji, user_can_use_emoji, CreateEmojiBody,
    EmojiOutcome, RemoveEmojiBody, EMOJI_MAX_BYTES, EMOJI_MAX_PER_USER,
};
use pollis_delivery::{build_router_with_state, AppState};
use std::sync::Arc;
use tower::ServiceExt as _;

mod common;


async fn fresh() -> common::TempDb {
    let db = common::TempDb::open("db.db").await;
    pollis_schema::apply::single_db(&db.conn().await.unwrap()).await.expect("schema");
    db
}

fn hash(seed: char) -> String {
    seed.to_string().repeat(64)
}

async fn add_group(db: &Db, group: &str) {
    let conn = db.conn().await.unwrap();
    // Registry claim first: 000017's triggers refuse an ungranted groups row (#948).
    conn.execute(
        "INSERT OR IGNORE INTO conversation (id, kind) VALUES (?1, 'group')",
        libsql::params![group.to_string()],
    )
    .await
    .unwrap();
    conn.execute(
        "INSERT OR IGNORE INTO groups (id) VALUES (?1)",
        libsql::params![group.to_string()],
    )
    .await
    .unwrap();
}

async fn add_member(db: &Db, group: &str, user: &str, role: &str) {
    add_group(db, group).await;
    db.conn().await
        .unwrap()
        .execute(
            "INSERT OR IGNORE INTO group_member (group_id, user_id, role) VALUES (?1, ?2, ?3)",
            libsql::params![group.to_string(), user.to_string(), role.to_string()],
        )
        .await
        .unwrap();
}

fn create_body(group: &str, shortcode: &str, content_hash: &str) -> CreateEmojiBody {
    CreateEmojiBody {
        group_id: group.into(),
        shortcode: shortcode.into(),
        content_hash: content_hash.into(),
        content_type: "image/webp".into(),
        size_bytes: 4_096,
        animated: false,
        created_by: None,
    }
}

async fn create_as(
    db: &Db,
    actor: &str,
    group: &str,
    shortcode: &str,
    content_hash: &str,
) -> EmojiOutcome {
    apply_create_emoji(
        &db.conn().await.unwrap(),
        Some(actor),
        &create_body(group, shortcode, content_hash),
    )
    .await
    .unwrap()
}

async fn remove_as(db: &Db, actor: &str, group: &str, shortcode: &str) -> EmojiOutcome {
    apply_remove_emoji(
        &db.conn().await.unwrap(),
        Some(actor),
        &RemoveEmojiBody {
            group_id: group.into(),
            shortcode: shortcode.into(),
            actor_id: None,
        },
    )
    .await
    .unwrap()
}

async fn count(db: &Db, sql: &str) -> i64 {
    let conn = db.conn().await.unwrap();
    let mut rows = conn.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

async fn object_exists(db: &Db, content_hash: &str) -> bool {
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT 1 FROM custom_emoji_object WHERE content_hash = ?1",
            libsql::params![content_hash.to_string()],
        )
        .await
        .unwrap();
    rows.next().await.unwrap().is_some()
}

async fn can_use(db: &Db, user: &str, content_hash: &str) -> bool {
    user_can_use_emoji(&db.conn().await.unwrap(), user, content_hash)
        .await
        .unwrap()
}

/// #848: use follows the user's memberships, not the conversation; C can't use it but must render it.
#[tokio::test]
async fn cross_group_use_follows_discords_rule() {
    let db = fresh().await;
    let parrot = hash('a');

    add_member(&db, "group_a", "user_a", "admin").await;
    add_member(&db, "group_a", "user_b", "member").await;
    add_member(&db, "group_b", "user_b", "member").await;
    add_member(&db, "group_b", "user_c", "admin").await;

    assert!(matches!(
        create_as(&db, "user_a", "group_a", "parrot", &parrot).await,
        EmojiOutcome::Ok
    ));

    assert!(
        can_use(&db, "user_a", &parrot).await,
        "A registered it in a group A is in — A may use it"
    );
    assert!(
        can_use(&db, "user_b", &parrot).await,
        "B is in group A, so B may use group A's emoji — INCLUDING while typing \
         in group B. The predicate asks about B's membership set, never about \
         the conversation; scoping it to the conversation is the classic wrong \
         implementation and would fail here (#848)"
    );
    assert!(
        !can_use(&db, "user_c", &parrot).await,
        "C is in no group that registered this emoji, so C may NOT use it"
    );

    // Rendering has no membership predicate, which is why emoji are stored unencrypted.
    assert!(
        object_exists(&db, &parrot).await,
        "the object C must render exists and is not scoped to any group"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM custom_emoji_object WHERE r2_key LIKE 'emoji/%'"
        )
        .await,
        1,
        "one plainly-stored object, fetchable by anyone — the render path for C"
    );

    assert!(matches!(
        create_as(&db, "user_b", "group_b", "parrot", &parrot).await,
        EmojiOutcome::Ok
    ));
    assert!(
        can_use(&db, "user_c", &parrot).await,
        "C is now in a group that registered it, so C may use it"
    );
}

/// "Use" is generous across groups; "add" is not.
#[tokio::test]
async fn a_non_member_cannot_add_an_emoji_to_a_group() {
    let db = fresh().await;
    add_member(&db, "group_a", "user_a", "admin").await;

    assert!(
        matches!(
            create_as(&db, "outsider", "group_a", "parrot", &hash('a')).await,
            EmojiOutcome::Forbidden
        ),
        "adding requires current membership of the target group"
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM group_emoji").await, 0);
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM custom_emoji_object").await,
        0,
        "a refused add must not leave the shared object behind either"
    );
}

#[tokio::test]
async fn a_plain_member_can_add_an_emoji() {
    let db = fresh().await;
    add_member(&db, "g", "boss", "admin").await;
    add_member(&db, "g", "pleb", "member").await;

    assert!(
        matches!(
            create_as(&db, "pleb", "g", "parrot", &hash('a')).await,
            EmojiOutcome::Ok
        ),
        "a non-admin member must be able to add an emoji"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM group_emoji WHERE group_id = 'g' AND shortcode = 'parrot' \
             AND created_by = 'pleb'"
        )
        .await,
        1
    );
    assert!(object_exists(&db, &hash('a')).await);
}

/// #1280: removal is admin-only, even for the creator; a refused remove changes nothing.
#[tokio::test]
async fn removal_is_admin_only_even_for_the_creator() {
    let db = fresh().await;
    add_member(&db, "g", "creator", "member").await;
    add_member(&db, "g", "bystander", "member").await;
    add_member(&db, "g", "boss", "admin").await;
    create_as(&db, "creator", "g", "parrot", &hash('a')).await;
    create_as(&db, "boss", "g", "shrug", &hash('b')).await;

    assert!(
        matches!(
            remove_as(&db, "bystander", "g", "parrot").await,
            EmojiOutcome::Forbidden
        ),
        "a plain member cannot delete another member's emoji"
    );
    assert!(
        matches!(
            remove_as(&db, "creator", "g", "parrot").await,
            EmojiOutcome::Forbidden
        ),
        "a plain member cannot delete their own emoji either"
    );
    assert!(
        matches!(
            remove_as(&db, "creator", "g", "shrug").await,
            EmojiOutcome::Forbidden
        ),
        "nor an admin's"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM group_emoji WHERE group_id = 'g'").await,
        2,
        "refused removes must change nothing"
    );
    assert!(object_exists(&db, &hash('a')).await);
    assert!(object_exists(&db, &hash('b')).await);

    assert!(
        matches!(remove_as(&db, "boss", "g", "parrot").await, EmojiOutcome::Ok),
        "an admin may remove a member's emoji"
    );
    assert!(
        matches!(remove_as(&db, "boss", "g", "shrug").await, EmojiOutcome::Ok),
        "and their own"
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM group_emoji").await, 0);
    assert!(
        !object_exists(&db, &hash('a')).await,
        "the admin's remove of the last reference collects the object"
    );
}

/// The role is re-derived per request, never cached.
#[tokio::test]
async fn a_demoted_admin_can_no_longer_remove() {
    let db = fresh().await;
    add_member(&db, "g", "ex_boss", "admin").await;
    create_as(&db, "ex_boss", "g", "parrot", &hash('a')).await;
    db.conn()
        .await
        .unwrap()
        .execute(
            "UPDATE group_member SET role = 'member' WHERE group_id = 'g' AND user_id = 'ex_boss'",
            (),
        )
        .await
        .unwrap();

    assert!(matches!(
        remove_as(&db, "ex_boss", "g", "parrot").await,
        EmojiOutcome::Forbidden
    ));
    assert_eq!(count(&db, "SELECT COUNT(*) FROM group_emoji").await, 1);
}

/// L3: membership is checked before the row is read, so a non-member can't probe shortcodes.
#[tokio::test]
async fn a_non_member_cannot_tell_an_existing_emoji_from_a_missing_one() {
    let db = fresh().await;
    add_member(&db, "g", "creator", "member").await;
    create_as(&db, "creator", "g", "parrot", &hash('a')).await;

    let present = remove_as(&db, "outsider", "g", "parrot").await;
    let absent = remove_as(&db, "outsider", "g", "no_such_code").await;
    assert!(
        matches!(present, EmojiOutcome::Forbidden),
        "got {present:?} for an existing shortcode"
    );
    assert!(
        matches!(absent, EmojiOutcome::Forbidden),
        "got {absent:?} for a missing one — a non-member must not be able to \
         tell the two apart"
    );

    assert_eq!(count(&db, "SELECT COUNT(*) FROM group_emoji").await, 1);
}

/// An admin's retried remove of a missing shortcode stays `Ok`.
#[tokio::test]
async fn an_admin_still_gets_an_idempotent_ok_for_a_missing_shortcode() {
    let db = fresh().await;
    add_member(&db, "g", "boss", "admin").await;
    add_member(&db, "g", "bystander", "member").await;
    assert!(matches!(
        remove_as(&db, "boss", "g", "no_such_code").await,
        EmojiOutcome::Ok
    ));
    assert!(matches!(
        remove_as(&db, "bystander", "g", "no_such_code").await,
        EmojiOutcome::Forbidden
    ));
}

/// `content_hash` is the primary key, so the second `INSERT OR IGNORE` writes nothing.
#[tokio::test]
async fn the_same_bytes_twice_are_one_stored_object() {
    let db = fresh().await;
    let parrot = hash('a');
    add_member(&db, "g1", "u1", "admin").await;
    add_member(&db, "g2", "u2", "admin").await;

    create_as(&db, "u1", "g1", "parrot", &parrot).await;
    create_as(&db, "u2", "g2", "party_parrot", &parrot).await;

    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM group_emoji").await,
        2,
        "two groups use it, under two different shortcodes"
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM custom_emoji_object").await,
        1,
        "…backed by exactly ONE stored object — no fifteen party-parrots (#848)"
    );

    // The key derives from the hash, so a client cannot name another.
    let conn = db.conn().await.unwrap();
    let mut rows = conn
        .query(
            "SELECT r2_key FROM custom_emoji_object WHERE content_hash = ?1",
            libsql::params![parrot.clone()],
        )
        .await
        .unwrap();
    let stored: String = rows.next().await.unwrap().unwrap().get(0).unwrap();
    assert_eq!(stored, emoji_r2_key(&parrot, "webp"));
}

/// A shortcode collision is reported, not swallowed.
#[tokio::test]
async fn a_colliding_shortcode_is_reported_not_swallowed() {
    let db = fresh().await;
    add_member(&db, "g", "u1", "admin").await;
    add_member(&db, "g", "u2", "member").await;

    create_as(&db, "u1", "g", "parrot", &hash('a')).await;
    assert!(matches!(
        create_as(&db, "u2", "g", "parrot", &hash('b')).await,
        EmojiOutcome::ShortcodeTaken
    ));

    let collected = sweep_unreferenced_emoji(&db.conn().await.unwrap()).await.unwrap();
    assert_eq!(collected, vec![emoji_r2_key(&hash('b'), "webp")]);
}

/// One conditional DELETE, so two concurrent removes can't both see "still referenced".
#[tokio::test]
async fn gc_reclaims_an_unreferenced_emoji_and_only_then() {
    let db = fresh().await;
    let parrot = hash('a');
    add_member(&db, "g1", "u1", "admin").await;
    add_member(&db, "g2", "u1", "admin").await;
    create_as(&db, "u1", "g1", "parrot", &parrot).await;
    create_as(&db, "u1", "g2", "parrot", &parrot).await;

    remove_as(&db, "u1", "g1", "parrot").await;
    assert!(
        object_exists(&db, &parrot).await,
        "g2 still uses it — collecting here would break g2's emoji everywhere it \
         has already been sent"
    );
    assert!(object_is_referenced(&db.conn().await.unwrap(), &parrot).await.unwrap());

    remove_as(&db, "u1", "g2", "parrot").await;
    assert!(
        !object_exists(&db, &parrot).await,
        "last reference gone → the object is collected"
    );
    assert!(!object_is_referenced(&db.conn().await.unwrap(), &parrot).await.unwrap());
}

/// Group delete only drops its rows; the sweep reclaims orphans and returns R2 keys.
#[tokio::test]
async fn deleting_a_group_releases_its_emoji_and_the_sweep_reclaims_them() {
    let db = fresh().await;
    let solo = hash('a');
    let shared = hash('b');
    add_member(&db, "doomed", "u1", "admin").await;
    add_member(&db, "survivor", "u1", "admin").await;

    create_as(&db, "u1", "doomed", "solo", &solo).await;
    create_as(&db, "u1", "doomed", "shared", &shared).await;
    create_as(&db, "u1", "survivor", "shared", &shared).await;

    release_group_emoji(&db.conn().await.unwrap(), "doomed").await.unwrap();

    let collected = sweep_unreferenced_emoji(&db.conn().await.unwrap()).await.unwrap();
    assert_eq!(
        collected,
        vec![emoji_r2_key(&solo, "webp")],
        "only the object nobody references any more is collected, and its R2 key \
         is returned so the blob is deleted too"
    );
    assert!(
        object_exists(&db, &shared).await,
        "the survivor group still uses the shared object — it must not be collected"
    );

    assert!(sweep_unreferenced_emoji(&db.conn().await.unwrap())
        .await
        .unwrap()
        .is_empty());
}

/// #848 forbids a per-group cap, so the per-user quota is the only bound.
#[tokio::test]
async fn one_user_is_bounded_but_a_group_is_not() {
    let db = fresh().await;
    add_member(&db, "g", "hog", "admin").await;
    add_member(&db, "g", "someone_else", "member").await;

    db.conn().await
        .unwrap()
        .execute(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ?1) \
             INSERT INTO group_emoji (group_id, shortcode, content_hash, created_by) \
             SELECT 'g', 'e' || i, 'h' || i, 'hog' FROM n",
            libsql::params![EMOJI_MAX_PER_USER],
        )
        .await
        .unwrap();

    assert!(
        matches!(
            create_as(&db, "hog", "g", "one_more", &hash('a')).await,
            EmojiOutcome::QuotaExceeded
        ),
        "one user cannot keep adding for ever"
    );
    assert!(
        matches!(
            create_as(&db, "someone_else", "g", "one_more", &hash('a')).await,
            EmojiOutcome::Ok
        ),
        "…and the GROUP is not capped — another member adds freely, which is \
         exactly what #848 requires (no per-group upload limit)"
    );
}

/// Only the re-encoder's two MIME types are allowed.
#[tokio::test]
async fn a_malformed_registration_is_refused() {
    let db = fresh().await;
    add_member(&db, "g", "u1", "admin").await;

    #[allow(clippy::type_complexity)]
    let cases: Vec<(&str, Box<dyn Fn(&mut CreateEmojiBody)>)> = vec![
        (
            "uppercase shortcode",
            Box::new(|b: &mut CreateEmojiBody| b.shortcode = "Parrot".into()),
        ),
        (
            "markup in the shortcode",
            Box::new(|b: &mut CreateEmojiBody| b.shortcode = "<script>".into()),
        ),
        (
            "short hash",
            Box::new(|b: &mut CreateEmojiBody| b.content_hash = "abc".into()),
        ),
        (
            "svg is not an allowed type",
            Box::new(|b: &mut CreateEmojiBody| b.content_type = "image/svg+xml".into()),
        ),
        (
            "png is not an allowed type (the encoder never emits it)",
            Box::new(|b: &mut CreateEmojiBody| b.content_type = "image/png".into()),
        ),
        (
            "oversized",
            Box::new(|b: &mut CreateEmojiBody| b.size_bytes = EMOJI_MAX_BYTES + 1),
        ),
        ("empty", Box::new(|b: &mut CreateEmojiBody| b.size_bytes = 0)),
    ];

    for (label, mutate) in cases {
        let mut body = create_body("g", "parrot", &hash('a'));
        mutate(&mut body);
        let out = apply_create_emoji(&db.conn().await.unwrap(), Some("u1"), &body)
            .await
            .unwrap();
        assert!(
            matches!(out, EmojiOutcome::Invalid(_)),
            "{label} must be refused, got {out:?}"
        );
    }
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM custom_emoji_object").await,
        0,
        "no refused registration may leave an object behind"
    );
}

fn r2_broker() -> pollis_delivery::broker::BrokerConfig {
    pollis_delivery::broker::BrokerConfig {
        r2_endpoint: Some("https://acct.r2.cloudflarestorage.com".into()),
        r2_region: "auto".into(),
        r2_bucket: Some("pollis-media".into()),
        r2_access_key_id: Some("AKIAEXAMPLE".into()),
        r2_secret_access_key: Some("secret-do-not-log".into()),
        ..Default::default()
    }
}

async fn presign(
    router: &axum::Router,
    body: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let req = Request::builder()
        .method("POST")
        .uri("/v1/r2/presign")
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// R2 enforces the size: a signed `content-length` fails any other body length.
#[tokio::test]
async fn an_emoji_put_presign_binds_the_exact_body_length() {
    let db = fresh().await;
    let state = AppState::new(Arc::clone(&db), false).with_broker_config(r2_broker());
    let router = build_router_with_state(state);
    let key = emoji_r2_key(&hash('a'), "webp");

    let (status, _) = presign(
        &router,
        serde_json::json!({ "operation": "put", "key": key, "user_id": "u1" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "an emoji put with no declared length must be refused — an unbounded \
         presign is an unbounded object"
    );

    let (status, _) = presign(
        &router,
        serde_json::json!({
            "operation": "put", "key": key, "user_id": "u1",
            "content_length": EMOJI_MAX_BYTES + 1,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "over the ceiling is refused");

    let (status, json) = presign(
        &router,
        serde_json::json!({
            "operation": "put", "key": key, "user_id": "u1", "content_length": 4096,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let url = json["url"].as_str().unwrap();
    assert!(
        url.contains("X-Amz-SignedHeaders=content-length%3Bhost"),
        "the length must be part of the SIGNATURE, not a hint: {url}"
    );

    // Media puts are length-signed too: an unbounded presign is an unbounded object.
    let media_key = format!("media/{}.enc", "d".repeat(64));
    let (status, _) = presign(
        &router,
        serde_json::json!({ "operation": "put", "key": media_key, "user_id": "u1" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a media put with no declared length must be refused too"
    );
    let (status, json) = presign(
        &router,
        serde_json::json!({
            "operation": "put", "key": media_key, "user_id": "u1", "content_length": 4096,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["url"]
        .as_str()
        .unwrap()
        .contains("X-Amz-SignedHeaders=content-length%3Bhost"));
}

/// Emoji blobs are public and shared, so `put` and `delete` are reference-gated; `get` never is.
#[tokio::test]
async fn emoji_put_and_delete_presigns_are_refused_while_referenced() {
    let db = fresh().await;
    let parrot = hash('a');
    let key = emoji_r2_key(&parrot, "webp");
    add_member(&db, "g", "u1", "admin").await;
    create_as(&db, "u1", "g", "parrot", &parrot).await;

    let state = AppState::new(Arc::clone(&db), false).with_broker_config(r2_broker());
    let router = build_router_with_state(state);

    let (status, _) = presign(
        &router,
        serde_json::json!({
            "operation": "put", "key": key, "user_id": "attacker", "content_length": 4096,
        }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "overwriting a live shared emoji must be refused — the bytes are already \
         there, so a legitimate uploader has nothing to write"
    );

    let (status, _) = presign(
        &router,
        serde_json::json!({ "operation": "delete", "key": key, "user_id": "attacker" }),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "refused while referenced");

    let (status, _) = presign(
        &router,
        serde_json::json!({ "operation": "get", "key": key, "user_id": "stranger" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a GET is never reference-gated or membership-gated: this is the render \
         path for a user who is in none of the registering groups (#848)"
    );

    remove_as(&db, "u1", "g", "parrot").await;
    for op in ["delete", "put"] {
        let (status, _) = presign(
            &router,
            serde_json::json!({
                "operation": op, "key": key, "user_id": "u1", "content_length": 4096,
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{op} is permitted once unreferenced");
    }
}
