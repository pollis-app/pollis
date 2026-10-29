//! Domain A: message envelopes, edits/deletes, reactions, watermarks and
//! attachment dedup rows (#419). Every table here lives in the main DB, not the
//! commit-log DB.
//!
//! Each handler is `gate_and_parse` → `apply_*` → `outcome_response`. The
//! `apply_*` fns take a bare [`Connection`], the authenticated user (`None` only
//! on the no-auth path) and the parsed body, and do BOTH the authz and the write,
//! so the in-process test harness runs exactly the production authz.
//!
//! ## Authorization
//!
//! - send / edit / delete: the caller is a current member
//!   ([`crate::writes::is_member`]). Authorship is not checked: under sealed
//!   sender `sender_id` is a blinded sentinel, so the client enforces it on
//!   ingest (#607). Admin-delete additionally requires group admin.
//! - reactions: a member, writing or removing only their own reaction.
//! - watermark: a member, keyed to the signing device; the cursor is admitted
//!   through [`check_cursor_stamp`] (as is every `sent_at`).
//! - envelope GC: bounded by the MIN watermark over the live member-device
//!   roster, never by message age (I3, #720). Triggered by
//!   [`sweep_envelope_gc`] (#689); [`apply_envelope_gc`] runs it for one
//!   conversation.
//! - attachment references (#690): the ref count is derived from envelope
//!   existence ([`live_ref_exists`]), so a ref is released only by deleting its
//!   envelope. Register binds each ref to the message's author (#1165); delete
//!   only needs a real device, since it collects an already-unreferenced object.
//!
//! On the no-auth path (`POLLIS_DS_REQUIRE_AUTH` off) the membership checks are
//! skipped and the actor comes from the body.

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

// Bodies live in `pollis-api` so client and DS share one declaration.
pub use pollis_api::messages::*;

// Envelope GC deletes only envelopes at or below the MIN watermark over every
// current, non-revoked, LIVE member device; never by age (I3 / F3 in
// `docs/backend-core-invariants.md`: do not add a time-based or any other
// delivery-blind arm). Liveness (#720) is keyed on the device's last report
// (`cw.reported_at`), never on its cursor or `sent_at`, and a NULL
// `reported_at` pins. Fails closed: a device with no watermark breaks the COUNT
// equality, and an empty roster aggregates to NULL. See
// `docs/metadata-retention-policy.md` §1.
//
// The roster joins live in macros (#722) so `roster_parity_tests` can SELECT the
// exact row set the DELETEs aggregate and compare it with
// `commit::current_member_devices` (I5).

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

// The `?2` liveness arm sits outside the roster macros because the commit-log
// roster has no liveness bound; inside, it would break roster parity. `?2` is
// bound only in `cleanup_conversation_envelopes`, so both GC paths share it.

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

/// Default staleness window (`POLLIS_DS_WATERMARK_STALE_MONTHS` unset), as a
/// SQLite `datetime()` modifier: the documented 12-month policy
/// (`docs/metadata-retention-policy.md` §1, #1486). It governs the third accepted
/// message loss, so changing it is a product decision.
pub const DEFAULT_WATERMARK_STALE_MODIFIER: &str = "-12 months";

/// The configured staleness window as a SQLite `datetime()` modifier, bound as
/// `?2` into the `CLEANUP_*` predicates. `0` disables the bound (`-1000 years`);
/// an unparseable value falls back to the default.
pub fn watermark_stale_modifier() -> String {
    stale_modifier_from(std::env::var("POLLIS_DS_WATERMARK_STALE_MONTHS").ok().as_deref())
}

fn stale_modifier_from(raw: Option<&str>) -> String {
    match raw.and_then(|v| v.trim().parse::<u32>().ok()) {
        Some(0) => "-1000 years".to_string(),
        Some(n) => format!("-{n} months"),
        None => DEFAULT_WATERMARK_STALE_MODIFIER.to_string(),
    }
}

/// Sweep cadence when `POLLIS_DS_GC_SWEEP_SECS` is unset or unparseable.
pub const DEFAULT_GC_SWEEP_SECS: u64 = 3600;

/// The envelope-GC cadence the sweep loop binds, in seconds (`0` disables).
/// `GET /v1/config` reports this same value, never the raw env string, so the
/// two cannot disagree (#760); a typo reports as the default.
pub fn gc_sweep_secs() -> u64 {
    std::env::var("POLLIS_DS_GC_SWEEP_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_GC_SWEEP_SECS)
}

/// Identity-free growth metrics for `message_envelope`, served on
/// `GET /v1/retention/metrics`. Holds no conversation or user id
/// (`docs/metadata-retention-policy.md` §3).
#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct RetentionSnapshot {
    /// Total surviving `message_envelope` rows across all conversations.
    pub total_envelopes: i64,
    /// How many distinct conversations still hold at least one envelope.
    pub conversations_with_envelopes: i64,
    /// The oldest `sent_at` still retained anywhere (a timestamp, not an id).
    pub oldest_sent_at: Option<String>,
    /// The largest single conversation's envelope count (no id).
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
/// These are compared lexically against client `to_rfc3339()` stamps, which
/// carry sub-second digits, so the DS always emits nanoseconds: a whole-second
/// `…:00+00:00` sorts below `…:00.123+00:00` (`'+'` < `'.'`).
pub(crate) fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, false)
}

/// The value every server-side seed writes into
/// `conversation_watermark.last_fetched_at` (#908): device registration, DM
/// create / member-add and group join all go through here.
///
/// It must be RFC 3339, because it is compared lexically against `sent_at`;
/// SQLite's `datetime('now')` sorts below every RFC 3339 stamp. `reported_at` is
/// the deliberate exception: it is compared against `datetime('now', ?)`, so it
/// stays in SQLite's format.
pub(crate) fn seeded_watermark_cursor() -> String {
    now_rfc3339()
}

// Client-chosen `sent_at` / `last_fetched_at` stamps are bounded, not trusted:
// one far-future stamp (`9999-…`) would push every recipient's cursor past all
// later mail and let GC delete the conversation. The value stays client-chosen
// (the sender's local copy carries it, #844); the DS admits only a canonical UTC
// RFC 3339 stamp no further ahead of its clock than the signature window.

/// How far ahead of the DS clock a client-chosen `sent_at` /
/// `last_fetched_at` may sit. The request-signature replay window
/// ([`crate::auth::REPLAY_WINDOW_SECS`]): a client that could not sign inside
/// it cannot post at all, so the bound costs an honest client nothing new.
pub const CURSOR_STAMP_SKEW_SECS: i64 = crate::auth::REPLAY_WINDOW_SECS;

/// Why a client-chosen cursor stamp was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampRejection {
    /// Not a canonical UTC RFC 3339 rendering (`…+00:00`, 0/3/6/9 fraction
    /// digits), the only shape that sorts chronologically against the column.
    NotCanonical,
    /// Parses, but denotes an instant more than [`CURSOR_STAMP_SKEW_SECS`] past
    /// the DS clock.
    InFuture,
}

/// The one admission check for a client-chosen `sent_at` or `last_fetched_at`.
/// `now` is injected for testing.
///
/// Canonical = RFC 3339 UTC that re-renders byte-for-byte at one of chrono's
/// four fraction widths (what `envelope_sent_at` and [`now_rfc3339`] produce).
/// `Z`, non-zero offsets, a space separator, odd fraction widths and SQLite's
/// format all fail: each sorts somewhere other than its instant.
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

    // Wake recipients only after the envelope landed (#987), detached so a push
    // failure never fails the send. The sender is the authenticated user, never
    // `body.sender_id` (a sealed-sender sentinel, #331). A replay (#1335) already
    // pushed.
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

/// INSERT a `type='message'` envelope. Authz: the authenticated user is a
/// current member.
///
/// Unsealed: the stored `sender_id` is bound to the authenticated user. Sealed
/// (#331): the body's `sender_id` is a blinded sentinel stored as-is; membership
/// is still checked against the authenticated user.
pub async fn apply_send_message(
    conn: &Connection,
    authed: Option<&str>,
    body: &SendMessageBody,
) -> anyhow::Result<WriteOutcome> {
    let sealed = body.sealed != 0;
    let (member_check_user, stored_sender) = if sealed {
        match authed {
            // Store the client's blinded sentinel, never the auth user.
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
    // Bounded before it can touch the table.
    if let Err(refused) = admit_sent_at(&body.sent_at) {
        return Ok(refused);
    }
    // Every new envelope must carry a deletion capability (#1135); without one
    // any member could delete another's unfetched envelope. `Invalid`, not
    // `Forbidden`: the caller may send, the body is inadmissible.
    if body.delete_token_hash.as_deref().unwrap_or("").is_empty() {
        return Ok(WriteOutcome::Invalid(
            "delete_token_hash is required on every new envelope",
        ));
    }
    // Seq allocation and the row land in one transaction.
    let tx = conn.transaction().await?;
    insert_envelope_with_seq(
        &tx,
        &body.conversation_id,
        // The deletion capability (#1086): an HMAC under a key only the author's
        // devices hold, stored as sent.
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

// The epoch gate (#1041): an envelope is decryptable only at the epoch it was
// sealed at (`max_past_epochs = 0`), so the DS refuses one whose asserted
// `(generation, epoch)` is not the commit-log head, answering 409
// `epoch_behind` with the head so the sender re-seals. No assertion (an older
// client) is admitted ungated.
//
// Insert-then-verify, because envelopes and the commit log may be separate
// databases. A concurrent commit CAS appends before its winner sweeps the
// closing epoch's envelopes: CAS before our head read → we delete and 409; CAS
// after → the committer's pre-merge sweep fetches our row. Either way no
// envelope is left at an epoch nobody can open.
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

/// [`send_envelope`], also returning whether the post was a REPLAY of a stored
/// envelope (`true`: nothing written, don't push again).
///
/// Replays come from the client's durable outbox (#1335) and are answered as
/// success without touching the stored row. Checked BEFORE the epoch gate,
/// whose post-check would otherwise delete the delivered row.
///
/// Every post of an id holds that id's lock across the replay check and the
/// gated write; otherwise a retry could see a row the first post's post-check
/// then deletes, and answer "delivered" for a lost message. The DS is
/// single-instance, so a process-local lock suffices.
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
/// `Ok` only when it is provably the same message from the same author: a
/// `message` in this conversation whose deletion-capability hash (an HMAC only
/// the author's devices can produce, #1086) matches. Current membership is not
/// consulted, so a sender removed after an unrecorded post still settles.
/// Anything else is `Forbidden`, like a non-member, revealing nothing about the
/// id.
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
/// transaction. Authz: the editor is a current member and holds the target's
/// deletion capability.
///
/// Authorship is not checked here (#607): the recipient's ingest drops an edit
/// whose MLS-authenticated author is not the target's author.
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
    // An edit REPLACES the target's pending edit (one per message,
    // `idx_envelope_one_edit_per_message`), so it is a delete and needs the
    // target's capability (#1086). That proves possession of a secret, not an
    // identity, so sealed sender (#607) still holds. Checked before the
    // transaction, so a refusal removes nothing.
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
    // Read BEFORE the DELETE below: the fallback reads the very edit row it
    // removes, on the same connection.
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
    /// The row carries no capability (pre-#1086, or already gone): fall back to
    /// the membership check.
    NotRequired,
    /// The presented token hashes to the stored value.
    Proved,
    /// The row HAS a capability and the caller did not open it.
    Refused,
}

/// Check `token` against the capability stored on `message_id`.
///
/// The sender stored `SHA-256(token)` at write time, where the token is an HMAC
/// under a key only the author's devices hold, so the DS can neither compute a
/// token nor replay the stored value as one. Scoped to `conversation_id`, the id
/// the caller was authorized against.
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
        // Target gone: its pending edit outlives it (GC collects rows
        // independently) and inherited its capability, so check that instead.
        // Otherwise any member could drop the pending edit uncontested (L3).
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

/// The capability hash carried by the PENDING EDIT of `message_id`: the last
/// copy once the target itself is collected.
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

/// The capability hash stored on `message_id`, for an edit envelope to inherit
/// so the next edit proves authorship of the original. Falls back to the edit
/// being replaced, so an edit written after the original was collected does not
/// inherit `NULL` (L3).
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
/// Never `MAX(seq)+1` over `message_envelope`: GC deletes rows, so an emptied
/// conversation would restart at 1, below every cursor, and the envelope would
/// reach nobody. `conversation_seq` outlives the rows; only teardown removes it.
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

/// Insert an envelope with the next delivery sequence (#1087). Every envelope
/// (send, edit, tombstone) goes through here.
///
/// Takes a `Transaction` on purpose: a writer slipping between allocation and
/// insert could make `N+1` visible while `N` is missing, a recipient would
/// advance past `N`, and `N` would reach nobody. A transaction whose first
/// statement is a write holds the write lock to commit, so writers serialise.
///
/// `columns` names the columns after `conversation_id` and `seq`;
/// `placeholders` supplies them from `?3`, bound from `params` in order.
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

/// Delete a message. The branch is the client's `msg_sender_id` hint, since
/// sealed sender leaves the DS unable to derive authorship (#607).
///
/// - Self (`msg_sender_id == actor`): membership plus the deletion capability
///   (#1086). Removes the envelope and any pending edit; no tombstone.
/// - Admin (otherwise): the actor must be a group admin of the channel. Removes
///   the envelope and pending edit and writes a `type='delete'` tombstone.
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
        if authed.is_some() && !is_member(conn, &body.conversation_id, &actor).await? {
            return Ok(WriteOutcome::Forbidden);
        }
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
        // Scoped to the authorized conversation: otherwise membership of any
        // conversation would authorize deleting any envelope by id.
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

    if authed.is_some() {
        match channel_group_role(conn, &body.conversation_id, &actor).await? {
            Some(role) if role == "admin" => {}
            _ => return Ok(WriteOutcome::Forbidden),
        }
    }

    let tombstone_id = Ulid::new().to_string();
    // The tombstone takes the next seq like any envelope, above every cursor by
    // construction (#1087); `sent_at` is display-only.
    let now = now_rfc3339();
    // Scoped, or an admin of one group could delete from any conversation.
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

/// Resolve the conversation a reaction is gated against, or `None` to refuse
/// (#1161).
///
/// GC collects most envelopes, so the client also declares the conversation
/// (`ReactionBody.conversation_id`, from its local copy):
///
/// - envelope present: its conversation is authoritative; a body declaring a
///   different one is refused.
/// - envelope gone: gate on the declared one.
/// - neither: refuse (an attacker would just omit the field).
///
/// Gated by `is_member`, not `in_desired_roster`: a reaction has no
/// pre-acceptance step like the invite handshake.
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

/// Membership-gated against [`reaction_conversation`].
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
    // Records the gated conversation for the read gate once GC collects the
    // message (#1433); a repeat add stamps a row that predates the column.
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
    // Scoped to the actor's own row, so it cannot touch anyone else's. Unlike
    // add, this does not fail closed on an unresolvable conversation: removal
    // grants nothing, and refusing would strand an un-react on a collected
    // message.
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

/// Monotone UPSERT of a `(conversation, user, device)` watermark. Authz: both
/// halves of the key are bound to the verified signature and the signed user is
/// a current member; the cursor is admitted only through [`check_cursor_stamp`].
///
/// `reported_at` is the liveness signal the GC staleness bound reads (#720). It
/// is stamped even when the cursor doesn't move, so a live device keeps pinning
/// and a returning device counts again at once. It stays separate from the
/// cursor, which may legitimately be old (F3).
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
    // Both cursors take `MAX(existing, reported)`, so neither can rewind.
    // `last_seq` is the one delivery and retention read (#1087); a device that
    // reports only `last_fetched_at` has no `last_seq` and so holds GC back.
    // A negative `last_seq` is refused, not clamped: sequences start at 1.
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

/// Run the watermark-gated envelope GC for one conversation on demand. Authz:
/// the actor is a current member (skipped on the no-auth path).
///
/// The decision consults the whole live member-device roster, so whoever fires
/// it, no device's undelivered mail is deleted. `stale` is the `?2` liveness
/// window, the same value [`sweep_envelope_gc`] passes.
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

/// The single place the `CLEANUP_*` predicate runs, shared by
/// [`apply_envelope_gc`] and [`sweep_envelope_gc`] so they cannot drift. No
/// authz: callers gate.
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

// Aged-out records (#762) run on the same sweep as envelope GC: one schedule,
// one place that can fail.

/// Days a `security_event` row is retained (default 90, `0` = forever). It is a
/// per-user, device-attributed audit trail: long enough to investigate a late
/// incident, short of a standing profile.
pub fn security_event_retention_days() -> u32 {
    std::env::var("POLLIS_DS_SECURITY_EVENT_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(90)
}

/// Days an unrefreshed `push_token` row is retained (default 180, `0` disables).
/// Clients re-register on every launch, so a reaped live token costs one missed
/// notification, not data.
pub fn push_token_retention_days() -> u32 {
    std::env::var("POLLIS_DS_PUSH_TOKEN_RETENTION_DAYS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .unwrap_or(180)
}

/// Delete `security_event` rows older than `sec_days` and `push_token` rows
/// unrefreshed for `tok_days`. Returns `(security_events_deleted,
/// push_tokens_deleted)`.
///
/// Age is a safe key here, unlike for envelopes: these rows' age is the thing
/// being bounded. `conversation_watermark` is deliberately not reaped
/// (`docs/metadata-retention-policy.md`). The windows are parameters so tests
/// need not mutate process env.
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

/// The server-side envelope-GC trigger (#689): run the watermark-gated cleanup
/// for every conversation that still has envelopes, gathering the identity-free
/// growth metrics on the same walk. `stale` is the `?2` liveness window (#720).
///
/// A conversation is a DM iff its id is in `dm_channel_member`. The wrong
/// predicate is a no-op (empty roster → NULL bound), so a misclassified id
/// never over-deletes.
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
    // Post-cleanup counts; the worst offender is tracked by shape, never id.
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
    // Clear orphaned `attachment_ref` rows (#690). The count is derived, so this
    // changes no deletion decision; it keeps the table bounded and reclaims
    // forged declarations. Runs even when nothing was visited.
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

// The reference count is DERIVED, not maintained (#690): an `attachment_ref`
// declaration counts only while its `message_envelope` exists, so every path
// that deletes an envelope releases its references without doing anything, and
// `/v1/attachments/delete` never mutates `attachment_ref` (a forged call cannot
// strand another member's file). Declarations naming no live envelope are inert
// and reaped by [`sweep_envelope_gc`].
//
// The vault leg (#107) works the same way: a `vault_attachment_ref` counts while
// its `vault_message` exists, so a file kept only in a vault survives.
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

/// Collect the shared dedup row iff no live reference remains, in one statement,
/// so a concurrent reference cannot slip in between the test and the delete.
const COLLECT_UNREFERENCED_OBJECT_SQL: &str = concat!(
    "DELETE FROM attachment_object WHERE content_hash = ?1 AND NOT ",
    live_ref_exists!()
);

/// Reap declaration rows whose envelope is gone. Hygiene only: the derived count
/// is already correct without it.
const REAP_ORPHANED_ATTACHMENT_REFS_SQL: &str = "\
DELETE FROM attachment_ref \
 WHERE NOT EXISTS (SELECT 1 FROM message_envelope me WHERE me.id = attachment_ref.message_id)";

/// The vault leg of the same hygiene; catches rows orphaned by account teardown
/// or a crash (`/v1/vault/delete` removes them inline).
const REAP_ORPHANED_VAULT_REFS_SQL: &str = "\
DELETE FROM vault_attachment_ref \
 WHERE NOT EXISTS (SELECT 1 FROM vault_message vm WHERE vm.id = vault_attachment_ref.vault_message_id)";

/// True when `message_id` names a still-existing envelope the caller provably
/// wrote (#1165).
///
/// An unsealed envelope names its sender. A sealed one (#331) carries the
/// sentinel `"sealed"`, so the proof is its deletion capability (#1086), which
/// only the author can open and which identifies no one. A sealed envelope with
/// no stored hash is refused.
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

/// Register an attachment object row (`content_hash → r2_key`) and, on the send
/// path, a `(content_hash, message_id)` reference declaration (#690).
/// `content_hash` is the object's random file id (#1382), not key material.
///
/// The object row is identical for every uploader, so any authenticated device
/// may register one. A reference pins a shared blob against collection, so it is
/// bound to the message's author (#1165, [`envelope_is_authored_by`]). A member
/// re-pinning a file they received onto their own message is indistinguishable
/// from an honest re-send and denies no one anything, so it is allowed. Nothing
/// records who uploaded or who declared.
///
/// Both INSERTs are `OR IGNORE`, so a retried send is idempotent. The authorship
/// check is skipped on the no-auth path.
pub async fn apply_register_attachment(
    conn: &Connection,
    authed: Option<&str>,
    body: &AttachmentRegisterBody,
) -> anyhow::Result<WriteOutcome> {
    // Decided before the transaction, so a refusal writes nothing, not even the
    // object row.
    if let (Some(actor), Some(message_id)) = (authed, body.message_id()) {
        if !envelope_is_authored_by(conn, message_id, actor, body.delete_token()).await? {
            return Ok(WriteOutcome::Forbidden);
        }
    }
    let tx = conn.transaction().await?;
    // No uploader or registrar is recorded: lifetime depends only on live
    // references, so either would be an identity linkage with no consumer.
    tx.execute(
        "INSERT OR IGNORE INTO attachment_object (content_hash, r2_key) VALUES (?1, ?2)",
        libsql::params![body.content_hash().to_string(), body.r2_key().to_string()],
    )
    .await?;
    if let Some(message_id) = body.message_id() {
        tx.execute(
            "INSERT OR IGNORE INTO attachment_ref (content_hash, message_id) VALUES (?1, ?2)",
            libsql::params![body.content_hash().to_string(), message_id.to_string()],
        )
        .await?;
    }
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

/// Collect the shared `attachment_object` row once no live reference remains
/// (#690, resolving the second `TODO(#419)`). Authz: any authenticated user,
/// since the derived count alone decides whether the object may go, for every
/// caller alike (uploader included).
///
/// It never releases a reference; that happens only by deleting the envelope.
/// `message_id` stays in the body for wire-compat and is unused, so a forged
/// call naming another member's message strands nothing. The R2 blob is gated by
/// the same [`object_is_referenced`] predicate in `/v1/r2/presign`.
pub async fn apply_delete_attachment(
    conn: &Connection,
    _authed: Option<&str>,
    body: &AttachmentDeleteBody,
) -> anyhow::Result<WriteOutcome> {
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

/// Drop every declaration naming `content_hash` (message and vault side) once
/// the object row is collected: a stale declaration would keep
/// [`object_is_referenced`] true and refuse the PUT that restores the bytes.
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

/// True when at least one still-existing message or vault entry references
/// `content_hash` (#690). The single gate for both the Turso collect
/// ([`apply_delete_attachment`]) and the R2 delete presign
/// ([`crate::broker::r2_presign`]).
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

    /// A DS stamp must sort above a client stamp from earlier in the same second:
    /// a whole-second `…:20+00:00` sorts below `…:20.000000001+00:00`.
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
    //! The envelope-GC cleanup SQL against a real in-memory libsql DB.
    //!
    //! - I3: an envelope far older than 30 days that a member device has not
    //!   collected must survive (the `no_ttl_*` tests, F3).
    //! - #685: a revoked device must not wedge deletion, either by a stale
    //!   watermark pinning `MIN(cw)` or by a missing row breaking the COUNT
    //!   equality.

    use super::*;


    /// Wide enough that fixtures with `reported_at = NULL` (treated as live) are
    /// never excluded; the #720 tests set `reported_at` explicitly.
    const TEST_STALE: &str = "-6 months";

    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        // Turso runs with foreign keys off; local libsql defaults them on.
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

    /// Seed a watermark at delivery sequence `seq` with `reported_at` NULL
    /// (treated as live). `last_fetched_at` is NOT NULL but irrelevant here.
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

    /// Seed a watermark with both the read position (`seq`) and the report time
    /// (`reported`), the two kinds #720 keeps apart.
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
        // Advance `conversation_seq` as `insert_envelope_with_seq` does.
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
        // Claim the `conversation` registry first: 000017's triggers require it
        // (#948).
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

    /// F3 regression: a live member device whose cursor sits below an envelope
    /// keeps it, however old it is.
    #[tokio::test]
    async fn no_ttl_channel_uncollected_ancient_envelope_survives() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob')", ())
            .await
            .unwrap();
        add_device(&conn, "alice", "a1", false).await;
        add_device(&conn, "bob", "b1", false).await;
        // Alice is caught up; bob's cursor is below the envelope, so it is still
        // owed to him.
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

    /// GC is bounded, not disabled: once every member device has collected past
    /// an envelope, it goes.
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

    /// Zero current member devices deletes nothing: the aggregate degenerates to
    /// NULL, never to "everybody collected".
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

    /// One sweep, with no member ingesting, collects every caught-up conversation
    /// (a channel and a DM, so both predicates are classified) and spares one
    /// with an uncollected recipient.
    #[tokio::test]
    async fn sweep_collects_quiet_conversations_without_any_ingest() {
        let conn = conn().await;

        // A channel whose one member device has caught up. Distinct users per
        // conversation: `user_device` is global, so a member's device in one
        // conversation counts toward every conversation they belong to.
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

        // The snapshot reflects post-cleanup survivors: only c2's envelope.
        assert_eq!(report.metrics.total_envelopes, 1, "one envelope survives GC (eB in c2)");
        assert_eq!(report.metrics.conversations_with_envelopes, 1, "held by one conversation");
        assert_eq!(report.metrics.largest_conversation_envelopes, 1, "worst offender holds one");
    }

    /// A device silent past the staleness window stops pinning: its envelope is
    /// collected once every live device has read past it.
    #[tokio::test]
    async fn channel_dormant_device_stops_pinning_after_the_bound() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'bob')", ())
            .await
            .unwrap();
        add_device(&conn, "alice", "a1", false).await;
        add_device(&conn, "bob", "b1", false).await;
        // Alice is live and caught up; bob last reported 400 days ago (dormant).
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

    /// Anti-F3: a recently-reported device pins every envelope below its cursor,
    /// however old; liveness is `reported_at`, never message or cursor age. The
    /// two legs differ only in `reported_at`, so the outcome pivots on liveness.
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
            // Reported one day ago: live.
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

    /// A NULL `reported_at` (never stamped) pins: unknown report time is not
    /// evidence of dormancy.
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

    /// An all-stale roster is empty, and an empty roster deletes nothing.
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

    /// The GC sweep releases a swept message's attachment reference (#690): the
    /// object becomes collectable and the orphaned declaration is reaped.
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

    /// A reference to a message that never had an envelope (a forged register)
    /// is never counted, and the sweep reaps it.
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
    //! I5: the envelope-GC rosters ([`CLEANUP_CHANNEL_ENVELOPES`],
    //! [`CLEANUP_DM_ENVELOPES`]) and [`crate::commit::current_member_devices`]
    //! must agree (#722). A narrower SQL roster deletes mail a device still
    //! needs; a wider one wedges retention.
    //!
    //! The SELECTs here are built from the same roster macros the DELETEs use, so
    //! there is one copy of the roster SQL.
    //! [`the_roster_is_what_the_delete_actually_gates_on`] ties the fragment to
    //! the DELETE's actual outcome.

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
        // Turso runs with foreign keys off; local libsql defaults them on.
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
        // Advance `conversation_seq` as `insert_envelope_with_seq` does.
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

    /// The roster the cleanup SQL measures against, sorted. Duplicates are kept:
    /// they would inflate `COUNT(ud.device_id)` and must show as a mismatch.
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

    /// The devices both rosters must resolve to, spelled out so a change made
    /// symmetrically to the SQL and the Rust roster still fails.
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
    /// A roster that leaked across conversations would delete one channel's mail
    /// on another channel's watermarks.
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

    /// An all-revoked member alone in the conversation yields the empty roster
    /// on both paths. Disagreement is costliest here: an empty roster disables
    /// envelope GC but removes the commit log's Tier-1 lower bound.
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

    /// Pins the exact SQL the DS executes, so the macro factoring cannot change
    /// it.
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

    /// Ties the roster fragment to the DELETE's behaviour: with every listed
    /// device past the envelope it goes; with one listed device behind it stays,
    /// however caught-up the excluded devices are.
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
            // `reported_at` is NULL (live), so the #720 arm excludes no one.
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
                // Only `bob-silent` is below the envelope (seq 2).
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
    //! #693 / #661: the admin-delete tombstone always reaches a caught-up
    //! recipient at the envelope/watermark layer, using the real DS write code
    //! and the production fetch, across two connections of one shared libsql
    //! `Database` (the flows harness's model: one `Db`, pooled connections). So
    //! #661's residual flake lies downstream, in the client's application of the
    //! tombstone.

    use super::*;
    use tempfile::TempDir;


    /// The shape a client writes for `sent_at` (`to_rfc3339()`, AutoSi: 0/3/6/9
    /// fraction digits), at `secs` plus `nanos`.
    fn client_stamp(secs: i64, nanos: u32) -> String {
        chrono::DateTime::from_timestamp(secs, nanos)
            .expect("valid instant")
            .to_rfc3339()
    }

    /// One shared libsql `Database` on a WAL file, as the flows harness gives its
    /// in-process DS. The tempdir is returned to keep it alive.
    async fn shared_db() -> (TempDir, libsql::Database) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("main.db");
        let db = libsql::Builder::new_local(&path).build().await.expect("build");
        {
            let conn = db.connect().expect("connect");
            conn.query("PRAGMA journal_mode=WAL", ()).await.expect("wal");
            // Turso runs with foreign keys off; local libsql defaults them on.
        conn.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        pollis_schema::apply::single_db(&conn).await.expect("schema");
        }
        (dir, db)
    }

    /// The production per-device fetch, as `(id, type, target_message_id)`.
    async fn ingest_fetch(
        conn: &Connection,
        conversation_id: &str,
        user_id: &str,
        device_id: &str,
    ) -> Vec<(String, String, Option<String>)> {
        // The production fetch, not a copy of its predicate, so it cannot drift
        // (#1087).
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

    /// The #661 dance for one client `sent_at` shape: writes on one connection,
    /// carol's reads on another of the same `Database`; her second fetch must see
    /// the admin tombstone.
    async fn assert_tombstone_reaches_carol(msg_sent_at: &str) {
        let (_dir, db) = shared_db().await;
        let w = db.connect().expect("write conn");
        let carol = db.connect().expect("carol read conn");

        let conv = "chan-general";
        let msg_id = "msg-bobs-post";

        // bob sends (sealed, no-auth path).
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
                generation: None,
                epoch: None,
                push_to: None,
                push_mentions: None,
                delete_token_hash: Some(FIXTURE_CAPABILITY_HASH.to_string()),
            },
        )
        .await
        .expect("send");

        // carol fetches bob's message and reports her watermark, as ingest does.
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

        // alice admin-deletes (`msg_sender_id != actor` selects the admin
        // branch; the no-auth path skips the role check).
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

        // carol's second fetch must see the tombstone (the #661 assertion).
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

    /// A whole second ahead of the DS clock, inside the
    /// [`CURSOR_STAMP_SKEW_SECS`] window a client stamp must sit within.
    fn base_second_ahead() -> i64 {
        chrono::Utc::now().timestamp() + CURSOR_STAMP_SKEW_SECS / 2
    }

    /// Across every `sent_at` fraction width a client can emit, and a client
    /// clock ahead of the DS, carol always fetches the tombstone.
    #[tokio::test]
    async fn admin_tombstone_always_reaches_a_caught_up_recipient() {
        let base = base_second_ahead();
        for nanos in [0u32, 1_000_000, 1_001_000, 1_001_001, 999_999_999] {
            assert_tombstone_reaches_carol(&client_stamp(base, nanos)).await;
        }
        // Client clock ahead of the DS by as much as the stamp bound admits.
        let ahead = (chrono::Utc::now() + chrono::Duration::seconds(CURSOR_STAMP_SKEW_SECS - 30))
            .to_rfc3339();
        assert_tombstone_reaches_carol(&ahead).await;
    }

    /// After a DS tombstone, a message the client stamps next is still fetched,
    /// even when its `sent_at` sorts below the tombstone's: the cursor is a
    /// DS-assigned sequence (#1087), so the stamp is display-only. The client
    /// side (pollis-core never truncating a real fraction) is pinned in
    /// `pollis_core::commands::messages::sent_at_format_tests`.
    #[tokio::test]
    async fn a_zero_nanosecond_client_message_still_outsorts_the_preceding_tombstone() {
        let (_dir, db) = shared_db().await;
        let w = db.connect().expect("write conn");
        let carol = db.connect().expect("carol read conn");

        let base = base_second_ahead();
        let conv = "chan-general";

        // An existing message at `…:00.999999999`, the highest fraction.
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
                generation: None,
                epoch: None,
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

        // Carol fetches the tombstone and advances onto it.
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
        // The tombstone's `sent_at` may sort below the message it redacts; its
        // seq is `MAX(seq)+1`, above every cursor (#1087).
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
        // Carol fetched the tombstone above regardless of how its `sent_at`
        // sorts against her `last_fetched_at`.
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

        // A fraction-less stamp from an earlier second (a client clock behind
        // the DS), below the tombstone's. The DS bounds `sent_at` only against
        // the future, so it is admissible.
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
                generation: None,
                epoch: None,
                push_to: None,
                push_mentions: None,
                delete_token_hash: Some(FIXTURE_CAPABILITY_HASH.to_string()),
            },
        )
        .await
        .expect("send after tombstone");
        // Delivered even though it sorts below the tombstone: the cursor is a
        // sequence and the stamp is decoration (#1087).
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

    /// A row written on one connection of a shared libsql `Database` is
    /// immediately visible on another connection of the same handle (the
    /// guarantee the flows harness relies on); a second, independent `Database`
    /// on the same file is not guaranteed to see it promptly.
    #[tokio::test]
    async fn read_your_writes_holds_across_connections_of_one_shared_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("main.db");

        let db = libsql::Builder::new_local(&path).build().await.expect("build");
        {
            let c = db.connect().expect("connect");
            c.query("PRAGMA journal_mode=WAL", ()).await.expect("wal");
            // Turso runs with foreign keys off; local libsql defaults them on.
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
    //! `/v1/messages/delete` must only touch the conversation it authorized
    //! against: authz reads `conversation_id` while the DELETE keys on
    //! `message_id`, so without the scope membership of any conversation would
    //! delete any envelope. The tests assert the victim row survives, not merely
    //! that the call returned Ok.

    use super::*;
    use libsql::Connection;


    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let c = db.connect().unwrap();
        // Turso runs with foreign keys off; local libsql defaults them on.
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

    /// #1335: an outbox re-post of a landed envelope is success and stores
    /// nothing new, even when sealed at an epoch the group has since left.
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
    /// Holding the id's lock stands in for that post mid-write; its row
    /// vanishing stands in for the post-check deleting it. The retry, released
    /// only then, must write its own envelope — not report the vanished one.
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
    /// belongs. Includes a non-zero offset that denotes a PAST instant yet sorts
    /// twelve hours into the future, which the chronological bound alone would
    /// have admitted.
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
        // Turso runs with foreign keys off; local libsql defaults them on.
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

    /// Write an envelope as a pre-#1135 client did, with a NULL
    /// `delete_token_hash`. A direct insert, because `send` now refuses such a
    /// body; this is the legacy population the fallback still serves.
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

    /// The attack: bob's `msg_sender_id == bob` hint selects the self-branch,
    /// but without alice's capability he cannot delete her envelope.
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

    /// A pre-capability row (NULL hash) keeps the membership-only delete.
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

    /// L3: GC collects the original before its pending edit, and the edit
    /// inherited the target's capability, so the edit stays guarded once the
    /// original is gone.
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

    /// #1135: a send with no capability is refused, so no new envelope can land
    /// on the legacy path.
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

    /// One pending edit per message (`idx_envelope_one_edit_per_message`), so
    /// accepting bob's would remove alice's; it is refused instead.
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

    /// The counter bump and the envelope row become visible together: a reader
    /// mid-send sees neither, so it can never advance past a missing `N`.
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

    /// The counter outlives GC: once GC empties a conversation, the next
    /// envelope must not restart at 1, below every cursor.
    #[tokio::test]
    async fn a_sequence_is_never_reused_after_gc_empties_the_conversation() {
        let c = conn().await;
        apply_send_message(&c, None, &send("m1")).await.unwrap();
        let first = seq_of(&c, "m1").await.expect("assigned");

        // The only member device reads past it, so the real cleanup collects it.
        // The legacy cursor is written in production's format, or
        // `watermark_seed_format` would flag this fixture.
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
                // The target has a capability (#1135) and an edit replaces, so
                // it presents the preimage.
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

    /// A database whose envelopes and watermarks predate the sequence, then
    /// migrated by the real 000028 SQL (a re-typed backfill would prove nothing).
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

    /// Every envelope must carry a delivery sequence: one without `seq` is
    /// never fetched (`NULL > n`) nor collected. A source-shape guard over
    /// production code only (test modules seed rows directly); it cannot see an
    /// INSERT built somewhere exotic.
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
    //! A reaction is membership-gated even when GC has collected its envelope
    //! (#1161): the declared conversation is gated, a declaration contradicting
    //! a surviving envelope is refused, and declaring nothing against a
    //! collected envelope is refused.

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

    /// With the envelope collected, a non-member's declared conversation is
    /// still gated.
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

    /// A surviving envelope stays authoritative; a body naming a different
    /// conversation is refused.
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

    /// Removal does not fail closed on an unresolvable conversation (see
    /// `apply_remove_reaction`).
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

    /// #1433: a member's reaction to a collected envelope is readable by
    /// members only.
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

    /// #1433: a reaction made while the envelope existed outlives it.
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
