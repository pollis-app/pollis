//! Domain A — message envelopes, edits/deletes, reactions, watermarks, and
//! attachment dedup rows (#419).
//!
//! Every domain-A table lives in the **MAIN DB**.
//!
//! ## Authorization (the security core)
//!
//! `gate` proves *which user* signed the request; each `apply_*` then proves the
//! user is *allowed* to make that specific write:
//!   - send / edit / delete: membership only; authorship is checked CLIENT-side (#607). Admin-delete also needs group admin.
//!   - reactions: the user is a member, and may only write/remove their OWN
//!     reaction (`user_id` is bound to the authenticated user).
//!   - watermark: own `(user, device)` only, as a member; the cursor passes [`check_cursor_stamp`].
//!   - envelope GC: MIN watermark over live member devices (#720), never message age (I3); see [`sweep_envelope_gc`].
//!   - attachment refs (#690): derived from envelope existence ([`live_ref_exists`]); register binds to the author (#1165).
//!
//! With `authed == None` (auth off) the membership/identity checks are skipped.

use axum::{
    extract::State,
    response::Response,
};
use libsql::Connection;
use ulid::Ulid;

use crate::error::AppError;
use crate::writes::{
    gate_and_parse,
    gate_identity_and_parse,
    is_member,
    outcome_response,
    resolve_actor,
    RawRequest,
    WriteOutcome,
};
use crate::AppState;

// Request bodies live in `pollis-api`, shared with pollis-core.
pub use pollis_api::messages::*;

// Envelope GC: delete only `sent_at` below the MIN `last_fetched_at` over LIVE member devices, never by age (I3).
//
// Never add a time-based or other delivery-blind gate here: age is not evidence of delivery (F3).
//
// #720: a device whose last REPORT (`reported_at`, not a cursor) is older than `?2` stops pinning.
//
// Fails CLOSED on `reported_at IS NULL`: unknown report time is not dormancy.
//
// What remains fails CLOSED on every edge:
//   * `COUNT(ud.device_id) = COUNT(cw.last_fetched_at)` — every current member
//     device must have REPORTED a watermark. The LEFT JOIN yields NULL for a
//     device with no `conversation_watermark` row and `COUNT` skips NULLs, so a
//     never-reported device (brand-new, or long absent) breaks the equality, the
//     CASE returns NULL, and `sent_at < NULL` is NULL — nothing is deleted.
//   * `MIN(cw.last_fetched_at)` — the floor is the SLOWEST reporter's cursor, so
//     one eager device racing ahead cannot raise it.
//   * an EMPTY member-device set aggregates to `COUNT 0 = COUNT 0` with
//     `MIN(...) = NULL` over zero rows, so the CASE again returns NULL. A roster
//     that resolves to nobody is not evidence that everybody collected.
//
// Revoked devices are excluded (#685, I5), matching `commit::current_member_devices`.
//
// The roster lives in a macro (#722) so `roster_parity_tests` SELECTs exactly what the DELETEs aggregate.

/// The channel roster: every non-revoked device of every member of the group
/// that owns channel `?1`, LEFT JOINed to that device's watermark for `?1`.
macro_rules! channel_member_device_rows {
    () => {
        "FROM group_member gm
       JOIN channels c ON c.id = ?1 AND c.group_id = gm.group_id
       JOIN user_device ud ON ud.user_id = gm.user_id AND ud.revoked_at IS NULL
       LEFT JOIN conversation_watermark cw
              ON cw.conversation_id = ?1
             AND cw.user_id = ud.user_id
             AND cw.device_id = ud.device_id"
    };
}

/// The DM roster: every non-revoked device of every member of DM channel `?1`,
/// LEFT JOINed to that device's watermark for `?1`.
macro_rules! dm_member_device_rows {
    () => {
        "FROM dm_channel_member dcm
       JOIN user_device ud ON ud.user_id = dcm.user_id AND ud.revoked_at IS NULL
       LEFT JOIN conversation_watermark cw
              ON cw.conversation_id = ?1
             AND cw.user_id = ud.user_id
             AND cw.device_id = ud.device_id
       WHERE dcm.dm_channel_id = ?1"
    };
}

// The `?2` staleness arm sits OUTSIDE the roster macro, for parity (I5).
//
// `?2` is a SQLite datetime modifier, bound only via `cleanup_conversation_envelopes` (#720).

const CLEANUP_CHANNEL_ENVELOPES: &str = concat!(
    "\
DELETE FROM message_envelope
 WHERE conversation_id = ?1
   AND seq <= (
       SELECT CASE
                WHEN COUNT(ud.device_id) = COUNT(cw.last_seq)
                THEN MIN(cw.last_seq)
                ELSE NULL
              END
       ",
    channel_member_device_rows!(),
    "
       WHERE cw.reported_at IS NULL OR cw.reported_at >= datetime('now', ?2)
     )"
);

const CLEANUP_DM_ENVELOPES: &str = concat!(
    "\
DELETE FROM message_envelope
 WHERE conversation_id = ?1
   AND seq <= (
       SELECT CASE
                WHEN COUNT(ud.device_id) = COUNT(cw.last_seq)
                THEN MIN(cw.last_seq)
                ELSE NULL
              END
       ",
    dm_member_device_rows!(),
    "
         AND (cw.reported_at IS NULL OR cw.reported_at >= datetime('now', ?2))
     )"
);

/// The staleness window when `POLLIS_DS_WATERMARK_STALE_MONTHS` is unset, as a
/// SQLite `datetime()` modifier: a member device that has not reported a
/// watermark in this long stops pinning envelope retention.
///
/// The documented policy, 12 months (#1486); changing it is a product decision, not a tuning knob.
pub const DEFAULT_WATERMARK_STALE_MODIFIER: &str = "-12 months";

/// The configured device-staleness window as a SQLite `datetime()` modifier,
/// bound as `?2` into the `CLEANUP_*` predicates.
///
/// `POLLIS_DS_WATERMARK_STALE_MONTHS` = N months (default 12); `0` disables it; garbage falls back.
pub fn watermark_stale_modifier() -> String {
    stale_modifier_from(std::env::var("POLLIS_DS_WATERMARK_STALE_MONTHS").ok().as_deref())
}

/// [`watermark_stale_modifier`] over an explicit raw value, so the mapping is
/// testable without mutating the process environment.
fn stale_modifier_from(raw: Option<&str>) -> String {
    match raw.and_then(|v| v.trim().parse::<u32>().ok()) {
        Some(0) => "-1000 years".to_string(),
        Some(n) => format!("-{n} months"),
        None => DEFAULT_WATERMARK_STALE_MODIFIER.to_string(),
    }
}

/// Sweep cadence when `POLLIS_DS_GC_SWEEP_SECS` is unset or unparseable.
pub const DEFAULT_GC_SWEEP_SECS: u64 = 3600;

/// The envelope-GC cadence the sweep loop **actually binds**, in seconds; `0`
/// disables the sweep.
///
/// Shared with `GET /v1/config` so the reported value is the effective one (#760).
pub fn gc_sweep_secs() -> u64 {
    std::env::var("POLLIS_DS_GC_SWEEP_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_GC_SWEEP_SECS)
}

/// Identity-free growth metrics for `message_envelope`, computed by the sweep and
/// served on `GET /v1/retention/metrics`. Deliberately holds NO conversation id
/// or user id (see `docs/metadata-retention-policy.md` §3) — only counts and the
/// shape of the worst offender.
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct RetentionSnapshot {
    /// Total surviving `message_envelope` rows across all conversations.
    pub total_envelopes: i64,
    /// How many distinct conversations still hold at least one envelope.
    pub conversations_with_envelopes: i64,
    /// The oldest `sent_at` still retained anywhere (a timestamp, not an id).
    pub oldest_sent_at: Option<String>,
    /// The worst offender's envelope count — the single conversation holding the
    /// most envelopes. No id: the SHAPE only.
    pub largest_conversation_envelopes: i64,
    /// That worst offender's oldest retained `sent_at`.
    pub largest_conversation_oldest_sent_at: Option<String>,
}

/// Shared, in-memory home for the latest [`RetentionSnapshot`]. The sweep writes
/// it after each run; `GET /v1/retention/metrics` reads it. `None` until the
/// first sweep completes.
pub type RetentionMetricsHandle = std::sync::Arc<std::sync::Mutex<Option<RetentionSnapshot>>>;

/// The conversation a message belongs to, resolved from any envelope carrying
/// its id (used to membership-gate reactions, which only know `message_id`).
async fn conversation_for_message(
    conn: &Connection,
    message_id: &str,
) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT conversation_id FROM message_envelope WHERE id = ?1 LIMIT 1",
            libsql::params![message_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(row.get::<String>(0)?),
        None => None,
    })
}

/// The caller's role in the group that owns `conversation_id` (a text channel).
/// `None` when the conversation is not a group channel (e.g. a DM, which has no
/// `channels` row and no admin concept) or the caller is not a member.
async fn channel_group_role(
    conn: &Connection,
    conversation_id: &str,
    user_id: &str,
) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT gm.role FROM channels c \
             JOIN group_member gm ON gm.group_id = c.group_id \
             WHERE c.id = ?1 AND gm.user_id = ?2",
            libsql::params![conversation_id.to_string(), user_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => Some(row.get::<String>(0)?),
        None => None,
    })
}

/// A DS-issued timestamp for the textual `sent_at` / `created_at` columns.
///
/// Lexically compared against client stamps, so always emit nanoseconds (`'+'` < `'.'`).
pub(crate) fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, false)
}

/// **The** value a server-side seed writes into
/// `conversation_watermark.last_fetched_at` (#908). Every seed path — device
/// registration ([`crate::bootstrap`]), DM creation and DM member-add
/// ([`crate::profile`]), group join ([`crate::groups`]) — goes through here.
///
/// Lexically compared against RFC 3339 `sent_at`; SQLite's `datetime('now')` sorts below all of them.
///
/// **Not for `reported_at`**, which is compared against `datetime('now', ?)` and keeps SQLite's format.
pub(crate) fn seeded_watermark_cursor() -> String {
    now_rfc3339()
}

// Client cursor stamps are BOUNDED: a `9999-…` one blacked out a chat.
//
// Kept client-chosen (#844) but must be canonical UTC RFC 3339, at most [`CURSOR_STAMP_SKEW_SECS`] ahead.

/// How far ahead of the DS clock a client-chosen `sent_at` /
/// `last_fetched_at` may sit. The request-signature replay window
/// ([`crate::auth::REPLAY_WINDOW_SECS`]): a client that could not sign inside
/// it cannot post at all, so the bound costs an honest client nothing new.
pub const CURSOR_STAMP_SKEW_SECS: i64 = crate::auth::REPLAY_WINDOW_SECS;

/// Why a client-chosen cursor stamp was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampRejection {
    /// Not one of the canonical UTC RFC 3339 renderings (`…+00:00`, with 0, 3, 6
    /// or 9 fraction digits) — which is the only shape whose lexical order is
    /// chronological order against everything else in the column.
    NotCanonical,
    /// Parses, but denotes an instant more than [`CURSOR_STAMP_SKEW_SECS`] past
    /// the DS clock.
    InFuture,
}

/// The one admission check for a client-chosen `sent_at` or `last_fetched_at`; `now` is injected.
///
/// Canonical = re-renders byte-for-byte as chrono UTC at 0/3/6/9 fraction digits; anything else mis-sorts.
pub fn check_cursor_stamp(
    value: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), StampRejection> {
    let parsed = chrono::DateTime::parse_from_rfc3339(value)
        .map_err(|_| StampRejection::NotCanonical)?;
    if parsed.offset().local_minus_utc() != 0 {
        return Err(StampRejection::NotCanonical);
    }
    let utc = parsed.to_utc();
    let canonical = [
        chrono::SecondsFormat::Secs,
        chrono::SecondsFormat::Millis,
        chrono::SecondsFormat::Micros,
        chrono::SecondsFormat::Nanos,
    ]
    .iter()
    .any(|fmt| utc.to_rfc3339_opts(*fmt, false) == value);
    if !canonical {
        return Err(StampRejection::NotCanonical);
    }
    if utc > now + chrono::Duration::seconds(CURSOR_STAMP_SKEW_SECS) {
        return Err(StampRejection::InFuture);
    }
    Ok(())
}

/// [`check_cursor_stamp`] at the DS clock, mapped to the `sent_at` refusals.
fn admit_sent_at(value: &str) -> Result<(), WriteOutcome> {
    match check_cursor_stamp(value, chrono::Utc::now()) {
        Ok(()) => Ok(()),
        Err(StampRejection::NotCanonical) => Err(WriteOutcome::Invalid(
            "sent_at must be a canonical UTC RFC 3339 stamp",
        )),
        Err(StampRejection::InFuture) => Err(WriteOutcome::Invalid(
            "sent_at is too far ahead of the server clock",
        )),
    }
}

/// [`check_cursor_stamp`] at the DS clock, mapped to the `last_fetched_at`
/// refusals.
fn admit_last_fetched_at(value: &str) -> Result<(), WriteOutcome> {
    match check_cursor_stamp(value, chrono::Utc::now()) {
        Ok(()) => Ok(()),
        Err(StampRejection::NotCanonical) => Err(WriteOutcome::Invalid(
            "last_fetched_at must be a canonical UTC RFC 3339 stamp",
        )),
        Err(StampRejection::InFuture) => Err(WriteOutcome::Invalid(
            "last_fetched_at is too far ahead of the server clock",
        )),
    }
}

pub async fn send_message(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<SendMessageBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let log = state.log_db.conn().await?;
    let (outcome, replayed) =
        send_envelope_once(&conn, &log, authed.as_deref(), &parsed).await?;

    // Best-effort push AFTER the envelope lands (#987); sender from the AUTHENTICATED user (#331); not on replay (#1335).
    if matches!(outcome, WriteOutcome::Ok) && !replayed {
        if let Some(audience) =
            crate::push::PushAudience::from_send(parsed.push_to.clone(), parsed.push_mentions.clone())
        {
            let sender = authed.clone().unwrap_or_else(|| {
                parsed.sender_id.clone().unwrap_or_default()
            });
            let conversation_id = parsed.conversation_id.clone();
            let db = state.db.clone();
            tokio::spawn(async move {
                let conn = match db.conn().await {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!("push fan-out: conn failed: {e}");
                        return;
                    }
                };
                if let Err(e) =
                    crate::push::notify_new_message(&conn, &conversation_id, &sender, &audience).await
                {
                    // No conversation id: push logs name nothing (#1423).
                    tracing::warn!("push fan-out failed: {e}");
                }
            });
        }
    }

    outcome_response::<SendMessageBody>(outcome)
}

/// INSERT a `type='message'` envelope (the send). Authz: the authenticated user
/// is a current member of the conversation.
///
/// Unsealed: `sender_id` is bound to the signer. Sealed (#331): a blinded sentinel is stored; membership still checked.
pub async fn apply_send_message(
    conn: &Connection,
    authed: Option<&str>,
    body: &SendMessageBody,
) -> anyhow::Result<WriteOutcome> {
    let sealed = body.sealed != 0;
    // `member_check_user` is whose membership we verify; `stored_sender` is what
    // lands in the `sender_id` column.
    let (member_check_user, stored_sender) = if sealed {
        match authed {
            // Verify the authenticated writer's membership; store the blinded
            // sentinel the client sent (never bind it to the auth user).
            Some(u) => (u.to_string(), body.sender_id.clone().unwrap_or_default()),
            // No-auth fallback: keep the body's sender for both.
            None => {
                let s = body.sender_id.clone().unwrap_or_default();
                (s.clone(), s)
            }
        }
    } else {
        match resolve_actor(authed, body.sender_id.as_deref()) {
            Ok(s) => (s.clone(), s),
            Err(o) => return Ok(o),
        }
    };
    if authed.is_some() && !is_member(conn, &body.conversation_id, &member_check_user).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    // The stamp is the delivery cursor every recipient will adopt — bounded
    // before it can touch the table (see `check_cursor_stamp`).
    if let Err(refused) = admit_sent_at(&body.sent_at) {
        return Ok(refused);
    }
    // Every NEW envelope must carry a deletion capability (#1135); old NULL rows age out (#720).
    //
    // `Invalid`, not `Forbidden`: the caller is entitled to send, the body is
    // what is not admissible.
    if body.delete_token_hash.as_deref().unwrap_or("").is_empty() {
        return Ok(WriteOutcome::Invalid(
            "delete_token_hash is required on every new envelope",
        ));
    }
    // Sequence allocation and row land together (see `insert_envelope_with_seq`).
    let tx = conn.transaction().await?;
    insert_envelope_with_seq(
        &tx,
        &body.conversation_id,
        // Deletion capability (#1086): an HMAC only the author's devices can compute; stored as sent.
        "id, sender_id, ciphertext, reply_to_id, sent_at, sealed, delete_token_hash",
        "?3, ?4, ?5, ?6, ?7, ?8, ?9",
        vec![
            body.id.clone().into(),
            stored_sender.into(),
            body.ciphertext.clone().into(),
            body.reply_to_id.clone().into(),
            body.sent_at.clone().into(),
            body.sealed.into(),
            body.delete_token_hash.clone().into(),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

pub async fn edit_message(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<EditMessageBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let log = state.log_db.conn().await?;
    outcome_response::<EditMessageBody>(
        edit_envelope(&conn, &log, authed.as_deref(), &parsed).await?,
    )
}

// Epoch gate (#1041): an envelope at a closed epoch is refused (409).
//
// The assertion is optional on the wire: older clients are admitted ungated.
//
// Insert-then-verify (two DBs, no spanning tx): insert, read the head, delete + 409 if it moved. Vs a CAS:
//   - CAS before our head read → we see the new head → delete + 409 → the
//     sender re-seals at the new epoch. Nothing at the old epoch remains.
//   - CAS after our head read → the row was already visible when the CAS
//     landed, so the committer's pre-merge sweep fetches it and decrypts it at
//     the epoch it closes. Every other member replays that epoch on their
//     next catch-up, bounded to it by the `head` the response carries.
// Either way no envelope sits at an epoch nobody can still open.

/// The commit-log identity the epoch gate reads the head for: a channel's
/// owning group, a DM's own id. An id the directory does not know resolves to
/// itself, which is the DM shape and what the in-process tests use.
async fn mls_group_of(conn: &Connection, conversation_id: &str) -> anyhow::Result<String> {
    Ok(crate::directory::resolve_conversation(conn, conversation_id)
        .await?
        .map(|(g, _)| g)
        .unwrap_or_else(|| conversation_id.to_string()))
}

/// An envelope's asserted lineage, or `None` when the client asserted nothing
/// (admitted ungated).
type Lineage = Option<(i64, i64)>;

/// `Some(EpochBehind)` when `asserted` is not the log head for `mls_group_id`.
async fn behind_head(
    log: &Connection,
    mls_group_id: &str,
    asserted: Lineage,
) -> anyhow::Result<Option<WriteOutcome>> {
    let Some(asserted) = asserted else {
        return Ok(None);
    };
    let head_generation = crate::commit::head_generation(log, mls_group_id).await?;
    let head_epoch = crate::commit::head_epoch_in(log, mls_group_id, head_generation).await?;
    if asserted == (head_generation, head_epoch) {
        return Ok(None);
    }
    Ok(Some(WriteOutcome::EpochBehind {
        head_generation,
        head_epoch,
    }))
}

/// Run `apply` under the epoch gate. The head is checked BEFORE the write (so a
/// sender that is plainly behind touches nothing — an edit's DELETE of the
/// prior pending edit included) and AGAIN after it, deleting the row `id` the
/// write inserted when the head moved in between.
async fn apply_at_head<F, Fut>(
    main: &Connection,
    log: &Connection,
    conversation_id: &str,
    id: &str,
    asserted: Lineage,
    apply: F,
) -> anyhow::Result<WriteOutcome>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<WriteOutcome>>,
{
    let mls_group_id = mls_group_of(main, conversation_id).await?;
    if let Some(behind) = behind_head(log, &mls_group_id, asserted).await? {
        return Ok(behind);
    }
    let outcome = apply().await?;
    if !matches!(outcome, WriteOutcome::Ok) {
        return Ok(outcome);
    }
    let Some(behind) = behind_head(log, &mls_group_id, asserted).await? else {
        return Ok(WriteOutcome::Ok);
    };
    main.execute(
        "DELETE FROM message_envelope WHERE id = ?1",
        libsql::params![id.to_string()],
    )
    .await?;
    Ok(behind)
}

/// [`apply_send_message`] under the epoch gate — what `/v1/messages/send` runs.
pub async fn send_envelope(
    main: &Connection,
    log: &Connection,
    authed: Option<&str>,
    body: &SendMessageBody,
) -> anyhow::Result<WriteOutcome> {
    Ok(send_envelope_once(main, log, authed, body).await?.0)
}

/// [`send_envelope`], also saying whether the envelope was a REPLAY of one this
/// DS already holds — `true` means nothing was written, and the handler must
/// not push the recipients a second time.
///
/// A replay is the durable outbox re-posting the same id (#1335): answered as success, row untouched.
///
/// Checked BEFORE the epoch gate, whose post-check would delete the delivered row.
///
/// **One post per envelope id at a time**: the id lock spans the replay check and the gated write.
pub async fn send_envelope_once(
    main: &Connection,
    log: &Connection,
    authed: Option<&str>,
    body: &SendMessageBody,
) -> anyhow::Result<(WriteOutcome, bool)> {
    let _one_at_a_time = EnvelopeIdLock::acquire(&body.id).await;
    if let Some(replay) = replayed_send(main, body).await? {
        return Ok((replay, true));
    }
    let outcome = apply_at_head(
        main,
        log,
        &body.conversation_id,
        &body.id,
        body.generation.zip(body.epoch),
        || apply_send_message(main, authed, body),
    )
    .await?;
    Ok((outcome, false))
}

/// `Some(outcome)` when `body.id` is already a stored envelope, `None` for a
/// new one.
///
/// `Ok` only for the SAME message from the SAME author (matching #1086 hash), not current membership.
///
/// Anything else is `Forbidden`, as for a non-member, so the id's existence leaks nothing.
async fn replayed_send(
    main: &Connection,
    body: &SendMessageBody,
) -> anyhow::Result<Option<WriteOutcome>> {
    use subtle::ConstantTimeEq;
    let mut rows = main
        .query(
            "SELECT conversation_id, type, delete_token_hash FROM message_envelope \
             WHERE id = ?1 LIMIT 1",
            libsql::params![body.id.clone()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(None);
    };
    let conversation_id: String = row.get(0)?;
    let kind: String = row.get(1)?;
    let stored_hash: Option<String> = row.get(2)?;
    let same_author = match (stored_hash.as_deref(), body.delete_token_hash.as_deref()) {
        (Some(stored), Some(sent)) if !stored.is_empty() => {
            bool::from(stored.as_bytes().ct_eq(sent.as_bytes()))
        }
        _ => false,
    };
    if conversation_id == body.conversation_id && kind == "message" && same_author {
        return Ok(Some(WriteOutcome::Ok));
    }
    Ok(Some(WriteOutcome::Forbidden))
}

/// The per-envelope-id lock [`send_envelope_once`] holds (see there). Entries
/// are removed when the last holder lets go, so the map stays as small as the
/// number of posts in flight.
struct EnvelopeIdLock {
    id: String,
    entry: std::sync::Arc<tokio::sync::Mutex<()>>,
    guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

type EnvelopeIdLocks =
    std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<tokio::sync::Mutex<()>>>>;

fn envelope_id_locks() -> &'static EnvelopeIdLocks {
    static LOCKS: std::sync::OnceLock<EnvelopeIdLocks> = std::sync::OnceLock::new();
    LOCKS.get_or_init(Default::default)
}

impl EnvelopeIdLock {
    async fn acquire(id: &str) -> Self {
        let entry = envelope_id_locks()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(id.to_string())
            .or_default()
            .clone();
        let guard = entry.clone().lock_owned().await;
        Self {
            id: id.to_string(),
            entry,
            guard: Some(guard),
        }
    }
}

impl Drop for EnvelopeIdLock {
    fn drop(&mut self) {
        self.guard.take();
        let mut map = envelope_id_locks().lock().unwrap_or_else(|e| e.into_inner());
        // Ours and the map's are the only references: nobody holds or awaits it.
        if std::sync::Arc::strong_count(&self.entry) <= 2 {
            map.remove(&self.id);
        }
    }
}

/// [`apply_edit_message`] under the epoch gate — what `/v1/messages/edit` runs.
pub async fn edit_envelope(
    main: &Connection,
    log: &Connection,
    authed: Option<&str>,
    body: &EditMessageBody,
) -> anyhow::Result<WriteOutcome> {
    apply_at_head(
        main,
        log,
        &body.conversation_id,
        &body.envelope_id,
        body.generation.zip(body.epoch),
        || apply_edit_message(main, authed, body),
    )
    .await
}

/// Replace the single pending edit envelope (DELETE prior + INSERT new) in one
/// transaction. Authz: the editor is a current member of the conversation.
pub async fn apply_edit_message(
    conn: &Connection,
    authed: Option<&str>,
    body: &EditMessageBody,
) -> anyhow::Result<WriteOutcome> {
    let sender = match resolve_actor(authed, body.sender_id.as_deref()) {
        Ok(s) => s,
        Err(o) => return Ok(o),
    };
    if authed.is_some() && !is_member(conn, &body.conversation_id, &sender).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    // Same bound as a send, and checked BEFORE the DELETE below: a refused edit
    // must not have removed the author's pending edit on its way out.
    if let Err(refused) = admit_sent_at(&body.sent_at) {
        return Ok(refused);
    }
    // An edit REPLACES the target's pending edit, so it is a delete, so it needs
    // the target's capability (#1086).
    //
    // One pending edit per message is a schema rule, so accepting one MUST remove the last; ingest's author check stays.
    //
    // Checked before the transaction opens, so a refusal removes nothing.
    if check_delete_capability(
        conn,
        &body.conversation_id,
        &body.target_message_id,
        body.delete_token.as_deref(),
    )
    .await?
        == DeleteCapability::Refused
    {
        return Ok(WriteOutcome::Forbidden);
    }
    // Read BEFORE the tx: the DELETE below removes the row the fallback reads.
    let inherited_capability =
        edit_capability_hash(conn, &body.conversation_id, &body.target_message_id).await?;
    let tx = conn.transaction().await?;
    tx.execute(
        "DELETE FROM message_envelope \
         WHERE conversation_id = ?1 AND target_message_id = ?2 AND type = 'edit'",
        libsql::params![body.conversation_id.clone(), body.target_message_id.clone()],
    )
    .await?;
    insert_envelope_with_seq(
        &tx,
        &body.conversation_id,
        "id, sender_id, ciphertext, sent_at, type, target_message_id, delete_token_hash",
        "?3, ?4, ?5, ?6, 'edit', ?7, ?8",
        vec![
            body.envelope_id.clone().into(),
            sender.into(),
            body.ciphertext.clone().into(),
            body.sent_at.clone().into(),
            body.target_message_id.clone().into(),
            // The edit envelope inherits the TARGET's capability, so the next
            // edit (or a delete) can replace it with the same proof.
            inherited_capability.into(),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

pub async fn delete_message(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<DeleteMessageBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<DeleteMessageBody>(apply_delete_message(&conn, authed.as_deref(), &parsed).await?)
}

/// A capability hash whose PREIMAGE is known, so a fixture can both write an
/// envelope that requires a capability (#1135) and then present the token to
/// edit or delete it. `base64(SHA-256("fixture-capability"))`.
#[cfg(test)]
pub(crate) const FIXTURE_CAPABILITY_HASH: &str = "MB8lyVjsEQamFuSzLTRZBVKGFQQ8nXoBjHFF7kvNuAo=";
/// The preimage of [`FIXTURE_CAPABILITY_HASH`].
#[cfg(test)]
pub(crate) const FIXTURE_CAPABILITY_TOKEN: &str = "fixture-capability";

/// Whether a caller has proved the right to remove an envelope (#1086).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DeleteCapability {
    /// The row carries no capability — written by a client that predates #1086,
    /// or already gone: the caller falls back to the membership check.
    NotRequired,
    /// The presented token hashes to the stored value.
    Proved,
    /// The row HAS a capability and the caller did not open it.
    Refused,
}

/// Check `token` against the capability stored on `message_id`.
///
/// Sealed sender hides authorship; the DS only compares `SHA-256(token)` with the stored hash.
///
/// Scoped to `conversation_id`, the id the caller was authorized against.
pub(crate) async fn check_delete_capability(
    conn: &Connection,
    conversation_id: &str,
    message_id: &str,
    token: Option<&str>,
) -> anyhow::Result<DeleteCapability> {
    let mut rows = conn
        .query(
            "SELECT delete_token_hash FROM message_envelope \
             WHERE id = ?1 AND conversation_id = ?2",
            libsql::params![message_id.to_string(), conversation_id.to_string()],
        )
        .await?;
    // TEXT, not BLOB: the value travels as base64 and is stored verbatim, so it
    // is a string all the way down. Reading it as bytes panics inside libsql.
    let stored: Option<String> = match rows.next().await? {
        // A row with no hash: the legacy path.
        Some(row) => row.get::<Option<String>>(0).ok().flatten(),
        // Target GONE: its pending edit outlives it, so without this any member could drop that edit (L3).
        //
        // The edit inherited the target's capability; `None` there is still `NotRequired`.
        None => {
            drop(rows);
            pending_edit_capability_hash(conn, conversation_id, message_id).await?
        }
    };
    let Some(stored) = stored else {
        return Ok(DeleteCapability::NotRequired);
    };

    // The row demands a capability from here on: no token is a refusal, not a
    // fallback, or presenting nothing would be the easiest way past the check.
    let Some(token) = token else {
        return Ok(DeleteCapability::Refused);
    };
    if capability_matches(&stored, token) {
        Ok(DeleteCapability::Proved)
    } else {
        Ok(DeleteCapability::Refused)
    }
}

/// The capability hash carried by the PENDING EDIT of `message_id`, if there is
/// one.
///
/// The edit inherited the target's hash when it was written, so once the target
/// itself is collected this is the last copy of the capability that guards both.
async fn pending_edit_capability_hash(
    conn: &Connection,
    conversation_id: &str,
    message_id: &str,
) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT delete_token_hash FROM message_envelope \
             WHERE conversation_id = ?1 AND target_message_id = ?2 AND type = 'edit' \
             LIMIT 1",
            libsql::params![conversation_id.to_string(), message_id.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get::<Option<String>>(0).ok().flatten(),
        None => None,
    })
}

/// The capability hash stored on `message_id`, for an edit envelope to inherit.
///
/// Copying the TARGET's hash lets the next edit prove authorship of the original.
///
/// Falls back to the replaced edit's hash, else a post-GC edit would inherit `NULL` (L3).
async fn edit_capability_hash(
    conn: &Connection,
    conversation_id: &str,
    message_id: &str,
) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(
            "SELECT delete_token_hash FROM message_envelope \
             WHERE id = ?1 AND conversation_id = ?2",
            libsql::params![message_id.to_string(), conversation_id.to_string()],
        )
        .await?;
    let from_target = match rows.next().await? {
        Some(row) => row.get::<Option<String>>(0).ok().flatten(),
        None => None,
    };
    drop(rows);
    match from_target {
        Some(h) => Ok(Some(h)),
        None => pending_edit_capability_hash(conn, conversation_id, message_id).await,
    }
}

/// Take the next delivery sequence for `conversation_id` (#1087).
///
/// Uses `conversation_seq`, never `MAX(seq)+1`: GC empties rows, which would reissue seqs below every cursor (#692).
///
/// GC never touches the counter; only conversation teardown does.
async fn next_delivery_seq(conn: &Connection, conversation_id: &str) -> anyhow::Result<i64> {
    let mut rows = conn
        .query(
            "INSERT INTO conversation_seq (conversation_id, next_seq) VALUES (?1, 1) \
             ON CONFLICT(conversation_id) DO UPDATE SET next_seq = next_seq + 1 \
             RETURNING next_seq",
            libsql::params![conversation_id.to_string()],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or_else(|| anyhow::anyhow!("conversation_seq returned no row for {conversation_id}"))?;
    Ok(row.get::<i64>(0)?)
}

/// Insert an envelope, assigning it the next delivery sequence (#1087).
///
/// **Every envelope goes through here** — send, edit, tombstone. The sequence is
/// what recipients fetch by, what their cursor points into, and what retention
/// compares against, so a writer that skipped it would create an envelope
/// delivered to nobody and collected by nothing.
///
/// **Takes a `Transaction`**: a writer between allocate and insert would make `N+1` visible before `N`, losing `N`.
///
/// `columns` names the columns AFTER `conversation_id` and `seq`; `placeholders`
/// supplies them starting at `?3`, and `params` binds them in that order.
async fn insert_envelope_with_seq(
    tx: &libsql::Transaction,
    conversation_id: &str,
    columns: &str,
    placeholders: &str,
    params: Vec<libsql::Value>,
) -> anyhow::Result<()> {
    let seq = next_delivery_seq(tx, conversation_id).await?;
    let sql = format!(
        "INSERT INTO message_envelope (conversation_id, seq, {columns}) \
         VALUES (?1, ?2, {placeholders})"
    );
    let mut bound: Vec<libsql::Value> = Vec::with_capacity(params.len() + 2);
    bound.push(conversation_id.into());
    bound.push(seq.into());
    bound.extend(params);
    tx.execute(&sql, bound).await?;
    Ok(())
}

/// Delete a message. Two branches, chosen by the client's `msg_sender_id` hint
/// (#607): sealed sender means the DS cannot derive authorship.
///
/// **Self-branch** (`msg_sender_id == actor`): membership (+ capability); removes envelope and pending edit, no tombstone.
///
/// **Admin-branch**: a re-derived group admin; also writes a `type='delete'` tombstone.
pub async fn apply_delete_message(
    conn: &Connection,
    authed: Option<&str>,
    body: &DeleteMessageBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.actor_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };

    let is_self_delete = body.msg_sender_id.as_deref() == Some(actor.as_str());

    if is_self_delete {
        // Self-branch authz is membership; skipped on the no-auth path.
        if authed.is_some() && !is_member(conn, &body.conversation_id, &actor).await? {
            return Ok(WriteOutcome::Forbidden);
        }
        // ...and, since #1086, capability PROOF; legacy rows keep the membership-only path.
        if check_delete_capability(
            conn,
            &body.conversation_id,
            &body.message_id,
            body.delete_token.as_deref(),
        )
        .await?
            == DeleteCapability::Refused
        {
            return Ok(WriteOutcome::Forbidden);
        }
        // SCOPED to the authorized conversation, or membership anywhere would authorize deleting any envelope.
        let tx = conn.transaction().await?;
        tx.execute(
            "DELETE FROM message_envelope WHERE id = ?1 AND conversation_id = ?2",
            libsql::params![body.message_id.clone(), body.conversation_id.clone()],
        )
        .await?;
        tx.execute(
            "DELETE FROM message_envelope \
             WHERE target_message_id = ?1 AND type = 'edit' AND conversation_id = ?2",
            libsql::params![body.message_id.clone(), body.conversation_id.clone()],
        )
        .await?;
        tx.commit().await?;
        return Ok(WriteOutcome::Ok);
    }

    // Admin-delete: the actor must be an admin of the group owning this channel.
    // Skipped on the no-auth path (mirrors submit / writes.rs).
    if authed.is_some() {
        match channel_group_role(conn, &body.conversation_id, &actor).await? {
            Some(role) if role == "admin" => {}
            _ => return Ok(WriteOutcome::Forbidden),
        }
    }

    let tombstone_id = Ulid::new().to_string();
    // #1087: a tombstone takes the next seq, above every cursor; `sent_at` is display-only.
    let now = now_rfc3339();
    // Scoped like the self-branch, or one group's admin could delete anywhere.
    let tx = conn.transaction().await?;
    tx.execute(
        "DELETE FROM message_envelope WHERE id = ?1 AND conversation_id = ?2",
        libsql::params![body.message_id.clone(), body.conversation_id.clone()],
    )
    .await?;
    tx.execute(
        "DELETE FROM message_envelope \
         WHERE target_message_id = ?1 AND type = 'edit' AND conversation_id = ?2",
        libsql::params![body.message_id.clone(), body.conversation_id.clone()],
    )
    .await?;
    insert_envelope_with_seq(
        &tx,
        &body.conversation_id,
        "id, sender_id, ciphertext, sent_at, type, target_message_id",
        "?3, ?4, '', ?5, 'delete', ?6",
        vec![
            tombstone_id.into(),
            actor.into(),
            now.into(),
            body.message_id.clone().into(),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

pub async fn add_reaction(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<AddReaction>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<AddReaction>(apply_add_reaction(&conn, authed.as_deref(), &parsed.0).await?)
}

pub async fn remove_reaction(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<RemoveReaction>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<RemoveReaction>(apply_remove_reaction(&conn, authed.as_deref(), &parsed.0).await?)
}

/// Resolve the conversation a reaction is to be gated against, and refuse if it
/// cannot be established (#1161).
///
/// Envelope GC (#689) collects rows fast, so gating only on the envelope used to skip the check.
///
/// `ReactionBody.conversation_id` (from the client's local copy) fills the gap:
///
///   * envelope present → its `conversation_id` is authoritative, and a body
///     that declares a DIFFERENT one is a lie, refused rather than ignored (it
///     would otherwise ask the membership question about a conversation the
///     actor is in, on behalf of a message in one they are not);
///   * envelope gone, body declares one → gate on that;
///   * neither → refuse (fail closed; the attacker composes the body).
///
/// `is_member`, not `in_desired_roster`: reactions have no pre-acceptance step.
async fn reaction_conversation(
    conn: &Connection,
    body: &ReactionBody,
) -> anyhow::Result<Option<String>> {
    match conversation_for_message(conn, &body.message_id).await? {
        Some(envelope_conv) => match body.conversation_id.as_deref() {
            Some(declared) if declared != envelope_conv => Ok(None),
            _ => Ok(Some(envelope_conv)),
        },
        None => Ok(body.conversation_id.clone().filter(|c| !c.is_empty())),
    }
}

/// A reaction is membership-gated against [`reaction_conversation`].
pub async fn apply_add_reaction(
    conn: &Connection,
    authed: Option<&str>,
    body: &ReactionBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    let conversation = reaction_conversation(conn, body).await?;
    if authed.is_some() {
        let Some(conv) = conversation.as_deref() else {
            return Ok(WriteOutcome::Forbidden);
        };
        if !is_member(conn, conv, &user).await? {
            return Ok(WriteOutcome::Forbidden);
        }
    }
    // Records the gated conversation (#1433) for the read gate after GC.
    let id = Ulid::new().to_string();
    let now = now_rfc3339();
    conn.execute(
        "INSERT INTO message_reaction (id, message_id, user_id, emoji, created_at, conversation_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
         ON CONFLICT(message_id, user_id, emoji) DO UPDATE \
            SET conversation_id = excluded.conversation_id \
          WHERE message_reaction.conversation_id IS NULL",
        libsql::params![id, body.message_id.clone(), user, body.emoji.clone(), now, conversation],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

pub async fn apply_remove_reaction(
    conn: &Connection,
    authed: Option<&str>,
    body: &ReactionBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    // The DELETE is scoped to `user_id = :user`, so it can never touch another's row.
    //
    // Unlike add (#1161) this does not fail closed: removal unwrites only the actor's own row.
    if authed.is_some() {
        if let Some(conv) = reaction_conversation(conn, body).await? {
            if !is_member(conn, &conv, &user).await? {
                return Ok(WriteOutcome::Forbidden);
            }
        }
    }
    conn.execute(
        "DELETE FROM message_reaction WHERE message_id = ?1 AND user_id = ?2 AND emoji = ?3",
        libsql::params![body.message_id.clone(), user, body.emoji.clone()],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

pub async fn advance_watermark(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    // The identity gate, not the user gate: the row is keyed on the DEVICE, so
    // the device half must come from the verified signature too.
    let (authed, parsed) = match gate_identity_and_parse::<WatermarkBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let identity = authed.as_ref().map(|(u, d)| (u.as_str(), d.as_str()));
    outcome_response::<WatermarkBody>(apply_advance_watermark(&conn, identity, &parsed).await?)
}

/// Monotone UPSERT of the signer's own `(conversation, user, device)` watermark, as a member, via [`check_cursor_stamp`].
///
/// `reported_at` is stamped on EVERY report (#720 liveness), apart from the cursor so a quiet live device still pins.
pub async fn apply_advance_watermark(
    conn: &Connection,
    authed: Option<(&str, &str)>,
    body: &WatermarkBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed.map(|(u, _)| u), body.user_id.as_deref()) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    // The device half: the signing device when authenticated (a body naming a
    // different device is a 403, mirroring `report_commit_since`), the body's
    // on the no-auth path (empty → nothing to key the row on).
    let device = match authed {
        Some((_, d)) => {
            if body.device_id != d {
                return Ok(WriteOutcome::Forbidden);
            }
            d.to_string()
        }
        None => {
            if body.device_id.is_empty() {
                return Ok(WriteOutcome::Forbidden);
            }
            body.device_id.clone()
        }
    };
    if authed.is_some() && !is_member(conn, &body.conversation_id, &user).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    if let Err(refused) = admit_last_fetched_at(&body.last_fetched_at) {
        return Ok(refused);
    }
    // Both columns take `MAX(existing, reported)`; `last_seq` is what delivery and retention read (#1087).
    //
    // A negative `last_seq` is a bug or probe: refuse, don't clamp.
    if body.last_seq.is_some_and(|s| s < 0) {
        return Ok(WriteOutcome::Invalid("last_seq must not be negative"));
    }
    conn.execute(
        "INSERT INTO conversation_watermark \
             (conversation_id, user_id, device_id, last_fetched_at, last_seq, reported_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, datetime('now')) \
         ON CONFLICT(conversation_id, user_id, device_id) DO UPDATE SET \
             last_fetched_at = MAX(last_fetched_at, excluded.last_fetched_at), \
             last_seq = MAX(COALESCE(last_seq, 0), COALESCE(excluded.last_seq, 0)), \
             reported_at = datetime('now')",
        libsql::params![
            body.conversation_id.clone(),
            user,
            device,
            body.last_fetched_at.clone(),
            body.last_seq,
        ],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

pub async fn envelope_gc(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<EnvelopeGcBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    let stale = watermark_stale_modifier();
    outcome_response::<EnvelopeGcBody>(apply_envelope_gc(&conn, authed.as_deref(), &parsed, &stale).await?)
}

/// Run the watermark-gated envelope GC for a SINGLE conversation on demand.
/// Authz: the actor is a current member (skipped on the no-auth path).
///
/// Same predicate and `stale` window as [`sweep_envelope_gc`], via [`cleanup_conversation_envelopes`].
pub async fn apply_envelope_gc(
    conn: &Connection,
    authed: Option<&str>,
    body: &EnvelopeGcBody,
    stale: &str,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.actor_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };
    if authed.is_some() && !is_member(conn, &body.conversation_id, &actor).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    cleanup_conversation_envelopes(conn, &body.conversation_id, body.is_dm, stale).await?;
    Ok(WriteOutcome::Ok)
}

/// Run the watermark-gated cleanup for ONE conversation. Pure DB effect, no
/// authz — callers gate. The single place the `CLEANUP_*` predicate is executed,
/// shared by the per-conversation [`apply_envelope_gc`] endpoint and the
/// server-side [`sweep_envelope_gc`], so the two can never drift — including on
/// the `?2` device-liveness window, which both pass through here (#720).
async fn cleanup_conversation_envelopes(
    conn: &Connection,
    conversation_id: &str,
    is_dm: bool,
    stale: &str,
) -> anyhow::Result<()> {
    let sql = if is_dm {
        CLEANUP_DM_ENVELOPES
    } else {
        CLEANUP_CHANNEL_ENVELOPES
    };
    conn.execute(
        sql,
        libsql::params![conversation_id.to_string(), stale.to_string()],
    )
    .await?;
    Ok(())
}

/// The report a sweep returns: how many conversations it visited, plus the
/// identity-free growth snapshot it gathered on the same walk.
#[derive(Clone, Debug, Default)]
pub struct SweepReport {
    pub visited: usize,
    pub metrics: RetentionSnapshot,
}

// Aged-out records (#762): run on the same sweep as envelope GC.

/// Days a `security_event` row is retained. Default 90.
///
/// A per-user audit trail: long enough for a late investigation, not a standing profile.
///
/// `0` disables the bound (retain forever), for an operator who would rather keep
/// the trail than bound it.
pub fn security_event_retention_days() -> u32 {
    std::env::var("POLLIS_DS_SECURITY_EVENT_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(90)
}

/// Days an unrefreshed `push_token` row is retained. Default 180.
///
/// Clients re-register on launch, so `updated_at` tracks liveness; a reaped token costs one missed push.
///
/// `0` disables the reap.
pub fn push_token_retention_days() -> u32 {
    std::env::var("POLLIS_DS_PUSH_TOKEN_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(180)
}

/// Delete `security_event` rows older than [`security_event_retention_days`] and
/// `push_token` rows unrefreshed for longer than [`push_token_retention_days`].
/// Returns `(security_events_deleted, push_tokens_deleted)`.
///
/// Age-keyed is safe here, unlike envelopes: the age is what's bounded.
///
/// **`conversation_watermark` is deliberately NOT reaped here** — see the module
/// docs and `docs/metadata-retention-policy.md`.
///
/// Windows are parameters resolved once in `main`, so tests needn't mutate env.
pub async fn sweep_aged_records(
    conn: &Connection,
    sec_days: u32,
    tok_days: u32,
) -> anyhow::Result<(u64, u64)> {
    let mut events = 0u64;
    if sec_days > 0 {
        let modifier = format!("-{sec_days} days");
        conn.execute(
            "DELETE FROM security_event WHERE created_at < datetime('now', ?1)",
            libsql::params![modifier.clone()],
        )
        .await?;
        events = conn.changes();
    }

    let mut tokens = 0u64;
    if tok_days > 0 {
        let modifier = format!("-{tok_days} days");
        conn.execute(
            "DELETE FROM push_token WHERE updated_at < datetime('now', ?1)",
            libsql::params![modifier.clone()],
        )
        .await?;
        tokens = conn.changes();
    }
    Ok((events, tokens))
}

/// The server-side envelope-GC trigger (#689): sweep every conversation that
/// still has envelopes and run the watermark-gated cleanup for each. `stale` is
/// the device-liveness window (`?2`, #720). Returns a [`SweepReport`] — the count
/// visited plus the identity-free growth metrics gathered on the SAME walk (#720
/// checkbox 1), so no second full scan is needed to emit them.
///
/// DM iff the id is in `dm_channel_member`; a misclassified id matches no roster and deletes nothing.
pub async fn sweep_envelope_gc(conn: &Connection, stale: &str) -> anyhow::Result<SweepReport> {
    let mut conversations: Vec<String> = Vec::new();
    {
        let mut rows = conn
            .query("SELECT DISTINCT conversation_id FROM message_envelope", ())
            .await?;
        while let Some(row) = rows.next().await? {
            conversations.push(row.get::<String>(0)?);
        }
    }
    // Identity-free metrics on the same walk, post-cleanup (#720).
    let mut metrics = RetentionSnapshot::default();
    for conversation_id in &conversations {
        let is_dm = {
            let mut rows = conn
                .query(
                    "SELECT 1 FROM dm_channel_member WHERE dm_channel_id = ?1 LIMIT 1",
                    libsql::params![conversation_id.clone()],
                )
                .await?;
            rows.next().await?.is_some()
        };
        cleanup_conversation_envelopes(conn, conversation_id, is_dm, stale).await?;

        let (count, oldest) = {
            let mut rows = conn
                .query(
                    "SELECT COUNT(*), MIN(sent_at) FROM message_envelope WHERE conversation_id = ?1",
                    libsql::params![conversation_id.clone()],
                )
                .await?;
            match rows.next().await? {
                Some(row) => (row.get::<i64>(0)?, row.get::<Option<String>>(1)?),
                None => (0, None),
            }
        };
        if count > 0 {
            metrics.total_envelopes += count;
            metrics.conversations_with_envelopes += 1;
            metrics.oldest_sent_at = min_opt(metrics.oldest_sent_at.take(), oldest.clone());
            if count > metrics.largest_conversation_envelopes {
                metrics.largest_conversation_envelopes = count;
                metrics.largest_conversation_oldest_sent_at = oldest;
            }
        }
    }
    // Reap orphaned `attachment_ref` rows (#690): hygiene only, as the count is derived.
    conn.execute(REAP_ORPHANED_ATTACHMENT_REFS_SQL, ()).await?;
    conn.execute(REAP_ORPHANED_VAULT_REFS_SQL, ()).await?;
    Ok(SweepReport {
        visited: conversations.len(),
        metrics,
    })
}

/// Lexical min of two optional timestamps, skipping `None` (mirrors SQLite's
/// NULL-skipping `MIN`). `sent_at` sorts lexically = chronologically here (see
/// [`now_rfc3339`]), so a string compare is the right "oldest".
fn min_opt(a: Option<String>, b: Option<String>) -> Option<String> {
    match (a, b) {
        (Some(a), Some(b)) => Some(if a <= b { a } else { b }),
        (Some(a), None) => Some(a),
        (None, b) => b,
    }
}

// The attachment reference count is DERIVED (#690): a declaration counts only while its envelope exists,
//
// so every envelope delete releases it, and releasing needs no authz of its own.
//
// Forged/orphaned declarations are inert, and reaped by [`sweep_envelope_gc`].
//
// The vault leg (#107): a `vault_attachment_ref` on a live `vault_message` counts the same way.
macro_rules! live_ref_exists {
    () => {
        "(EXISTS (SELECT 1 FROM attachment_ref ar \
                  JOIN message_envelope me ON me.id = ar.message_id \
                  WHERE ar.content_hash = ?1) \
          OR EXISTS (SELECT 1 FROM vault_attachment_ref var \
                     JOIN vault_message vm ON vm.id = var.vault_message_id \
                     WHERE var.content_hash = ?1))"
    };
}

/// `SELECT` form of [`live_ref_exists`] — yields a single `0`/`1` row for
/// [`object_is_referenced`].
const OBJECT_IS_REFERENCED_SQL: &str = concat!("SELECT ", live_ref_exists!());

/// Collect the shared dedup row iff NO live reference remains. One predicate, no
/// Rust-side count, so it holds under concurrent deletes.
const COLLECT_UNREFERENCED_OBJECT_SQL: &str = concat!(
    "DELETE FROM attachment_object WHERE content_hash = ?1 AND NOT ",
    live_ref_exists!()
);

/// Reap declaration rows whose message envelope is gone (GC'd, deleted, aged out,
/// or a forged id). Hygiene only: the count is already correct (see [`live_ref_exists`]).
const REAP_ORPHANED_ATTACHMENT_REFS_SQL: &str = "\
DELETE FROM attachment_ref \
 WHERE NOT EXISTS (SELECT 1 FROM message_envelope me WHERE me.id = attachment_ref.message_id)";

/// The vault leg of the same hygiene: reap reference rows whose vault entry is
/// gone (teardown or a crash); `/v1/vault/delete` already removes them inline.
const REAP_ORPHANED_VAULT_REFS_SQL: &str = "\
DELETE FROM vault_attachment_ref \
 WHERE NOT EXISTS (SELECT 1 FROM vault_message vm WHERE vm.id = vault_attachment_ref.vault_message_id)";

/// True when `message_id` names a STILL-EXISTING envelope the caller provably
/// wrote.
///
/// A deleted or GC'd envelope answers `false`.
///
/// Unsealed: `sender_id == user_id`. Sealed (#331): its deletion capability (#1086); no stored hash → refused.
async fn envelope_is_authored_by(
    conn: &Connection,
    message_id: &str,
    user_id: &str,
    delete_token: Option<&str>,
) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT sender_id, delete_token_hash FROM message_envelope WHERE id = ?1 LIMIT 1",
            libsql::params![message_id.to_string()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Ok(false);
    };
    let sender: String = row.get(0)?;
    // TEXT, as in `check_delete_capability`.
    let stored: Option<String> = row.get::<Option<String>>(1).ok().flatten();
    if sender == user_id {
        return Ok(true);
    }
    Ok(match (stored, delete_token) {
        (Some(stored), Some(token)) => capability_matches(&stored, token),
        _ => false,
    })
}

/// Does `token` hash to the stored capability `stored`? Compared as base64, in
/// constant time: both are fixed-length encodings of a 32-byte digest, so a
/// length difference is itself a mismatch rather than something to branch on.
fn capability_matches(stored: &str, token: &str) -> bool {
    use base64::Engine as _;
    use sha2::{Digest, Sha256};
    use subtle::ConstantTimeEq;
    let digest = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(token.as_bytes()));
    digest.as_bytes().ct_eq(stored.as_bytes()).into()
}

pub async fn register_attachment(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<AttachmentRegisterBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    outcome_response::<AttachmentRegisterBody>(apply_register_attachment(&conn, authed.as_deref(), &parsed).await?)
}

pub async fn delete_attachment(
    State(state): State<AppState>,
    req: RawRequest,
) -> Result<Response, AppError> {
    let (authed, parsed) = match gate_and_parse::<AttachmentDeleteBody>(&state, &req).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let conn = state.db.conn().await?;
    // The verified actor is passed and deliberately unused: the collect asks
    // only whether anything still references the object, never who is asking.
    outcome_response::<AttachmentDeleteBody>(apply_delete_attachment(&conn, authed.as_deref(), &parsed).await?)
}

/// Register an attachment object row (`content_hash → r2_key`) and, when a
/// `message_id` is supplied, a `(content_hash, message_id)` REFERENCE
/// declaration (#690). `content_hash` is the object's random file id since
/// #1382 — a name, not key material and not derived from the content.
///
/// ## Authz
///
/// The OBJECT row is content-addressed, so any authenticated device may register one.
///
/// The REFERENCE row pins a shared blob, so it is bound to the message's author (#1165, [`envelope_is_authored_by`]).
///
/// Skipped when `authed` is `None`: the no-auth (harness/dev) path has no actor
/// to bind to, exactly as the membership gates elsewhere in this module are.
///
/// Both INSERTs are `OR IGNORE`; the count tracks messages, not uploads.
pub async fn apply_register_attachment(
    conn: &Connection,
    authed: Option<&str>,
    body: &AttachmentRegisterBody,
) -> anyhow::Result<WriteOutcome> {
    // Decided BEFORE the tx, so a refusal writes nothing.
    if let (Some(actor), Some(message_id)) = (authed, body.message_id()) {
        if !envelope_is_authored_by(conn, message_id, actor, body.delete_token()).await? {
            return Ok(WriteOutcome::Forbidden);
        }
    }
    let tx = conn.transaction().await?;
    // No uploader recorded: lifetime depends only on live references (000032).
    tx.execute(
        "INSERT OR IGNORE INTO attachment_object (content_hash, r2_key) VALUES (?1, ?2)",
        libsql::params![body.content_hash().to_string(), body.r2_key().to_string()],
    )
    .await?;
    // The reference exists only on the send variant (#925).
    if let Some(message_id) = body.message_id() {
        // No registrar recorded either (000032).
        tx.execute(
            "INSERT OR IGNORE INTO attachment_ref (content_hash, message_id) VALUES (?1, ?2)",
            libsql::params![body.content_hash().to_string(), message_id.to_string()],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

/// COLLECT the shared `attachment_object` row once NO live reference remains
/// (#690, resolving the second `TODO(#419)`). Authz: any authenticated user —
/// deliberately, because this endpoint performs no destructive act that the
/// reference count did not already permit, for any caller.
///
/// It does NOT release a reference (Blocking 1); `message_id` is kept for wire-compat but UNUSED.
///
/// One identity-free conditional `DELETE`; collecting also runs [`clear_declarations`].
///
/// The R2 object is gated separately and by the SAME evidence: `/v1/r2/presign`
/// refuses to mint a `delete` while [`object_is_referenced`] holds
/// (`broker.rs`). Turso row and R2 blob are collected by one predicate, so
/// neither can outlive the other and the two cannot disagree.
pub async fn apply_delete_attachment(
    conn: &Connection,
    _authed: Option<&str>,
    body: &AttachmentDeleteBody,
) -> anyhow::Result<WriteOutcome> {
    // Collect only; the identity-free conditional DELETE is atomic against a concurrent reference.
    let collected = conn
        .execute(
            COLLECT_UNREFERENCED_OBJECT_SQL,
            libsql::params![body.content_hash.clone()],
        )
        .await?;
    if collected > 0 {
        clear_declarations(conn, &body.content_hash).await?;
    }
    Ok(WriteOutcome::Ok)
}

/// Drop every declaration naming `content_hash`, message-side and vault-side.
///
/// Only after the object row is collected: stale declarations would refuse the PUT that restores it.
async fn clear_declarations(conn: &Connection, content_hash: &str) -> anyhow::Result<()> {
    conn.execute(
        "DELETE FROM attachment_ref WHERE content_hash = ?1",
        libsql::params![content_hash.to_string()],
    )
    .await?;
    conn.execute(
        "DELETE FROM vault_attachment_ref WHERE content_hash = ?1",
        libsql::params![content_hash.to_string()],
    )
    .await?;
    Ok(())
}

/// True when at least one STILL-EXISTING message references the attachment
/// object `content_hash` (#690) — the count derived by joining
/// `attachment_ref` to `message_envelope` (see [`live_ref_exists`]), never the
/// raw declaration rows. Gates both the Turso collect and the R2 `delete` presign.
pub async fn object_is_referenced(conn: &Connection, content_hash: &str) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            OBJECT_IS_REFERENCED_SQL,
            libsql::params![content_hash.to_string()],
        )
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get::<i64>(0)? != 0,
        None => false,
    })
}

#[cfg(test)]
mod timestamp_tests {

    /// The exact shape a client writes for `sent_at` (`pollis-core` sends
    /// `chrono::Utc::now().to_rfc3339()`, i.e. `SecondsFormat::AutoSi`).
    fn client_stamp(nanos: u32) -> String {
        chrono::DateTime::from_timestamp(1_800_000_000, nanos)
            .expect("valid instant")
            .to_rfc3339()
    }

    /// The regression: a DS timestamp must sort ABOVE a client timestamp taken
    /// earlier in the same wall-clock second (`'+'` < `'.'`).
    #[test]
    fn a_ds_stamp_sorts_above_a_client_stamp_from_the_same_second() {
        let client = client_stamp(1);
        let ds = chrono::DateTime::from_timestamp(1_800_000_000, 500_000_000)
            .expect("valid instant")
            .to_rfc3339_opts(chrono::SecondsFormat::Nanos, false);
        assert!(
            ds > client,
            "DS stamp {ds} must sort above same-second client stamp {client}"
        );

        // What the old formatter produced, pinned as the thing we must not regress to.
        let whole_second = "2027-01-15T08:00:00+00:00";
        assert!(
            whole_second < client.as_str(),
            "a whole-second stamp sorts below a sub-second one — this is the bug"
        );
    }

    /// Lexical order must match chronological order across every fraction width
    /// chrono's `AutoSi` can emit (0, 3, 6, 9 digits), because both formats live
    /// in the same column and the watermark only ever compares them as text.
    #[test]
    fn lexical_order_matches_chronological_order_across_precisions() {
        let stamps = [
            client_stamp(0),
            client_stamp(1_000_000),
            client_stamp(1_001_000),
            client_stamp(1_001_001),
            client_stamp(999_999_999),
        ];
        for pair in stamps.windows(2) {
            assert!(
                pair[0] < pair[1],
                "{} must sort below {}",
                pair[0],
                pair[1]
            );
        }
    }

}

#[cfg(test)]
mod gc_sql_tests {
    //! The envelope-GC cleanup SQL, driven directly against a real (in-memory)
    //! libsql DB. Two invariants are encoded here.
    //!
    //! **I3**: an envelope far older than 30 days that a member has not collected must survive.
    //!
    //! **#685**: a revoked device (stale row or no row) must not wedge deletion.

    use super::*;


    /// A staleness window wide enough that none of the legacy fixtures (which seed
    /// `reported_at = NULL`) are ever excluded.
    const TEST_STALE: &str = "-6 months";

    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        // Production Turso has FKs off; libsql's local backend turns them on.
        conn.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        pollis_schema::apply::single_db(&conn).await.expect("schema");
        conn
    }

    /// Run one `CLEANUP_*` statement with the standard test staleness window.
    async fn run_cleanup(conn: &Connection, sql: &str, conv: &str) {
        conn.execute(sql, libsql::params![conv.to_string(), TEST_STALE.to_string()])
            .await
            .unwrap();
    }

    async fn add_device(conn: &Connection, user: &str, device: &str, revoked: bool) {
        conn.execute(
            "INSERT INTO user_device (user_id, device_id, revoked_at) \
             VALUES (?1, ?2, CASE WHEN ?3 THEN datetime('now') ELSE NULL END)",
            libsql::params![user.to_string(), device.to_string(), revoked as i64],
        )
        .await
        .unwrap();
    }

    /// Seed a watermark row whose cursor is the delivery sequence `seq` (#1087).
    /// `reported_at` is left NULL — "report time unknown", treated as live — so
    /// these fixtures keep their pre-#720 meaning.
    async fn seed_watermark(conn: &Connection, conv: &str, user: &str, device: &str, seq: i64) {
        conn.execute(
            "INSERT INTO conversation_watermark \
                 (conversation_id, user_id, device_id, last_fetched_at, last_seq) \
             VALUES (?1, ?2, ?3, datetime('now'), ?4)",
            libsql::params![conv.to_string(), user.to_string(), device.to_string(), seq],
        )
        .await
        .unwrap();
    }

    /// Seed a watermark row with BOTH the delivery cursor (`last_seq`) and the
    /// wall-clock report time (`reported_at`): a position and a time, kept distinct (#720).
    async fn seed_watermark_reported(
        conn: &Connection,
        conv: &str,
        user: &str,
        device: &str,
        seq: i64,
        reported: &str,
    ) {
        conn.execute(
            "INSERT INTO conversation_watermark \
                 (conversation_id, user_id, device_id, last_fetched_at, last_seq, reported_at) \
             VALUES (?1, ?2, ?3, datetime('now'), ?4, datetime('now', ?5))",
            libsql::params![
                conv.to_string(),
                user.to_string(),
                device.to_string(),
                seq,
                reported.to_string()
            ],
        )
        .await
        .unwrap();
    }

    /// Insert one envelope at delivery sequence `seq`.
    async fn add_envelope(conn: &Connection, id: &str, conv: &str, seq: i64) {
        conn.execute(
            "INSERT INTO message_envelope \
                 (id, conversation_id, sent_at, sender_id, ciphertext, seq) \
             VALUES (?1, ?2, datetime('now'), 'sender', 'ct', ?3)",
            libsql::params![id.to_string(), conv.to_string(), seq],
        )
        .await
        .unwrap();
        // Keep `conversation_seq` consistent, as `insert_envelope_with_seq` does.
        conn.execute(
            "INSERT INTO conversation_seq (conversation_id, next_seq) VALUES (?1, ?2) \
             ON CONFLICT(conversation_id) DO UPDATE SET next_seq = MAX(next_seq, excluded.next_seq)",
            libsql::params![conv.to_string(), seq],
        )
        .await
        .unwrap();
    }

    async fn envelope_count(conn: &Connection, conv: &str) -> i64 {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM message_envelope WHERE conversation_id = ?1",
                libsql::params![conv.to_string()],
            )
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    async fn channel_fixture(conn: &Connection) {
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice')", ())
            .await
            .unwrap();
        // Registry claim first: 000017's guard triggers refuse unclaimed rows (#948).
        conn.execute_batch(
            "INSERT INTO conversation (id, kind) VALUES ('c1', 'channel');
             INSERT INTO channels (id, group_id, name) VALUES ('c1', 'g1', 'chan');",
        )
        .await
        .unwrap();
    }

    /// A revoked device with a STALE watermark row must not keep an envelope the
    /// live device has already read past.
    #[tokio::test]
    async fn channel_stale_revoked_watermark_does_not_pin_gc() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        add_device(&conn, "alice", "a-live", false).await;
        add_device(&conn, "alice", "a-old", true).await;
        // Live cursor is recent; the revoked device is stuck 10 days back.
        seed_watermark(&conn, "c1", "alice", "a-live", 10).await;
        seed_watermark(&conn, "c1", "alice", "a-old", 5).await;
        // The envelope sits between them: above the stale cursor, below the live one.
        add_envelope(&conn, "e1", "c1", 7).await;

        run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "c1").await;

        assert_eq!(
            envelope_count(&conn, "c1").await,
            0,
            "envelope below the LIVE device's cursor must be pruned; the revoked \
             device's stale watermark must not pin MIN(cw) (#685)"
        );
    }

    /// A revoked device with NO watermark row must not disable pruning via the
    /// `COUNT(ud) != COUNT(cw)` "every device reported" check.
    #[tokio::test]
    async fn channel_missing_revoked_watermark_does_not_disable_gc() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        add_device(&conn, "alice", "a-live", false).await;
        add_device(&conn, "alice", "a-old", true).await;
        // Only the live device has a watermark; the revoked device has none.
        seed_watermark(&conn, "c1", "alice", "a-live", 10).await;
        add_envelope(&conn, "e1", "c1", 7).await;

        run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "c1").await;

        assert_eq!(
            envelope_count(&conn, "c1").await,
            0,
            "with the revoked device excluded, COUNT(ud) == COUNT(cw) and the \
             watermark gate prunes; its absent row must not disable GC (#685)"
        );
    }

    async fn dm_fixture(conn: &Connection) {
        conn.execute("INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('d1', 'alice', 'creator')", ())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn dm_stale_revoked_watermark_does_not_pin_gc() {
        let conn = conn().await;
        dm_fixture(&conn).await;
        add_device(&conn, "alice", "a-live", false).await;
        add_device(&conn, "alice", "a-old", true).await;
        seed_watermark(&conn, "d1", "alice", "a-live", 10).await;
        seed_watermark(&conn, "d1", "alice", "a-old", 5).await;
        add_envelope(&conn, "e1", "d1", 7).await;

        run_cleanup(&conn, CLEANUP_DM_ENVELOPES, "d1").await;

        assert_eq!(
            envelope_count(&conn, "d1").await,
            0,
            "DM: the revoked device's stale watermark must not pin MIN(cw) (#685)"
        );
    }

    #[tokio::test]
    async fn dm_missing_revoked_watermark_does_not_disable_gc() {
        let conn = conn().await;
        dm_fixture(&conn).await;
        add_device(&conn, "alice", "a-live", false).await;
        add_device(&conn, "alice", "a-old", true).await;
        seed_watermark(&conn, "d1", "alice", "a-live", 10).await;
        add_envelope(&conn, "e1", "d1", 7).await;

        run_cleanup(&conn, CLEANUP_DM_ENVELOPES, "d1").await;

        assert_eq!(
            envelope_count(&conn, "d1").await,
            0,
            "DM: the revoked device's absent watermark must not disable GC (#685)"
        );
    }

    /// **The regression test for F3.** A LIVE member device whose cursor sits
    /// below an envelope must keep that envelope alive no matter how old it is.
    /// The envelope is 400 days old, far past the deleted 30-day TTL (F3).
    #[tokio::test]
    async fn no_ttl_channel_uncollected_ancient_envelope_survives() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob')", ())
            .await
            .unwrap();
        add_device(&conn, "alice", "a1", false).await;
        add_device(&conn, "bob", "b1", false).await;
        // Alice is fully caught up. Bob's device has been offline for 500 days —
        // its cursor is BELOW the envelope, so the envelope is still owed to it.
        seed_watermark(&conn, "c1", "alice", "a1", 10).await;
        seed_watermark(&conn, "c1", "bob", "b1", 1).await;
        add_envelope(&conn, "ancient", "c1", 2).await;

        run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "c1").await;

        assert_eq!(
            envelope_count(&conn, "c1").await,
            1,
            "a 400-day-old envelope BELOW a live member device's cursor must \
             survive — retention is bounded by the slowest member device, never by \
             wall-clock age (I3/F3). A TTL arm would have deleted it."
        );
    }

    /// The same property with the other never-collected shape: the absent member
    /// device has never reported a watermark AT ALL. `COUNT(ud) != COUNT(cw)` →
    /// CASE NULL → nothing deleted, however old the envelope is.
    #[tokio::test]
    async fn no_ttl_channel_never_reported_device_holds_ancient_envelope() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob')", ())
            .await
            .unwrap();
        add_device(&conn, "alice", "a1", false).await;
        add_device(&conn, "bob", "b-silent", false).await;
        // Only alice has ever reported; bob's device has no watermark row.
        seed_watermark(&conn, "c1", "alice", "a1", 10).await;
        add_envelope(&conn, "ancient", "c1", 2).await;

        run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "c1").await;

        assert_eq!(
            envelope_count(&conn, "c1").await,
            1,
            "a member device that has never reported must hold even a 400-day-old \
             envelope: no watermark is no evidence of delivery (I3/F3)"
        );
    }

    /// DM shape, same regression.
    #[tokio::test]
    async fn no_ttl_dm_uncollected_ancient_envelope_survives() {
        let conn = conn().await;
        dm_fixture(&conn).await;
        conn.execute("INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('d1', 'bob', 'creator')", ())
            .await
            .unwrap();
        add_device(&conn, "alice", "a1", false).await;
        add_device(&conn, "bob", "b1", false).await;
        seed_watermark(&conn, "d1", "alice", "a1", 10).await;
        seed_watermark(&conn, "d1", "bob", "b1", 1).await;
        add_envelope(&conn, "ancient", "d1", 2).await;

        run_cleanup(&conn, CLEANUP_DM_ENVELOPES, "d1").await;

        assert_eq!(
            envelope_count(&conn, "d1").await,
            1,
            "DM: a 400-day-old envelope below the slowest member device's cursor \
             must survive (I3/F3)"
        );
    }

    /// GC is not simply disabled: once EVERY current member device has collected
    /// past an envelope, it goes — including one far younger than the old TTL, so
    /// this cannot be passing via the deleted arm.
    #[tokio::test]
    async fn channel_envelope_is_deleted_once_every_device_collected_past_it() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob')", ())
            .await
            .unwrap();
        add_device(&conn, "alice", "a1", false).await;
        add_device(&conn, "alice", "a2", false).await;
        add_device(&conn, "bob", "b1", false).await;
        // Every device's cursor is strictly above `collected`, and strictly below
        // `pending` — so exactly one of the two envelopes may go.
        add_envelope(&conn, "collected", "c1", 8).await;
        add_envelope(&conn, "pending", "c1", 11).await;
        seed_watermark(&conn, "c1", "alice", "a1", 9).await;
        seed_watermark(&conn, "c1", "alice", "a2", 9).await;
        seed_watermark(&conn, "c1", "bob", "b1", 9).await;

        run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "c1").await;

        assert_eq!(
            envelope_count(&conn, "c1").await,
            1,
            "the envelope every member device collected must be pruned, and the \
             one still above the floor must remain — GC is bounded, not disabled"
        );
        let mut rows = conn
            .query("SELECT id FROM message_envelope WHERE conversation_id = 'c1'", ())
            .await
            .unwrap();
        let survivor = rows.next().await.unwrap().unwrap().get::<String>(0).unwrap();
        assert_eq!(survivor, "pending", "the wrong envelope was pruned");
    }

    /// The slowest device sets the floor: one device racing ahead must not raise
    /// it and evict mail a sibling device still needs.
    #[tokio::test]
    async fn channel_floor_is_the_minimum_not_the_maximum() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        add_device(&conn, "alice", "a-fast", false).await;
        add_device(&conn, "alice", "a-slow", false).await;
        seed_watermark(&conn, "c1", "alice", "a-fast", 11).await;
        seed_watermark(&conn, "c1", "alice", "a-slow", 6).await;
        // Above the slow cursor, below the fast one: MIN keeps it, MAX would not.
        add_envelope(&conn, "between", "c1", 8).await;

        run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "c1").await;

        assert_eq!(
            envelope_count(&conn, "c1").await,
            1,
            "the floor must be MIN over member devices — the fast device's cursor \
             must not evict what the slow one has yet to collect"
        );
    }

    /// A conversation with ZERO current member devices deletes NOTHING. The
    /// aggregate yields NULL; an empty roster is not "everybody collected".
    #[tokio::test]
    async fn an_empty_member_device_set_deletes_nothing() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        // A member row exists but the member has no device at all.
        add_envelope(&conn, "ancient", "c1", 2).await;
        add_envelope(&conn, "fresh", "c1", 11).await;

        run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "c1").await;

        assert_eq!(
            envelope_count(&conn, "c1").await,
            2,
            "an empty member-device set must delete NOTHING, not everything"
        );
    }

    /// The same, one step further: the conversation id resolves to no membership
    /// row whatsoever (unknown/deleted conversation).
    #[tokio::test]
    async fn an_unknown_conversation_deletes_nothing() {
        let conn = conn().await;
        add_envelope(&conn, "ancient", "ghost", 2).await;

        run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "ghost").await;
        run_cleanup(&conn, CLEANUP_DM_ENVELOPES, "ghost").await;

        assert_eq!(
            envelope_count(&conn, "ghost").await,
            1,
            "a conversation with no resolvable membership must delete nothing"
        );
    }

    /// The revoked-device exclusion must not be able to *manufacture* an empty
    /// roster that deletes: when EVERY device is revoked the roster is empty, and
    /// an empty roster deletes nothing.
    #[tokio::test]
    async fn an_all_revoked_roster_deletes_nothing() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        add_device(&conn, "alice", "a-old", true).await;
        seed_watermark(&conn, "c1", "alice", "a-old", 1).await;
        add_envelope(&conn, "ancient", "c1", 2).await;

        run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "c1").await;

        assert_eq!(
            envelope_count(&conn, "c1").await,
            1,
            "excluding revoked devices must never leave a roster that deletes by \
             virtue of being empty"
        );
    }

    /// The GC trigger is a DS-internal sweep, not a member's ingest path. With NO
    /// member calling the per-conversation endpoint, one sweep collects every
    /// conversation whose devices have caught up (a channel AND a DM) and spares an uncollected one.
    #[tokio::test]
    async fn sweep_collects_quiet_conversations_without_any_ingest() {
        let conn = conn().await;

        // Distinct users per conversation: `user_device` is global.
        channel_fixture(&conn).await;
        add_device(&conn, "alice", "a1", false).await;
        seed_watermark(&conn, "c1", "alice", "a1", 10).await;
        add_envelope(&conn, "eA", "c1", 7).await;

        // A DM whose one member device has caught up (exercises the DM predicate).
        conn.execute("INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('d1', 'dave', 'creator')", ())
            .await
            .unwrap();
        add_device(&conn, "dave", "d-dev", false).await;
        seed_watermark(&conn, "d1", "dave", "d-dev", 10).await;
        add_envelope(&conn, "eD", "d1", 7).await;

        // A channel with an uncollected recipient: bob's device never reported.
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g2', 'bob')", ())
            .await
            .unwrap();
        conn.execute_batch(
            "INSERT INTO conversation (id, kind) VALUES ('c2', 'channel');
             INSERT INTO channels (id, group_id, name) VALUES ('c2', 'g2', 'chan');",
        )
        .await
        .unwrap();
        add_device(&conn, "bob", "b1", false).await;
        add_envelope(&conn, "eB", "c2", 7).await;

        // No member ingests; the sweep is the sole trigger.
        let report = sweep_envelope_gc(&conn, TEST_STALE).await.unwrap();

        assert_eq!(report.visited, 3, "the sweep visits every conversation that still has envelopes");
        assert_eq!(envelope_count(&conn, "c1").await, 0, "quiet channel, all caught up → swept");
        assert_eq!(envelope_count(&conn, "d1").await, 0, "quiet DM, all caught up → swept");
        assert_eq!(
            envelope_count(&conn, "c2").await,
            1,
            "a conversation with an uncollected recipient must NOT be swept"
        );

        // The growth snapshot is gathered on the SAME walk (#720 checkbox 1) and
        // reflects POST-cleanup survivors: only c2's uncollected envelope remains.
        assert_eq!(report.metrics.total_envelopes, 1, "one envelope survives GC (eB in c2)");
        assert_eq!(report.metrics.conversations_with_envelopes, 1, "held by one conversation");
        assert_eq!(report.metrics.largest_conversation_envelopes, 1, "worst offender holds one");
    }

    // #720 device-liveness bound: each test asserts a collection the pre-#720 predicate never performs.

    /// A dormant device — one that reported a watermark long ago and then went
    /// silent — stops pinning after the staleness window: its envelope is
    /// collected once every LIVE device has read past it.
    #[tokio::test]
    async fn channel_dormant_device_stops_pinning_after_the_bound() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob')", ())
            .await
            .unwrap();
        add_device(&conn, "alice", "a1", false).await;
        add_device(&conn, "bob", "b1", false).await;
        // Alice is live and caught up. Bob reported 400 days ago (past the 6-month
        // window) and his cursor is stuck 500 days back — dormant.
        seed_watermark_reported(&conn, "c1", "alice", "a1", 10, "-1 day").await;
        seed_watermark_reported(&conn, "c1", "bob", "b1", 1, "-400 days").await;
        // The envelope sits above bob's stuck cursor but below alice's.
        add_envelope(&conn, "e1", "c1", 3).await;

        run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "c1").await;

        assert_eq!(
            envelope_count(&conn, "c1").await,
            0,
            "a device silent past the window stops pinning; the envelope every LIVE \
             device read past is collected (#720). Pre-change bob pins it forever."
        );
    }

    /// **The anti-F3 test — the important one.** A device that reported RECENTLY
    /// still pins every envelope below its cursor, no matter how ANCIENT the
    /// envelope is: liveness is `reported_at`, never the message's age or the
    /// cursor's age. Both legs share one fixture and differ ONLY in `reported_at`,
    /// so the outcome provably pivots on liveness alone.
    #[tokio::test]
    async fn channel_recent_report_pins_ancient_envelope_but_dormant_does_not() {
        // Leg 1 — bob reported yesterday, cursor and envelope both ancient: PINS.
        {
            let conn = conn().await;
            channel_fixture(&conn).await;
            conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob')", ())
                .await
                .unwrap();
            add_device(&conn, "alice", "a1", false).await;
            add_device(&conn, "bob", "b1", false).await;
            seed_watermark_reported(&conn, "c1", "alice", "a1", 10, "-1 day").await;
            // Cursor 500 days back, but reported ONE day ago — live.
            seed_watermark_reported(&conn, "c1", "bob", "b1", 1, "-1 day").await;
            add_envelope(&conn, "ancient", "c1", 2).await;

            run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "c1").await;

            assert_eq!(
                envelope_count(&conn, "c1").await,
                1,
                "a 400-day-old envelope below a RECENTLY-REPORTED device's cursor \
                 must survive — age is not dormancy (anti-F3, #720)"
            );
        }
        // Leg 2 — identical, except bob reported 400 days ago: stale → COLLECTED.
        {
            let conn = conn().await;
            channel_fixture(&conn).await;
            conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob')", ())
                .await
                .unwrap();
            add_device(&conn, "alice", "a1", false).await;
            add_device(&conn, "bob", "b1", false).await;
            seed_watermark_reported(&conn, "c1", "alice", "a1", 10, "-1 day").await;
            // Same cursor, but reported 400 days ago — dormant.
            seed_watermark_reported(&conn, "c1", "bob", "b1", 1, "-400 days").await;
            add_envelope(&conn, "ancient", "c1", 2).await;

            run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "c1").await;

            assert_eq!(
                envelope_count(&conn, "c1").await,
                0,
                "flipping ONLY reported_at to stale collects the same envelope: the \
                 outcome pivots on liveness, not age (#720). Fails pre-change."
            );
        }
    }

    /// A `reported_at IS NULL` device (never stamped — a pre-migration row, or a
    /// device with no watermark row at all) is treated as LIVE and keeps pinning:
    /// unknown report time is not evidence of dormancy (fail-closed). Guards
    /// against a staleness arm that would exclude NULLs and drop mail on rollout.
    #[tokio::test]
    async fn channel_null_reported_at_still_pins() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob')", ())
            .await
            .unwrap();
        add_device(&conn, "alice", "a1", false).await;
        add_device(&conn, "bob", "b1", false).await;
        seed_watermark_reported(&conn, "c1", "alice", "a1", 10, "-1 day").await;
        // Legacy row: cursor set, reported_at NULL.
        seed_watermark(&conn, "c1", "bob", "b1", 1).await;
        add_envelope(&conn, "e1", "c1", 3).await;

        run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "c1").await;

        assert_eq!(
            envelope_count(&conn, "c1").await,
            1,
            "a NULL reported_at must pin (fail-closed) — unknown ≠ dormant (#720)"
        );
    }

    /// Whole-roster dormancy must not manufacture an empty roster that deletes:
    /// when EVERY device is stale, the empty roster deletes nothing.
    #[tokio::test]
    async fn an_all_stale_roster_deletes_nothing() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        add_device(&conn, "alice", "a1", false).await;
        seed_watermark_reported(&conn, "c1", "alice", "a1", 3, "-400 days").await;
        add_envelope(&conn, "ancient", "c1", 4).await;

        run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "c1").await;

        assert_eq!(
            envelope_count(&conn, "c1").await,
            1,
            "an all-stale roster is empty, and an empty roster deletes nothing — \
             staleness must not wipe a fully-dormant conversation (#720)"
        );
    }

    /// The DM predicate carries the same bound (the `?2` arm is `AND`-ed onto the
    /// DM roster's existing `WHERE`).
    #[tokio::test]
    async fn dm_dormant_device_stops_pinning_after_the_bound() {
        let conn = conn().await;
        dm_fixture(&conn).await;
        conn.execute("INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('d1', 'bob', 'creator')", ())
            .await
            .unwrap();
        add_device(&conn, "alice", "a1", false).await;
        add_device(&conn, "bob", "b1", false).await;
        seed_watermark_reported(&conn, "d1", "alice", "a1", 10, "-1 day").await;
        seed_watermark_reported(&conn, "d1", "bob", "b1", 1, "-400 days").await;
        add_envelope(&conn, "e1", "d1", 3).await;

        run_cleanup(&conn, CLEANUP_DM_ENVELOPES, "d1").await;

        assert_eq!(
            envelope_count(&conn, "d1").await,
            0,
            "DM: a device silent past the window stops pinning (#720)"
        );
    }

    /// Insert an attachment object + a `(content_hash, message_id)`
    /// declaration keyed to `msg`.
    async fn add_attachment(conn: &Connection, hash: &str, msg: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO attachment_object (content_hash, r2_key) VALUES (?1, ?2)",
            libsql::params![hash.to_string(), format!("media/{hash}/f.enc")],
        )
        .await
        .unwrap();
        conn.execute(
            "INSERT INTO attachment_ref (content_hash, message_id) VALUES (?1, ?2)",
            libsql::params![hash.to_string(), msg.to_string()],
        )
        .await
        .unwrap();
    }

    async fn raw_ref_count(conn: &Connection, hash: &str) -> i64 {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM attachment_ref WHERE content_hash = ?1",
                libsql::params![hash.to_string()],
            )
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    /// The DS-internal GC sweep releases a swept message's attachment reference:
    /// once the envelope is gone the object is no longer referenced (collectable),
    /// and the orphaned declaration row is reaped so `attachment_ref` stays
    /// bounded. Without this the reference outlived its message forever and pinned
    /// the object + R2 blob — the #690 Blocking 2 storage leak.
    #[tokio::test]
    async fn gc_releases_a_swept_messages_attachment_reference() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        add_device(&conn, "alice", "a1", false).await;
        // Alice's one device has collected past the message, so GC will sweep it.
        seed_watermark(&conn, "c1", "alice", "a1", 10).await;
        add_envelope(&conn, "m1", "c1", 7).await;
        add_attachment(&conn, "h", "m1").await;

        assert!(
            object_is_referenced(&conn, "h").await.unwrap(),
            "referenced while the message is live"
        );

        let report = sweep_envelope_gc(&conn, TEST_STALE).await.unwrap();
        assert_eq!(report.visited, 1, "the sweep visits the one conversation with envelopes");
        assert_eq!(envelope_count(&conn, "c1").await, 0, "the message was swept");
        assert!(
            !object_is_referenced(&conn, "h").await.unwrap(),
            "with the message gone the reference must not survive — the object is \
             now collectable (#690 Blocking 2)"
        );
        assert_eq!(
            raw_ref_count(&conn, "h").await,
            0,
            "the orphaned declaration row is reaped so attachment_ref stays bounded"
        );
    }

    /// A forged-add reference — a `register` for a `message_id` that never had an
    /// envelope, the residue of the Blocking 1 attack — is never counted, and is
    /// reclaimed by the sweep rather than pinning the object forever.
    #[tokio::test]
    async fn a_reference_to_a_nonexistent_message_is_never_counted_and_is_reaped() {
        let conn = conn().await;
        // No `message_envelope` row 'ghost' is ever created.
        add_attachment(&conn, "h", "ghost").await;

        assert!(
            !object_is_referenced(&conn, "h").await.unwrap(),
            "a declaration whose message never existed must never count (#690)"
        );

        sweep_envelope_gc(&conn, TEST_STALE).await.unwrap();
        assert_eq!(
            raw_ref_count(&conn, "h").await,
            0,
            "the forged/orphaned declaration is reaped, not immortal"
        );
    }
}

#[cfg(test)]
mod roster_parity_tests {
    //! **I5 — the three rosters must agree (#722).**
    //!
    //! Envelope GC's SQL rosters and [`crate::commit::current_member_devices`] are one rule in three places.
    //!
    //!   * SQL roster **narrower** than the Rust one → envelope GC deletes mail a
    //!     device the commit log still serves has not collected (loss, F3-shaped).
    //!   * SQL roster **wider** → a device that cannot rejoin the tree holds the
    //!     watermark floor down forever and envelopes never clear (stuck
    //!     retention).
    //!
    //! The SELECTs below reuse the DELETE's own roster macros; expected rosters are spelled out literally.

    use super::*;
    use crate::commit::current_member_devices;

    /// The row set `COUNT(ud.device_id)` is counted over in the channel DELETE —
    /// same text, projected instead of aggregated.
    const CHANNEL_ROSTER_SELECT: &str = concat!(
        "SELECT ud.device_id
       ",
        channel_member_device_rows!()
    );

    /// The DM equivalent.
    const DM_ROSTER_SELECT: &str = concat!(
        "SELECT ud.device_id
       ",
        dm_member_device_rows!()
    );


    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        // Production Turso has FKs off; libsql's local backend turns them on.
        conn.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        pollis_schema::apply::single_db(&conn).await.expect("schema");
        conn
    }

    async fn exec(conn: &Connection, sql: &str) {
        conn.execute(sql, ()).await.unwrap();
    }

    async fn device(conn: &Connection, user: &str, device: &str, revoked: bool) {
        conn.execute(
            "INSERT INTO user_device (device_id, user_id, revoked_at) \
             VALUES (?1, ?2, CASE WHEN ?3 THEN datetime('now') ELSE NULL END)",
            libsql::params![device.to_string(), user.to_string(), revoked as i64],
        )
        .await
        .unwrap();
    }

    async fn watermark(conn: &Connection, conv: &str, user: &str, device: &str, seq: i64) {
        conn.execute(
            "INSERT INTO conversation_watermark \
                 (conversation_id, user_id, device_id, last_fetched_at, last_seq) \
             VALUES (?1, ?2, ?3, datetime('now'), ?4)",
            libsql::params![conv.to_string(), user.to_string(), device.to_string(), seq],
        )
        .await
        .unwrap();
    }

    async fn envelope(conn: &Connection, id: &str, conv: &str, seq: i64) {
        conn.execute(
            "INSERT INTO message_envelope \
                 (id, conversation_id, sent_at, sender_id, ciphertext, seq) \
             VALUES (?1, ?2, datetime('now'), 'sender', 'ct', ?3)",
            libsql::params![id.to_string(), conv.to_string(), seq],
        )
        .await
        .unwrap();
        // Keep `conversation_seq` consistent, as `insert_envelope_with_seq` does.
        conn.execute(
            "INSERT INTO conversation_seq (conversation_id, next_seq) VALUES (?1, ?2) \
             ON CONFLICT(conversation_id) DO UPDATE SET next_seq = MAX(next_seq, excluded.next_seq)",
            libsql::params![conv.to_string(), seq],
        )
        .await
        .unwrap();
    }

    async fn envelope_count(conn: &Connection, conv: &str) -> i64 {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM message_envelope WHERE conversation_id = ?1",
                libsql::params![conv.to_string()],
            )
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    /// The roster the cleanup SQL measures against, as a sorted device list.
    /// Duplicates are deliberately NOT collapsed: a repeated row would inflate
    /// `COUNT(ud.device_id)` and break the "every device reported" gate, so it
    /// must show up as a mismatch rather than be normalised away.
    async fn sql_roster(conn: &Connection, select: &str, conv: &str) -> Vec<String> {
        let mut rows = conn
            .query(select, libsql::params![conv.to_string()])
            .await
            .unwrap();
        let mut out = Vec::new();
        while let Some(r) = rows.next().await.unwrap() {
            out.push(r.get::<String>(0).unwrap());
        }
        out.sort();
        out
    }

    async fn rust_roster(conn: &Connection, conv: &str) -> Vec<String> {
        let mut out = current_member_devices(conn, conv).await.unwrap();
        out.sort();
        out
    }

    /// Every drift-prone shape in one fixture, seeded identically for a channel
    /// (`c-main`, owned by group `g-main`) and a DM (`dm-main`):
    ///
    ///   * `alice` — a plain member with a single active device.
    ///   * `bob` — several devices, one of them revoked, and one active device
    ///     that has never reported a watermark.
    ///   * `carol` — a member whose devices are ALL revoked.
    ///   * `dave` — a member whose one active device has no watermark row.
    ///   * `mallory` — not a member of anything, with an active device (and a
    ///     stray watermark row for both conversations, so a roster that reached
    ///     through `conversation_watermark` instead of through membership would
    ///     be caught).
    ///   * `eve` — a member of a DIFFERENT group/DM, to pin conversation scoping.
    async fn fixture(conn: &Connection) {
        // Registry claims first — 000017's guard triggers refuse unclaimed rows.
        exec(conn, "INSERT INTO conversation (id, kind) VALUES ('c-main', 'channel')").await;
        exec(conn, "INSERT INTO conversation (id, kind) VALUES ('c-other', 'channel')").await;
        exec(conn, "INSERT INTO channels (id, group_id, name) VALUES ('c-main', 'g-main', 'chan')").await;
        exec(conn, "INSERT INTO channels (id, group_id, name) VALUES ('c-other', 'g-other', 'chan')").await;
        for user in ["alice", "bob", "carol", "dave"] {
            conn.execute(
                "INSERT INTO group_member (group_id, user_id) VALUES ('g-main', ?1)",
                libsql::params![user.to_string()],
            )
            .await
            .unwrap();
            conn.execute(
                "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('dm-main', ?1, 'creator')",
                libsql::params![user.to_string()],
            )
            .await
            .unwrap();
        }
        exec(conn, "INSERT INTO group_member (group_id, user_id) VALUES ('g-other', 'eve')").await;
        exec(
            conn,
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('dm-other', 'eve', 'creator')",
        )
        .await;

        device(conn, "alice", "alice-1", false).await;
        device(conn, "bob", "bob-live", false).await;
        device(conn, "bob", "bob-revoked", true).await;
        device(conn, "bob", "bob-silent", false).await;
        device(conn, "carol", "carol-old-1", true).await;
        device(conn, "carol", "carol-old-2", true).await;
        device(conn, "dave", "dave-1", false).await;
        device(conn, "eve", "eve-1", false).await;
        device(conn, "mallory", "mallory-1", false).await;
    }

    /// The devices the fixture's rosters must resolve to, for BOTH conversation
    /// shapes, spelled out so a symmetric change still fails.
    const EXPECTED: [&str; 4] = ["alice-1", "bob-live", "bob-silent", "dave-1"];

    fn expected() -> Vec<String> {
        let mut v: Vec<String> = EXPECTED.iter().map(|s| s.to_string()).collect();
        v.sort();
        v
    }

    /// Seed the watermark spread the roster must be insensitive to: some roster
    /// devices reported, some never did, revoked devices carry ancient cursors,
    /// and a non-member has a row for the conversation.
    async fn seed_mixed_watermarks(conn: &Connection, conv: &str) {
        watermark(conn, conv, "alice", "alice-1", 4).await;
        watermark(conn, conv, "bob", "bob-live", 3).await;
        // `bob-silent` and `dave-1` deliberately have NO row.
        watermark(conn, conv, "bob", "bob-revoked", 1).await;
        watermark(conn, conv, "carol", "carol-old-1", 1).await;
        watermark(conn, conv, "carol", "carol-old-2", 1).await;
        watermark(conn, conv, "mallory", "mallory-1", 1).await;
    }

    #[tokio::test]
    async fn channel_sql_and_rust_rosters_agree() {
        let conn = conn().await;
        fixture(&conn).await;
        seed_mixed_watermarks(&conn, "c-main").await;

        let sql = sql_roster(&conn, CHANNEL_ROSTER_SELECT, "c-main").await;
        let rust = rust_roster(&conn, "c-main").await;

        assert_eq!(
            sql, rust,
            "CLEANUP_CHANNEL_ENVELOPES and commit::current_member_devices must \
             resolve the SAME member-device roster for a channel (I5, #722). \
             SQL: {sql:?} / Rust: {rust:?}"
        );
        assert_eq!(
            sql,
            expected(),
            "the agreed roster is not the intended one: active devices of current \
             members only — revoked devices excluded, a member with no live device \
             contributing nothing, and non-members absent"
        );
    }

    #[tokio::test]
    async fn dm_sql_and_rust_rosters_agree() {
        let conn = conn().await;
        fixture(&conn).await;
        seed_mixed_watermarks(&conn, "dm-main").await;

        let sql = sql_roster(&conn, DM_ROSTER_SELECT, "dm-main").await;
        let rust = rust_roster(&conn, "dm-main").await;

        assert_eq!(
            sql, rust,
            "CLEANUP_DM_ENVELOPES and commit::current_member_devices must resolve \
             the SAME member-device roster for a DM (I5, #722). \
             SQL: {sql:?} / Rust: {rust:?}"
        );
        assert_eq!(sql, expected(), "the agreed DM roster is not the intended one");
    }

    /// Scoping: the roster of a sibling conversation is its own, on both paths.
    #[tokio::test]
    async fn a_sibling_conversation_resolves_its_own_roster_on_both_paths() {
        let conn = conn().await;
        fixture(&conn).await;

        for (select, conv) in [
            (CHANNEL_ROSTER_SELECT, "c-other"),
            (DM_ROSTER_SELECT, "dm-other"),
        ] {
            let sql = sql_roster(&conn, select, conv).await;
            let rust = rust_roster(&conn, conv).await;
            assert_eq!(sql, rust, "{conv}: rosters diverge");
            assert_eq!(
                sql,
                vec!["eve-1".to_string()],
                "{conv} must see only its own member's device"
            );
        }
    }

    /// A member whose devices are ALL revoked, alone in the conversation: both
    /// paths must return the EMPTY roster.
    #[tokio::test]
    async fn an_all_revoked_member_yields_the_empty_roster_on_both_paths() {
        let conn = conn().await;
        exec(&conn, "INSERT INTO conversation (id, kind) VALUES ('c-dead', 'channel')").await;
        exec(&conn, "INSERT INTO channels (id, group_id, name) VALUES ('c-dead', 'g-dead', 'chan')").await;
        exec(&conn, "INSERT INTO group_member (group_id, user_id) VALUES ('g-dead', 'carol')").await;
        exec(
            &conn,
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('dm-dead', 'carol', 'creator')",
        )
        .await;
        device(&conn, "carol", "carol-old-1", true).await;
        device(&conn, "carol", "carol-old-2", true).await;
        watermark(&conn, "c-dead", "carol", "carol-old-1", 1).await;

        for (select, conv) in [
            (CHANNEL_ROSTER_SELECT, "c-dead"),
            (DM_ROSTER_SELECT, "dm-dead"),
        ] {
            let sql = sql_roster(&conn, select, conv).await;
            let rust = rust_roster(&conn, conv).await;
            assert_eq!(sql, rust, "{conv}: rosters diverge on an all-revoked member");
            assert!(
                sql.is_empty(),
                "{conv}: an all-revoked member contributes no devices, got {sql:?}"
            );
        }
    }

    /// An id that names no conversation: both paths must return nothing rather
    /// than, say, every device in the table.
    #[tokio::test]
    async fn an_unknown_conversation_yields_the_empty_roster_on_both_paths() {
        let conn = conn().await;
        fixture(&conn).await;

        for select in [CHANNEL_ROSTER_SELECT, DM_ROSTER_SELECT] {
            let sql = sql_roster(&conn, select, "ghost").await;
            assert!(sql.is_empty(), "unknown conversation resolved {sql:?}");
        }
        assert!(rust_roster(&conn, "ghost").await.is_empty());
    }

    /// Factoring the roster out of the DELETE must not have changed the SQL the
    /// DS executes; pinned against the pre-extraction literal statements.
    #[test]
    fn the_extraction_did_not_change_the_cleanup_sql() {
        assert_eq!(
            CLEANUP_CHANNEL_ENVELOPES,
            "\
DELETE FROM message_envelope
 WHERE conversation_id = ?1
   AND seq <= (
       SELECT CASE
                WHEN COUNT(ud.device_id) = COUNT(cw.last_seq)
                THEN MIN(cw.last_seq)
                ELSE NULL
              END
       FROM group_member gm
       JOIN channels c ON c.id = ?1 AND c.group_id = gm.group_id
       JOIN user_device ud ON ud.user_id = gm.user_id AND ud.revoked_at IS NULL
       LEFT JOIN conversation_watermark cw
              ON cw.conversation_id = ?1
             AND cw.user_id = ud.user_id
             AND cw.device_id = ud.device_id
       WHERE cw.reported_at IS NULL OR cw.reported_at >= datetime('now', ?2)
     )"
        );
        assert_eq!(
            CLEANUP_DM_ENVELOPES,
            "\
DELETE FROM message_envelope
 WHERE conversation_id = ?1
   AND seq <= (
       SELECT CASE
                WHEN COUNT(ud.device_id) = COUNT(cw.last_seq)
                THEN MIN(cw.last_seq)
                ELSE NULL
              END
       FROM dm_channel_member dcm
       JOIN user_device ud ON ud.user_id = dcm.user_id AND ud.revoked_at IS NULL
       LEFT JOIN conversation_watermark cw
              ON cw.conversation_id = ?1
             AND cw.user_id = ud.user_id
             AND cw.device_id = ud.device_id
       WHERE dcm.dm_channel_id = ?1
         AND (cw.reported_at IS NULL OR cw.reported_at >= datetime('now', ?2))
     )"
        );
    }

    /// The parity above compares a FRAGMENT of the DELETE against the Rust
    /// roster; this ties that fragment to the DELETE's observable behaviour, so a
    /// roster condition smuggled in ELSEWHERE in the statement cannot hide.
    #[tokio::test]
    async fn the_roster_is_what_the_delete_actually_gates_on() {
        for (conv, sql, select) in [
            ("c-main", CLEANUP_CHANNEL_ENVELOPES, CHANNEL_ROSTER_SELECT),
            ("dm-main", CLEANUP_DM_ENVELOPES, DM_ROSTER_SELECT),
        ] {
            // Leg 1 — every roster device collected past the envelope.
            let caught_up = conn().await;
            fixture(&caught_up).await;
            let roster = sql_roster(&caught_up, select, conv).await;
            assert_eq!(roster, expected(), "{conv}: unexpected roster");
            for d in &roster {
                let user = d.split('-').next().unwrap().to_string();
                watermark(&caught_up, conv, &user, d, 4).await;
            }
            // Excluded devices are far behind — they must not be consulted.
            watermark(&caught_up, conv, "bob", "bob-revoked", 1).await;
            watermark(&caught_up, conv, "carol", "carol-old-1", 1).await;
            watermark(&caught_up, conv, "carol", "carol-old-2", 1).await;
            watermark(&caught_up, conv, "mallory", "mallory-1", 1).await;
            envelope(&caught_up, "e1", conv, 2).await;
            // `reported_at` NULL (live), so this isolates the roster.
            caught_up
                .execute(sql, libsql::params![conv.to_string(), "-6 months".to_string()])
                .await
                .unwrap();
            assert_eq!(
                envelope_count(&caught_up, conv).await,
                0,
                "{conv}: with every device the roster lists caught up, the envelope \
                 must go — a device the roster EXCLUDES must not hold it"
            );

            // Leg 2 — one roster device, and only it, falls behind.
            let held = conn().await;
            fixture(&held).await;
            for d in &roster {
                let user = d.split('-').next().unwrap().to_string();
                // Only `bob-silent` is below the envelope's seq (2).
                let cursor = if d.as_str() == "bob-silent" { 1 } else { 4 };
                watermark(&held, conv, &user, d, cursor).await;
            }
            watermark(&held, conv, "bob", "bob-revoked", 4).await;
            watermark(&held, conv, "carol", "carol-old-1", 4).await;
            watermark(&held, conv, "carol", "carol-old-2", 4).await;
            watermark(&held, conv, "mallory", "mallory-1", 4).await;
            envelope(&held, "e1", conv, 2).await;
            held.execute(sql, libsql::params![conv.to_string(), "-6 months".to_string()])
                .await
                .unwrap();
            assert_eq!(
                envelope_count(&held, conv).await,
                1,
                "{conv}: a single device the roster LISTS still holds the envelope, \
                 however caught-up the excluded devices are"
            );
        }
    }
}

#[cfg(test)]
mod admin_delete_visibility_tests {
    //! #693 / #661: the admin-delete tombstone always reaches a caught-up recipient, at the envelope layer.
    //!
    //! Two connections of ONE shared `Database` model the flows harness's single `Db`.
    //!
    //! So #661's residual flake lives downstream, in the client's application of the tombstone.

    use super::*;
    use tempfile::TempDir;


    /// The exact shape a CLIENT writes for `sent_at`
    /// (`chrono::Utc::now().to_rfc3339()`, i.e. `SecondsFormat::AutoSi`), at a
    /// fixed instant plus `nanos` (0/3/6/9 fraction digits).
    fn client_stamp(secs: i64, nanos: u32) -> String {
        chrono::DateTime::from_timestamp(secs, nanos)
            .expect("valid instant")
            .to_rfc3339()
    }

    /// One shared libsql `Database` on a WAL file — the faithful model of the
    /// single `Db` the flows harness hands to its in-process DS.
    async fn shared_db() -> (TempDir, libsql::Database) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("main.db");
        let db = libsql::Builder::new_local(&path).build().await.expect("build");
        {
            let conn = db.connect().expect("connect");
            conn.query("PRAGMA journal_mode=WAL", ()).await.expect("wal");
            // Production Turso has FKs off; libsql's local backend turns them on.
        conn.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        pollis_schema::apply::single_db(&conn).await.expect("schema");
        }
        (dir, db)
    }

    /// The EXACT per-conversation fetch `ingest_group_envelopes_interleaved` runs
    /// (`pollis_core::commands::messages::ingest`): strictly past the recipient's
    /// own `(conversation, user, device)` watermark, ordered `sent_at ASC, id
    /// ASC`. Returns `(id, type, target_message_id)` for each fetched envelope.
    async fn ingest_fetch(
        conn: &Connection,
        conversation_id: &str,
        user_id: &str,
        device_id: &str,
    ) -> Vec<(String, String, Option<String>)> {
        // The PRODUCTION fetch, never a copy of its predicate (a copy drifted at #1087).
        crate::directory::envelopes_for_device(conn, conversation_id, user_id, device_id)
            .await
            .expect("ingest fetch")
            .into_iter()
            .map(|e| (e.id, e.kind, e.target_message_id))
            .collect()
    }

    /// A catch-up spans every channel of a group, and each envelope carries
    /// where it arrived in the WHOLE group, not just its own channel: a sender's
    /// MLS generations run across channels, so the client decrypts in this order
    /// (`ingest::into_sealing_order`). Sent a, b, b, a — the fetch lists them
    /// channel by channel, the arrival order still says a, b, b, a.
    #[tokio::test]
    async fn a_catch_up_carries_the_group_wide_arrival_order() {
        let (_dir, db) = shared_db().await;
        let conn = db.connect().expect("conn");
        let order = [("e1", "chan-a"), ("e2", "chan-b"), ("e3", "chan-b"), ("e4", "chan-a")];
        for (id, conv) in order {
            apply_send_message(
                &conn,
                None,
                &SendMessageBody {
                    id: id.to_string(),
                    conversation_id: conv.to_string(),
                    sender_id: Some("sealed".to_string()),
                    ciphertext: "mls:00".to_string(),
                    reply_to_id: None,
                    sent_at: "2026-09-01T00:00:00+00:00".to_string(),
                    sealed: 1,
                    generation: None,
                    epoch: None,
                    push_to: None,
                    push_mentions: None,
                    delete_token_hash: Some(FIXTURE_CAPABILITY_HASH.to_string()),
                },
            )
            .await
            .expect("send");
        }
        let mut got = crate::directory::envelopes_for_conversations(
            &conn,
            &["chan-a".to_string(), "chan-b".to_string()],
            "carol",
            "carol-dev",
        )
        .await
        .expect("fetch");
        assert!(got.iter().all(|e| e.arrival.is_some()), "every envelope has a position");
        got.sort_by_key(|e| e.arrival);
        let ids: Vec<&str> = got.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(ids, ["e1", "e2", "e3", "e4"]);
    }

    /// Drive the full #661 envelope-layer dance for a given client `sent_at`
    /// shape and assert carol's SECOND fetch, on another connection, sees the admin tombstone.
    async fn assert_tombstone_reaches_carol(msg_sent_at: &str) {
        let (_dir, db) = shared_db().await;
        // Two distinct connections on the SHARED handle: `w` stands in for the
        // DS's write connection, `carol` for carol's read connection.
        let w = db.connect().expect("write conn");
        let carol = db.connect().expect("carol read conn");

        let conv = "chan-general";
        let msg_id = "msg-bobs-post";

        // bob sends (sealed sentinel sender, no-auth path).
        apply_send_message(
            &w,
            None,
            &SendMessageBody {
                id: msg_id.to_string(),
                conversation_id: conv.to_string(),
                sender_id: Some("sealed".to_string()),
                ciphertext: "mls:00".to_string(),
                reply_to_id: None,
                sent_at: msg_sent_at.to_string(),
                sealed: 1,
                // Unasserted lineage; the epoch gate has its own tests.
                generation: None,
                epoch: None,
                // No push from a unit test — these assert envelope columns.
                push_to: None,
                push_mentions: None,
                delete_token_hash: Some(FIXTURE_CAPABILITY_HASH.to_string()),
            },
        )
        .await
        .expect("send");

        // carol's FIRST fetch sees bob's message; she reports her watermark as ingest does.
        let first = ingest_fetch(&carol, conv, "carol", "carol-dev").await;
        assert_eq!(
            first.len(),
            1,
            "carol's first fetch must see bob's message (sent_at {msg_sent_at})"
        );
        apply_advance_watermark(
            &w,
            None,
            &WatermarkBody {
                conversation_id: conv.to_string(),
                user_id: Some("carol".to_string()),
                device_id: "carol-dev".to_string(),
                last_fetched_at: msg_sent_at.to_string(),
                last_seq: None,
            },
        )
        .await
        .expect("carol watermark");
        // alice reports hers too (she also fetched in the scenario).
        apply_advance_watermark(
            &w,
            None,
            &WatermarkBody {
                conversation_id: conv.to_string(),
                user_id: Some("alice".to_string()),
                device_id: "alice-dev".to_string(),
                last_fetched_at: msg_sent_at.to_string(),
                last_seq: None,
            },
        )
        .await
        .expect("alice watermark");

        // alice admin-deletes (`msg_sender_id != actor` selects the admin branch; no-auth skips the role check).
        let outcome = apply_delete_message(
            &w,
            None,
            &DeleteMessageBody {
                message_id: msg_id.to_string(),
                conversation_id: conv.to_string(),
                msg_sender_id: Some("bob".to_string()),
                actor_id: Some("alice".to_string()),
                delete_token: None,
            },
        )
        .await
        .expect("admin delete");
        assert!(matches!(outcome, WriteOutcome::Ok), "admin delete must succeed");

        // carol's SECOND fetch MUST see the tombstone (the #661 flake).
        let second = ingest_fetch(&carol, conv, "carol", "carol-dev").await;
        let tombstones: Vec<_> = second
            .iter()
            .filter(|(_, ty, target)| ty == "delete" && target.as_deref() == Some(msg_id))
            .collect();
        assert_eq!(
            tombstones.len(),
            1,
            "carol's second fetch must return exactly one admin tombstone for the \
             deleted message (client sent_at {msg_sent_at}); fetched: {second:?}. \
             A miss here would be candidate 1 (read-after-write / sent_at ordering) \
             — it never happens, so #661 lives in the client's APPLICATION of the \
             tombstone, not its delivery."
        );
        // …and the original message envelope is gone from the server.
        assert!(
            !second.iter().any(|(id, ty, _)| id == msg_id && ty == "message"),
            "the admin-deleted original envelope must be removed"
        );
    }

    /// A whole second in the DS's near future, inside [`CURSOR_STAMP_SKEW_SECS`].
    fn base_second_ahead() -> i64 {
        chrono::Utc::now().timestamp() + CURSOR_STAMP_SKEW_SECS / 2
    }

    /// The headline: across every `sent_at` fraction width a client clock can
    /// emit — including the whole-second (`'+' < '.'`) shape and a stamp in the
    /// DS's FUTURE (client clock ahead) — carol always fetches the tombstone.
    #[tokio::test]
    async fn admin_tombstone_always_reaches_a_caught_up_recipient() {
        // A base second ahead of the DS clock, then the fraction widths AutoSi
        // produces (0/3/6/9 digits) plus the max-nanos edge.
        let base = base_second_ahead();
        for nanos in [0u32, 1_000_000, 1_001_000, 1_001_001, 999_999_999] {
            assert_tombstone_reaches_carol(&client_stamp(base, nanos)).await;
        }
        // Client clock AHEAD of the DS, as far as the stamp bound admits.
        let ahead = (chrono::Utc::now() + chrono::Duration::seconds(CURSOR_STAMP_SKEW_SECS - 30))
            .to_rfc3339();
        assert_tombstone_reaches_carol(&ahead).await;
    }

    /// The other direction of the same ordering question, and the one the
    /// tombstone tests above do not reach: after a DS tombstone lands, does a
    /// message the CLIENT stamps next still get fetched?
    ///
    /// **Since #1087 a tombstone takes `MAX(seq)+1`**, so it is delivered even when its `sent_at` sorts below.
    #[tokio::test]
    async fn a_zero_nanosecond_client_message_still_outsorts_the_preceding_tombstone() {
        let (_dir, db) = shared_db().await;
        let w = db.connect().expect("write conn");
        let carol = db.connect().expect("carol read conn");

        let base = base_second_ahead();
        let conv = "chan-general";

        // An existing message at `…:00.999999999`, the highest fraction there is.
        apply_send_message(
            &w,
            None,
            &SendMessageBody {
                id: "msg-bobs-post".to_string(),
                conversation_id: conv.to_string(),
                sender_id: Some("sealed".to_string()),
                ciphertext: "mls:00".to_string(),
                reply_to_id: None,
                sent_at: client_stamp(base, 999_999_999),
                sealed: 1,
                // Unasserted lineage; the epoch gate has its own tests.
                generation: None,
                epoch: None,
                // No push from a unit test — these assert envelope columns.
                push_to: None,
                push_mentions: None,
                delete_token_hash: Some(FIXTURE_CAPABILITY_HASH.to_string()),
            },
        )
        .await
        .expect("send");

        // Carol catches up and reports her watermark, then alice admin-deletes.
        assert_eq!(ingest_fetch(&carol, conv, "carol", "carol-dev").await.len(), 1);
        apply_advance_watermark(
            &w,
            None,
            &WatermarkBody {
                conversation_id: conv.to_string(),
                user_id: Some("carol".to_string()),
                device_id: "carol-dev".to_string(),
                last_fetched_at: client_stamp(base, 999_999_999),
                last_seq: None,
            },
        )
        .await
        .expect("carol watermark");
        apply_delete_message(
            &w,
            None,
            &DeleteMessageBody {
                message_id: "msg-bobs-post".to_string(),
                conversation_id: conv.to_string(),
                msg_sender_id: Some("bob".to_string()),
                actor_id: Some("alice".to_string()),
                delete_token: None,
            },
        )
        .await
        .expect("admin delete");

        // Carol fetches the tombstone and advances onto it — the cursor is now a
        // DS-shaped, nanosecond-precision stamp.
        let fetched = ingest_fetch(&carol, conv, "carol", "carol-dev").await;
        let tombstone = fetched
            .iter()
            .find(|(_, ty, _)| ty == "delete")
            .expect("carol must fetch the tombstone");
        let cursor: String = {
            let mut rows = w
                .query(
                    "SELECT sent_at FROM message_envelope WHERE id = ?1",
                    libsql::params![tombstone.0.clone()],
                )
                .await
                .expect("tombstone sent_at");
            rows.next().await.expect("row").expect("row").get(0).expect("sent_at")
        };
        // #1087: the tombstone's SEQUENCE, not its `sent_at`, puts it above every cursor.
        let (tomb_seq, msg_seq): (i64, Option<i64>) = {
            let mut rows = w
                .query(
                    "SELECT \
                       (SELECT seq FROM message_envelope WHERE id = ?1), \
                       (SELECT seq FROM message_envelope WHERE id = 'msg-bobs-post')",
                    libsql::params![tombstone.0.clone()],
                )
                .await
                .expect("sequences");
            let row = rows.next().await.expect("row").expect("row");
            (row.get(0).expect("tombstone seq"), row.get(1).expect("message seq"))
        };
        assert!(
            msg_seq.is_none(),
            "the admin delete removes the message it redacts"
        );
        assert!(
            tomb_seq > 0,
            "the tombstone must carry a delivery sequence, got {tomb_seq}"
        );
        // Carol fetched it even though its `sent_at` ({cursor}) sorts BELOW her `last_fetched_at`.
        let _ = &cursor;
        apply_advance_watermark(
            &w,
            None,
            &WatermarkBody {
                conversation_id: conv.to_string(),
                user_id: Some("carol".to_string()),
                device_id: "carol-dev".to_string(),
                last_fetched_at: cursor.clone(),
                last_seq: None,
            },
        )
        .await
        .expect("carol watermark 2");

        // A fraction-less stamp far in the past, below the tombstone's `now`: admissible and, since #1087, harmless.
        let boundary = "2020-01-01T00:00:00+00:00".to_string();
        assert!(
            !boundary.contains('.'),
            "the premise of this test is a fraction-less stamp; got {boundary}"
        );
        apply_send_message(
            &w,
            None,
            &SendMessageBody {
                id: "msg-after-tombstone".to_string(),
                conversation_id: conv.to_string(),
                sender_id: Some("sealed".to_string()),
                ciphertext: "mls:01".to_string(),
                reply_to_id: None,
                sent_at: boundary.clone(),
                sealed: 1,
                // Unasserted lineage; the epoch gate has its own tests.
                generation: None,
                epoch: None,
                // No push from a unit test — these assert envelope columns.
                push_to: None,
                push_mentions: None,
                delete_token_hash: Some(FIXTURE_CAPABILITY_HASH.to_string()),
            },
        )
        .await
        .expect("send after tombstone");
        // Delivered even though it sorts BELOW: the cursor is a sequence, the stamp decoration (#1087).
        assert!(
            boundary < cursor,
            "premise: this message's stamp ({boundary}) must sort BELOW the \
             tombstone's ({cursor}) — that is the burial case"
        );

        let after = ingest_fetch(&carol, conv, "carol", "carol-dev").await;
        assert!(
            after.iter().any(|(id, _, _)| id == "msg-after-tombstone"),
            "a message sent after an admin delete must reach a caught-up recipient \
             EVEN THOUGH its sent_at ({boundary}) sorts below the tombstone's \
             ({cursor}); fetched: {after:?}"
        );
    }

    /// The candidate-1 primitive in isolation: a row written on one connection of
    /// a shared `Database` is IMMEDIATELY visible on another connection of it (a second `Database` need not be).
    #[tokio::test]
    async fn read_your_writes_holds_across_connections_of_one_shared_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("main.db");

        let db = libsql::Builder::new_local(&path).build().await.expect("build");
        {
            let c = db.connect().expect("connect");
            c.query("PRAGMA journal_mode=WAL", ()).await.expect("wal");
            // Production Turso has FKs off; libsql's local backend turns them on.
        c.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        pollis_schema::apply::single_db(&c).await.expect("schema");
        }

        let writer = db.connect().expect("writer");
        let reader = db.connect().expect("reader");

        writer
            .execute(
                "INSERT INTO message_envelope \
                     (id, conversation_id, sender_id, ciphertext, sent_at, type, target_message_id) \
                 VALUES ('t1', 'c1', 'alice', '', '2026-01-01T00:00:00.000000001+00:00', 'delete', 'm1')",
                (),
            )
            .await
            .expect("write tombstone");

        // Same-handle sibling connection: the write is visible with no lag.
        let seen: i64 = {
            let mut rows = reader
                .query(
                    "SELECT COUNT(*) FROM message_envelope WHERE id = 't1'",
                    (),
                )
                .await
                .expect("read");
            rows.next().await.expect("row").expect("some").get(0).expect("count")
        };
        assert_eq!(
            seen, 1,
            "a sibling connection of the SAME libsql Database must see the just-\
             written row immediately — this is the read-your-writes property the \
             flows harness relies on to make #661 candidate 1 impossible"
        );
    }
}

#[cfg(test)]
mod delete_scope_tests {
    //! `/v1/messages/delete` must only ever touch the conversation it authorised
    //! against.
    //!
    //! The bug: authz read `conversation_id` while the DELETE acted on an unrelated `message_id`.
    //!
    //! Both tests assert the VICTIM row survives. Asserting only that the call
    //! returns Ok would pass against the vulnerable code.

    use super::*;
    use libsql::Connection;


    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let c = db.connect().unwrap();
        // Production Turso has FKs off; libsql's local backend turns them on.
        c.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        pollis_schema::apply::single_db(&c).await.expect("schema");
        c
    }

    async fn seed_envelope(c: &Connection, id: &str, conv: &str, sender: &str) {
        c.execute(
            "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) \
             VALUES (?1, ?2, ?3, 'x', '2026-01-01T00:00:00.000000000+00:00')",
            libsql::params![id.to_string(), conv.to_string(), sender.to_string()],
        )
        .await
        .unwrap();
    }

    async fn exists(c: &Connection, id: &str) -> bool {
        let mut rows = c
            .query(
                "SELECT 1 FROM message_envelope WHERE id = ?1",
                libsql::params![id.to_string()],
            )
            .await
            .unwrap();
        rows.next().await.unwrap().is_some()
    }

    /// Self-branch: a member of their own DM cannot delete an envelope that
    /// lives in someone else's conversation.
    #[tokio::test]
    async fn self_delete_cannot_reach_another_conversation() {
        let c = conn().await;
        // The attacker is a legitimate member of exactly one conversation.
        c.execute(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('mine', 'mallory', 'creator')",
            (),
        )
        .await
        .unwrap();
        c.execute_batch(
            "INSERT INTO conversation (id, kind) VALUES ('mine', 'dm');
             INSERT INTO dm_channel (id, created_by) VALUES ('mine', 'creator');",
        )
        .await
        .unwrap();
        seed_envelope(&c, "victim-envelope", "someone-elses-conversation", "alice").await;

        let body = DeleteMessageBody {
            message_id: "victim-envelope".to_string(),
            // Authorised against a conversation they really are in …
            conversation_id: "mine".to_string(),
            // … and the self-branch is entered by naming themselves as author.
            msg_sender_id: Some("mallory".to_string()),
            actor_id: None,
            delete_token: None,
        };
        let outcome = apply_delete_message(&c, Some("mallory"), &body).await.unwrap();

        assert!(
            exists(&c, "victim-envelope").await,
            "a member of 'mine' deleted an envelope in another conversation \
             (outcome was {outcome:?}) — authz and the DELETE are keyed on \
             different attacker-supplied fields"
        );
    }

    /// Admin-branch: being an admin somewhere does not grant deletion everywhere.
    #[tokio::test]
    async fn admin_delete_cannot_reach_another_conversation() {
        let c = conn().await;
        c.execute_batch(
            "INSERT INTO conversation (id, kind) VALUES ('g1', 'group');
             INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'grp', 'owner');
             INSERT INTO conversation (id, kind) VALUES ('my-channel', 'channel');
             INSERT INTO channels (id, group_id, name) VALUES ('my-channel', 'g1', 'chan');",
        )
        .await
        .unwrap();
        c.execute(
            "INSERT INTO group_member (group_id, user_id, role) VALUES ('g1', 'mallory', 'admin')",
            (),
        )
        .await
        .unwrap();
        seed_envelope(&c, "victim-envelope", "someone-elses-conversation", "alice").await;

        let body = DeleteMessageBody {
            message_id: "victim-envelope".to_string(),
            conversation_id: "my-channel".to_string(),
            // A different author than the actor takes the admin branch.
            msg_sender_id: Some("alice".to_string()),
            actor_id: None,
            delete_token: None,
        };
        let outcome = apply_delete_message(&c, Some("mallory"), &body).await.unwrap();

        assert!(
            exists(&c, "victim-envelope").await,
            "an admin of 'g1' deleted an envelope in another conversation \
             (outcome was {outcome:?})"
        );
    }

    /// The legitimate path still works — otherwise the fix above could be
    /// "delete nothing, ever" and both tests would still pass.
    #[tokio::test]
    async fn self_delete_still_removes_your_own_envelope() {
        let c = conn().await;
        c.execute(
            "INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('mine', 'mallory', 'creator')",
            (),
        )
        .await
        .unwrap();
        c.execute_batch(
            "INSERT INTO conversation (id, kind) VALUES ('mine', 'dm');
             INSERT INTO dm_channel (id, created_by) VALUES ('mine', 'creator');",
        )
        .await
        .unwrap();
        seed_envelope(&c, "my-envelope", "mine", "mallory").await;

        let body = DeleteMessageBody {
            message_id: "my-envelope".to_string(),
            conversation_id: "mine".to_string(),
            msg_sender_id: Some("mallory".to_string()),
            actor_id: None,
            delete_token: None,
        };
        apply_delete_message(&c, Some("mallory"), &body).await.unwrap();

        assert!(
            !exists(&c, "my-envelope").await,
            "the legitimate self-delete must still remove the envelope"
        );
    }
}

#[cfg(test)]
mod epoch_gate_tests {
    //! The epoch gate (#1041): `/v1/messages/send` and `/v1/messages/edit`
    //! keep an envelope only when the `(generation, epoch)` it asserts is the
    //! commit log's head. The invalid state — an envelope stored at an epoch
    //! the group has already left — is what these prove cannot be created.

    use super::*;

    /// One DB standing in for both handles (the flows harness's shape).
    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        pollis_schema::apply::single_db(&conn).await.expect("schema");
        conn
    }

    /// Append the head commit of `generation` — the log's head epoch becomes
    /// `epoch + 1`, exactly as a real CAS win does.
    async fn append_commit(conn: &Connection, conv: &str, generation: i64, epoch: i64) {
        conn.execute(
            "INSERT INTO mls_commit_log (conversation_id, generation, epoch, sender_id, commit_data) \
             VALUES (?1, ?2, ?3, 'alice', x'00')",
            libsql::params![conv.to_string(), generation, epoch],
        )
        .await
        .expect("append commit");
    }

    async fn stored(conn: &Connection, id: &str) -> bool {
        let mut rows = conn
            .query(
                "SELECT 1 FROM message_envelope WHERE id = ?1",
                libsql::params![id.to_string()],
            )
            .await
            .unwrap();
        rows.next().await.unwrap().is_some()
    }

    fn send(id: &str, conv: &str, generation: Option<i64>, epoch: Option<i64>) -> SendMessageBody {
        SendMessageBody {
            id: id.to_string(),
            conversation_id: conv.to_string(),
            sender_id: Some("sealed".to_string()),
            ciphertext: "mls:00".to_string(),
            reply_to_id: None,
            sent_at: "2026-09-01T00:00:00+00:00".to_string(),
            sealed: 1,
            generation,
            epoch,
            push_to: None,
            push_mentions: None,
            delete_token_hash: Some(FIXTURE_CAPABILITY_HASH.to_string()),
        }
    }

    fn edit(
        id: &str,
        conv: &str,
        target: &str,
        generation: Option<i64>,
        epoch: Option<i64>,
    ) -> EditMessageBody {
        EditMessageBody {
            envelope_id: id.to_string(),
            conversation_id: conv.to_string(),
            target_message_id: target.to_string(),
            sender_id: Some("alice".to_string()),
            ciphertext: "mls:00".to_string(),
            sent_at: "2026-09-01T00:00:00+00:00".to_string(),
            generation,
            epoch,
            delete_token: None,
        }
    }

    #[tokio::test]
    async fn an_envelope_sealed_at_the_head_epoch_lands() {
        let c = conn().await;
        append_commit(&c, "dm", 0, 0).await;
        append_commit(&c, "dm", 0, 1).await;
        // Head is (0, 2).
        let out = send_envelope(&c, &c, None, &send("m1", "dm", Some(0), Some(2)))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Ok), "{out:?}");
        assert!(stored(&c, "m1").await);
    }

    #[tokio::test]
    async fn an_envelope_sealed_at_a_left_epoch_is_refused_and_nothing_remains() {
        let c = conn().await;
        append_commit(&c, "dm", 0, 0).await;
        append_commit(&c, "dm", 0, 1).await;
        // Sealed at epoch 1, but the group is at 2.
        let out = send_envelope(&c, &c, None, &send("m1", "dm", Some(0), Some(1)))
            .await
            .unwrap();
        assert!(
            matches!(
                out,
                WriteOutcome::EpochBehind {
                    head_generation: 0,
                    head_epoch: 2
                }
            ),
            "{out:?}"
        );
        assert!(!stored(&c, "m1").await, "a behind envelope must leave no row");
    }

    #[tokio::test]
    async fn an_envelope_from_a_closed_generation_is_refused() {
        let c = conn().await;
        append_commit(&c, "dm", 0, 0).await;
        append_commit(&c, "dm", 1, 0).await;
        // Head is (1, 1); an envelope at (0, 1) is from the migrated-away lineage.
        let out = send_envelope(&c, &c, None, &send("m1", "dm", Some(0), Some(1)))
            .await
            .unwrap();
        assert!(
            matches!(
                out,
                WriteOutcome::EpochBehind {
                    head_generation: 1,
                    head_epoch: 1
                }
            ),
            "{out:?}"
        );
        assert!(!stored(&c, "m1").await);
    }

    #[tokio::test]
    async fn an_empty_log_is_head_zero() {
        let c = conn().await;
        let ok = send_envelope(&c, &c, None, &send("m0", "dm", Some(0), Some(0)))
            .await
            .unwrap();
        assert!(matches!(ok, WriteOutcome::Ok), "{ok:?}");
        let behind = send_envelope(&c, &c, None, &send("m1", "dm", Some(0), Some(1)))
            .await
            .unwrap();
        assert!(matches!(behind, WriteOutcome::EpochBehind { .. }), "{behind:?}");
        assert!(stored(&c, "m0").await);
        assert!(!stored(&c, "m1").await);
    }

    /// #1335: a sender's outbox re-posting an envelope that already landed is
    /// told it succeeded, and nothing is stored twice — even when the re-post
    /// was sealed at an epoch the group has since left, because the epoch
    /// gate's clean-up would otherwise delete the DELIVERED row.
    #[tokio::test]
    async fn a_replayed_envelope_is_success_and_stores_nothing_new() {
        let c = conn().await;
        append_commit(&c, "dm", 0, 0).await;
        // Head is (0, 1); the first post lands.
        let (first, replayed) =
            send_envelope_once(&c, &c, None, &send("m1", "dm", Some(0), Some(1)))
                .await
                .unwrap();
        assert!(matches!(first, WriteOutcome::Ok) && !replayed, "{first:?}");

        // The group moves on, and the retry is sealed at the old epoch.
        append_commit(&c, "dm", 0, 1).await;
        let mut again = send("m1", "dm", Some(0), Some(1));
        again.ciphertext = "mls:01".to_string();
        let (second, replayed) = send_envelope_once(&c, &c, None, &again).await.unwrap();
        assert!(matches!(second, WriteOutcome::Ok), "{second:?}");
        assert!(replayed, "the handler must know not to push again");

        let mut rows = c
            .query(
                "SELECT COUNT(*), MIN(ciphertext) FROM message_envelope WHERE id = 'm1'",
                (),
            )
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();
        assert_eq!(row.get::<i64>(0).unwrap(), 1, "one envelope, not two");
        assert_eq!(
            row.get::<String>(1).unwrap(),
            "mls:00",
            "the delivered envelope is untouched"
        );
    }

    /// An id another conversation's envelope holds is refused with the same
    /// answer a non-member gets, never treated as a replay — and so is the
    /// same id and conversation from someone who cannot produce the author's
    /// capability hash.
    #[tokio::test]
    async fn a_replay_is_bound_to_its_conversation_and_its_author() {
        let c = conn().await;
        let ok = send_envelope(&c, &c, None, &send("m1", "dm", None, None))
            .await
            .unwrap();
        assert!(matches!(ok, WriteOutcome::Ok), "{ok:?}");
        let elsewhere = send_envelope(&c, &c, None, &send("m1", "other-dm", None, None))
            .await
            .unwrap();
        assert!(matches!(elsewhere, WriteOutcome::Forbidden), "{elsewhere:?}");
        let mut impostor = send("m1", "dm", None, None);
        impostor.delete_token_hash = Some("not-the-authors-hash".to_string());
        let (out, replayed) = send_envelope_once(&c, &c, None, &impostor).await.unwrap();
        assert!(matches!(out, WriteOutcome::Forbidden) && replayed, "{out:?}");
    }

    /// A delivered-but-unrecorded post replays as delivered even after its
    /// author left the conversation: they were a member when it landed.
    #[tokio::test]
    async fn a_replay_settles_even_after_the_author_was_removed() {
        let c = conn().await;
        let (first, _) = send_envelope_once(&c, &c, None, &send("m1", "dm", None, None))
            .await
            .unwrap();
        assert!(matches!(first, WriteOutcome::Ok), "{first:?}");
        // Authenticated as someone with no membership row at all.
        let (again, replayed) =
            send_envelope_once(&c, &c, Some("removed-user"), &send("m1", "dm", None, None))
                .await
                .unwrap();
        assert!(matches!(again, WriteOutcome::Ok) && replayed, "{again:?}");
    }

    /// The race the per-id lock closes: a retry must not answer "replay" while
    /// the first post can still be rolled back by its own epoch post-check.
    /// Holding the id's lock simulates that post mid-write; the retry must write its own envelope.
    #[tokio::test]
    async fn a_retry_waits_for_the_first_post_to_settle() {
        let c = conn().await;
        send_envelope(&c, &c, None, &send("m1", "dm", None, None))
            .await
            .unwrap();
        let first_post = EnvelopeIdLock::acquire("m1").await;

        let c2 = c.clone();
        let retry = tokio::spawn(async move {
            send_envelope_once(&c2, &c2, None, &send("m1", "dm", None, None)).await
        });
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(!retry.is_finished(), "the retry must wait for the first post");

        // The first post's post-check finds the head moved and deletes its row.
        c.execute("DELETE FROM message_envelope WHERE id = 'm1'", ())
            .await
            .unwrap();
        drop(first_post);

        let (out, replayed) = retry.await.unwrap().unwrap();
        assert!(matches!(out, WriteOutcome::Ok), "{out:?}");
        assert!(!replayed, "the row it would have replayed is gone");
        assert!(stored(&c, "m1").await, "the retry stored its own envelope");
    }

    #[tokio::test]
    async fn an_envelope_asserting_nothing_is_admitted_ungated() {
        let c = conn().await;
        append_commit(&c, "dm", 0, 0).await;
        let out = send_envelope(&c, &c, None, &send("m1", "dm", None, None))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Ok), "{out:?}");
        assert!(stored(&c, "m1").await);
    }

    #[tokio::test]
    async fn an_edit_behind_the_head_is_refused_and_the_prior_edit_survives() {
        let c = conn().await;
        append_commit(&c, "dm", 0, 0).await;
        // Head is (0, 1). A first edit lands at head.
        let first = edit_envelope(&c, &c, None, &edit("e1", "dm", "m", Some(0), Some(1)))
            .await
            .unwrap();
        assert!(matches!(first, WriteOutcome::Ok), "{first:?}");
        // The group moves on; a second edit still sealed at epoch 1 is refused
        // BEFORE it replaces the pending one.
        append_commit(&c, "dm", 0, 1).await;
        let second = edit_envelope(&c, &c, None, &edit("e2", "dm", "m", Some(0), Some(1)))
            .await
            .unwrap();
        assert!(
            matches!(second, WriteOutcome::EpochBehind { head_epoch: 2, .. }),
            "{second:?}"
        );
        assert!(
            stored(&c, "e1").await,
            "the pending edit at the old head must survive a refused replacement"
        );
        assert!(!stored(&c, "e2").await);
        // Re-sealed at the new head it replaces e1.
        let third = edit_envelope(&c, &c, None, &edit("e3", "dm", "m", Some(0), Some(2)))
            .await
            .unwrap();
        assert!(matches!(third, WriteOutcome::Ok), "{third:?}");
        assert!(!stored(&c, "e1").await);
        assert!(stored(&c, "e3").await);
    }

    #[tokio::test]
    async fn a_channel_envelope_is_gated_on_its_owning_groups_log() {
        let c = conn().await;
        c.execute_batch(
            "INSERT INTO conversation (id, kind) VALUES ('g', 'group'), ('ch', 'channel');
             INSERT INTO groups (id, name, owner_id) VALUES ('g', 'g', 'alice');",
        )
        .await
        .unwrap();
        c.execute(
            "INSERT INTO channels (id, group_id, name) VALUES ('ch', 'g', 'general')",
            (),
        )
        .await
        .unwrap();
        append_commit(&c, "g", 0, 0).await;
        // The channel id itself has no log; the gate must read the group's.
        let behind = send_envelope(&c, &c, None, &send("m1", "ch", Some(0), Some(0)))
            .await
            .unwrap();
        assert!(
            matches!(behind, WriteOutcome::EpochBehind { head_epoch: 1, .. }),
            "{behind:?}"
        );
        let ok = send_envelope(&c, &c, None, &send("m2", "ch", Some(0), Some(1)))
            .await
            .unwrap();
        assert!(matches!(ok, WriteOutcome::Ok), "{ok:?}");
        assert!(!stored(&c, "m1").await);
        assert!(stored(&c, "m2").await);
    }
}

#[cfg(test)]
mod cursor_stamp_tests {
    //! [`check_cursor_stamp`] — the admission rule for every client-chosen
    //! `sent_at` / `last_fetched_at`. The invalid state it makes unrepresentable
    //! is a stored cursor stamp that either sorts somewhere other than where its
    //! instant belongs, or sits in the far future — the `9999-…` blackout.

    use super::*;

    fn at(secs: i64, nanos: u32) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(secs, nanos).expect("valid instant")
    }

    /// Every shape the two real writers produce is admitted: the client's
    /// `AutoSi` at each fraction width, and the DS's own `Nanos`.
    #[test]
    fn admits_every_canonical_width_at_or_below_now() {
        let now = at(1_800_000_000, 0);
        for nanos in [0u32, 1_000_000, 1_001_000, 1_001_001, 999_999_999] {
            let client = at(1_799_999_000, nanos).to_rfc3339();
            assert_eq!(check_cursor_stamp(&client, now), Ok(()), "{client}");
        }
        let ds = at(1_799_999_000, 5).to_rfc3339_opts(chrono::SecondsFormat::Nanos, false);
        assert_eq!(check_cursor_stamp(&ds, now), Ok(()), "{ds}");
        // A zero-fraction instant rendered with explicit nanoseconds is also
        // canonical (it is what `now_rfc3339` writes on a second boundary).
        let ds_zero = at(1_799_999_000, 0).to_rfc3339_opts(chrono::SecondsFormat::Nanos, false);
        assert_eq!(check_cursor_stamp(&ds_zero, now), Ok(()), "{ds_zero}");
    }

    /// The skew allowance is inclusive at the boundary and refuses one
    /// nanosecond past it.
    #[test]
    fn the_future_bound_is_the_signature_window() {
        let now = at(1_800_000_000, 0);
        let at_bound = at(1_800_000_000 + CURSOR_STAMP_SKEW_SECS, 0).to_rfc3339();
        assert_eq!(check_cursor_stamp(&at_bound, now), Ok(()));
        let past_bound = at(1_800_000_000 + CURSOR_STAMP_SKEW_SECS, 1).to_rfc3339();
        assert_eq!(
            check_cursor_stamp(&past_bound, now),
            Err(StampRejection::InFuture)
        );
    }

    /// The attack value, and its close relatives.
    #[test]
    fn refuses_the_far_future() {
        let now = at(1_800_000_000, 0);
        for poison in [
            "9999-12-31T23:59:59.000000000+00:00",
            "9999-12-31T23:59:59+00:00",
            "2100-01-01T00:00:00+00:00",
        ] {
            assert_eq!(
                check_cursor_stamp(poison, now),
                Err(StampRejection::InFuture),
                "{poison}"
            );
        }
    }

    /// Strings that parse (or nearly parse) as a time but whose lexical position
    /// is not their instant's — each would let a cursor sort away from where it
    /// belongs, incl. a non-zero offset (past instant, future-sorting string).
    #[test]
    fn refuses_every_non_canonical_shape() {
        let now = at(1_800_000_000, 0);
        let past = at(1_799_990_000, 123_000_000);
        let shapes = [
            // `Z` sorts above `.` and `+` within its second.
            past.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            // A non-zero offset: instant in the past, string in the future.
            past.with_timezone(&chrono::FixedOffset::east_opt(12 * 3600).unwrap()).to_rfc3339(),
            // SQLite's own shape (the #908 seed bug): a space sorts below `T`.
            past.format("%Y-%m-%d %H:%M:%S").to_string(),
            // Lowercase separator.
            past.to_rfc3339().replacen('T', "t", 1),
            // A truncated fraction (the #692 shape) and a padded one.
            format!("{}+00:00", past.format("%Y-%m-%dT%H:%M:%S.1")),
            format!("{}+00:00", past.format("%Y-%m-%dT%H:%M:%S.1230")),
            // Not a time at all.
            String::new(),
            "not-a-timestamp".to_string(),
        ];
        for shape in shapes {
            assert_eq!(
                check_cursor_stamp(&shape, now),
                Err(StampRejection::NotCanonical),
                "{shape:?}"
            );
        }
    }

    /// The two writers this rule guards agree with it on live values, so the
    /// honest client is never refused.
    #[test]
    fn live_stamps_from_both_writers_are_admitted() {
        let now = chrono::Utc::now();
        assert_eq!(check_cursor_stamp(&now_rfc3339(), now), Ok(()));
        assert_eq!(check_cursor_stamp(&seeded_watermark_cursor(), now), Ok(()));
        assert_eq!(check_cursor_stamp(&chrono::Utc::now().to_rfc3339(), now), Ok(()));
    }
}

/// Fixtures shared by the two #1086 test modules.
#[cfg(test)]
mod delete_capability_tests_support {
    use super::*;
    use pollis_api::messages::SendMessageBody;

    /// Two members of one conversation, `alice` and `bob`, both in group `g1`.
    pub(super) async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        // Production Turso has FKs off; libsql's local backend turns them on.
        conn.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        pollis_schema::apply::single_db(&conn).await.expect("schema");
        conn.execute_batch(
            "INSERT INTO conversation (id, kind) VALUES ('c1', 'channel');
             INSERT INTO channels (id, group_id, name) VALUES ('c1', 'g1', 'chan');
             INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice');
             INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob');",
        )
        .await
        .unwrap();
        conn
    }

    /// Stand-in for the author's HMAC; the DS only ever hashes what it is handed.
    pub(super) const TOKEN: &str = "the-authors-capability";

    pub(super) fn hash_of(token: &str) -> String {
        use base64::Engine as _;
        use sha2::{Digest, Sha256};
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(token.as_bytes()))
    }

    /// Write an envelope the way a **pre-#1135 client** did: no capability at
    /// all, NULL `delete_token_hash`.
    ///
    /// A direct insert: since #1135 the send endpoint refuses such a body.
    pub(super) async fn insert_pre_1135_envelope(conn: &Connection, id: &str) {
        let seq = next_delivery_seq(conn, "c1").await.unwrap();
        conn.execute(
            "INSERT INTO message_envelope \
                 (id, conversation_id, sender_id, ciphertext, sent_at, sealed, seq) \
             VALUES (?1, 'c1', 'sealed', 'mls:00', ?2, 1, ?3)",
            libsql::params![id.to_string(), chrono::Utc::now().to_rfc3339(), seq],
        )
        .await
        .unwrap();
    }

    pub(super) fn send(id: &str, delete_token_hash: Option<String>) -> SendMessageBody {
        SendMessageBody {
            id: id.to_string(),
            conversation_id: "c1".to_string(),
            sender_id: Some("sealed".to_string()),
            ciphertext: "mls:00".to_string(),
            reply_to_id: None,
            sent_at: chrono::Utc::now().to_rfc3339(),
            sealed: 1,
            generation: None,
            epoch: None,
            push_to: None,
            push_mentions: None,
            delete_token_hash,
        }
    }
}

/// The per-envelope deletion capability (#1086).
#[cfg(test)]
mod delete_capability_tests {
    use super::delete_capability_tests_support::*;
    use super::*;
    use pollis_api::messages::DeleteMessageBody;

    fn del(id: &str, actor: &str, delete_token: Option<String>) -> DeleteMessageBody {
        DeleteMessageBody {
            message_id: id.to_string(),
            conversation_id: "c1".to_string(),
            // The self-branch hint: the caller claims to be the author.
            msg_sender_id: Some(actor.to_string()),
            actor_id: Some(actor.to_string()),
            delete_token,
        }
    }

    async fn envelope_exists(conn: &Connection, id: &str) -> bool {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM message_envelope WHERE id = ?1",
                libsql::params![id.to_string()],
            )
            .await
            .unwrap();
        let n: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        n > 0
    }

    /// **The attack.** Bob is a member of the conversation, so the pre-#1086
    /// membership and self-branch checks passed; he cannot compute Alice's capability.
    #[tokio::test]
    async fn a_member_cannot_delete_another_members_envelope() {
        let c = conn().await;
        apply_send_message(&c, Some("alice"), &send("m1", Some(hash_of(TOKEN))))
            .await
            .unwrap();

        // No token at all: presenting nothing must not be the easy way past.
        let out = apply_delete_message(&c, Some("bob"), &del("m1", "bob", None))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Forbidden), "{out:?}");
        assert!(envelope_exists(&c, "m1").await, "the envelope must survive");

        // A guessed token fares no better.
        let out = apply_delete_message(
            &c,
            Some("bob"),
            &del("m1", "bob", Some("not-the-token".to_string())),
        )
        .await
        .unwrap();
        assert!(matches!(out, WriteOutcome::Forbidden), "{out:?}");
        assert!(envelope_exists(&c, "m1").await, "the envelope must survive");
    }

    /// The author still deletes their own message — the capability must not
    /// break the thing it protects.
    #[tokio::test]
    async fn the_author_deletes_with_the_capability() {
        let c = conn().await;
        apply_send_message(&c, Some("alice"), &send("m1", Some(hash_of(TOKEN))))
            .await
            .unwrap();

        let out = apply_delete_message(
            &c,
            Some("alice"),
            &del("m1", "alice", Some(TOKEN.to_string())),
        )
        .await
        .unwrap();
        assert!(matches!(out, WriteOutcome::Ok), "{out:?}");
        assert!(!envelope_exists(&c, "m1").await, "the envelope must be gone");
    }

    /// A capability opens exactly one envelope. Holding a token for your own
    /// message must not let you remove somebody else's.
    #[tokio::test]
    async fn a_capability_does_not_travel_between_envelopes() {
        let c = conn().await;
        apply_send_message(&c, Some("alice"), &send("mine", Some(hash_of("token-mine"))))
            .await
            .unwrap();
        apply_send_message(&c, Some("bob"), &send("theirs", Some(hash_of("token-theirs"))))
            .await
            .unwrap();

        let out = apply_delete_message(
            &c,
            Some("alice"),
            &del("theirs", "alice", Some("token-mine".to_string())),
        )
        .await
        .unwrap();
        assert!(matches!(out, WriteOutcome::Forbidden), "{out:?}");
        assert!(envelope_exists(&c, "theirs").await);
    }

    /// The rollout path: a row written before capabilities existed has a NULL
    /// hash and keeps the membership-only behaviour.
    #[tokio::test]
    async fn an_envelope_from_an_older_client_keeps_the_legacy_path() {
        let c = conn().await;
        insert_pre_1135_envelope(&c, "m1").await;

        let out = apply_delete_message(&c, Some("bob"), &del("m1", "bob", None))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Ok), "{out:?}");
        assert!(!envelope_exists(&c, "m1").await);
    }

    /// **L3 — the capability must outlive its target.** An edit envelope is a
    /// separate row that names the message it edits, and envelope GC collects
    /// each row on its own watermark — so the ORIGINAL goes first (it is older)
    /// and the pending edit is left behind.
    ///
    /// The edit INHERITED the target's hash, so it is read from there now.
    #[tokio::test]
    async fn a_pending_edit_keeps_its_capability_after_the_original_is_collected() {
        let c = conn().await;
        apply_send_message(&c, Some("alice"), &send("m1", Some(hash_of(TOKEN))))
            .await
            .unwrap();
        let edit = |id: &str, sender: &str, token: Option<String>| {
            pollis_api::messages::EditMessageBody {
                envelope_id: id.to_string(),
                conversation_id: "c1".to_string(),
                target_message_id: "m1".to_string(),
                sender_id: Some(sender.to_string()),
                ciphertext: "mls:01".to_string(),
                sent_at: chrono::Utc::now().to_rfc3339(),
                generation: None,
                epoch: None,
                delete_token: token,
            }
        };
        let out = apply_edit_message(&c, Some("alice"), &edit("e1", "alice", Some(TOKEN.into())))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Ok), "{out:?}");

        // Envelope GC collects the original; the edit that names it survives.
        c.execute("DELETE FROM message_envelope WHERE id = 'm1'", ())
            .await
            .unwrap();
        assert!(envelope_exists(&c, "e1").await);

        // Bob, a member, tries to drop Alice's pending edit — as a delete of the
        // (now absent) original, which is what removes it.
        let out = apply_delete_message(&c, Some("bob"), &del("m1", "bob", None))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Forbidden), "{out:?}");
        assert!(
            envelope_exists(&c, "e1").await,
            "the pending edit must survive a member who cannot prove the capability"
        );

        // ...and as a replacing EDIT, the other door onto the same row.
        let out = apply_edit_message(&c, Some("bob"), &edit("e2", "bob", None))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Forbidden), "{out:?}");
        assert!(envelope_exists(&c, "e1").await);

        // The author, holding the capability, still can — through both doors.
        let out = apply_edit_message(&c, Some("alice"), &edit("e3", "alice", Some(TOKEN.into())))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Ok), "{out:?}");
        assert!(!envelope_exists(&c, "e1").await, "the edit was replaced");
        assert!(
            envelope_exists(&c, "e3").await,
            "and the replacement landed"
        );

        let out = apply_delete_message(&c, Some("alice"), &del("m1", "alice", Some(TOKEN.into())))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Ok), "{out:?}");
        assert!(!envelope_exists(&c, "e3").await);
    }

    /// The replacement edit must carry the capability forward even though the
    /// original it inherited from is already gone — otherwise the chain drops
    /// back to the legacy no-capability path one edit later.
    #[tokio::test]
    async fn a_replacement_edit_inherits_the_capability_from_the_edit_it_replaces() {
        let c = conn().await;
        apply_send_message(&c, Some("alice"), &send("m1", Some(hash_of(TOKEN))))
            .await
            .unwrap();
        let edit = |id: &str, token: Option<String>| pollis_api::messages::EditMessageBody {
            envelope_id: id.to_string(),
            conversation_id: "c1".to_string(),
            target_message_id: "m1".to_string(),
            sender_id: Some("alice".to_string()),
            ciphertext: "mls:01".to_string(),
            sent_at: chrono::Utc::now().to_rfc3339(),
            generation: None,
            epoch: None,
            delete_token: token,
        };
        apply_edit_message(&c, Some("alice"), &edit("e1", Some(TOKEN.into())))
            .await
            .unwrap();
        c.execute("DELETE FROM message_envelope WHERE id = 'm1'", ())
            .await
            .unwrap();
        apply_edit_message(&c, Some("alice"), &edit("e2", Some(TOKEN.into())))
            .await
            .unwrap();

        let mut rows = c
            .query(
                "SELECT delete_token_hash FROM message_envelope WHERE id = 'e2'",
                (),
            )
            .await
            .unwrap();
        let stored: Option<String> = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(
            stored.as_deref(),
            Some(hash_of(TOKEN).as_str()),
            "the replacement must carry the capability forward"
        );
    }

    /// A non-member is refused before the capability is even consulted — the
    /// membership gate stays, the capability is added to it.
    #[tokio::test]
    async fn membership_is_still_required() {
        let c = conn().await;
        apply_send_message(&c, Some("alice"), &send("m1", Some(hash_of(TOKEN))))
            .await
            .unwrap();

        let out = apply_delete_message(
            &c,
            Some("mallory"),
            &del("m1", "mallory", Some(TOKEN.to_string())),
        )
        .await
        .unwrap();
        assert!(matches!(out, WriteOutcome::Forbidden), "{out:?}");
        assert!(envelope_exists(&c, "m1").await);
    }

    /// #1135: a send with no capability is refused outright, so no NEW envelope
    /// can land on the legacy path.
    #[tokio::test]
    async fn a_send_without_a_capability_is_refused() {
        let c = conn().await;

        let out = apply_send_message(&c, Some("alice"), &send("m1", None))
            .await
            .unwrap();
        assert!(
            matches!(out, WriteOutcome::Invalid(_)),
            "a send with no delete_token_hash must be refused, got {out:?}"
        );
        assert!(
            !envelope_exists(&c, "m1").await,
            "the refused send must store nothing"
        );

        // An empty string is the same omission wearing a different hat.
        let out = apply_send_message(&c, Some("alice"), &send("m2", Some(String::new())))
            .await
            .unwrap();
        assert!(
            matches!(out, WriteOutcome::Invalid(_)),
            "an empty delete_token_hash must be refused too, got {out:?}"
        );
        assert!(!envelope_exists(&c, "m2").await);

        // And the same send with a capability lands, so the gate is not simply
        // rejecting everything.
        let out = apply_send_message(&c, Some("alice"), &send("m3", Some(hash_of(TOKEN))))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Ok), "{out:?}");
        assert!(envelope_exists(&c, "m3").await);
    }

    /// The check itself, in isolation: a missing row is `NotRequired` (the
    /// delete is a no-op, so there is nothing to refuse) and a NULL hash is the
    /// legacy path.
    #[tokio::test]
    async fn the_check_classifies_every_state() {
        let c = conn().await;
        assert_eq!(
            check_delete_capability(&c, "c1", "absent", None).await.unwrap(),
            DeleteCapability::NotRequired
        );

        // A row that EXISTS with a NULL hash — distinct from the absent case
        // above, and the only thing the legacy fallback is still for.
        insert_pre_1135_envelope(&c, "legacy").await;
        assert_eq!(
            check_delete_capability(&c, "c1", "legacy", None).await.unwrap(),
            DeleteCapability::NotRequired
        );

        apply_send_message(&c, Some("alice"), &send("guarded", Some(hash_of(TOKEN))))
            .await
            .unwrap();
        assert_eq!(
            check_delete_capability(&c, "c1", "guarded", Some(TOKEN)).await.unwrap(),
            DeleteCapability::Proved
        );
        assert_eq!(
            check_delete_capability(&c, "c1", "guarded", None).await.unwrap(),
            DeleteCapability::Refused
        );
        assert_eq!(
            check_delete_capability(&c, "c1", "guarded", Some("wrong")).await.unwrap(),
            DeleteCapability::Refused
        );

        // Scoped: the same id in another conversation is a different row, and
        // the lookup must not reach across.
        assert_eq!(
            check_delete_capability(&c, "other", "guarded", Some(TOKEN)).await.unwrap(),
            DeleteCapability::NotRequired
        );
    }
}

/// The edit half of #1086: replacing a pending edit needs proof, inserting one
/// does not.
#[cfg(test)]
mod edit_capability_tests {
    use super::delete_capability_tests_support::*;
    use super::*;
    use pollis_api::messages::EditMessageBody;

    fn edit(envelope_id: &str, target: &str, sender: &str, token: Option<&str>) -> EditMessageBody {
        EditMessageBody {
            envelope_id: envelope_id.to_string(),
            conversation_id: "c1".to_string(),
            target_message_id: target.to_string(),
            sender_id: Some(sender.to_string()),
            ciphertext: "mls:01".to_string(),
            sent_at: chrono::Utc::now().to_rfc3339(),
            generation: None,
            epoch: None,
            delete_token: token.map(str::to_string),
        }
    }

    async fn pending_edits(conn: &Connection, target: &str) -> i64 {
        let mut rows = conn
            .query(
                "SELECT COUNT(*) FROM message_envelope \
                 WHERE target_message_id = ?1 AND type = 'edit'",
                libsql::params![target.to_string()],
            )
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get(0).unwrap()
    }

    /// The clobber. `idx_envelope_one_edit_per_message` allows exactly one
    /// pending edit per message, so accepting Bob's MUST remove Alice's — there
    /// is no "accept but do not clobber" state, so Bob's edit is refused.
    #[tokio::test]
    async fn a_member_cannot_clobber_another_authors_pending_edit() {
        let c = conn().await;
        apply_send_message(&c, Some("alice"), &send("m1", Some(hash_of(TOKEN))))
            .await
            .unwrap();

        // Alice edits her own message, proving the capability.
        let out = apply_edit_message(&c, Some("alice"), &edit("e1", "m1", "alice", Some(TOKEN)))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Ok), "{out:?}");
        assert_eq!(pending_edits(&c, "m1").await, 1);

        // Bob forges one, with no capability and then with a guess.
        for token in [None, Some("not-the-token")] {
            let out = apply_edit_message(&c, Some("bob"), &edit("e2", "m1", "bob", token))
                .await
                .unwrap();
            assert!(matches!(out, WriteOutcome::Forbidden), "{out:?}");
        }

        assert_eq!(
            pending_edits(&c, "m1").await,
            1,
            "alice's pending edit must survive bob's"
        );
        let mut rows = c
            .query("SELECT COUNT(*) FROM message_envelope WHERE id = 'e1'", ())
            .await
            .unwrap();
        let alices: i64 = rows.next().await.unwrap().unwrap().get(0).unwrap();
        assert_eq!(alices, 1, "MESSAGE LOSS: bob clobbered alice's pending edit");
    }

    /// The author replaces their own pending edit, so repeated edits do not pile
    /// up — the behaviour the DELETE exists for.
    #[tokio::test]
    async fn the_author_replaces_their_own_pending_edit() {
        let c = conn().await;
        apply_send_message(&c, Some("alice"), &send("m1", Some(hash_of(TOKEN))))
            .await
            .unwrap();

        for envelope in ["e1", "e2", "e3"] {
            apply_edit_message(
                &c,
                Some("alice"),
                &edit(envelope, "m1", "alice", Some(TOKEN)),
            )
            .await
            .unwrap();
        }
        assert_eq!(
            pending_edits(&c, "m1").await,
            1,
            "the author's edits must replace, not accumulate"
        );
    }

    /// A target written before capabilities existed keeps the old replace
    /// behaviour, or editing an older message would start piling up envelopes.
    #[tokio::test]
    async fn a_legacy_target_still_replaces() {
        let c = conn().await;
        apply_send_message(&c, Some("alice"), &send("m1", None)).await.unwrap();
        apply_edit_message(&c, Some("alice"), &edit("e1", "m1", "alice", None))
            .await
            .unwrap();
        apply_edit_message(&c, Some("alice"), &edit("e2", "m1", "alice", None))
            .await
            .unwrap();
        assert_eq!(pending_edits(&c, "m1").await, 1);
    }
}

/// The delivery sequence (#1087) — the property the counter table exists for.
#[cfg(test)]
mod delivery_sequence_tests {
    use super::*;
    use pollis_api::messages::SendMessageBody;

    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        pollis_schema::apply::single_db(&conn).await.expect("schema");
        conn.execute_batch(
            "INSERT INTO conversation (id, kind) VALUES ('c1', 'channel');
             INSERT INTO channels (id, group_id, name) VALUES ('c1', 'g1', 'chan');
             INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice');
             INSERT INTO user_device (user_id, device_id) VALUES ('alice', 'a1');",
        )
        .await
        .unwrap();
        conn
    }

    fn send(id: &str) -> SendMessageBody {
        SendMessageBody {
            id: id.to_string(),
            conversation_id: "c1".to_string(),
            sender_id: Some("sealed".to_string()),
            ciphertext: "mls:00".to_string(),
            reply_to_id: None,
            sent_at: chrono::Utc::now().to_rfc3339(),
            sealed: 1,
            delete_token_hash: Some(FIXTURE_CAPABILITY_HASH.to_string()),
            generation: None,
            epoch: None,
            push_to: None,
            push_mentions: None,
        }
    }

    /// `conversation_seq.next_seq` as this connection can see it.
    async fn visible_counter(conn: &Connection) -> i64 {
        let mut rows = conn
            .query(
                "SELECT next_seq FROM conversation_seq WHERE conversation_id = 'c1'",
                (),
            )
            .await
            .unwrap();
        rows.next().await.unwrap().map(|r| r.get::<i64>(0).unwrap()).unwrap_or(0)
    }

    async fn seq_of(conn: &Connection, id: &str) -> Option<i64> {
        let mut rows = conn
            .query(
                "SELECT seq FROM message_envelope WHERE id = ?1",
                libsql::params![id.to_string()],
            )
            .await
            .unwrap();
        rows.next().await.unwrap().and_then(|r| r.get::<Option<i64>>(0).ok().flatten())
    }

    /// The counter bump and the envelope row must become visible together.
    ///
    /// Otherwise a concurrent `N+1` could publish first and strand `N` below every cursor.
    #[tokio::test]
    async fn an_in_flight_send_never_exposes_a_sequence_without_its_envelope() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ds.db");

        let writer = libsql::Builder::new_local(&path).build().await.unwrap();
        let writer = writer.connect().unwrap();
        writer.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        pollis_schema::apply::single_db(&writer).await.expect("schema");
        writer
            .execute_batch(
                "INSERT INTO conversation (id, kind) VALUES ('c1', 'channel');
                 INSERT INTO channels (id, group_id, name) VALUES ('c1', 'g1', 'chan');
                 INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice');
                 INSERT INTO user_device (user_id, device_id) VALUES ('alice', 'a1');",
            )
            .await
            .unwrap();

        // A genuinely separate connection, the way a second DS worker would be.
        let reader = libsql::Builder::new_local(&path).build().await.unwrap();
        let reader = reader.connect().unwrap();

        // One committed envelope, so the reader has a real cursor to sit on.
        apply_send_message(&writer, None, &send("m1")).await.unwrap();
        let first = seq_of(&writer, "m1").await.expect("committed envelope has a seq");

        // A send caught mid-flight: sequence taken, row written, not yet committed.
        let tx = writer.transaction().await.unwrap();
        insert_envelope_with_seq(
            &tx,
            "c1",
            "id, sender_id, ciphertext, sent_at, sealed",
            "?3, ?4, ?5, ?6, 1",
            vec![
                "m2".into(),
                "sealed".into(),
                "mls:00".into(),
                chrono::Utc::now().to_rfc3339().into(),
            ],
        )
        .await
        .unwrap();

        assert_eq!(
            seq_of(&reader, "m2").await,
            None,
            "an uncommitted envelope must not be visible to another connection"
        );
        assert_eq!(
            visible_counter(&reader).await,
            first,
            "the sequence bump must not commit ahead of the row it belongs to — \
             a reader that sees the higher counter can be handed a gap"
        );

        tx.commit().await.unwrap();

        let second = seq_of(&reader, "m2").await.expect("committed envelope has a seq");
        assert!(
            second > first,
            "a committed send must sit strictly above the cursor a reader could \
             have advanced to while it was in flight ({second} vs {first})"
        );
    }

    /// Sequences are handed out strictly increasing, per conversation.
    #[tokio::test]
    async fn sequences_increase() {
        let c = conn().await;
        for id in ["m1", "m2", "m3"] {
            apply_send_message(&c, None, &send(id)).await.unwrap();
        }
        let mut seqs: Vec<i64> = Vec::new();
        for id in ["m1", "m2", "m3"] {
            seqs.push(
                seq_of(&c, id)
                    .await
                    .expect("every envelope the DS writes carries a sequence"),
            );
        }
        assert!(seqs[0] < seqs[1] && seqs[1] < seqs[2], "{seqs:?}");
    }

    /// **The bug the counter table exists to prevent.**
    ///
    /// `MAX(seq)+1` would reissue seq 1 after GC empties a conversation, below every cursor.
    #[tokio::test]
    async fn a_sequence_is_never_reused_after_gc_empties_the_conversation() {
        let c = conn().await;
        apply_send_message(&c, None, &send("m1")).await.unwrap();
        let first = seq_of(&c, "m1").await.expect("assigned");

        // The only member device reads past it, so the real cleanup collects it.
        // The legacy cursor column uses the production RFC 3339 format (`watermark_seed_format`).
        c.execute(
            "INSERT INTO conversation_watermark \
                 (conversation_id, user_id, device_id, last_fetched_at, last_seq, reported_at) \
             VALUES ('c1', 'alice', 'a1', ?2, ?1, datetime('now'))",
            libsql::params![first, seeded_watermark_cursor()],
        )
        .await
        .unwrap();
        cleanup_conversation_envelopes(&c, "c1", false, "-12 months").await.unwrap();
        let remaining: i64 = {
            let mut rows = c
                .query("SELECT COUNT(*) FROM message_envelope WHERE conversation_id = 'c1'", ())
                .await
                .unwrap();
            rows.next().await.unwrap().unwrap().get(0).unwrap()
        };
        assert_eq!(remaining, 0, "fixture precondition: GC must have emptied it");

        // The next envelope must NOT reuse the collected sequence.
        apply_send_message(&c, None, &send("m2")).await.unwrap();
        let second = seq_of(&c, "m2").await.expect("assigned");
        assert!(
            second > first,
            "MESSAGE LOSS: sequence {second} reuses or regresses below {first} after GC \
             emptied the conversation — every device's cursor is already at {first}, so \
             this envelope reaches nobody"
        );
    }

    /// A sequence is per conversation, so two conversations number independently
    /// and one busy channel does not push another's cursor forward.
    #[tokio::test]
    async fn sequences_are_scoped_to_their_conversation() {
        let c = conn().await;
        c.execute_batch(
            "INSERT INTO conversation (id, kind) VALUES ('c2', 'channel');
             INSERT INTO channels (id, group_id, name) VALUES ('c2', 'g1', 'other');",
        )
        .await
        .unwrap();
        apply_send_message(&c, None, &send("m1")).await.unwrap();
        apply_send_message(&c, None, &send("m2")).await.unwrap();

        let mut other = send("n1");
        other.conversation_id = "c2".to_string();
        apply_send_message(&c, None, &other).await.unwrap();

        assert_eq!(seq_of(&c, "m2").await, Some(2));
        assert_eq!(
            seq_of(&c, "n1").await,
            Some(1),
            "a fresh conversation starts at 1 regardless of another's traffic"
        );
    }

    /// Edits and tombstones take sequences too — an envelope that skipped the
    /// chokepoint would be fetched by nobody and collected by nothing.
    #[tokio::test]
    async fn every_envelope_kind_is_sequenced() {
        let c = conn().await;
        apply_send_message(&c, None, &send("m1")).await.unwrap();

        apply_edit_message(
            &c,
            None,
            &pollis_api::messages::EditMessageBody {
                envelope_id: "e1".to_string(),
                conversation_id: "c1".to_string(),
                target_message_id: "m1".to_string(),
                sender_id: Some("alice".to_string()),
                ciphertext: "mls:01".to_string(),
                sent_at: chrono::Utc::now().to_rfc3339(),
                generation: None,
                epoch: None,
                // The target has a capability (#1135); replacing its edit is a delete, so present the preimage.
                delete_token: Some(FIXTURE_CAPABILITY_TOKEN.to_string()),
            },
        )
        .await
        .unwrap();
        assert!(seq_of(&c, "e1").await.is_some(), "an edit must be sequenced");

        let unsequenced: i64 = {
            let mut rows = c
                .query(
                    "SELECT COUNT(*) FROM message_envelope WHERE seq IS NULL",
                    (),
                )
                .await
                .unwrap();
            rows.next().await.unwrap().unwrap().get(0).unwrap()
        };
        assert_eq!(unsequenced, 0, "no envelope may be written without a sequence");
    }
}

/// The `000028` backfill — order preservation on a database that already holds
/// envelopes.
#[cfg(test)]
mod delivery_sequence_backfill_tests {
    use super::*;

    /// A database whose envelopes and watermarks predate the sequence: rows are
    /// inserted with `seq` NULL, exactly as they exist before the migration, and
    /// the migration's own statements are then applied.
    ///
    /// Runs the REAL migration SQL, not a re-typed copy.
    async fn migrated(rows: &[(&str, &str, &str)], cursors: &[(&str, &str)]) -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        conn.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();

        // Everything up to but NOT including 000028, so the fixture can write
        // pre-migration rows.
        conn.execute_batch(pollis_schema::BASELINE_SQL).await.expect("baseline");
        for (v, _, sql) in pollis_schema::POST_BASELINE_MIGRATIONS {
            if *v < 28 {
                conn.execute_batch(sql).await.unwrap_or_else(|e| panic!("migration {v}: {e}"));
            }
        }

        for (id, conv, sent_at) in rows {
            conn.execute(
                "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) \
                 VALUES (?1, ?2, 'sender', 'ct', ?3)",
                libsql::params![id.to_string(), conv.to_string(), sent_at.to_string()],
            )
            .await
            .unwrap();
        }
        for (device, cursor) in cursors {
            conn.execute(
                "INSERT INTO conversation_watermark \
                     (conversation_id, user_id, device_id, last_fetched_at) \
                 VALUES ('c1', 'alice', ?1, ?2)",
                libsql::params![device.to_string(), cursor.to_string()],
            )
            .await
            .unwrap();
        }

        let (_, _, sql) = pollis_schema::POST_BASELINE_MIGRATIONS
            .iter()
            .find(|(v, _, _)| *v == 28)
            .expect("000028 is registered");
        conn.execute_batch(sql).await.expect("000028");
        conn
    }

    async fn seqs(conn: &Connection, conv: &str) -> Vec<(String, i64)> {
        let mut rows = conn
            .query(
                "SELECT id, seq FROM message_envelope WHERE conversation_id = ?1 ORDER BY seq",
                libsql::params![conv.to_string()],
            )
            .await
            .unwrap();
        let mut out = Vec::new();
        while let Some(r) = rows.next().await.unwrap() {
            out.push((r.get::<String>(0).unwrap(), r.get::<i64>(1).unwrap()));
        }
        out
    }

    /// The backfill numbers existing envelopes in the order the OLD cursor would
    /// have delivered them — `sent_at`, then `id`. Getting this wrong would
    /// reorder history for anyone who had already read part of it.
    #[tokio::test]
    async fn the_backfill_preserves_the_delivery_order() {
        let conn = migrated(
            &[
                ("m3", "c1", "2026-01-01T00:00:03+00:00"),
                ("m1", "c1", "2026-01-01T00:00:01+00:00"),
                ("m2", "c1", "2026-01-01T00:00:02+00:00"),
            ],
            &[],
        )
        .await;
        let got: Vec<String> = seqs(&conn, "c1").await.into_iter().map(|(id, _)| id).collect();
        assert_eq!(got, vec!["m1", "m2", "m3"], "sequence order must match sent_at order");
    }

    /// Sequences start at 1 and are contiguous over a fresh backfill, and the
    /// counter is seeded ABOVE them so the next real send continues rather than
    /// colliding.
    #[tokio::test]
    async fn the_backfill_seeds_the_counter_above_what_it_assigned() {
        let conn = migrated(
            &[
                ("m1", "c1", "2026-01-01T00:00:01+00:00"),
                ("m2", "c1", "2026-01-01T00:00:02+00:00"),
            ],
            &[],
        )
        .await;
        assert_eq!(
            seqs(&conn, "c1").await,
            vec![("m1".to_string(), 1), ("m2".to_string(), 2)]
        );

        let next = next_delivery_seq(&conn, "c1").await.unwrap();
        assert_eq!(next, 3, "the next send must continue, not collide with m2");
    }

    /// An existing cursor maps to the highest sequence at or below where that
    /// device had read — so a device that had consumed two of three messages
    /// still receives exactly the third, and no more.
    #[tokio::test]
    async fn an_existing_cursor_maps_to_the_right_position() {
        let conn = migrated(
            &[
                ("m1", "c1", "2026-01-01T00:00:01+00:00"),
                ("m2", "c1", "2026-01-01T00:00:02+00:00"),
                ("m3", "c1", "2026-01-01T00:00:03+00:00"),
            ],
            // read through m2; and a device that has read nothing at all
            &[("read-two", "2026-01-01T00:00:02+00:00"), ("read-none", "2020-01-01T00:00:00+00:00")],
        )
        .await;

        let cursor = |device: &'static str| {
            let conn = &conn;
            async move {
                let mut rows = conn
                    .query(
                        "SELECT last_seq FROM conversation_watermark WHERE device_id = ?1",
                        libsql::params![device.to_string()],
                    )
                    .await
                    .unwrap();
                rows.next().await.unwrap().unwrap().get::<Option<i64>>(0).unwrap()
            }
        };
        assert_eq!(cursor("read-two").await, Some(2), "must not re-deliver m1 and m2");
        assert_eq!(
            cursor("read-none").await,
            Some(0),
            "a device that read nothing maps below every sequence, so it gets everything"
        );
    }
}

/// The envelope-write chokepoint, enforced on the source (#1087).
#[cfg(test)]
mod envelope_insert_site_tests {
    use std::path::{Path, PathBuf};

    /// The only production function allowed to INSERT into `message_envelope`.
    const ALLOWED_FN: &str = "insert_envelope_with_seq";

    /// Every envelope must carry a delivery sequence, and the only way to be
    /// sure is for every write to go through the one function that assigns one.
    ///
    /// A seq-less envelope is silently never fetched nor collected (`NULL > n`).
    ///
    /// A source-shape guard over production code only, not a proof.
    #[test]
    fn every_envelope_insert_goes_through_the_sequence_chokepoint() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        collect_rs(&src, &mut files);
        assert!(files.len() > 5, "walked only {} files — the walk is broken", files.len());

        // Split so this file's own source does not match itself.
        let needle = concat!("INSERT INTO ", "message_envelope");

        let mut offenders = Vec::new();
        for file in &files {
            let Ok(text) = std::fs::read_to_string(file) else {
                continue;
            };
            let rel = file.strip_prefix(&src).unwrap_or(file).display().to_string();
            let lines: Vec<&str> = text.lines().collect();
            let test_spans = cfg_test_spans(&lines);
            for (i, line) in lines.iter().enumerate() {
                if !line.contains(needle) {
                    continue;
                }
                let t = line.trim_start();
                if t.starts_with("//") || t.starts_with("///") {
                    continue;
                }
                if test_spans.iter().any(|(a, b)| i >= *a && i <= *b) {
                    continue;
                }
                let enclosing = enclosing_fn(&lines, i).unwrap_or_else(|| "<none>".into());
                if enclosing == ALLOWED_FN {
                    continue;
                }
                offenders.push(format!("{rel}:{} in fn {enclosing}", i + 1));
            }
        }

        assert!(
            offenders.is_empty(),
            "an envelope written outside `{ALLOWED_FN}` carries no delivery sequence, so it \
             is fetched by nobody and collected by nothing (#1087):\n  {}",
            offenders.join("\n  ")
        );
    }

    /// Line ranges covered by `#[cfg(test)] mod … { … }`, by brace counting.
    fn cfg_test_spans(lines: &[&str]) -> Vec<(usize, usize)> {
        let mut out = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            if lines[i].trim() == "#[cfg(test)]" {
                // Find the `{` that opens the module and count to its match.
                let mut depth = 0i32;
                let mut opened = false;
                let mut j = i;
                while j < lines.len() {
                    for ch in lines[j].chars() {
                        match ch {
                            '{' => {
                                depth += 1;
                                opened = true;
                            }
                            '}' => depth -= 1,
                            _ => {}
                        }
                    }
                    if opened && depth <= 0 {
                        break;
                    }
                    j += 1;
                }
                out.push((i, j.min(lines.len() - 1)));
                i = j + 1;
                continue;
            }
            i += 1;
        }
        out
    }

    /// The nearest `fn` at or above `line`.
    fn enclosing_fn(lines: &[&str], line: usize) -> Option<String> {
        for l in lines[..=line].iter().rev() {
            let t = l.trim_start();
            let t = t.strip_prefix("pub ").unwrap_or(t);
            let t = match t.find(") ") {
                Some(i) if t.starts_with("pub(") => &t[i + 2..],
                _ => t,
            };
            let t = t.strip_prefix("async ").unwrap_or(t);
            if let Some(rest) = t.strip_prefix("fn ") {
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                if !name.is_empty() {
                    return Some(name);
                }
            }
        }
        None
    }

    fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rs(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }
}

#[cfg(test)]
mod reaction_membership_tests {
    //! A reaction must be membership-gated even when the envelope is gone
    //! (#1161).
    //!
    //! The tests pin all three arms of the `ReactionBody.conversation_id` gate.

    use super::*;
    use libsql::Connection;

    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let c = db.connect().unwrap();
        c.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        pollis_schema::apply::single_db(&c).await.expect("schema");
        // `alice` is in g1. `mallory` is in a group of her own, so she is a real
        // authenticated account with a real membership — just not this one.
        c.execute_batch(
            "INSERT INTO conversation (id, kind) VALUES ('g1', 'group');
             INSERT INTO conversation (id, kind) VALUES ('g2', 'group');
             INSERT INTO groups (id, name, owner_id) VALUES ('g1', 'g1', 'alice');
             INSERT INTO groups (id, name, owner_id) VALUES ('g2', 'g2', 'mallory');
             INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice');
             INSERT INTO group_member (group_id, user_id) VALUES ('g2', 'mallory');",
        )
        .await
        .unwrap();
        c
    }

    async fn seed_envelope(c: &Connection, id: &str, conv: &str) {
        c.execute(
            "INSERT INTO message_envelope (id, conversation_id, sender_id, ciphertext, sent_at) \
             VALUES (?1, ?2, 'alice', 'x', '2026-01-01T00:00:00.000000000+00:00')",
            libsql::params![id.to_string(), conv.to_string()],
        )
        .await
        .unwrap();
    }

    fn body(message_id: &str, conversation_id: Option<&str>) -> ReactionBody {
        ReactionBody {
            message_id: message_id.to_string(),
            emoji: "🔥".to_string(),
            user_id: None,
            conversation_id: conversation_id.map(str::to_string),
        }
    }

    async fn reaction_count(c: &Connection, message_id: &str) -> i64 {
        let mut rows = c
            .query(
                "SELECT COUNT(*) FROM message_reaction WHERE message_id = ?1",
                libsql::params![message_id.to_string()],
            )
            .await
            .unwrap();
        rows.next().await.unwrap().unwrap().get::<i64>(0).unwrap()
    }

    /// **The finding.** The envelope has been collected, so the old gate had
    /// nothing to check and allowed the write. FAILS before the change: the
    /// outcome is `Ok` and the row lands.
    #[tokio::test]
    async fn a_non_member_cannot_react_to_a_collected_envelope() {
        let c = conn().await;
        let out = apply_add_reaction(&c, Some("mallory"), &body("m-gone", Some("g1")))
            .await
            .unwrap();
        assert!(
            matches!(out, WriteOutcome::Forbidden),
            "a declared conversation the actor is not in must be refused"
        );
        assert_eq!(reaction_count(&c, "m-gone").await, 0);
    }

    /// Declaring nothing is not an opt-out. An attacker composes the body, so a
    /// missing `conversation_id` treated as "unknown, therefore allowed" would
    /// leave the hole exactly where it was.
    #[tokio::test]
    async fn an_undeclared_conversation_is_refused_when_the_envelope_is_gone() {
        let c = conn().await;
        let out = apply_add_reaction(&c, Some("mallory"), &body("m-gone", None))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Forbidden));
        assert_eq!(reaction_count(&c, "m-gone").await, 0);

        // And the same refusal for an account that IS a member somewhere — the
        // refusal is about the absent evidence, not about who is asking.
        let out = apply_add_reaction(&c, Some("alice"), &body("m-gone", None))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Forbidden));
        assert_eq!(reaction_count(&c, "m-gone").await, 0);
    }

    /// The control: a member reacting to a collected envelope — the ordinary act
    /// the old skip existed to allow — still works.
    #[tokio::test]
    async fn a_member_can_still_react_to_a_collected_envelope() {
        let c = conn().await;
        let out = apply_add_reaction(&c, Some("alice"), &body("m-gone", Some("g1")))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Ok));
        assert_eq!(reaction_count(&c, "m-gone").await, 1);
    }

    /// A surviving envelope stays authoritative, and a body that names a
    /// DIFFERENT conversation is refused rather than ignored.
    #[tokio::test]
    async fn a_declaration_that_contradicts_the_envelope_is_refused() {
        let c = conn().await;
        seed_envelope(&c, "m-live", "g1").await;

        let out = apply_add_reaction(&c, Some("mallory"), &body("m-live", Some("g2")))
            .await
            .unwrap();
        assert!(
            matches!(out, WriteOutcome::Forbidden),
            "mallory is a member of g2, but the message is in g1"
        );
        assert_eq!(reaction_count(&c, "m-live").await, 0);

        // The envelope-present path is otherwise unchanged: a member lands, a
        // non-member does not.
        let out = apply_add_reaction(&c, Some("alice"), &body("m-live", Some("g1")))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Ok));
        let out = apply_add_reaction(&c, Some("mallory"), &body("m-live", None))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Forbidden));
        assert_eq!(reaction_count(&c, "m-live").await, 1);
    }

    /// Removal deliberately does NOT fail closed on an unresolvable
    /// conversation: the DELETE is scoped to the actor's own row, so it can only
    /// unwrite something the add gate already admitted, and refusing would
    /// strand an un-react on a message whose envelope has been collected.
    #[tokio::test]
    async fn removal_stays_possible_once_the_envelope_is_gone() {
        let c = conn().await;
        apply_add_reaction(&c, Some("alice"), &body("m-gone", Some("g1")))
            .await
            .unwrap();
        assert_eq!(reaction_count(&c, "m-gone").await, 1);

        let out = apply_remove_reaction(&c, Some("alice"), &body("m-gone", None))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Ok));
        assert_eq!(reaction_count(&c, "m-gone").await, 0);
    }

    /// And removal still cannot reach anyone else's row, declared conversation
    /// or not — the property that makes the permissive arm above safe.
    #[tokio::test]
    async fn removal_cannot_reach_another_accounts_reaction() {
        let c = conn().await;
        apply_add_reaction(&c, Some("alice"), &body("m-gone", Some("g1")))
            .await
            .unwrap();

        let out = apply_remove_reaction(&c, Some("mallory"), &body("m-gone", None))
            .await
            .unwrap();
        assert!(matches!(out, WriteOutcome::Ok));
        assert_eq!(
            reaction_count(&c, "m-gone").await,
            1,
            "the DELETE is scoped to the actor's own user_id"
        );
    }

    async fn seen_by(c: &Connection, who: &str, message_id: &str) -> Vec<(String, Vec<String>)> {
        let mut membership = std::collections::HashMap::new();
        crate::directory::visible_reactions(c, message_id, who, &mut membership)
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.emoji, r.user_ids))
            .collect()
    }

    async fn collect_envelope(c: &Connection, id: &str) {
        c.execute(
            "DELETE FROM message_envelope WHERE id = ?1",
            libsql::params![id.to_string()],
        )
        .await
        .unwrap();
    }

    /// #1433, the finding. A member's reaction to a message whose envelope GC
    /// has collected is written (the add gate above) but was never readable:
    /// the read gate asked only the envelope. FAILS before the change — empty.
    #[tokio::test]
    async fn a_reaction_to_a_collected_envelope_is_readable_by_members() {
        let c = conn().await;
        apply_add_reaction(&c, Some("alice"), &body("m-gone", Some("g1")))
            .await
            .unwrap();
        assert_eq!(
            seen_by(&c, "alice", "m-gone").await,
            vec![("🔥".to_string(), vec!["alice".to_string()])]
        );
        assert!(seen_by(&c, "mallory", "m-gone").await.is_empty());
    }

    /// #1433: a reaction made while the envelope existed outlives it. FAILS
    /// before the change — the pill vanished with the sweep.
    #[tokio::test]
    async fn a_reaction_survives_its_envelope_being_collected() {
        let c = conn().await;
        seed_envelope(&c, "m-live", "g1").await;
        apply_add_reaction(&c, Some("alice"), &body("m-live", None))
            .await
            .unwrap();
        assert_eq!(seen_by(&c, "alice", "m-live").await.len(), 1);

        collect_envelope(&c, "m-live").await;
        assert_eq!(seen_by(&c, "alice", "m-live").await.len(), 1);
        assert!(seen_by(&c, "mallory", "m-live").await.is_empty());
    }

    /// A row declared against another conversation once the envelope is gone
    /// stays in that conversation: mallory sees her own row, alice never sees
    /// it in her pills, and mallory cannot read alice's.
    #[tokio::test]
    async fn a_row_declared_elsewhere_does_not_cross_conversations() {
        let c = conn().await;
        apply_add_reaction(&c, Some("alice"), &body("m-gone", Some("g1")))
            .await
            .unwrap();
        let mut forged = body("m-gone", Some("g2"));
        forged.emoji = "💀".to_string();
        apply_add_reaction(&c, Some("mallory"), &forged).await.unwrap();

        assert_eq!(
            seen_by(&c, "alice", "m-gone").await,
            vec![("🔥".to_string(), vec!["alice".to_string()])]
        );
        assert_eq!(
            seen_by(&c, "mallory", "m-gone").await,
            vec![("💀".to_string(), vec!["mallory".to_string()])]
        );
    }

    /// A row from before the column existed is envelope-gated, as it always
    /// was, and the actor's own repeat add stamps it so it survives GC.
    #[tokio::test]
    async fn a_legacy_row_is_stamped_by_a_repeat_add() {
        let c = conn().await;
        c.execute(
            "INSERT INTO message_reaction (id, message_id, user_id, emoji) \
             VALUES ('r1', 'm-old', 'alice', '🔥')",
            (),
        )
        .await
        .unwrap();
        seed_envelope(&c, "m-old", "g1").await;
        assert_eq!(seen_by(&c, "alice", "m-old").await.len(), 1);

        collect_envelope(&c, "m-old").await;
        assert!(seen_by(&c, "alice", "m-old").await.is_empty());

        apply_add_reaction(&c, Some("alice"), &body("m-old", Some("g1")))
            .await
            .unwrap();
        assert_eq!(reaction_count(&c, "m-old").await, 1, "still one row");
        assert_eq!(seen_by(&c, "alice", "m-old").await.len(), 1);
    }
}

#[cfg(test)]
mod stale_window_tests {
    use super::{stale_modifier_from, DEFAULT_WATERMARK_STALE_MODIFIER};

    /// The code default is the documented policy (12 months, CLAUDE.md and
    /// `docs/metadata-retention-policy.md` §1), so a DS that omits the var does
    /// not silently get a shorter loss window (#1486).
    #[test]
    fn unset_or_unparseable_falls_back_to_the_documented_twelve_months() {
        assert_eq!(DEFAULT_WATERMARK_STALE_MODIFIER, "-12 months");
        assert_eq!(stale_modifier_from(None), "-12 months");
        assert_eq!(stale_modifier_from(Some("")), "-12 months");
        assert_eq!(stale_modifier_from(Some("twelve")), "-12 months");
        assert_eq!(stale_modifier_from(Some("-3")), "-12 months");
    }

    #[test]
    fn explicit_values_and_zero_disable() {
        assert_eq!(stale_modifier_from(Some("12")), "-12 months");
        assert_eq!(stale_modifier_from(Some(" 6 ")), "-6 months");
        assert_eq!(stale_modifier_from(Some("0")), "-1000 years");
    }
}
