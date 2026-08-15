//! Custom per-group emoji (#848) — the rules, pinned.
//!
//! Four properties the ticket is explicit about, each with a test that fails if
//! the property stops holding:
//!
//!   * **The cross-group permission matrix.** A adds an emoji to group A; B (in
//!     group A) may use it in group B; C (only in group B) may NOT use it, but
//!     must still be able to RENDER it. This is Discord's rule verbatim, and it
//!     is the one an implementation gets wrong by scoping "use" to the
//!     conversation instead of to the user's membership set.
//!   * **Dedup.** Fifteen uploads of party-parrot are one stored object. Here:
//!     the same bytes registered by two different users into two different
//!     groups produce two `group_emoji` rows and exactly ONE
//!     `custom_emoji_object`.
//!   * **GC reclaims.** Removing the last reference collects the object; a
//!     deleted group releases its references and the sweep reclaims them,
//!     returning the R2 keys so the blobs go too.
//!   * **Bounds hold.** A per-USER quota (there is deliberately no per-group
//!     cap), a size ceiling the DS re-checks, and — the part that actually
//!     enforces it — a presigned PUT whose signature binds the exact body
//!     length, so R2 refuses an oversized upload the DS never sees.
//!
//! Driven against a local libsql DB exactly as the DS runs, mirroring
//! `attachment_refs.rs`: `apply_*` fns directly for the write rules, and the
//! real router via `tower::oneshot` for the R2 chokepoint.

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

/// Only the tables this domain reads or writes. `group_member` carries `role`
/// because removal authz re-derives admin from it.
const SCHEMA: &str = "\
CREATE TABLE groups (id TEXT PRIMARY KEY, name TEXT NOT NULL DEFAULT 'g');\
CREATE TABLE group_member (\
  group_id TEXT NOT NULL, user_id TEXT NOT NULL, role TEXT NOT NULL DEFAULT 'member', \
  PRIMARY KEY (group_id, user_id));\
/* `message_envelope` + `attachment_ref` are here only so the MEDIA presign path \
   (which consults the attachment reference count) can be exercised alongside \
   the emoji one — the point being that the media presign is UNCHANGED. */\
CREATE TABLE custom_emoji_object (\
  content_hash TEXT PRIMARY KEY, r2_key TEXT NOT NULL, content_type TEXT NOT NULL, \
  size_bytes INTEGER NOT NULL, animated INTEGER NOT NULL DEFAULT 0, \
  created_at TEXT NOT NULL DEFAULT (datetime('now')));\
CREATE TABLE group_emoji (\
  group_id TEXT NOT NULL, shortcode TEXT NOT NULL, content_hash TEXT NOT NULL, \
  created_by TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT (datetime('now')), \
  PRIMARY KEY (group_id, shortcode));\
CREATE INDEX idx_group_emoji_content_hash ON group_emoji(content_hash);\
CREATE INDEX idx_group_emoji_created_by ON group_emoji(created_by);\
CREATE TABLE message_envelope (id TEXT PRIMARY KEY);\
CREATE TABLE attachment_ref (\
  content_hash TEXT NOT NULL, message_id TEXT NOT NULL, PRIMARY KEY (content_hash, message_id));";

async fn fresh() -> Arc<Db> {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("db.db");
    std::mem::forget(dir);
    let db = Db::connect_local(path.to_str().unwrap()).await.expect("local db");
    db.conn().unwrap().execute_batch(SCHEMA).await.expect("schema");
    Arc::new(db)
}

/// A well-formed content hash. Distinct `seed`s give distinct objects.
fn hash(seed: char) -> String {
    seed.to_string().repeat(64)
}

async fn add_group(db: &Db, group: &str) {
    db.conn()
        .unwrap()
        .execute(
            "INSERT OR IGNORE INTO groups (id) VALUES (?1)",
            libsql::params![group.to_string()],
        )
        .await
        .unwrap();
}

async fn add_member(db: &Db, group: &str, user: &str, role: &str) {
    add_group(db, group).await;
    db.conn()
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

/// Add an emoji as `actor` (auth ON — the authorized path).
async fn create_as(
    db: &Db,
    actor: &str,
    group: &str,
    shortcode: &str,
    content_hash: &str,
) -> EmojiOutcome {
    apply_create_emoji(
        &db.conn().unwrap(),
        Some(actor),
        &create_body(group, shortcode, content_hash),
    )
    .await
    .unwrap()
}

async fn remove_as(db: &Db, actor: &str, group: &str, shortcode: &str) -> EmojiOutcome {
    apply_remove_emoji(
        &db.conn().unwrap(),
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
    let conn = db.conn().unwrap();
    let mut rows = conn.query(sql, ()).await.unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
}

async fn object_exists(db: &Db, content_hash: &str) -> bool {
    let conn = db.conn().unwrap();
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
    user_can_use_emoji(&db.conn().unwrap(), user, content_hash)
        .await
        .unwrap()
}

// ── 1. The cross-group permission matrix (the ticket's worked example) ────────

/// **The rule, exactly as #848 states it.**
///
/// > user A adds an emoji to group A; user B (in group A) can send it in group
/// > B; user C (only in group B) cannot use it — but must still render it when
/// > B sends it.
///
/// The whole matrix in one test, because the interesting part is the
/// *relationship* between the three answers, not any one of them. In particular
/// B's `true` is the assertion that would fail under the obvious-but-wrong
/// implementation ("you may use an emoji registered to the conversation you are
/// typing in") — that rule gives B `false` in group B, which is precisely the
/// Discord behaviour the ticket asks for.
#[tokio::test]
async fn cross_group_use_follows_discords_rule() {
    let db = fresh().await;
    let parrot = hash('a');

    // A and B share group A. B and C share group B. A and C share nothing.
    add_member(&db, "group_a", "user_a", "admin").await;
    add_member(&db, "group_a", "user_b", "member").await;
    add_member(&db, "group_b", "user_b", "member").await;
    add_member(&db, "group_b", "user_c", "admin").await;

    // A adds the emoji to group A.
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

    // ...and C must still RENDER it. Rendering consults nothing in this module:
    // the object row is readable and its R2 key resolvable with no membership
    // predicate anywhere in the path, which is exactly why these objects are
    // stored unencrypted (an encrypted emoji is one that cannot cross a group
    // boundary at all — there is no key C could hold).
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

    // Once B adds the same emoji to group B (its bytes are already stored), C
    // gains use rights the same way anyone else would: through membership.
    assert!(matches!(
        create_as(&db, "user_b", "group_b", "parrot", &parrot).await,
        EmojiOutcome::Ok
    ));
    assert!(
        can_use(&db, "user_c", &parrot).await,
        "C is now in a group that registered it, so C may use it"
    );
}

/// A user who is not a member of the target group cannot add an emoji to it.
/// The "use" rule is deliberately generous across groups; the "add" rule is not.
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

/// Removal is asymmetric with creation on purpose: adding costs the adder
/// (quota), removing costs everyone who was using it. So the creator or an
/// admin may remove; a plain member may not remove someone else's.
#[tokio::test]
async fn removal_is_creator_or_admin_only() {
    let db = fresh().await;
    add_member(&db, "g", "creator", "member").await;
    add_member(&db, "g", "bystander", "member").await;
    add_member(&db, "g", "boss", "admin").await;
    create_as(&db, "creator", "g", "parrot", &hash('a')).await;
    create_as(&db, "creator", "g", "shrug", &hash('b')).await;

    assert!(
        matches!(
            remove_as(&db, "bystander", "g", "parrot").await,
            EmojiOutcome::Forbidden
        ),
        "a plain member cannot delete another member's emoji"
    );
    assert!(matches!(
        remove_as(&db, "creator", "g", "parrot").await,
        EmojiOutcome::Ok
    ));
    assert!(
        matches!(remove_as(&db, "boss", "g", "shrug").await, EmojiOutcome::Ok),
        "an admin may remove anyone's"
    );
}

// ── 2. Dedup — "no fifteen party-parrots" ────────────────────────────────────

/// The same bytes registered twice — by two different users, into two different
/// groups — must produce ONE stored object.
///
/// This is a property of the schema, not of a dedup pass: `content_hash` is the
/// primary key of `custom_emoji_object`, and the second registration's `INSERT
/// OR IGNORE` writes nothing. The R2 side follows from the same fact — the
/// uploader finds the hash already present and skips the PUT entirely.
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

    // And the key is DERIVED from the hash, so both registrations necessarily
    // agree on where the bytes live; a client cannot name a different one.
    let conn = db.conn().unwrap();
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

/// Within ONE group a shortcode is unique, and a collision is REPORTED rather
/// than silently swallowed — otherwise a user would believe they had added their
/// image when `:parrot:` still resolves to somebody else's.
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

    // The loser's object must not linger: it has no reference, so the sweep
    // reclaims it rather than leaving a paid-for blob nobody can reach.
    let collected = sweep_unreferenced_emoji(&db.conn().unwrap()).await.unwrap();
    assert_eq!(collected, vec![emoji_r2_key(&hash('b'), "webp")]);
}

// ── 3. GC ────────────────────────────────────────────────────────────────────

/// Removing the LAST reference collects the object; removing a non-last one does
/// not. Same derived-count discipline as attachment refs (#690): the object goes
/// only when no `group_emoji` row names its hash, expressed as one conditional
/// DELETE so two concurrent removes cannot each conclude "still referenced".
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
    assert!(object_is_referenced(&db.conn().unwrap(), &parrot).await.unwrap());

    remove_as(&db, "u1", "g2", "parrot").await;
    assert!(
        !object_exists(&db, &parrot).await,
        "last reference gone → the object is collected"
    );
    assert!(!object_is_referenced(&db.conn().unwrap(), &parrot).await.unwrap());
}

/// A deleted group releases its emoji references, and the sweep then reclaims
/// whatever that orphaned — returning the R2 keys so the blobs go too.
///
/// This is why release and collect are separate operations. The group-delete
/// path knows nothing about emoji objects and should not: it deletes its own
/// rows, and the references disappear as a consequence. Nothing has to remember
/// to decrement a counter, which is the failure mode a maintained refcount has.
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

    // What `apply_delete_group` now does before dropping the group row.
    release_group_emoji(&db.conn().unwrap(), "doomed").await.unwrap();

    let collected = sweep_unreferenced_emoji(&db.conn().unwrap()).await.unwrap();
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

    // The sweep is idempotent: running it again finds nothing.
    assert!(sweep_unreferenced_emoji(&db.conn().unwrap())
        .await
        .unwrap()
        .is_empty());
}

// ── 4. Bounds ────────────────────────────────────────────────────────────────

/// The per-USER quota. #848 forbids a per-group cap, so this is the only thing
/// standing between one account and unbounded rows. The bound is stated in the
/// module as `EMOJI_MAX_PER_USER * EMOJI_MAX_BYTES` (48 MiB) of worst-case
/// storage attributable to one person.
#[tokio::test]
async fn one_user_is_bounded_but_a_group_is_not() {
    let db = fresh().await;
    add_member(&db, "g", "hog", "admin").await;
    add_member(&db, "g", "someone_else", "member").await;

    // Fill the hog's quota directly (the API path is exercised elsewhere; this
    // is about the ceiling, not the write).
    db.conn()
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

/// Everything about the object that a client could lie about is rejected here.
/// The MIME allowlist matters most: the re-encoder emits exactly two types, and
/// accepting a third would mean storing something no decoder in the render path
/// expects.
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
        let out = apply_create_emoji(&db.conn().unwrap(), Some("u1"), &body)
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

// ── The R2 chokepoint, through the real router ───────────────────────────────

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

/// A `put` for an emoji object must declare its exact size, within the ceiling,
/// and that size must end up SIGNED into the URL.
///
/// This is the difference between a bound and a wish. The DS validates
/// `size_bytes` at registration, but registration happens *after* the upload —
/// nothing there could stop a PUT that carried a gigabyte. Signing
/// `content-length` hands the check to R2, the only party that sees the bytes:
/// a body of any other length fails the signature.
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

    // Media presigns are untouched — no declared length, `host` alone signed,
    // byte-identical to before this feature existed.
    let (status, json) = presign(
        &router,
        serde_json::json!({ "operation": "put", "key": "media/abc.enc", "user_id": "u1" }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(json["url"]
        .as_str()
        .unwrap()
        .contains("X-Amz-SignedHeaders=host&"));
}

/// One emoji blob backs every group that registered its hash, and it is
/// UNENCRYPTED and served to anyone — so an ungated `put` would let any
/// authenticated user replace an image that every member of every referencing
/// group renders. The same reference evidence gates `delete`: collecting the
/// blob while a group still uses it would 404 that group's emoji.
///
/// `get` is never gated. That is not an oversight — it is the render half of the
/// #848 rule: a user in none of the registering groups must still be able to
/// fetch the bytes.
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

    // Release the last reference — the object is collectable, and the gate opens
    // for both the delete and a fresh upload of the same hash.
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
