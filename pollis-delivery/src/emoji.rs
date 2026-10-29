//! Custom per-group emoji (#848). Storage model and collection rules:
//! `.codesight/wiki/database.md` (`custom_emoji_object`, `group_emoji`).
//!
//! Everything a client sends about an object is re-derived or bounded here:
//! `r2_key` is computed from `(content_hash, content_type)` (a client naming the
//! key could bind its hash to someone else's object), `content_type` is the
//! re-encoder's two outputs, `size_bytes` is bounded and bound into the presign,
//! and `shortcode` is `[a-z0-9_]{2,32}` so it cannot carry markup or break the
//! `<:name:hash>` token grammar.
//!
//! Adding requires membership; using an emoji is allowed in any conversation
//! for a member of any group that registered it ([`user_can_use_emoji`],
//! enforced by `pollis-core` at composition time since content is E2EE).
//! Rendering is unauthorized by design.

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

pub use pollis_api::emoji::*;

/// Hard ceiling on one stored emoji object, in bytes. `pollis-core`'s
/// re-encoder treats it as a postcondition; [`crate::broker`] binds it into the
/// presigned PUT so R2 rejects an oversized body.
pub const EMOJI_MAX_BYTES: u64 = 48 * 1024;

/// Hard ceiling on `group_emoji` rows one creator may hold, across every
/// group. #848 forbids a per-group cap, so the bound is per person.
pub const EMOJI_MAX_PER_USER: i64 = 1000;

const SHORTCODE_MIN: usize = 2;
const SHORTCODE_MAX: usize = 32;

/// `[a-z0-9_]{2,32}`. Hand-rolled: `pollis-delivery` carries no regex
/// dependency.
pub fn shortcode_is_valid(shortcode: &str) -> bool {
    let len = shortcode.len();
    if !(SHORTCODE_MIN..=SHORTCODE_MAX).contains(&len) {
        return false;
    }
    shortcode
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// 64 lowercase hex characters. Case matters: the hash is a primary key and an
/// R2 key stem, so accepting both cases would store one image twice.
pub fn content_hash_is_valid(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The file extension for an allowed emoji content type; doubles as the
/// content-type allowlist.
pub fn emoji_ext_for_content_type(content_type: &str) -> Option<&'static str> {
    match content_type {
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}

/// The R2 key for an emoji object, derived, never accepted from a client.
/// Public so `pollis-core` computes the identical key.
pub fn emoji_r2_key(content_hash: &str, ext: &str) -> String {
    format!("emoji/{content_hash}.{ext}")
}

/// The content hash embedded in an emoji R2 key (`emoji/<hash>.<ext>`), or
/// `None` for any key not of that shape.
pub fn content_hash_from_emoji_key(key: &str) -> Option<&str> {
    let rest = key.strip_prefix("emoji/")?;
    let (hash, ext) = rest.rsplit_once('.')?;
    if !content_hash_is_valid(hash) || !matches!(ext, "webp" | "gif") {
        return None;
    }
    Some(hash)
}

/// The result of an emoji write. Wider than [`WriteOutcome`]: a malformed body
/// is a 400 and a quota or shortcode refusal a 409, not a 403.
#[derive(Debug)]
pub enum EmojiOutcome {
    Ok,
    Forbidden,
    /// Returned verbatim; no user input is interpolated into it.
    Invalid(&'static str),
    QuotaExceeded,
    ShortcodeTaken,
}

/// Map an [`EmojiOutcome`] to its HTTP response. Generic over the request type
/// for the same reason as [`crate::writes::outcome_response`] (#922).
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

/// True when at least one `group_emoji` row still names `content_hash`. The
/// R2 `put`/`delete` presign gates in [`crate::broker`] use it; an object may be
/// collected only when this is `false`.
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

/// Collect the shared object iff no group still references it. One conditional
/// `DELETE`, no Rust-side count, so concurrent removes cannot strand a
/// reference.
const COLLECT_UNREFERENCED_EMOJI_SQL: &str = "\
DELETE FROM custom_emoji_object \
 WHERE content_hash = ?1 \
   AND NOT EXISTS (SELECT 1 FROM group_emoji WHERE content_hash = ?1)";

/// The permission rule (#848): `user_id` may use `content_hash` when it is
/// registered to any group the user is a current member of. Deliberately takes
/// no conversation: scoping use to the conversation would break the Discord
/// behaviour #848 asks for.
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
/// shortcode)`. Any current member may add (#848); the cost is bounded by
/// [`EMOJI_MAX_PER_USER`]. A colliding `(group, shortcode)` is reported as
/// [`EmojiOutcome::ShortcodeTaken`], never swallowed.
pub async fn apply_create_emoji(
    conn: &Connection,
    authed: Option<&str>,
    body: &CreateEmojiBody,
) -> anyhow::Result<EmojiOutcome> {
    let actor = match resolve_actor(authed, body.created_by.as_deref()) {
        Ok(a) => a,
        Err(_) => return Ok(EmojiOutcome::Forbidden),
    };

    // Shape first: a malformed body is never a permission question.
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

    if authed.is_some() && !is_group_member(conn, &body.group_id, &actor).await? {
        return Ok(EmojiOutcome::Forbidden);
    }

    // Racing requests can overshoot by at most the number in flight.
    if creator_row_count(conn, &actor).await? >= EMOJI_MAX_PER_USER {
        return Ok(EmojiOutcome::QuotaExceeded);
    }

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
/// last reference. Group admins only, not even the creator (#1280): removal is
/// a moderation action, and also gates "edit" (a rename is remove plus add).
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

    // Admin check before the row read: otherwise `Ok` for a missing row vs
    // `Forbidden` for an existing one is an existence oracle for non-admins.
    // A non-member has no role, so this is also the membership gate.
    if authed.is_some() && !is_group_admin(conn, &body.group_id, &actor).await? {
        return Ok(EmojiOutcome::Forbidden);
    }

    let mut rows = conn
        .query(
            "SELECT content_hash FROM group_emoji \
             WHERE group_id = ?1 AND shortcode = ?2",
            libsql::params![body.group_id.clone(), body.shortcode.clone()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        // Idempotent: a retried remove is a success, not a 404.
        return Ok(EmojiOutcome::Ok);
    };
    let content_hash: String = row.get(0)?;

    let tx = conn.transaction().await?;
    tx.execute(
        "DELETE FROM group_emoji WHERE group_id = ?1 AND shortcode = ?2",
        libsql::params![body.group_id.clone(), body.shortcode.clone()],
    )
    .await?;
    // Same transaction as the release, so two concurrent removes cannot each
    // conclude "still referenced".
    tx.execute(
        COLLECT_UNREFERENCED_EMOJI_SQL,
        libsql::params![content_hash.clone()],
    )
    .await?;
    tx.commit().await?;
    Ok(EmojiOutcome::Ok)
}

/// POST /v1/emoji/gc: collect every unreferenced `custom_emoji_object` and
/// return the collected R2 keys so the caller can delete the blobs. Covers
/// references dropped outside the remove path (group deletion). Any
/// authenticated device may run it: it can only remove unreferenced rows.
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

/// Collect every unreferenced object, returning their R2 keys (the row is the
/// only record of where the blob lives).
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
        // The conditional DELETE re-checks the predicate: a group could have
        // re-added the emoji since the SELECT.
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

/// Release every emoji reference a group held (group deletion). Does not
/// collect; `POST /v1/emoji/gc` does.
pub async fn release_group_emoji(conn: &Connection, group_id: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM group_emoji WHERE group_id = ?1",
        libsql::params![group_id.to_string()],
    )
    .await?;
    Ok(())
}

/// Collapse to [`WriteOutcome`] for callers that only care whether the write
/// landed.
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
