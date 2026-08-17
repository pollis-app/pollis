//! Domain F — custom per-group emoji (#848).
//!
//! Follows domain B ([`crate::groups`]) verbatim: one `*Body` per endpoint, a
//! pure `apply_*(conn, authed, body)` that embeds BOTH the authorization
//! decision and the write, and a thin axum handler that gates, parses, and maps
//! the outcome. The handler and the tests call the same `apply_*`, so nothing is
//! authorized twice or authorized only in one of them.
//!
//! ## The storage model in one paragraph
//!
//! A custom emoji is a **content-addressed object** (`custom_emoji_object`,
//! keyed by SHA-256 of the re-encoded bytes) plus **one row per group that uses
//! it** (`group_emoji`). Fifteen groups adding party-parrot produce fifteen rows
//! and one object — the "no fifteen party-parrots" requirement is a consequence
//! of the schema, not of a dedup pass. The object is collectable exactly when no
//! `group_emoji` row names its hash; that count is DERIVED (see
//! [`object_is_referenced`]), so releasing a reference is simply the
//! disappearance of the row, and no caller can forget to decrement.
//!
//! ## What this module refuses, and why
//!
//! Everything a client sends about an object is either re-derived or bounded
//! here, because the DS is the only chokepoint that sees every upload:
//!
//!   - **`r2_key` is not accepted.** It is COMPUTED from `(content_hash,
//!     content_type)`. A client that could name the key could register its hash
//!     against somebody else's object.
//!   - **`content_type` is a two-value allowlist** (`image/webp`, `image/gif`) —
//!     the only two the re-encoder emits. Not the browser's declared MIME, and
//!     not anything a decoder would have to guess at.
//!   - **`size_bytes` must be `1..=EMOJI_MAX_BYTES`.** Paired with the
//!     content-length-bound presign in [`crate::broker`], this is what actually
//!     stops a runaway object: the presigned PUT is signed over the exact byte
//!     count the client declared here, so R2 itself rejects a body of any other
//!     size.
//!   - **`shortcode` must match `[a-z0-9_]{2,32}`.** A shortcode is echoed into
//!     rendered message text; a charset with no `<`, `&`, `:` or whitespace
//!     cannot carry markup or break the `<:name:hash>` token grammar.
//!   - **A creator may hold at most [`EMOJI_MAX_PER_USER`] rows** across every
//!     group they are in. There is deliberately NO per-group cap (#848 is
//!     explicit about that); the bound is per-PERSON, which is the party whose
//!     cost you can actually attribute and the one an attacker has to pay for.
//!
//! ## Permission rule (Discord's, exactly)
//!
//! *Adding* an emoji to a group requires current membership of that group.
//! *Using* one is broader: a user may use any emoji registered to any group they
//! are currently a member of, in ANY conversation — and a recipient who is in
//! none of those groups still RENDERS it. [`user_can_use_emoji`] is the single
//! predicate for the "use" half; [`pollis_core`'s `commands::emoji`] enforces it
//! at composition time (the DS cannot enforce it at send time — message content
//! is E2EE, so the DS never learns which emoji a message carries, which is the
//! point of the product). Rendering is unauthorized by design: the object is
//! public-ish and unencrypted precisely so a non-member can fetch it.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
    Json,
};
use libsql::Connection;

use crate::error::{AppError, AuthRejection};
use crate::writes::{bad_request, gate, ok_json, resolve_actor, WriteOutcome};
use crate::AppState;

// The request bodies for this module's endpoints live in `pollis-api`, the
// crate pollis-core builds its requests from — one declaration, both ends, so
// a client field that does not exist here is a compile error rather than a
// silently-absent JSON key. Re-exported so `pollis_delivery::emoji::*Body`
// keeps resolving for handlers, tests and the flows harness.
pub use pollis_api::emoji::*;

// ── Bounds ───────────────────────────────────────────────────────────────────

/// Hard ceiling on ONE stored emoji object, in bytes.
///
/// Deliberately tiny. `pollis-core`'s re-encoder treats this as a postcondition
/// — it degrades resolution and frame count until the output fits, and fails
/// rather than emit something larger — so this number is not a hope, it is the
/// contract. The DS re-checks it here (a client could lie) and
/// [`crate::broker`] binds it into the presigned PUT's signature, so R2 rejects
/// an oversized body without the DS ever seeing the bytes.
pub const EMOJI_MAX_BYTES: u64 = 48 * 1024;

/// Hard ceiling on how many `group_emoji` rows ONE creator may hold, across
/// every group.
///
/// #848 forbids a per-GROUP cap, so this is where "no runaway file counts" is
/// actually enforced. The storage story falls out of the two numbers: a single
/// user can be responsible for at most `EMOJI_MAX_PER_USER` rows and therefore
/// at most `EMOJI_MAX_PER_USER * EMOJI_MAX_BYTES` = 48 MiB of objects — and
/// usually far less, because dedup means their second copy of a popular emoji
/// costs a row and no bytes at all.
pub const EMOJI_MAX_PER_USER: i64 = 1000;

/// Shortest / longest permitted shortcode.
const SHORTCODE_MIN: usize = 2;
const SHORTCODE_MAX: usize = 32;

// ── Validation helpers (pure — unit-tested directly) ─────────────────────────

/// `[a-z0-9_]{2,32}`. Hand-rolled because `pollis-delivery` deliberately carries
/// no regex dependency, and because the rule is small enough that a regex would
/// obscure it.
pub fn shortcode_is_valid(shortcode: &str) -> bool {
    let len = shortcode.len();
    if !(SHORTCODE_MIN..=SHORTCODE_MAX).contains(&len) {
        return false;
    }
    shortcode
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A SHA-256 digest in the exact form the rest of the system writes it:
/// 64 LOWERCASE hex characters. Case matters because the hash is a primary key
/// and an R2 key stem — accepting both cases would let one image occupy two
/// rows and two blobs, which is precisely the dedup failure this feature exists
/// to avoid.
pub fn content_hash_is_valid(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The file extension for an allowed emoji content type. `None` rejects the
/// type outright — this doubles as the content-type allowlist.
pub fn emoji_ext_for_content_type(content_type: &str) -> Option<&'static str> {
    match content_type {
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

/// The R2 key for an emoji object. DERIVED, never accepted from a client — see
/// the module docs. Public because `pollis-core` must compute the identical key
/// to upload to, and one function is how the two stay in step.
pub fn emoji_r2_key(content_hash: &str, ext: &str) -> String {
    format!("emoji/{content_hash}.{ext}")
}

/// The content hash embedded in an emoji R2 key (`emoji/<hash>.<ext>`), or
/// `None` for any key not of that shape. Used by [`crate::broker`] to route a
/// presign through the emoji reference gate.
pub fn content_hash_from_emoji_key(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("emoji/")?;
    let (hash, ext) = rest.rsplit_once('.')?;
    if !content_hash_is_valid(hash) || !matches!(ext, "webp" | "gif") {
        return None;
    }
    Some(hash)
}

// ── Outcomes ─────────────────────────────────────────────────────────────────

/// The result of an emoji write. Wider than [`WriteOutcome`] because this domain
/// has two failure modes that genuinely are not "forbidden": a malformed
/// shortcode is the caller's mistake (400), and a quota refusal is a conflict
/// with state rather than a permission problem (409). Collapsing either into 403
/// would tell a user to go ask an admin for a permission that does not exist.
#[derive(Debug)]
pub enum EmojiOutcome {
    Ok,
    /// The actor may not do this — not a member, or not the owner/admin.
    Forbidden,
    /// The body is malformed. The `&'static str` is safe to return verbatim (no
    /// user input is interpolated into it).
    Invalid(&'static str),
    /// The actor is at [`EMOJI_MAX_PER_USER`].
    QuotaExceeded,
    /// The `(group_id, shortcode)` pair is already taken.
    ShortcodeTaken,
}

fn emoji_outcome_response(outcome: EmojiOutcome) -> Result<Response, AppError> {
    Ok(match outcome {
        EmojiOutcome::Ok => ok_json(serde_json::json!({ "status": "ok" })),
        EmojiOutcome::Forbidden => AuthRejection::Forbidden.into_response(),
        EmojiOutcome::Invalid(why) => bad_request(why),
        EmojiOutcome::QuotaExceeded => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({
                "error": "emoji quota exceeded",
                "limit": EMOJI_MAX_PER_USER,
            })),
        )
            .into_response(),
        EmojiOutcome::ShortcodeTaken => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "shortcode already used in this group" })),
        )
            .into_response(),
    })
}

// ── Shared predicates ────────────────────────────────────────────────────────

/// True when at least one `group_emoji` row still names `content_hash`.
///
/// The single source of truth for BOTH the conditional collect in
/// [`apply_remove_emoji`] / [`sweep_unreferenced_emoji`] and the R2
/// `put`/`delete` presign gates in [`crate::broker`]. An object may be
/// collected — in Turso or in R2 — only when this is `false`. Mirrors
/// `messages::object_is_referenced` deliberately: same shape, same guarantee,
/// so a reader who understands attachment refcounting already understands this.
pub async fn object_is_referenced(conn: &Connection, content_hash: &str) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT EXISTS (SELECT 1 FROM group_emoji WHERE content_hash = ?1)",
            libsql::params![content_hash.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get::<i64>(0)? != 0,
        None => false,
    })
}

/// Collect the shared object iff NO group still references it. ONE conditional
/// `DELETE`, no Rust-side count, so the row cannot go while a referencing
/// `group_emoji` row survives even under concurrent removes (CLAUDE.md "invalid
/// states unrepresentable").
const COLLECT_UNREFERENCED_EMOJI_SQL: &str = "\
DELETE FROM custom_emoji_object \
 WHERE content_hash = ?1 \
   AND NOT EXISTS (SELECT 1 FROM group_emoji WHERE content_hash = ?1)";

/// **The permission rule (#848).** True when `user_id` may USE the custom emoji
/// `content_hash` — i.e. it is registered to at least one group the user is a
/// current member of.
///
/// The worked example from the ticket, and what this returns for each party:
/// user A adds the emoji to group A → `true` for A. User B is also in group A →
/// `true` for B, *including when B is typing in group B*, because the predicate
/// asks about the user's membership set, never about the conversation. User C is
/// only in group B → `false`; C cannot send it. C still renders it when B does,
/// because rendering fetches an unencrypted, ungated object and never consults
/// this function.
///
/// Note what is deliberately absent: any conversation argument. Scoping "use" to
/// the conversation is the obvious-looking rule and it is the wrong one — it is
/// exactly the Discord behaviour #848 asks for that it would break.
pub async fn user_can_use_emoji(
    conn: &Connection,
    user_id: &str,
    content_hash: &str,
) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT EXISTS (\
               SELECT 1 FROM group_emoji ge \
               JOIN group_member gm ON gm.group_id = ge.group_id \
               WHERE ge.content_hash = ?1 AND gm.user_id = ?2)",
            libsql::params![content_hash.to_string(), user_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get::<i64>(0)? != 0,
        None => false,
    })
}

/// How many `group_emoji` rows `user_id` has created, across every group.
async fn creator_row_count(conn: &Connection, user_id: &str) -> anyhow::Result<i64> {
    let mut rows = conn
        .query(
            "SELECT COUNT(*) FROM group_emoji WHERE created_by = ?1",
            libsql::params![user_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get::<i64>(0)?,
        None => 0,
    })
}

// Membership / admin come from [`crate::groups`] — this module used to keep its
// own pair, and the copy's `row.get::<String>(0).ok()` turned a decode error on
// the `role` column into `false` (a silent deny) where the original propagates
// it. #875 deleted the copy rather than fixing it twice.
use crate::groups::{is_admin as is_group_admin, is_member as is_group_member};

// ── POST /v1/emoji/create ────────────────────────────────────────────────────

pub async fn create_emoji(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: CreateEmojiBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    emoji_outcome_response(apply_create_emoji(&conn, authed.as_deref(), &parsed).await?)
}

/// Register the shared object (idempotent) and bind it to `(group_id,
/// shortcode)`.
///
/// Authz: the actor is a current MEMBER of the group. Not admin — #848 wants
/// members to be able to add emoji, and the cost is bounded per-person by
/// [`EMOJI_MAX_PER_USER`] rather than by gatekeeping. Removal is where the
/// admin/owner distinction lives.
///
/// The object INSERT is `OR IGNORE`: it is content-addressed and byte-identical
/// for every uploader, so the second person to add party-parrot writes no object
/// row and (having found the hash already present) uploaded no bytes either. The
/// `group_emoji` INSERT is NOT `OR IGNORE` — a colliding `(group, shortcode)`
/// must be reported, not silently swallowed, or a user would think they had
/// added an emoji that is actually someone else's image.
pub async fn apply_create_emoji(
    conn: &Connection,
    authed: Option<&str>,
    body: &CreateEmojiBody,
) -> anyhow::Result<EmojiOutcome> {
    let actor = match resolve_actor(authed, body.created_by.as_deref()) {
        Ok(a) => a,
        Err(_) => return Ok(EmojiOutcome::Forbidden),
    };

    // Shape first — a malformed body is never a permission question.
    if body.group_id.trim().is_empty() {
        return Ok(EmojiOutcome::Invalid("group_id required"));
    }
    if !shortcode_is_valid(&body.shortcode) {
        return Ok(EmojiOutcome::Invalid(
            "shortcode must be 2-32 chars of [a-z0-9_]",
        ));
    }
    if !content_hash_is_valid(&body.content_hash) {
        return Ok(EmojiOutcome::Invalid(
            "content_hash must be 64 lowercase hex chars",
        ));
    }
    let Some(ext) = emoji_ext_for_content_type(&body.content_type) else {
        return Ok(EmojiOutcome::Invalid(
            "content_type must be image/webp or image/gif",
        ));
    };
    if body.size_bytes == 0 || body.size_bytes > EMOJI_MAX_BYTES {
        return Ok(EmojiOutcome::Invalid("size_bytes out of range"));
    }

    // Authz: current member of the target group. Skipped on the no-auth path,
    // mirroring every other domain.
    if authed.is_some() && !is_group_member(conn, &body.group_id, &actor).await? {
        return Ok(EmojiOutcome::Forbidden);
    }

    // Per-creator quota. Checked before the write, and again implicitly by the
    // PK on retry — a racing pair of requests can at worst overshoot by the
    // number of concurrent requests, which is a bound, not a leak.
    if creator_row_count(conn, &actor).await? >= EMOJI_MAX_PER_USER {
        return Ok(EmojiOutcome::QuotaExceeded);
    }

    // The key is DERIVED. Nothing the client sent decides where the bytes live.
    let r2_key = emoji_r2_key(&body.content_hash, ext);

    let tx = conn.transaction().await?;
    tx.execute(
        "INSERT OR IGNORE INTO custom_emoji_object \
           (content_hash, r2_key, content_type, size_bytes, animated) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        libsql::params![
            body.content_hash.clone(),
            r2_key,
            body.content_type.clone(),
            body.size_bytes as i64,
            i64::from(body.animated)
        ],
    )
    .await?;
    let inserted = tx
        .execute(
            "INSERT OR IGNORE INTO group_emoji (group_id, shortcode, content_hash, created_by) \
             VALUES (?1, ?2, ?3, ?4)",
            libsql::params![
                body.group_id.clone(),
                body.shortcode.clone(),
                body.content_hash.clone(),
                actor.clone()
            ],
        )
        .await?;
    tx.commit().await?;

    if inserted == 0 {
        return Ok(EmojiOutcome::ShortcodeTaken);
    }
    Ok(EmojiOutcome::Ok)
}

// ── POST /v1/emoji/remove ────────────────────────────────────────────────────

pub async fn remove_emoji(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    let authed = match gate(&state, &headers, &method, &uri, &body).await? {
        Ok(a) => a,
        Err(resp) => return Ok(resp),
    };
    let parsed: RemoveEmojiBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    emoji_outcome_response(apply_remove_emoji(&conn, authed.as_deref(), &parsed).await?)
}

/// Remove `(group_id, shortcode)` and collect the shared object if that was its
/// last reference.
///
/// Authz: the row's creator, or an admin of the group. A plain member cannot
/// delete someone else's emoji — that asymmetry with `create` is deliberate:
/// adding costs the adder (quota), removing costs everyone who was using it.
pub async fn apply_remove_emoji(
    conn: &Connection,
    authed: Option<&str>,
    body: &RemoveEmojiBody,
) -> anyhow::Result<EmojiOutcome> {
    let actor = match resolve_actor(authed, body.actor_id.as_deref()) {
        Ok(a) => a,
        Err(_) => return Ok(EmojiOutcome::Forbidden),
    };
    if !shortcode_is_valid(&body.shortcode) {
        return Ok(EmojiOutcome::Invalid(
            "shortcode must be 2-32 chars of [a-z0-9_]",
        ));
    }

    // Read the row first: we need its hash to run the conditional collect, and
    // its creator to authorize the delete.
    let mut rows = conn
        .query(
            "SELECT content_hash, created_by FROM group_emoji \
             WHERE group_id = ?1 AND shortcode = ?2",
            libsql::params![body.group_id.clone(), body.shortcode.clone()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        // Already gone. Idempotent — a retried remove is a success, not a 404.
        return Ok(EmojiOutcome::Ok);
    };
    let content_hash: String = row.get(0)?;
    let created_by: String = row.get(1)?;

    if authed.is_some()
        && created_by != actor
        && !is_group_admin(conn, &body.group_id, &actor).await?
    {
        return Ok(EmojiOutcome::Forbidden);
    }

    let tx = conn.transaction().await?;
    tx.execute(
        "DELETE FROM group_emoji WHERE group_id = ?1 AND shortcode = ?2",
        libsql::params![body.group_id.clone(), body.shortcode.clone()],
    )
    .await?;
    // Same transaction as the release, so the collect sees the post-release
    // state and two concurrent removes cannot each conclude "still referenced".
    tx.execute(
        COLLECT_UNREFERENCED_EMOJI_SQL,
        libsql::params![content_hash.clone()],
    )
    .await?;
    tx.commit().await?;
    Ok(EmojiOutcome::Ok)
}

// ── POST /v1/emoji/gc ────────────────────────────────────────────────────────

/// POST /v1/emoji/gc — collect EVERY `custom_emoji_object` no group references
/// any more, and return the R2 keys that were collected so the caller can delete
/// the blobs.
///
/// Event-driven, never on a timer (the repo bans periodic polling). The remove
/// path already collects the object it just released; this sweep exists for the
/// references that are dropped WITHOUT going through it — a deleted group takes
/// its `group_emoji` rows with it, and nothing in that path knows or should know
/// about emoji objects. Any authenticated device may run it: it is idempotent,
/// it can only ever remove rows that are already unreferenced, and gating it to
/// an operator would mean the sweep runs only when someone remembers.
pub async fn emoji_gc(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, AppError> {
    if let Err(resp) = gate(&state, &headers, &method, &uri, &body).await? {
        return Ok(resp);
    }
    let conn = state.db.conn().await?;
    let collected = sweep_unreferenced_emoji(&conn).await?;
    Ok(ok_json(serde_json::json!({
        "collected": collected.len(),
        "r2_keys": collected,
    })))
}

/// Collect every unreferenced object, returning their R2 keys. Reads the keys
/// first so the caller can clean R2 — the row is the only record of where the
/// blob lives, so it has to be read before it is deleted.
pub async fn sweep_unreferenced_emoji(conn: &Connection) -> anyhow::Result<Vec<String>> {
    let mut rows = conn
        .query(
            "SELECT content_hash, r2_key FROM custom_emoji_object o \
             WHERE NOT EXISTS (SELECT 1 FROM group_emoji ge WHERE ge.content_hash = o.content_hash)",
            (),
        )
        .await?;
    let mut doomed: Vec<(String, String)> = Vec::new();
    while let Some(row) = rows.next().await? {
        doomed.push((row.get(0)?, row.get(1)?));
    }

    let mut collected = Vec::with_capacity(doomed.len());
    for (hash, key) in doomed {
        // Re-check the predicate inside the DELETE rather than trusting the
        // SELECT: a group could have re-added the emoji between the two. The
        // conditional delete makes that race a no-op instead of a stranded blob.
        let removed = conn
            .execute(
                COLLECT_UNREFERENCED_EMOJI_SQL,
                libsql::params![hash.clone()],
            )
            .await?;
        if removed > 0 {
            collected.push(key);
        }
    }
    Ok(collected)
}

/// Release every emoji reference a group held. Called from
/// [`crate::groups::apply_delete_group`] — a deleted group's shortcodes are
/// meaningless, and leaving the rows would pin their objects forever.
///
/// Deliberately does NOT collect: it releases, and `POST /v1/emoji/gc` collects.
/// Keeping release and collect separate is what lets EVERY future teardown path
/// release correctly by doing nothing more than deleting its own rows.
pub async fn release_group_emoji(conn: &Connection, group_id: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM group_emoji WHERE group_id = ?1",
        libsql::params![group_id.to_string()],
    )
    .await?;
    Ok(())
}

/// Bridge to [`WriteOutcome`] for callers that only care whether the write
/// landed. Not used by this module's handlers (they want the wider mapping) —
/// present so a future domain can compose an emoji write into a `WriteOutcome`
/// pipeline without re-deriving the collapse.
impl From<EmojiOutcome> for WriteOutcome {
    fn from(outcome: EmojiOutcome) -> Self {
        match outcome {
            EmojiOutcome::Ok => WriteOutcome::Ok,
            _ => WriteOutcome::Forbidden,
        }
    }
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    #[test]
    fn shortcodes_reject_anything_that_could_carry_markup() {
        assert!(shortcode_is_valid("party_parrot"));
        assert!(shortcode_is_valid("a1"));
        assert!(shortcode_is_valid(&"a".repeat(SHORTCODE_MAX)));

        assert!(!shortcode_is_valid("a"), "too short");
        assert!(!shortcode_is_valid(&"a".repeat(SHORTCODE_MAX + 1)), "too long");
        assert!(!shortcode_is_valid("Party"), "uppercase would alias");
        assert!(!shortcode_is_valid("a-b"), "hyphen not in charset");
        assert!(!shortcode_is_valid("a b"), "whitespace splits the token");
        // The injection cases the charset exists to make unrepresentable.
        assert!(!shortcode_is_valid("<img src=x onerror=alert(1)>"));
        assert!(!shortcode_is_valid("a:b"), "colon breaks <:name:hash>");
        assert!(!shortcode_is_valid("&amp;"));
        assert!(!shortcode_is_valid("../../etc/passwd"));
    }

    #[test]
    fn content_hashes_must_be_64_lowercase_hex() {
        let ok = "a".repeat(64);
        assert!(content_hash_is_valid(&ok));
        assert!(!content_hash_is_valid(&"A".repeat(64)), "uppercase would double-store");
        assert!(!content_hash_is_valid(&"a".repeat(63)));
        assert!(!content_hash_is_valid(&"a".repeat(65)));
        assert!(!content_hash_is_valid("../etc/passwd"));
        assert!(!content_hash_is_valid(""));
    }

    #[test]
    fn only_the_two_re_encoder_outputs_are_accepted() {
        assert_eq!(emoji_ext_for_content_type("image/webp"), Some("webp"));
        assert_eq!(emoji_ext_for_content_type("image/gif"), Some("gif"));
        // Everything a browser might declare, and everything a decoder would
        // have to guess at.
        assert_eq!(emoji_ext_for_content_type("image/svg+xml"), None);
        assert_eq!(emoji_ext_for_content_type("image/png"), None);
        assert_eq!(emoji_ext_for_content_type("text/html"), None);
        assert_eq!(emoji_ext_for_content_type("application/octet-stream"), None);
    }

    #[test]
    fn emoji_keys_round_trip_and_reject_traversal() {
        let hash = "b".repeat(64);
        let key = emoji_r2_key(&hash, "webp");
        assert_eq!(key, format!("emoji/{hash}.webp"));
        assert_eq!(content_hash_from_emoji_key(&key), Some(hash.as_str()));

        assert_eq!(content_hash_from_emoji_key("media/abc.enc"), None);
        assert_eq!(content_hash_from_emoji_key("emoji/../secret.webp"), None);
        assert_eq!(content_hash_from_emoji_key(&format!("emoji/{hash}.svg")), None);
        assert_eq!(content_hash_from_emoji_key("emoji/short.webp"), None);
    }
}
