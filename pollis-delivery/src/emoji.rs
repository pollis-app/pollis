//! Domain F — custom per-group emoji (#848).
//!
//! One content-addressed object plus one `group_emoji` row per group; the refcount is DERIVED.
//!
//! ## What this module refuses, and why
//!
//! Everything a client sends about an object is re-derived or bounded here:
//!
//!   - **`r2_key` is not accepted**; it is COMPUTED from `(content_hash, content_type)`.
//!   - **`content_type` is a two-value allowlist** (`image/webp`, `image/gif`).
//!   - **`size_bytes` must be `1..=EMOJI_MAX_BYTES`**, which the presigned PUT also binds.
//!   - **`shortcode` must match `[a-z0-9_]{2,32}`**, so it can't carry markup.
//!   - **A creator may hold at most [`EMOJI_MAX_PER_USER`] rows**; deliberately no per-group cap.
//!
//! ## Permission rule (Discord's, exactly)
//!
//! *Adding* needs group membership; *using* any group holding it ([`user_can_use_emoji`]).

use axum::{
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use libsql::Connection;

use crate::error::{AppError, AuthRejection};
use crate::writes::{bad_request, gate, gate_and_parse, resolve_actor, RawRequest, WriteOutcome};
use crate::AppState;

// Request bodies live in `pollis-api`, shared with pollis-core; re-exported here.
pub use pollis_api::emoji::*;

/// Hard ceiling on ONE stored emoji object, in bytes.
pub const EMOJI_MAX_BYTES: u64 = 48 * 1024;

/// Hard ceiling on how many `group_emoji` rows ONE creator may hold, across
/// every group.
pub const EMOJI_MAX_PER_USER: i64 = 1000;

/// Shortest / longest permitted shortcode.
const SHORTCODE_MIN: usize = 2;
const SHORTCODE_MAX: usize = 32;

/// `[a-z0-9_]{2,32}`, hand-rolled (no regex dependency).
pub fn shortcode_is_valid(shortcode: &str) -> bool {
    let len = shortcode.len();
    if !(SHORTCODE_MIN..=SHORTCODE_MAX).contains(&len) {
        return false;
    }
    shortcode
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// 64 LOWERCASE hex: the hash is a primary key, so two cases would defeat dedup.
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

/// The R2 key for an emoji object, DERIVED; pollis-core computes the same.
pub fn emoji_r2_key(content_hash: &str, ext: &str) -> String {
    format!("emoji/{content_hash}.{ext}")
}

/// The content hash in an `emoji/<hash>.<ext>` key, or `None`.
pub fn content_hash_from_emoji_key(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("emoji/")?;
    let (hash, ext) = rest.rsplit_once('.')?;
    if !content_hash_is_valid(hash) || !matches!(ext, "webp" | "gif") {
        return None;
    }
    Some(hash)
}

/// An emoji write result; 400 and 409 are not "forbidden", so they aren't 403s.
#[derive(Debug)]
pub enum EmojiOutcome {
    Ok,
    /// The actor may not do this — not a member (add), or not an admin (remove).
    Forbidden,
    /// The body is malformed. The `&'static str` is safe to return verbatim (no
    /// user input is interpolated into it).
    Invalid(&'static str),
    /// The actor is at [`EMOJI_MAX_PER_USER`].
    QuotaExceeded,
    /// The `(group_id, shortcode)` pair is already taken.
    ShortcodeTaken,
}

/// Map an [`EmojiOutcome`] to its HTTP response.
fn emoji_outcome_response<B>(outcome: EmojiOutcome) -> Result<Response, AppError>
where
    B: pollis_api::DsRequest<Response = pollis_api::StatusOk>,
{
    Ok(match outcome {
        EmojiOutcome::Ok => crate::writes::ok_response::<B>(pollis_api::StatusOk::Ok),
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

/// True when at least one `group_emoji` row still names `content_hash`.
///
/// The one predicate for both the collect and the R2 presign gates in [`crate::broker`].
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

/// Collect the object iff NO group still references it, in ONE conditional `DELETE`.
const COLLECT_UNREFERENCED_EMOJI_SQL: &str = "\
DELETE FROM custom_emoji_object \
 WHERE content_hash = ?1 \
   AND NOT EXISTS (SELECT 1 FROM group_emoji WHERE content_hash = ?1)";

/// **The permission rule (#848).** True when `user_id` may USE the custom emoji
/// `content_hash` — i.e. it is registered to at least one group the user is a
/// current member of.
///
/// No conversation argument: scoping "use" to it would break #848's rule.
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

use crate::groups::{is_admin as is_group_admin, is_member as is_group_member};

pub async fn create_emoji(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<CreateEmojiBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    emoji_outcome_response::<CreateEmojiBody>(apply_create_emoji(&conn, authed.as_deref(), &parsed).await?)
}

/// Register the shared object (idempotent) and bind it to `(group_id,
/// shortcode)`.
///
/// Authz: a current MEMBER (#848); quota bounds the cost. Removal is admin-only (#1280).
///
/// The object INSERT is `OR IGNORE`; the `group_emoji` one is not, so collisions surface.
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

    // Authz: current member of the target group (skipped on the no-auth path).
    if authed.is_some() && !is_group_member(conn, &body.group_id, &actor).await? {
        return Ok(EmojiOutcome::Forbidden);
    }

    // Per-creator quota; a race overshoots by at most the concurrent requests.
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

pub async fn remove_emoji(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<RemoveEmojiBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    emoji_outcome_response::<RemoveEmojiBody>(apply_remove_emoji(&conn, authed.as_deref(), &parsed).await?)
}

/// Remove `(group_id, shortcode)` and collect the shared object if that was its
/// last reference.
///
/// Authz: a group ADMIN only, not even the creator (#1280); edit is remove + add.
///
/// Checking the role first stops a stranger reading the emoji set off 200 vs 403 (L3).
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

    // Admin BEFORE the row read (no existence oracle).
    if authed.is_some() && !is_group_admin(conn, &body.group_id, &actor).await? {
        return Ok(EmojiOutcome::Forbidden);
    }

    // Read the row: we need its hash to run the conditional collect.
    let mut rows = conn
        .query(
            "SELECT content_hash FROM group_emoji \
             WHERE group_id = ?1 AND shortcode = ?2",
            libsql::params![body.group_id.clone(), body.shortcode.clone()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        // Already gone. Idempotent — a retried remove is a success, not a 404.
        return Ok(EmojiOutcome::Ok);
    };
    let content_hash: String = row.get(0)?;

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

/// POST /v1/emoji/gc — collect every unreferenced object, returning their R2 keys.
///
/// Event-driven; catches references dropped outside remove. Idempotent, so any device may run it.
pub async fn emoji_gc(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    if let Err(resp) = gate(&state, &req).await? {
        return Ok(resp);
    }
    let conn = state.db.conn().await?;
    let collected = sweep_unreferenced_emoji(&conn).await?;
    Ok(crate::writes::ok_response::<EmojiGcBody>(EmojiGcResponse {
        collected: collected.len(),
        r2_keys: collected,
    }))
}

/// Collect every unreferenced object; keys are read first, as the row is the only record.
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
        // Re-check inside the DELETE: a group may have re-added the emoji.
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

/// Release every emoji reference a group held (from [`crate::groups::apply_delete_group`]).
///
/// Releases only; `POST /v1/emoji/gc` collects.
pub async fn release_group_emoji(conn: &Connection, group_id: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM group_emoji WHERE group_id = ?1",
        libsql::params![group_id.to_string()],
    )
    .await?;
    Ok(())
}

/// Bridge to [`WriteOutcome`] for callers that only care whether the write landed.
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
