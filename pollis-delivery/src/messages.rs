//! Domain A — message envelopes, edits/deletes, reactions, watermarks, and
//! attachment dedup rows. The **reference vertical slice** for Goal B (#419):
//! every later write-domain (B/C/D) copies the shape established here.
//!
//! ## The per-domain convention (copy this)
//!
//!   - **Bodies** — one `#[derive(Deserialize)] *Body` per endpoint. Binary
//!     fields are base64 (STANDARD); everything else is plain JSON. (Domain A
//!     has no binary fields — `ciphertext` is already the `"mls:<hex>"` text the
//!     client stores, and content hashes / R2 keys are text.)
//!   - **Pure conn-level fns** — `apply_*` functions take a bare
//!     [`Connection`], the authenticated user (`Option<&str>`; `None` only on
//!     the no-auth path), and a parsed `*Body`. They embed BOTH the
//!     authorization decision AND the write, returning [`WriteOutcome`]. Putting
//!     authz inside the pure fn (rather than the axum handler, as `writes.rs`
//!     does) is deliberate: it makes the in-process test harness exercise the
//!     *exact* same authz the production handler runs, with zero duplication —
//!     both call sites are reduced to `gate → parse → apply → map outcome`.
//!   - **axum handlers** — `(State, Method, Uri, HeaderMap, Bytes) -> Response`,
//!     all identical in shape. They `gate` (shared auth, see
//!     [`crate::writes::gate`]), parse, call the matching `apply_*`, and map the
//!     outcome to 200 / 403 / 400 / 500.
//!
//! ## Where the writes land
//!
//! Every domain-A table lives in the **MAIN DB** (`state.db`), NOT the commit-log
//! DB — these are message-delivery rows, not MLS control-plane rows. So all
//! `apply_*` fns run on the main connection.
//!
//! ## Authorization (the security core)
//!
//! `gate` proves *which user* signed the request; each `apply_*` then proves the
//! user is *allowed* to make that specific write:
//!   - send / edit / delete: the user is a current member of the conversation
//!     (reusing [`crate::writes::is_member`]). Authorship is NOT checked here
//!     (Solution A, #607): under unconditional sealed sender the stored
//!     `sender_id` is a blinded sentinel, so the DS cannot prove who authored a
//!     message. Edit/self-delete are membership-gated only; the author check that
//!     used to live here is enforced CLIENT-side on ingest (the edit/redaction's
//!     MLS-authenticated credential must equal the target's author). Admin-delete
//!     still additionally requires the user be a group admin of the channel's
//!     owning group (a permission check, not an author check).
//!   - reactions: the user is a member, and may only write/remove their OWN
//!     reaction (`user_id` is bound to the authenticated user).
//!   - watermark: the row is per `(conversation, user, device)`; the user may
//!     only advance their own.
//!   - envelope GC: the deletion *decision* is many-member correct regardless of
//!     who triggers it — bounded by the MIN watermark over the whole current
//!     member-device roster, and by device liveness (#720: a device silent past
//!     N months stops pinning), but never by wall-clock message age (invariant
//!     I3). The *trigger* is the DS-internal sweep [`sweep_envelope_gc`] (#689),
//!     not a member's ingest path; the per-conversation [`apply_envelope_gc`]
//!     endpoint runs the same watermark-gated cleanup for one conversation on
//!     demand.
//!   - attachment references (#690): releasing a reference is NOT a gated
//!     endpoint at all — the reference count is DERIVED from envelope existence
//!     (`attachment_ref ⋈ message_envelope`, see [`live_ref_exists`]), so the
//!     only way to release one is to delete its message envelope through the
//!     already-authorized delete/GC/teardown paths above. `/v1/attachments/
//!     register` and `/v1/attachments/delete` need only prove a real device:
//!     the former adds a declaration that counts only against a live envelope, the
//!     latter merely collects an already-unreferenced object.
//!
//! On the no-auth path (`authed == None`, only reachable when the DS runs with
//! `POLLIS_DS_REQUIRE_AUTH` off) the membership/identity checks are skipped and
//! the actor comes from the body — mirroring `commit::submit` and `writes.rs`.

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, Method, Uri},
    response::Response,
};
use libsql::Connection;
use ulid::Ulid;

use crate::error::AppError;
use crate::writes::{
    bad_request, gate, is_member, outcome_response, resolve_actor, WriteOutcome,
};
use crate::AppState;

// The request bodies for this module's endpoints live in `pollis-api`, the
// crate pollis-core builds its requests from — one declaration, both ends, so
// a client field that does not exist here is a compile error rather than a
// silently-absent JSON key. Re-exported so `pollis_delivery::messages::*Body`
// keeps resolving for handlers, tests and the flows harness.
pub use pollis_api::messages::*;

// ── Envelope GC SQL ──────────────────────────────────────────────────────────
//
// Envelope cleanup is gated on the DELIVERY WATERMARK: a row is deleted only when
// its `sent_at` sits strictly below the MINIMUM `last_fetched_at` over every
// current member device of the conversation that is still LIVE. Retention is
// bounded by the slowest LIVE member device — never by wall-clock message age.
// This is invariant I3 ("no TTL") in `docs/backend-core-invariants.md`.
//
// There used to be a second, OR'd arm: `sent_at < datetime('now', '-30 days')`.
// Because it was OR'd it deleted ON ITS OWN, without consulting a single
// watermark — so encrypted mail that no recipient device had ever collected was
// destroyed 30 days after it was sent, and a device offline for longer than that
// silently and permanently lost messages. That is failure mode F3, and it broke
// the product's headline guarantee (a member receives every message sent while
// they were a member; the only acceptable losses are pre-join history and a
// brand-new empty device). The arm is gone. Do not reintroduce a time-based — or
// any other delivery-blind — deletion gate here: age is not evidence of delivery.
//
// ## The device-liveness bound (#720, WS1)
//
// Without more, a device that installs, joins a busy group and never opens the
// app again pins that conversation's envelopes forever: its `last_fetched_at`
// never advances, so it holds `MIN(cw)` down indefinitely. #720 bounds this by
// LIVENESS — a member device that has not REPORTED a watermark in N months stops
// counting toward the gate (the `?2` staleness arm below; N is the `?2` modifier,
// set to 12 months in both deployed environments — see
// `DEFAULT_WATERMARK_STALE_MODIFIER`). It mirrors the revoked-device exclusion (#685): a
// stale device is filtered out of the roster the gate measures against, exactly
// as a revoked one is.
//
// The bound is on the device's own LAST REPORT (`cw.reported_at`, a wall-clock
// timestamp the DS stamps on every watermark write), NEVER on `last_fetched_at`
// (a message-cursor value) or the envelope's `sent_at`. That distinction is the
// whole point: a device that reported yesterday pins EVERY envelope below its
// cursor no matter how old the envelope is (anti-F3). Reusing `last_fetched_at`
// or `sent_at` for staleness would resurrect F3 — a live device in a quiet
// conversation legitimately carries an old cursor without being dormant.
//
// This makes envelope retention bounded by member watermarks AND by device
// liveness — a strictly SMALLER bound than the commit log's, whose stale-device
// case is instead handled by its Tier-2 hard cap (`commit::prune_floor`). The
// consequence is a THIRD accepted loss (beyond pre-join history and a new empty
// device): a device dormant past N months that returns may find gaps. This is a
// deliberate storage/correctness trade, gated behind a conservative N; see
// invariant I3 and `docs/metadata-retention-policy.md` §1.
//
// It fails CLOSED on `reported_at IS NULL`: a device with NO watermark row (a
// LEFT JOIN miss — brand-new, never synced) and a pre-migration row whose
// `reported_at` was never stamped both read NULL and are KEPT in the roster (they
// pin), because unknown report time is not evidence of dormancy. Only a device
// with a KNOWN report time older than the window is excluded.
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
// The member-device roster is filtered by `ud.revoked_at IS NULL` (#685): a
// revoked device can never rejoin the MLS tree (I5), so it must not count toward
// the roster the watermark gate is measured against — otherwise it holds the
// floor down forever. Without the filter a revoked device wedges cleanup in
// either direction: its stale `last_fetched_at` pins `MIN(cw)` to a dead cursor,
// or its missing row breaks the "every device reported" check and disables
// pruning outright. This mirrors `commit::current_member_devices`, which excludes
// revoked devices from the commit-log retention floor for the same reason — keep
// the two rosters in agreement (I5).
//
// ## Why the roster lives in a macro (#722)
//
// The roster — "which member devices does the watermark gate measure against" —
// is the SAME rule in three places: these two DELETEs and
// [`crate::commit::current_member_devices`]. Nothing structural forces them to
// agree, and a divergence is an I5 violation surfacing as dropped mail (a roster
// too small deletes what a device still needs) or wedged retention (a roster too
// large never clears). So the join chain each DELETE aggregates over is factored
// out into a macro expanding to a string literal, and `concat!`-ed back into the
// statement — the SQL the DS executes is byte-for-byte what it always was, but
// the *roster half* of it now has exactly one definition that the parity test in
// `roster_parity_tests` can SELECT from directly. Editing the roster inside the
// DELETE now necessarily edits what the test reads, so any drift away from the
// Rust roster shows up as a red test rather than as silent divergence.
//
// The fragments deliberately include the `LEFT JOIN conversation_watermark`: it
// is what `COUNT(ud.device_id)` is counted over, so `SELECT ud.device_id` +
// fragment is *precisely* the row set the gate aggregates — including any
// accidental fan-out — not a paraphrase of it.

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

// ## The `?2` device-liveness (staleness) arm, and why it lives OUTSIDE the macro
//
// The staleness filter (`cw.reported_at IS NULL OR cw.reported_at >=
// datetime('now', ?2)`) is appended to each DELETE *around* the shared roster
// fragment, NOT inside it. This is deliberate and load-bearing for the I5 roster
// parity (see `roster_parity_tests`): the roster macro is the ONE definition of
// "which member devices exist" shared with `commit::current_member_devices`, and
// the commit log applies NO liveness bound (it uses its own Tier-2 hard cap). If
// staleness went inside the macro, the envelope roster and the commit roster
// would diverge and the parity test would (correctly) fail. Keeping it outside
// leaves the shared roster byte-for-byte identical — the parity SELECTs read the
// pure roster — while the envelope DELETE applies liveness as an ADDITIONAL,
// envelope-only filter on top of that roster. The channel fragment has no `WHERE`
// so the arm opens one; the DM fragment already ends in `WHERE dcm.dm_channel_id
// = ?1` so the arm is `AND`-ed onto it.
//
// `?2` is a SQLite datetime modifier (e.g. `'-6 months'`), computed in Rust from
// config (`watermark_stale_modifier`). Both the sweep and the per-conversation
// endpoint bind the same value through the one chokepoint
// `cleanup_conversation_envelopes`, so the two GC paths can never apply different
// predicates (#720 checkbox 3).

const CLEANUP_CHANNEL_ENVELOPES: &str = concat!(
    "\
DELETE FROM message_envelope
 WHERE conversation_id = ?1
   AND sent_at < (
       SELECT CASE
                WHEN COUNT(ud.device_id) = COUNT(cw.last_fetched_at)
                THEN MIN(cw.last_fetched_at)
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
   AND sent_at < (
       SELECT CASE
                WHEN COUNT(ud.device_id) = COUNT(cw.last_fetched_at)
                THEN MIN(cw.last_fetched_at)
                ELSE NULL
              END
       ",
    dm_member_device_rows!(),
    "
         AND (cw.reported_at IS NULL OR cw.reported_at >= datetime('now', ?2))
     )"
);

// ── Device-liveness (staleness) window (#720) ────────────────────────────────

/// The **fallback** staleness window, as a SQLite `datetime()` modifier: a member
/// device that has not reported a watermark in this long stops pinning envelope
/// retention.
///
/// This is only what applies when `POLLIS_DS_WATERMARK_STALE_MONTHS` is unset —
/// e.g. a local run or a test. **Both deployed environments set it explicitly to
/// 12**, decided 2026-08-04 (`wrangler.{dev,prod}.jsonc`; rationale in
/// `docs/metadata-retention-policy.md` §1). N governs the third
/// accepted-message-loss, so the deployed value is a product decision that is
/// disclosed to users, not a default anyone should rely on silently.
pub const DEFAULT_WATERMARK_STALE_MODIFIER: &str = "-6 months";

/// The configured device-staleness window as a SQLite `datetime()` modifier,
/// bound as `?2` into the `CLEANUP_*` predicates.
///
/// `POLLIS_DS_WATERMARK_STALE_MONTHS` = N months (default 6). `0` DISABLES the
/// bound — it returns a window so far in the past (`-1000 years`) that no device
/// is ever stale, restoring the pre-#720 "watermark-only" behaviour without a
/// second SQL path.
pub fn watermark_stale_modifier() -> String {
    match std::env::var("POLLIS_DS_WATERMARK_STALE_MONTHS")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
    {
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
/// Lives here, next to [`watermark_stale_modifier`], for the same reason: it is
/// read by both the loop that uses it and `GET /v1/config`, which reports it, and
/// those two must never be able to disagree. Reporting the raw environment string
/// instead let the endpoint answer `"1h"` while the sweep ran at 3600 — a
/// configured-not-effective answer, which is precisely the failure /v1/config
/// exists to make impossible (#760).
///
/// An unparseable value falls back rather than failing, and the fallback is what
/// gets reported, so a typo is visible as "the default is running" rather than
/// silently believed.
pub fn gc_sweep_secs() -> u64 {
    std::env::var("POLLIS_DS_GC_SWEEP_SECS")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_GC_SWEEP_SECS)
}

// ── Retention growth metrics (#720 checkbox 1) ───────────────────────────────

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

// ── Shared authz helpers ─────────────────────────────────────────────────────

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
/// These columns are compared **lexically** — the message watermark advances on
/// `sent_at > cursor` — so a DS timestamp has to sort correctly against the
/// client-issued ones it shares a column with. Clients write
/// `chrono::Utc::now().to_rfc3339()`, which carries sub-second digits, so the DS
/// must emit them too: an earlier hand-rolled whole-second formatter produced
/// `…T09:00:00+00:00`, which sorts BELOW `…T09:00:00.123456789+00:00` because
/// `'+'` (0x2B) < `'.'` (0x2E). An admin delete tombstone written in the same
/// wall-clock second as the message it redacts therefore landed under every
/// recipient's watermark and was never fetched — the delete silently did
/// nothing. Always emitting nanoseconds restores the ordering (a shorter
/// fraction is a prefix of a longer one, so lexical order matches chronological
/// order across both precisions).
pub(crate) fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Nanos, false)
}

/// **The** value a server-side seed writes into
/// `conversation_watermark.last_fetched_at` (#908). Every seed path — device
/// registration ([`crate::bootstrap`]), DM creation and DM member-add
/// ([`crate::profile`]), group join ([`crate::groups`]) — goes through here.
///
/// This is the DS's counterpart to `pollis_core::commands::messages::
/// envelope_sent_at`, and it exists for the same reason: the format is the
/// correctness property, so it belongs to one function rather than to five call
/// sites that each look like an innocent "now".
///
/// **The bug it replaces.** Those five sites wrote SQLite's `datetime('now')`,
/// which formats as `YYYY-MM-DD HH:MM:SS`. `last_fetched_at` is compared
/// **lexically** against `message_envelope.sent_at`, which is RFC 3339 from every
/// other writer — and a space (0x20) sorts below a `T` (0x54), so a seeded
/// watermark compared as **older than every real timestamp that has ever
/// existed**, in any year. The seed's stated purpose is "this device has already
/// consumed everything up to now, so pre-join messages do not pin envelope
/// retention"; what it actually said was "this device has consumed nothing".
///
/// It was fail-safe, which is why nothing broke: the GC gate takes
/// `MIN(last_fetched_at)` over member devices, so an artificially-low seed
/// over-pins and under-collects. Envelopes were retained longer than intended,
/// never dropped. But a seed that means the opposite of its comment is one
/// refactor away from being load-bearing, and the same string is the
/// [`TOMBSTONE_FLOOR`] input, where "greatest cursor any recipient could hold"
/// silently stopped counting seeded rows.
///
/// **Not to be used for `reported_at`**, which is the deliberate exception: that
/// column is compared against `datetime('now', ?)` (see
/// [`CLEANUP_CHANNEL_ENVELOPES`]),
/// so it must stay in SQLite's own format. Two columns of one table in two
/// formats looks like an oversight and is not — each matches what it is compared
/// against, which is the only thing that decides a text timestamp's format.
pub(crate) fn seeded_watermark_cursor() -> String {
    now_rfc3339()
}

// ── The tombstone `sent_at` floor ────────────────────────────────────────────
//
// A DS-stamped tombstone is only ever fetched if it sorts strictly ABOVE the
// recipient's `conversation_watermark.last_fetched_at`: ingest selects
// `sent_at > last_fetched_at` (strictly greater) and advances the cursor to the
// highest `sent_at` it consumed, so the cursor NEVER rewinds and anything
// stamped at or below it is skipped permanently, not merely delayed
// (`pollis_core::commands::messages::ingest`).
//
// The floor is therefore the highest cursor value any recipient could already
// hold, and it has TWO sources — one of which outlives the other:
//
//   * `MAX(sent_at)` over `message_envelope` — the conversation's high-water
//     mark, but only over envelopes STILL PRESENT. Envelope GC deletes rows once
//     every current member device has reported a watermark past them (see
//     `CLEANUP_*` above; since #688 that deletion is purely watermark-gated, with
//     no TTL arm to bound it). Once a conversation has been fully pruned this
//     side is NULL.
//   * `MAX(last_fetched_at)` over `conversation_watermark` — the highest cursor
//     actually reported for the conversation. These rows are NOT touched by
//     envelope GC; they are only ever advanced (monotone UPSERT in
//     `apply_advance_watermark`) or deleted with the device itself
//     (`apply_revoke_device`). They therefore SURVIVE the pruning that empties
//     the envelope side.
//
// #692: using only the envelope side meant that once GC had pruned a
// conversation the floor went NULL, `sent_at_after` fell back to wall-clock
// `now`, and the clock-skew dependency this guard exists to remove was back — a
// recipient whose watermark came from a client clock running AHEAD of the DS
// clock would never fetch the tombstone, and the deleted message stayed readable
// on that device forever. The watermark side is precisely the evidence that
// survives GC, so the floor is the greater of the two.
//
// Expressed as one SQL statement rather than two queries max'd in Rust: each arm
// is an aggregate over a possibly-empty set, so each yields exactly one
// (possibly NULL) row, and the outer `MAX` skips NULLs — giving "greater of the
// two, or NULL when both are absent" with no Rust-side NULL bookkeeping and a
// single round trip. The comparison is lexical in both SQL and Rust (see
// [`now_rfc3339`] on why lexical order matches chronological order here), so the
// two formulations agree.
const TOMBSTONE_FLOOR: &str = "\
SELECT MAX(v) FROM (
    SELECT MAX(sent_at)         AS v FROM message_envelope       WHERE conversation_id = ?1
    UNION ALL
    SELECT MAX(last_fetched_at) AS v FROM conversation_watermark WHERE conversation_id = ?1
)";

/// The greatest cursor value any recipient of `conversation_id` could already
/// hold — see the [`TOMBSTONE_FLOOR`] block comment. `None` only when the
/// conversation has neither a surviving envelope nor a reported watermark.
async fn tombstone_floor(
    conn: &Connection,
    conversation_id: &str,
) -> anyhow::Result<Option<String>> {
    let mut rows = conn
        .query(TOMBSTONE_FLOOR, libsql::params![conversation_id.to_string()])
        .await?;
    Ok(match rows.next().await? {
        Some(row) => row.get::<Option<String>>(0)?,
        None => None,
    })
}

/// The `sent_at` an envelope must carry to be guaranteed visible to every
/// member of `conversation_id`, given `floor` — the greatest cursor value any
/// recipient could already hold (see [`tombstone_floor`]).
///
/// Precision parity (see [`now_rfc3339`]) fixes ordering between two correct
/// clocks, but `sent_at` for ordinary messages comes from the *client's* clock
/// while a tombstone's comes from the *DS's*. A client running ahead would put
/// its messages — and so every recipient's watermark — in the DS's future, and
/// the tombstone would again sort underneath and never be fetched. Stamping the
/// tombstone strictly after everything already in the conversation removes the
/// dependence on the two clocks agreeing: the cursor cannot already be past it.
///
/// Falls back to `now` when the floor is absent or unparseable, which is the
/// pre-existing behaviour and never worse than it.
fn sent_at_after(now: String, floor: Option<String>) -> String {
    let Some(floor) = floor else {
        return now;
    };
    if floor < now {
        return now;
    }
    match chrono::DateTime::parse_from_rfc3339(&floor) {
        Ok(t) => (t + chrono::Duration::nanoseconds(1))
            .to_utc()
            .to_rfc3339_opts(chrono::SecondsFormat::Nanos, false),
        Err(_) => now,
    }
}

// ── POST /v1/messages/send ───────────────────────────────────────────────────

pub async fn send_message(
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
    let parsed: SendMessageBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    let outcome = apply_send_message(&conn, authed.as_deref(), &parsed).await?;

    // Wake the recipients, AFTER the envelope has landed and only if it did
    // (#987). Best-effort and detached: a push that fails must never fail a send
    // that already succeeded, and the sender must not wait on Expo.
    //
    // The sender is taken from the AUTHENTICATED user, never from the body:
    // under sealed sender (#331) `body.sender_id` is a blinded sentinel, so
    // using it would exclude nobody and push the sender their own message.
    if matches!(outcome, WriteOutcome::Ok) {
        if let Some(push_to) = parsed.push_to.clone() {
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
                let only = if push_to.is_empty() {
                    None
                } else {
                    Some(push_to.as_slice())
                };
                if let Err(e) =
                    crate::push::notify_new_message(&conn, &conversation_id, &sender, only).await
                {
                    tracing::warn!("push fan-out for {conversation_id}: {e}");
                }
            });
        }
    }

    outcome_response::<SendMessageBody>(outcome)
}

/// INSERT a `type='message'` envelope (the send). Authz: the authenticated user
/// is a current member of the conversation.
///
/// Sender binding depends on sealing (issue #331,
/// `docs/metadata-minimization-design.md` §2):
///   - **Unsealed** (`sealed = 0`): unchanged — a signed request may only send as
///     itself, so the stored `sender_id` is bound to the authenticated user via
///     [`resolve_actor`].
///   - **Sealed** (`sealed = 1`): the body's `sender_id` is a non-identifying
///     sentinel, deliberately NOT the authenticated user, so the
///     "send-as-yourself" equality check is relaxed and the sentinel is persisted
///     as-is. Membership authz is UNCHANGED — we still verify the *authenticated*
///     user is a member; sealing only blinds the stored sender column, it does
///     not weaken who is allowed to write.
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
        // Unsealed: bind the stored sender to the actor and require a signed
        // request to send only as itself.
        match resolve_actor(authed, body.sender_id.as_deref()) {
            Ok(s) => (s.clone(), s),
            Err(o) => return Ok(o),
        }
    };
    if authed.is_some() && !is_member(conn, &body.conversation_id, &member_check_user).await? {
        return Ok(WriteOutcome::Forbidden);
    }
    conn.execute(
        "INSERT INTO message_envelope \
             (id, conversation_id, sender_id, ciphertext, reply_to_id, sent_at, sealed) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        libsql::params![
            body.id.clone(),
            body.conversation_id.clone(),
            stored_sender,
            body.ciphertext.clone(),
            body.reply_to_id.clone(),
            body.sent_at.clone(),
            body.sealed,
        ],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/messages/edit ───────────────────────────────────────────────────

pub async fn edit_message(
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
    let parsed: EditMessageBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    outcome_response::<EditMessageBody>(apply_edit_message(&conn, authed.as_deref(), &parsed).await?)
}

/// Replace the single pending edit envelope (DELETE prior + INSERT new) in one
/// transaction. Authz: the editor is a current member of the conversation.
///
/// Authorship is NOT checked here (Solution A, #607): under unconditional sealed
/// sender the stored `sender_id` is always a blinded sentinel, so the DS cannot
/// verify the editor authored the target — and it deliberately does not try. The
/// edit's content only ever lands on a recipient whose ingest
/// (`pollis_core::commands::messages::ingest`) confirms the edit's
/// MLS-authenticated author (the credential inside the ciphertext) equals the
/// target message's author; a non-author's edit envelope is accepted for storage
/// here but dropped, cryptographically, on ingest. The DS's job is reduced to the
/// membership gate: only a member may write an edit envelope to the conversation.
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
    let tx = conn.transaction().await?;
    tx.execute(
        "DELETE FROM message_envelope \
         WHERE conversation_id = ?1 AND target_message_id = ?2 AND type = 'edit'",
        libsql::params![body.conversation_id.clone(), body.target_message_id.clone()],
    )
    .await?;
    tx.execute(
        "INSERT INTO message_envelope \
             (id, conversation_id, sender_id, ciphertext, sent_at, type, target_message_id) \
         VALUES (?1, ?2, ?3, ?4, ?5, 'edit', ?6)",
        libsql::params![
            body.envelope_id.clone(),
            body.conversation_id.clone(),
            sender,
            body.ciphertext.clone(),
            body.sent_at.clone(),
            body.target_message_id.clone(),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/messages/delete ─────────────────────────────────────────────────

pub async fn delete_message(
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
    let parsed: DeleteMessageBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    outcome_response::<DeleteMessageBody>(apply_delete_message(&conn, authed.as_deref(), &parsed).await?)
}

/// Delete a message. Two branches, chosen by the client's `msg_sender_id` hint
/// (Solution A, #607): under unconditional sealed sender the stored `sender_id`
/// is always a blinded sentinel, so the DS can no longer derive who authored the
/// message — it trusts the client to say whether this is a self-delete.
///
/// **Self-branch** (`msg_sender_id == actor`): gated on **membership** only.
/// Removes the original envelope (unscoped — a sealed row has no matchable
/// sender) and any pending edit; writes **no** tombstone. A non-author member can
/// thus remove a not-yet-fetched envelope (an accepted availability trade, #607),
/// but cannot forge a *delete appearance*: making other members drop an
/// already-fetched copy requires either a valid E2EE redaction (honored on ingest
/// only when its MLS-authenticated author matches the target's author) or an
/// admin tombstone (below) — neither of which a non-author can produce.
///
/// **Admin-branch** (`msg_sender_id != actor`): the actor must be a group admin
/// of the channel (a re-derived permission check, not an author check). Removes
/// the envelope + pending edit and writes a `type='delete'` tombstone so every
/// member soft-deletes on next ingest. Server-authorized moderation.
pub async fn apply_delete_message(
    conn: &Connection,
    authed: Option<&str>,
    body: &DeleteMessageBody,
) -> anyhow::Result<WriteOutcome> {
    let actor = match resolve_actor(authed, body.actor_id.as_deref()) {
        Ok(a) => a,
        Err(o) => return Ok(o),
    };

    // Branch selection comes from the client's hint — the DS cannot re-derive
    // authorship from the (sealed) envelope. Self-branch when the hint names the
    // actor as the author; admin-branch otherwise.
    let is_self_delete = body.msg_sender_id.as_deref() == Some(actor.as_str());

    if is_self_delete {
        // Self-branch authz is membership: any member may remove an envelope
        // they claim to have authored (authorship itself is enforced
        // client-side on ingest, never here). Skipped on the no-auth path.
        if authed.is_some() && !is_member(conn, &body.conversation_id, &actor).await? {
            return Ok(WriteOutcome::Forbidden);
        }
        // Both deletes are SCOPED to the conversation that was just authorised.
        // Without `AND conversation_id`, authz reads one attacker-supplied field
        // (`conversation_id`) while the delete acts on a second, independent one
        // (`message_id`) — so membership of any conversation, including a DM with
        // yourself, authorised deleting any envelope in the deployment. Clients
        // hold a whole-DB read-only Turso token, so the ids are enumerable.
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
    // Read the conversation's floor BEFORE the deletes below remove the target —
    // a recipient's watermark can be anywhere up to this value, and the tombstone
    // is only ever fetched if it sorts above it.
    let floor = tombstone_floor(conn, &body.conversation_id).await?;
    let now = sent_at_after(now_rfc3339(), floor);
    // Scoped for the same reason as the self-branch above: the admin check is
    // against `conversation_id`, so the delete must be too, or an admin of any
    // one group could remove envelopes from every other conversation.
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
    tx.execute(
        "INSERT INTO message_envelope \
             (id, conversation_id, sender_id, ciphertext, sent_at, type, target_message_id) \
         VALUES (?1, ?2, ?3, '', ?4, 'delete', ?5)",
        libsql::params![
            tombstone_id,
            body.conversation_id.clone(),
            actor,
            now,
            body.message_id.clone(),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/reactions/add  &  /v1/reactions/remove ──────────────────────────

pub async fn add_reaction(
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
    let parsed: AddReaction = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    outcome_response::<AddReaction>(apply_add_reaction(&conn, authed.as_deref(), &parsed.0).await?)
}

pub async fn remove_reaction(
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
    let parsed: RemoveReaction = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    outcome_response::<RemoveReaction>(apply_remove_reaction(&conn, authed.as_deref(), &parsed.0).await?)
}

/// A reaction is membership-gated through the reacted-to message's envelope.
/// When the envelope has aged out we cannot resolve the conversation, so we
/// allow the write (it is already scoped to the actor's own `user_id`) —
/// reacting to a still-locally-visible but GC'd message must keep working.
pub async fn apply_add_reaction(
    conn: &Connection,
    authed: Option<&str>,
    body: &ReactionBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    if authed.is_some() {
        if let Some(conv) = conversation_for_message(conn, &body.message_id).await? {
            if !is_member(conn, &conv, &user).await? {
                return Ok(WriteOutcome::Forbidden);
            }
        }
    }
    let id = Ulid::new().to_string();
    let now = now_rfc3339();
    conn.execute(
        "INSERT OR IGNORE INTO message_reaction (id, message_id, user_id, emoji, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        libsql::params![id, body.message_id.clone(), user, body.emoji.clone(), now],
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
    // A user may only ever remove their OWN reaction — the DELETE is scoped to
    // `user_id = :user`, so even without a membership lookup it cannot touch
    // anyone else's row. We still membership-gate when determinable.
    if authed.is_some() {
        if let Some(conv) = conversation_for_message(conn, &body.message_id).await? {
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

// ── POST /v1/watermarks/advance ──────────────────────────────────────────────

pub async fn advance_watermark(
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
    let parsed: WatermarkBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    outcome_response::<WatermarkBody>(apply_advance_watermark(&conn, authed.as_deref(), &parsed).await?)
}

/// Monotone UPSERT of a `(conversation, user, device)` watermark. Authz: the row
/// belongs to the actor (`user_id` bound to the authenticated user).
///
/// `reported_at` is server-stamped to `datetime('now')` on BOTH insert and
/// update — this is the wall-clock "device liveness" signal the envelope-GC
/// staleness bound reads (#720). It is stamped UNCONDITIONALLY, even when the
/// monotone `last_fetched_at` guard keeps the cursor where it is: a device that
/// re-reports the same cursor is still LIVE, and a live device must keep pinning.
/// This is also what makes the bound reversible — a dormant device that comes
/// back and reports refreshes `reported_at` to now and immediately counts again.
/// `reported_at` is a distinct column from `last_fetched_at` on purpose: the
/// cursor is a message timestamp (it may legitimately be old for a live device in
/// a quiet conversation), while liveness is when the DS last heard from the
/// device — conflating them would resurrect F3.
pub async fn apply_advance_watermark(
    conn: &Connection,
    authed: Option<&str>,
    body: &WatermarkBody,
) -> anyhow::Result<WriteOutcome> {
    let user = match resolve_actor(authed, body.user_id.as_deref()) {
        Ok(u) => u,
        Err(o) => return Ok(o),
    };
    conn.execute(
        "INSERT INTO conversation_watermark \
             (conversation_id, user_id, device_id, last_fetched_at, reported_at) \
         VALUES (?1, ?2, ?3, ?4, datetime('now')) \
         ON CONFLICT(conversation_id, user_id, device_id) DO UPDATE SET \
             last_fetched_at = MAX(last_fetched_at, excluded.last_fetched_at), \
             reported_at = datetime('now')",
        libsql::params![
            body.conversation_id.clone(),
            user,
            body.device_id.clone(),
            body.last_fetched_at.clone(),
        ],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/envelopes/gc ────────────────────────────────────────────────────

pub async fn envelope_gc(
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
    let parsed: EnvelopeGcBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    let stale = watermark_stale_modifier();
    outcome_response::<EnvelopeGcBody>(apply_envelope_gc(&conn, authed.as_deref(), &parsed, &stale).await?)
}

/// Run the watermark-gated envelope GC for a SINGLE conversation on demand.
/// Authz: the actor is a current member (skipped on the no-auth path).
///
/// The deletion predicate is bounded by the SLOWEST current *live* member device
/// and by nothing else — see the [`CLEANUP_CHANNEL_ENVELOPES`] block comment. The
/// 30-day TTL that used to be OR'd into it is gone (invariant I3, failure mode
/// F3): an envelope no live member device has collected is retained however old
/// it is. `stale` is the device-liveness window (`?2`, #720) — the SAME value the
/// sweep passes, threaded here so the two paths cannot apply different predicates.
///
/// Because the predicate consults the WHOLE current member-device roster, the
/// *decision* is the same no matter who fires it — so a single eager device can
/// never delete another device's mail, whatever the trigger.
///
/// This is the per-conversation form. The GC *trigger* is now the DS-internal
/// sweep [`sweep_envelope_gc`] (#689): GC no longer rides a member's ingest path,
/// so a conversation whose members all went quiet is still collected. Both share
/// the one cleanup chokepoint [`cleanup_conversation_envelopes`].
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

// ── Aged-out records (#762) ──────────────────────────────────────────────────
//
// Three tables inherited "indefinite" retention — not because anyone chose it,
// but because nothing ever deleted from them. #720 set the shape for fixing that:
// pick N, write down why, disclose it, and configure it explicitly rather than
// leaning on a code default.
//
// These run on the SAME sweep as envelope GC rather than as a second task, so
// there is one schedule to reason about and one place that can fail.

/// Days a `security_event` row is retained. Default 90.
///
/// It is a per-user, timestamped, device-attributed audit trail — precisely the
/// record a privacy-first product should age out. 90 days is long enough to
/// investigate an incident someone noticed late, short enough that it is not a
/// standing longitudinal profile of when each user changed devices.
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
/// A token for an uninstalled app otherwise persists until the account is
/// deleted. Clients re-register on every launch, so `updated_at` tracks liveness
/// directly and a live device refreshes long before this. Six months is
/// deliberately generous: a reaped token means a missed notification until the
/// next launch re-registers it, which is a nuisance, not data loss.
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
/// Both are keyed on the row's own timestamp, which is safe here in a way it is
/// NOT for envelopes: an envelope's age says nothing about whether its recipient
/// collected it (that is why #688 removed the envelope TTL), whereas an audit
/// record's age *is* the thing being bounded, and a push token's `updated_at` is
/// its own liveness signal.
///
/// **`conversation_watermark` is deliberately NOT reaped here** — see the module
/// docs and `docs/metadata-retention-policy.md`.
///
/// The windows are PARAMETERS, not globals read in here: `main` resolves them from
/// the environment once at startup. Reading env inside the sweep would force every
/// test to mutate process-wide state, which races the other tests in the same
/// binary — the bounds are configuration, and configuration belongs at the edge.
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
/// This replaces the old trigger — "whichever member device happens to ingest
/// calls `/v1/envelopes/gc`" — which never fired for a conversation whose members
/// all went quiet and fired far more often than needed for a chatty one. Nothing
/// about *which rows qualify* changes here beyond the #720 liveness bound;
/// retention stays bounded by the slowest LIVE member device (invariant I3), only
/// *what drives* GC moves off the member ingest path.
///
/// A conversation is a DM iff its id appears in `dm_channel_member` (mirroring
/// [`is_member`]); every other id is a group/channel. Running the wrong predicate
/// is a no-op — its roster join yields zero rows, `COUNT(ud) = COUNT(cw) = 0`, the
/// CASE returns NULL and `sent_at < NULL` deletes nothing — so a misclassified id
/// can never over-delete.
pub async fn sweep_envelope_gc(conn: &Connection, stale: &str) -> anyhow::Result<SweepReport> {
    // Only conversations that still hold envelopes are worth visiting; steady
    // state this set is small.
    let mut conversations: Vec<String> = Vec::new();
    {
        let mut rows = conn
            .query("SELECT DISTINCT conversation_id FROM message_envelope", ())
            .await?;
        while let Some(row) = rows.next().await? {
            conversations.push(row.get::<String>(0)?);
        }
    }
    // Growth metrics accumulated on the SAME per-conversation walk that runs
    // cleanup — no second full scan (#720 checkbox 1). Post-cleanup counts, so
    // they reflect what actually survives GC. Deliberately identity-free: the
    // worst offender is tracked by its shape (count + oldest), never its id.
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
    // Reap dead attachment reference declarations (#690, Blocking 2). Envelopes
    // deleted just above (and by every other deleter) already stopped counting
    // toward `object_is_referenced` — the count is derived — so this changes NO
    // deletion decision; it only clears the now-orphaned `attachment_ref` rows so
    // the table stays bounded, and reclaims forged declarations whose `message_id`
    // never named a real envelope. Runs unconditionally: even a fully-pruned DB
    // (the loop visited nothing) may hold such orphans.
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

// ── POST /v1/attachments/register  &  /v1/attachments/delete ─────────────────

// ── The reference count is DERIVED, not maintained (#690) ─────────────────────
//
// A `(content_hash, message_id)` row in `attachment_ref` is a client DECLARATION
// — "message M carries the file with this hash" — but it only COUNTS while the
// message it names still exists, i.e. while a `message_envelope` row carries that
// id. The live count is therefore `attachment_ref` JOINed to `message_envelope`,
// never the raw `attachment_ref` rows.
//
// This is what makes the two blocking properties true by construction rather than
// by discipline:
//
//   * **A reference cannot outlive its message (Blocking 2).** EVERY path that
//     deletes an envelope — self/admin delete (`apply_delete_message`), the
//     watermark-gated GC sweep (#689, `cleanup_conversation_envelopes`), the
//     retention sweep (#720), account teardown (`account.rs`), group/profile
//     teardown (`groups.rs`/`profile.rs`), even a test harness — drops the join
//     partner and so releases the reference FOR FREE. Nothing has to remember to
//     release; release is the disappearance of the envelope, not a step any of
//     those N deleters performs. That is the #691 ELECTRON lesson applied: don't
//     ask three-plus deleters to each remember the same cleanup — make the
//     invariant derive so the next deleter cannot forget it.
//
//   * **Releasing a reference is not an unauthorized action (Blocking 1).**
//     Because the count is derived from envelope existence, the ONLY way to drop
//     it is to delete the envelope — which already goes through the
//     membership-gated (self) / admin-gated (moderation) `/v1/messages/delete`,
//     the server-internal sweeps, or account/group teardown. `/v1/attachments/
//     delete` no longer mutates `attachment_ref` at all (see
//     [`apply_delete_attachment`]), so a forged call naming another member's
//     `message_id` cannot strand anything: the referencing envelope still exists,
//     so the object stays referenced. The attack is removed structurally, not
//     access-checked.
//
// A declaration whose `message_id` names an envelope that never existed (a forged
// register) or has already gone simply never counts — it is inert, and reaped in
// bulk by [`sweep_envelope_gc`] so `attachment_ref` stays bounded.
//
// The second leg is the VAULT's (#107): a `vault_attachment_ref` row joined to a
// live `vault_message` keeps the object alive by exactly the same construction —
// the reference derives from the entry's existence, `/v1/vault/save` replaces the
// set on every save, and `/v1/vault/delete` drops it with the entry. Without this
// leg, a file that lives only in someone's vault counts as unreferenced the
// moment any client's cleanup pass looks at its hash, and a personal "cloud
// drive" that silently loses files is not one.
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
/// Rust-side count — the row cannot go while a referencing envelope survives even
/// under concurrent deletes (CLAUDE.md "invalid states unrepresentable").
const COLLECT_UNREFERENCED_OBJECT_SQL: &str = concat!(
    "DELETE FROM attachment_object WHERE content_hash = ?1 AND NOT ",
    live_ref_exists!()
);

/// Reap declaration rows whose message envelope is gone (GC'd, deleted, aged out,
/// or a forged id that never had an envelope). Pure storage hygiene: the count is
/// already correct without it (see [`live_ref_exists`]); this only keeps
/// `attachment_ref` from accumulating dead rows as envelopes churn, and turns a
/// forged/orphaned declaration from merely inert into actually reclaimed.
const REAP_ORPHANED_ATTACHMENT_REFS_SQL: &str = "\
DELETE FROM attachment_ref \
 WHERE NOT EXISTS (SELECT 1 FROM message_envelope me WHERE me.id = attachment_ref.message_id)";

/// The vault leg of the same hygiene: reap reference rows whose vault entry is
/// gone. `/v1/vault/delete` already removes them inline, so this only catches
/// rows orphaned by account teardown or a crash between the two deletes — the
/// liveness predicate is correct either way.
const REAP_ORPHANED_VAULT_REFS_SQL: &str = "\
DELETE FROM vault_attachment_ref \
 WHERE NOT EXISTS (SELECT 1 FROM vault_message vm WHERE vm.id = vault_attachment_ref.vault_message_id)";

pub async fn register_attachment(
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
    let parsed: AttachmentRegisterBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    outcome_response::<AttachmentRegisterBody>(apply_register_attachment(&conn, authed.as_deref(), &parsed).await?)
}

pub async fn delete_attachment(
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
    let parsed: AttachmentDeleteBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return Ok(bad_request("invalid body")),
    };
    let conn = state.db.conn().await?;
    outcome_response::<AttachmentDeleteBody>(apply_delete_attachment(&conn, authed.as_deref(), &parsed).await?)
}

/// Register a convergent-encryption dedup row (`content_hash → r2_key`) and,
/// when a `message_id` is supplied, a `(content_hash, message_id)` REFERENCE
/// declaration (#690). Authz: any authenticated user. There is no conversation
/// context at upload time — the row is content-addressed and identical for every
/// uploader — so there is nothing finer to gate on. The signature still proves a
/// real device, which is all the no-token model can assert here.
///
/// A forged register is inert, not merely fail-safe: the declaration only counts
/// while a `message_envelope` with that id exists (see [`live_ref_exists`]), and
/// a member does not control whether a given id names a live envelope. Forging a
/// reference for a `message_id` it does not own therefore adds a row that never
/// counts (reaped by [`sweep_envelope_gc`]); it cannot even over-retain, let
/// alone release someone else's reference.
///
/// The object INSERT stays `OR IGNORE` (the row is convergent — identical for
/// every uploader). The reference INSERT is likewise `OR IGNORE`: the PK
/// `(content_hash, message_id)` makes a retried send idempotent rather than
/// double-counting. Registering the reference here (rather than at upload) is
/// deliberate — a file is uploaded once but referenced by every message that
/// carries it, and the count must track messages, not uploads.
pub async fn apply_register_attachment(
    conn: &Connection,
    _authed: Option<&str>,
    body: &AttachmentRegisterBody,
) -> anyhow::Result<WriteOutcome> {
    let tx = conn.transaction().await?;
    tx.execute(
        "INSERT OR IGNORE INTO attachment_object (content_hash, r2_key) VALUES (?1, ?2)",
        libsql::params![body.content_hash().to_string(), body.r2_key().to_string()],
    )
    .await?;
    // The reference row exists only on the send path, which is now a distinct
    // VARIANT rather than a `Some` (#925) — so "registered an attachment for a
    // message but recorded no reference to it" is not a state a caller can
    // construct.
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

/// COLLECT the shared `attachment_object` row once no LIVE reference remains
/// (#690, resolving the second `TODO(#419)`). Authz: any authenticated user —
/// deliberately, because this endpoint no longer performs the destructive act.
///
/// It does NOT release a reference. Releasing is not an action of this endpoint
/// (Blocking 1): a reference is released only by deleting the message envelope
/// that declared it, and the count is DERIVED from envelope existence (see
/// [`live_ref_exists`]). The client still calls this after a message-delete
/// (having already deleted the envelope via `/v1/messages/delete`) to trigger the
/// collection, and `message_id` is retained in the body for wire-compat, but it
/// is intentionally UNUSED: a forged call naming another member's message cannot
/// strand anything, because that member's envelope still exists and so the object
/// stays referenced. The old code deleted `attachment_ref (content_hash,
/// message_id)` here, which is exactly the deliberate-strand path this revision
/// closes.
///
/// The collect is a SINGLE conditional `DELETE`: the object goes iff no
/// `attachment_ref ⋈ message_envelope` row remains for the hash. One predicate,
/// no Rust-side count, so the row cannot be removed while a referencing envelope
/// survives even under concurrent deletes (CLAUDE.md "invalid states
/// unrepresentable").
///
/// The R2 object is gated separately and by the SAME evidence: `/v1/r2/presign`
/// refuses to mint a `delete` while [`object_is_referenced`] holds (`broker.rs`).
/// Turso row and R2 blob are collected together, only once the last live
/// reference is gone.
///
/// Legacy / pre-#690 rows and messageless hashes have no live reference, so the
/// predicate treats them as collectable — today's behaviour, no worse. Any send
/// from an updated client re-references the hash (with a live envelope) and
/// promotes it to counted protection.
pub async fn apply_delete_attachment(
    conn: &Connection,
    _authed: Option<&str>,
    body: &AttachmentDeleteBody,
) -> anyhow::Result<WriteOutcome> {
    // Collect only. No `attachment_ref` mutation — release happens when the
    // envelope is deleted (authorized), not here.
    conn.execute(
        COLLECT_UNREFERENCED_OBJECT_SQL,
        libsql::params![body.content_hash.clone()],
    )
    .await?;
    Ok(WriteOutcome::Ok)
}

/// True when at least one STILL-EXISTING message references the convergent
/// attachment `content_hash` (#690) — the count derived by joining
/// `attachment_ref` to `message_envelope` (see [`live_ref_exists`]), never the
/// raw declaration rows. The single source of truth for BOTH the counted Turso
/// collect in [`apply_delete_attachment`] and the R2 `delete`-presign gate in
/// [`crate::broker::r2_presign`]: an object may be collected — in Turso or in R2
/// — only when this returns `false`. Collecting the R2 blob out from under a live
/// reference would 404 that attachment for every conversation still holding it.
/// A reference whose message has been GC'd/deleted/aged-out — or never existed —
/// does not keep this `true`, so the object cannot be pinned forever.
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
    use super::*;

    /// The exact shape a client writes for `sent_at` (`pollis-core` sends
    /// `chrono::Utc::now().to_rfc3339()`, i.e. `SecondsFormat::AutoSi`).
    fn client_stamp(nanos: u32) -> String {
        chrono::DateTime::from_timestamp(1_800_000_000, nanos)
            .expect("valid instant")
            .to_rfc3339()
    }

    /// The regression: a DS timestamp must sort ABOVE a client timestamp taken
    /// earlier in the same wall-clock second. The old whole-second formatter
    /// emitted `…:20+00:00`, and `'+'` (0x2B) < `'.'` (0x2E), so it sorted
    /// BELOW `…:20.000000001+00:00` — an admin tombstone written in the same
    /// second as the message it redacts fell under every recipient's watermark
    /// and was never fetched, so the delete silently did nothing.
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

    /// A tombstone must outrank everything already in the conversation even when
    /// the client's clock runs ahead of the DS's — otherwise the recipient's
    /// watermark is already past it and it is never fetched.
    #[test]
    fn a_tombstone_outranks_a_conversation_stamped_in_the_future() {
        let now = now_rfc3339();
        let ahead = chrono::Utc::now() + chrono::Duration::hours(1);
        let floor = ahead.to_rfc3339();

        let stamped = sent_at_after(now.clone(), Some(floor.clone()));
        assert!(
            stamped > floor,
            "tombstone {stamped} must sort above the conversation's high-water mark {floor}"
        );
    }

    /// When nothing in the conversation is ahead of the DS clock, the tombstone
    /// keeps the plain current time — the guard only ever pushes forward.
    #[test]
    fn the_floor_guard_is_a_no_op_when_the_conversation_is_behind() {
        let now = now_rfc3339();
        let behind = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        assert_eq!(sent_at_after(now.clone(), Some(behind)), now);
        assert_eq!(sent_at_after(now.clone(), None), now);
        assert_eq!(sent_at_after(now.clone(), Some("not-a-timestamp".into())), now);
    }
}

#[cfg(test)]
mod tombstone_floor_tests {
    //! [`TOMBSTONE_FLOOR`] driven against a real (in-memory) libsql DB.
    //!
    //! **#692 — the floor must outlive envelope GC.** A tombstone is only ever
    //! fetched when it sorts strictly above the recipient's watermark, because
    //! ingest selects `sent_at > last_fetched_at` and the cursor never rewinds.
    //! The old floor was `MAX(sent_at)` over envelopes STILL PRESENT, so once
    //! watermark-gated GC had pruned a conversation the floor went NULL, the
    //! stamp fell back to wall-clock `now`, and a recipient whose watermark came
    //! from a client clock running AHEAD of the DS clock never fetched the
    //! tombstone — the deleted message stayed readable on that device forever.
    //!
    //! `conversation_watermark` rows survive that pruning, so they are the
    //! evidence the floor must also consult. Each test below asserts the floor
    //! AND the stamp `sent_at_after` derives from it, since the stamp is what
    //! ingest actually compares.

    use super::*;


    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        // Production is Turso, where foreign-key enforcement is off; libsql's LOCAL
        // backend turns it ON by default, so say so explicitly rather than
        // inherit a constraint no deploy has (`Db::connect_local` does the same).
        conn.execute_batch("PRAGMA foreign_keys=OFF;").await.unwrap();
        pollis_schema::apply::single_db(&conn).await.expect("schema");
        conn
    }

    /// An RFC3339 stamp `offset` from now, in the shape a CLIENT writes it
    /// (`chrono::Utc::now().to_rfc3339()`), which is what actually lands in
    /// `sent_at` and — via ingest — in `last_fetched_at`.
    fn stamp(offset: chrono::Duration) -> String {
        (chrono::Utc::now() + offset).to_rfc3339()
    }

    async fn add_envelope(conn: &Connection, id: &str, conv: &str, sent_at: &str) {
        conn.execute(
            "INSERT INTO message_envelope (id, conversation_id, sent_at, sender_id, ciphertext) VALUES (?1, ?2, ?3, 'sender', 'ct')",
            libsql::params![id.to_string(), conv.to_string(), sent_at.to_string()],
        )
        .await
        .unwrap();
    }

    async fn add_watermark(conn: &Connection, conv: &str, user: &str, device: &str, at: &str) {
        conn.execute(
            "INSERT INTO conversation_watermark \
                 (conversation_id, user_id, device_id, last_fetched_at) \
             VALUES (?1, ?2, ?3, ?4)",
            libsql::params![
                conv.to_string(),
                user.to_string(),
                device.to_string(),
                at.to_string()
            ],
        )
        .await
        .unwrap();
    }

    /// **The #692 regression test.** Every envelope in the conversation has been
    /// GC'd (watermark-gated deletion: every current member device reported past
    /// them), but the watermark rows survive — and one of them was written from a
    /// client clock running an hour AHEAD of the DS clock, so it sits in the DS's
    /// future. The tombstone must still be stamped strictly above it.
    ///
    /// Against the old `MAX(sent_at)`-only floor this FAILS: with no envelopes
    /// left the floor is NULL, `sent_at_after` returns plain `now`, and `now` is
    /// an hour BELOW the surviving cursor — so ingest's `sent_at >
    /// last_fetched_at` never selects the tombstone and the delete silently does
    /// nothing on that device, permanently.
    #[tokio::test]
    async fn a_future_watermark_surviving_gc_still_floors_the_tombstone() {
        let conn = conn().await;
        // The recipient's clock ran ahead; its reported cursor is in the DS's future.
        let ahead = stamp(chrono::Duration::hours(1));
        add_watermark(&conn, "c1", "alice", "a1", &ahead).await;
        // GC has pruned every envelope — `message_envelope` is empty for c1.

        let floor = tombstone_floor(&conn, "c1").await.unwrap();
        assert_eq!(
            floor.as_deref(),
            Some(ahead.as_str()),
            "with every envelope GC'd, the surviving watermark must still supply \
             the floor — MAX(sent_at) alone yields NULL here (#692)"
        );

        let stamped = sent_at_after(now_rfc3339(), floor);
        assert!(
            stamped > ahead,
            "tombstone {stamped} must sort strictly above the surviving watermark \
             {ahead}, or ingest's `sent_at > last_fetched_at` never selects it and \
             the deleted message stays readable on that device forever (#692)"
        );
    }

    /// Envelopes present and every watermark BEHIND them: the floor is the
    /// envelope high-water mark exactly as before — this fix widens the floor,
    /// it never lowers it.
    #[tokio::test]
    async fn envelopes_present_with_watermarks_behind_keeps_todays_floor() {
        let conn = conn().await;
        let newest = stamp(chrono::Duration::minutes(-1));
        add_envelope(&conn, "e1", "c1", &stamp(chrono::Duration::hours(-2))).await;
        add_envelope(&conn, "e2", "c1", &newest).await;
        add_watermark(&conn, "c1", "alice", "a1", &stamp(chrono::Duration::hours(-3))).await;
        add_watermark(&conn, "c1", "bob", "b1", &stamp(chrono::Duration::minutes(-30))).await;

        let floor = tombstone_floor(&conn, "c1").await.unwrap();
        assert_eq!(
            floor.as_deref(),
            Some(newest.as_str()),
            "with every watermark behind the newest envelope the floor is \
             unchanged from the MAX(sent_at)-only behaviour"
        );

        // Both floors are behind the DS clock, so the guard is a no-op and the
        // tombstone keeps plain `now` — today's behaviour, preserved.
        let now = now_rfc3339();
        assert_eq!(sent_at_after(now.clone(), floor), now);
    }

    /// Neither envelopes nor watermarks: the floor is absent and `sent_at_after`
    /// falls back to `now` — the pre-existing behaviour, unchanged.
    #[tokio::test]
    async fn no_envelopes_and_no_watermarks_falls_back_to_now() {
        let conn = conn().await;
        // Rows exist, but for a DIFFERENT conversation — the floor is scoped.
        add_envelope(&conn, "e1", "other", &stamp(chrono::Duration::hours(1))).await;
        add_watermark(&conn, "other", "alice", "a1", &stamp(chrono::Duration::hours(1))).await;

        let floor = tombstone_floor(&conn, "c1").await.unwrap();
        assert_eq!(floor, None, "an empty conversation has no floor");

        let now = now_rfc3339();
        assert_eq!(
            sent_at_after(now.clone(), floor),
            now,
            "with no floor the tombstone keeps plain `now` (unchanged fallback)"
        );
    }

    /// Mixed: envelopes survive, but a watermark sits ABOVE the newest of them
    /// (a client clock ahead of everyone else's). The greater of the two wins, so
    /// the watermark supplies the floor and the tombstone clears it.
    #[tokio::test]
    async fn a_watermark_ahead_of_the_newest_envelope_wins() {
        let conn = conn().await;
        add_envelope(&conn, "e1", "c1", &stamp(chrono::Duration::minutes(-5))).await;
        let ahead = stamp(chrono::Duration::hours(2));
        add_watermark(&conn, "c1", "alice", "a1", &stamp(chrono::Duration::hours(-1))).await;
        add_watermark(&conn, "c1", "bob", "b1", &ahead).await;

        let floor = tombstone_floor(&conn, "c1").await.unwrap();
        assert_eq!(
            floor.as_deref(),
            Some(ahead.as_str()),
            "the floor is the GREATER of MAX(sent_at) and MAX(last_fetched_at)"
        );

        let stamped = sent_at_after(now_rfc3339(), floor);
        assert!(
            stamped > ahead,
            "tombstone {stamped} must clear the highest surviving cursor {ahead}"
        );
    }

    /// The symmetric case: an envelope ahead of every watermark still wins, so
    /// widening the floor cannot regress the case the guard originally covered.
    #[tokio::test]
    async fn an_envelope_ahead_of_every_watermark_wins() {
        let conn = conn().await;
        let ahead = stamp(chrono::Duration::hours(2));
        add_envelope(&conn, "e1", "c1", &ahead).await;
        add_watermark(&conn, "c1", "alice", "a1", &stamp(chrono::Duration::hours(-1))).await;

        let floor = tombstone_floor(&conn, "c1").await.unwrap();
        assert_eq!(floor.as_deref(), Some(ahead.as_str()));

        let stamped = sent_at_after(now_rfc3339(), floor);
        assert!(stamped > ahead, "tombstone {stamped} must clear {ahead}");
    }
}

#[cfg(test)]
mod gc_sql_tests {
    //! The envelope-GC cleanup SQL, driven directly against a real (in-memory)
    //! libsql DB. Two invariants are encoded here.
    //!
    //! **I3 — retention is bounded by the slowest member device, never a TTL.**
    //! The `no_ttl_*` tests below construct exactly the state the deleted 30-day
    //! TTL destroyed (failure mode F3): an envelope FAR older than 30 days that a
    //! current member device has not collected. It must survive. Ages are written
    //! as explicit `datetime('now', '-N days')` offsets, so "well over 30 days" is
    //! a property of the fixture rather than of when the suite happens to run.
    //!
    //! **#685 — a REVOKED device must not wedge deletion.** A revoked device can
    //! never rejoin the tree, so it must not count toward the roster the watermark
    //! gate is measured against. Both wedge modes are covered for each
    //! conversation shape:
    //!   * **stale watermark row present** — the revoked device's old
    //!     `last_fetched_at` pins `MIN(cw)` down, so an envelope above the LIVE
    //!     device's cursor is (wrongly) kept.
    //!   * **no watermark row at all** — the revoked device has no `cw` row, so
    //!     `COUNT(ud) != COUNT(cw)` and the CASE returns NULL, disabling the
    //!     watermark gate entirely.
    //!
    //! All timestamps use SQLite's `datetime()` format on BOTH `sent_at` and
    //! `last_fetched_at` so the lexical comparison the gate performs is
    //! unambiguous.

    use super::*;


    /// A staleness window wide enough that none of the legacy fixtures (which seed
    /// `reported_at = NULL`, i.e. "report time unknown" → treated as live) are ever
    /// excluded — so the pre-#720 behaviour they pin is exactly preserved. The
    /// #720 tests set `reported_at` explicitly and probe across this boundary.
    const TEST_STALE: &str = "-6 months";

    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let conn = db.connect().unwrap();
        // Production is Turso, where foreign-key enforcement is off; libsql's LOCAL
        // backend turns it ON by default, so say so explicitly rather than
        // inherit a constraint no deploy has (`Db::connect_local` does the same).
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

    /// Seed a watermark row whose `last_fetched_at` is `datetime('now', offset)`.
    /// `reported_at` is left NULL — "report time unknown", treated as live — so
    /// legacy fixtures keep their pre-#720 meaning.
    async fn seed_watermark(conn: &Connection, conv: &str, user: &str, device: &str, offset: &str) {
        conn.execute(
            "INSERT INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at) \
             VALUES (?1, ?2, ?3, datetime('now', ?4))",
            libsql::params![conv.to_string(), user.to_string(), device.to_string(), offset.to_string()],
        )
        .await
        .unwrap();
    }

    /// Seed a watermark row with BOTH the message cursor (`last_fetched_at`) and
    /// the wall-clock report time (`reported_at`) as explicit `datetime('now', …)`
    /// offsets. The two are deliberately independent: `cursor` is how far the
    /// device has read (a message timestamp), `reported` is when the DS last heard
    /// from it (the #720 liveness signal).
    async fn seed_watermark_reported(
        conn: &Connection,
        conv: &str,
        user: &str,
        device: &str,
        cursor: &str,
        reported: &str,
    ) {
        conn.execute(
            "INSERT INTO conversation_watermark \
                 (conversation_id, user_id, device_id, last_fetched_at, reported_at) \
             VALUES (?1, ?2, ?3, datetime('now', ?4), datetime('now', ?5))",
            libsql::params![
                conv.to_string(),
                user.to_string(),
                device.to_string(),
                cursor.to_string(),
                reported.to_string()
            ],
        )
        .await
        .unwrap();
    }

    /// Insert one envelope at `datetime('now', offset)`.
    async fn add_envelope(conn: &Connection, id: &str, conv: &str, offset: &str) {
        conn.execute(
            "INSERT INTO message_envelope (id, conversation_id, sent_at, sender_id, ciphertext) \
             VALUES (?1, ?2, datetime('now', ?3), 'sender', 'ct')",
            libsql::params![id.to_string(), conv.to_string(), offset.to_string()],
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

    // ── Channel ──────────────────────────────────────────────────────────────

    async fn channel_fixture(conn: &Connection) {
        conn.execute("INSERT INTO group_member (group_id, user_id) VALUES ('g1', 'alice')", ())
            .await
            .unwrap();
        // Registry claim first: 000017's guard triggers refuse a channels row
        // whose id the `conversation` registry did not grant (#948), so every
        // fixture seeds the way production acquires a row.
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
        seed_watermark(&conn, "c1", "alice", "a-live", "-1 day").await;
        seed_watermark(&conn, "c1", "alice", "a-old", "-10 days").await;
        // The envelope sits between them: above the stale cursor, below the live one.
        add_envelope(&conn, "e1", "c1", "-5 days").await;

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
        seed_watermark(&conn, "c1", "alice", "a-live", "-1 day").await;
        add_envelope(&conn, "e1", "c1", "-5 days").await;

        run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "c1").await;

        assert_eq!(
            envelope_count(&conn, "c1").await,
            0,
            "with the revoked device excluded, COUNT(ud) == COUNT(cw) and the \
             watermark gate prunes; its absent row must not disable GC (#685)"
        );
    }

    // ── DM ───────────────────────────────────────────────────────────────────

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
        seed_watermark(&conn, "d1", "alice", "a-live", "-1 day").await;
        seed_watermark(&conn, "d1", "alice", "a-old", "-10 days").await;
        add_envelope(&conn, "e1", "d1", "-5 days").await;

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
        seed_watermark(&conn, "d1", "alice", "a-live", "-1 day").await;
        add_envelope(&conn, "e1", "d1", "-5 days").await;

        run_cleanup(&conn, CLEANUP_DM_ENVELOPES, "d1").await;

        assert_eq!(
            envelope_count(&conn, "d1").await,
            0,
            "DM: the revoked device's absent watermark must not disable GC (#685)"
        );
    }

    // ── I3 — no TTL: age alone never deletes ─────────────────────────────────

    /// **The regression test for F3.** A LIVE member device whose cursor sits
    /// below an envelope must keep that envelope alive no matter how old it is.
    /// The fixture is deliberately extreme — the envelope is 400 days old, more
    /// than 13× the deleted 30-day TTL — so the assertion cannot pass by accident
    /// of when the suite runs. Under the old `sent_at < datetime('now','-30 days')
    /// OR ...` predicate the TTL arm alone deleted this row and the recipient
    /// permanently lost the message.
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
        seed_watermark(&conn, "c1", "alice", "a1", "-1 day").await;
        seed_watermark(&conn, "c1", "bob", "b1", "-500 days").await;
        add_envelope(&conn, "ancient", "c1", "-400 days").await;

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
        seed_watermark(&conn, "c1", "alice", "a1", "-1 day").await;
        add_envelope(&conn, "ancient", "c1", "-400 days").await;

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
        seed_watermark(&conn, "d1", "alice", "a1", "-1 day").await;
        seed_watermark(&conn, "d1", "bob", "b1", "-500 days").await;
        add_envelope(&conn, "ancient", "d1", "-400 days").await;

        run_cleanup(&conn, CLEANUP_DM_ENVELOPES, "d1").await;

        assert_eq!(
            envelope_count(&conn, "d1").await,
            1,
            "DM: a 400-day-old envelope below the slowest member device's cursor \
             must survive (I3/F3)"
        );
    }

    // ── The positive leg: deletion still happens ─────────────────────────────

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
        add_envelope(&conn, "collected", "c1", "-3 days").await;
        add_envelope(&conn, "pending", "c1", "-1 hour").await;
        seed_watermark(&conn, "c1", "alice", "a1", "-2 days").await;
        seed_watermark(&conn, "c1", "alice", "a2", "-2 days").await;
        seed_watermark(&conn, "c1", "bob", "b1", "-2 days").await;

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
        seed_watermark(&conn, "c1", "alice", "a-fast", "-1 hour").await;
        seed_watermark(&conn, "c1", "alice", "a-slow", "-6 days").await;
        // Above the slow cursor, below the fast one: MIN keeps it, MAX would not.
        add_envelope(&conn, "between", "c1", "-3 days").await;

        run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "c1").await;

        assert_eq!(
            envelope_count(&conn, "c1").await,
            1,
            "the floor must be MIN over member devices — the fast device's cursor \
             must not evict what the slow one has yet to collect"
        );
    }

    // ── Conservative edges ───────────────────────────────────────────────────

    /// A conversation with ZERO current member devices deletes NOTHING. The
    /// aggregate degenerates to `COUNT 0 = COUNT 0` with `MIN(...) = NULL` over
    /// zero rows, so the CASE returns NULL and `sent_at < NULL` is NULL. Pinned as
    /// a test because the alternative reading of an empty roster — "everybody has
    /// collected, delete it all" — is exactly the unbounded delete this ticket
    /// removes.
    #[tokio::test]
    async fn an_empty_member_device_set_deletes_nothing() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        // A member row exists but the member has no device at all.
        add_envelope(&conn, "ancient", "c1", "-400 days").await;
        add_envelope(&conn, "fresh", "c1", "-1 hour").await;

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
        add_envelope(&conn, "ancient", "ghost", "-400 days").await;

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
        seed_watermark(&conn, "c1", "alice", "a-old", "-500 days").await;
        add_envelope(&conn, "ancient", "c1", "-400 days").await;

        run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "c1").await;

        assert_eq!(
            envelope_count(&conn, "c1").await,
            1,
            "excluding revoked devices must never leave a roster that deletes by \
             virtue of being empty"
        );
    }

    // ── The server-side sweep is the trigger (#689) ──────────────────────────

    /// The GC trigger is a DS-internal sweep, not a member's ingest path. With NO
    /// member calling the per-conversation endpoint, one sweep collects every
    /// conversation whose devices have all caught up — a channel AND a DM (proving
    /// it classifies both predicates) — while sparing one with an uncollected
    /// recipient. On the old trigger nothing drives GC when no member ingests, so
    /// the quiet-but-collected conversations would survive.
    #[tokio::test]
    async fn sweep_collects_quiet_conversations_without_any_ingest() {
        let conn = conn().await;

        // A channel whose one member device has caught up. Distinct users per
        // conversation: `user_device` is global, so a member's device in one
        // conversation counts toward every conversation they belong to.
        channel_fixture(&conn).await;
        add_device(&conn, "alice", "a1", false).await;
        seed_watermark(&conn, "c1", "alice", "a1", "-1 day").await;
        add_envelope(&conn, "eA", "c1", "-5 days").await;

        // A DM whose one member device has caught up (exercises the DM predicate).
        conn.execute("INSERT INTO dm_channel_member (dm_channel_id, user_id, added_by) VALUES ('d1', 'dave', 'creator')", ())
            .await
            .unwrap();
        add_device(&conn, "dave", "d-dev", false).await;
        seed_watermark(&conn, "d1", "dave", "d-dev", "-1 day").await;
        add_envelope(&conn, "eD", "d1", "-5 days").await;

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
        add_envelope(&conn, "eB", "c2", "-5 days").await;

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

    // ── The #720 device-liveness bound ───────────────────────────────────────
    //
    // Each test below FAILS against the pre-#720 code (no `reported_at`, no
    // staleness arm) by asserting a COLLECTION the watermark-only predicate never
    // performs — confirmed by removing the `?2` arm from `CLEANUP_*`, which leaves
    // the dormant device pinning and flips the survivor count back to 1.

    /// A dormant device — one that reported a watermark long ago and then went
    /// silent — stops pinning after the staleness window: its envelope is
    /// collected once every LIVE device has read past it. Pre-#720 (bob still in
    /// the roster with his ancient cursor) `MIN(cw) = bob` sits below the envelope
    /// and it is retained forever; this asserts the opposite.
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
        seed_watermark_reported(&conn, "c1", "alice", "a1", "-1 day", "-1 day").await;
        seed_watermark_reported(&conn, "c1", "bob", "b1", "-500 days", "-400 days").await;
        // The envelope sits above bob's stuck cursor but below alice's.
        add_envelope(&conn, "e1", "c1", "-100 days").await;

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
    ///
    /// Leg 2 (dormant) is what fails against pre-#720 code — it asserts the ancient
    /// envelope IS collected once the blocker is stale. Leg 1 (live) is the F3
    /// guard: a naive staleness keyed on `last_fetched_at` (the cursor, 500 days
    /// old here) or on `sent_at` (400 days old) would wrongly collect it; keying on
    /// `reported_at` (1 day old) keeps it.
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
            seed_watermark_reported(&conn, "c1", "alice", "a1", "-1 day", "-1 day").await;
            // Cursor 500 days back, but reported ONE day ago — live.
            seed_watermark_reported(&conn, "c1", "bob", "b1", "-500 days", "-1 day").await;
            add_envelope(&conn, "ancient", "c1", "-400 days").await;

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
            seed_watermark_reported(&conn, "c1", "alice", "a1", "-1 day", "-1 day").await;
            // Same cursor, but reported 400 days ago — dormant.
            seed_watermark_reported(&conn, "c1", "bob", "b1", "-500 days", "-400 days").await;
            add_envelope(&conn, "ancient", "c1", "-400 days").await;

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
        seed_watermark_reported(&conn, "c1", "alice", "a1", "-1 day", "-1 day").await;
        // Legacy row: cursor set, reported_at NULL.
        seed_watermark(&conn, "c1", "bob", "b1", "-500 days").await;
        add_envelope(&conn, "e1", "c1", "-100 days").await;

        run_cleanup(&conn, CLEANUP_CHANNEL_ENVELOPES, "c1").await;

        assert_eq!(
            envelope_count(&conn, "c1").await,
            1,
            "a NULL reported_at must pin (fail-closed) — unknown ≠ dormant (#720)"
        );
    }

    /// Whole-roster dormancy must not manufacture an empty roster that deletes:
    /// when EVERY device is stale the roster is empty, and — exactly like the
    /// all-revoked case — an empty roster deletes nothing (`MIN` over zero rows is
    /// NULL). A dead conversation retains its (bounded) envelopes; it is not wiped.
    #[tokio::test]
    async fn an_all_stale_roster_deletes_nothing() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        add_device(&conn, "alice", "a1", false).await;
        seed_watermark_reported(&conn, "c1", "alice", "a1", "-100 days", "-400 days").await;
        add_envelope(&conn, "ancient", "c1", "-50 days").await;

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
        seed_watermark_reported(&conn, "d1", "alice", "a1", "-1 day", "-1 day").await;
        seed_watermark_reported(&conn, "d1", "bob", "b1", "-500 days", "-400 days").await;
        add_envelope(&conn, "e1", "d1", "-100 days").await;

        run_cleanup(&conn, CLEANUP_DM_ENVELOPES, "d1").await;

        assert_eq!(
            envelope_count(&conn, "d1").await,
            0,
            "DM: a device silent past the window stops pinning (#720)"
        );
    }

    // ── #690 — the GC path releases attachment references (Blocking 2) ────────

    /// Insert a convergent dedup object + a `(content_hash, message_id)`
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
    ///
    /// **Fails against the pre-change code:** `object_is_referenced` read the raw
    /// `attachment_ref` row (still present after GC) and `sweep_envelope_gc` never
    /// reaped, so the object stayed pinned and referenced for the hash forever.
    #[tokio::test]
    async fn gc_releases_a_swept_messages_attachment_reference() {
        let conn = conn().await;
        channel_fixture(&conn).await;
        add_device(&conn, "alice", "a1", false).await;
        // Alice's one device has collected past the message, so GC will sweep it.
        seed_watermark(&conn, "c1", "alice", "a1", "-1 day").await;
        add_envelope(&conn, "m1", "c1", "-5 days").await;
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
    ///
    /// **Fails against the pre-change code**, where the raw row made
    /// `object_is_referenced` true with no envelope ever behind it, and no sweep
    /// reaped it: an attacker could pin any object permanently.
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
    //! "Which member devices count toward retention" is one rule with three
    //! implementations: [`CLEANUP_CHANNEL_ENVELOPES`], [`CLEANUP_DM_ENVELOPES`]
    //! (envelope GC, SQL) and [`crate::commit::current_member_devices`]
    //! (commit-log retention floor, Rust). #685/#686 brought them into agreement
    //! on revoked devices; nothing structural keeps them there. A divergence is
    //! not cosmetic:
    //!
    //!   * SQL roster **narrower** than the Rust one → envelope GC deletes mail a
    //!     device the commit log still serves has not collected (loss, F3-shaped).
    //!   * SQL roster **wider** → a device that cannot rejoin the tree holds the
    //!     watermark floor down forever and envelopes never clear (stuck
    //!     retention).
    //!
    //! ## How this test avoids being a restatement of the SQL
    //!
    //! The cleanup SQL embeds its roster inside a DELETE, so it cannot be read
    //! back directly. Rather than hand-copy an "equivalent" SELECT here — which
    //! would only ever prove *this file* agrees with *this file*, and would sit
    //! there passing while the DELETE drifted — the join chain is factored out of
    //! the DELETE into `channel_member_device_rows!` / `dm_member_device_rows!`
    //! and `concat!`-ed back in. The statements the DS executes are unchanged
    //! byte-for-byte (`the_extraction_did_not_change_the_cleanup_sql` pins that),
    //! and the SELECTs below are built from the SAME macro the DELETE is. There
    //! is exactly one copy of the roster SQL in the crate.
    //!
    //! So the drift this catches is the drift that matters: change the roster on
    //! either side — the JOIN inside the DELETE, or the query in
    //! `current_member_devices` — and the two sides disagree here. The expected
    //! rosters are additionally spelled out literally, so a change applied
    //! symmetrically to *both* implementations still fails rather than silently
    //! redefining the invariant.
    //!
    //! [`the_roster_is_what_the_delete_actually_gates_on`] closes the remaining
    //! gap between "the fragment" and "the statement": it proves the deletion
    //! outcome flips exactly on the watermark of a device the fragment lists, and
    //! does not move for devices it excludes.

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
        // Production is Turso, where foreign-key enforcement is off; libsql's LOCAL
        // backend turns it ON by default, so say so explicitly rather than
        // inherit a constraint no deploy has (`Db::connect_local` does the same).
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

    async fn watermark(conn: &Connection, conv: &str, user: &str, device: &str, offset: &str) {
        conn.execute(
            "INSERT INTO conversation_watermark (conversation_id, user_id, device_id, last_fetched_at) \
             VALUES (?1, ?2, ?3, datetime('now', ?4))",
            libsql::params![
                conv.to_string(),
                user.to_string(),
                device.to_string(),
                offset.to_string()
            ],
        )
        .await
        .unwrap();
    }

    async fn envelope(conn: &Connection, id: &str, conv: &str, offset: &str) {
        conn.execute(
            "INSERT INTO message_envelope (id, conversation_id, sent_at, sender_id, ciphertext) \
             VALUES (?1, ?2, datetime('now', ?3), 'sender', 'ct')",
            libsql::params![id.to_string(), conv.to_string(), offset.to_string()],
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
    ///
    /// Watermarks are seeded only where a test needs them; membership and
    /// devices are the shared part.
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
    /// shapes. Spelled out rather than derived, so a change made symmetrically to
    /// the SQL and the Rust roster still fails here instead of quietly
    /// redefining the invariant.
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
        watermark(conn, conv, "alice", "alice-1", "-1 day").await;
        watermark(conn, conv, "bob", "bob-live", "-2 days").await;
        // `bob-silent` and `dave-1` deliberately have NO row.
        watermark(conn, conv, "bob", "bob-revoked", "-400 days").await;
        watermark(conn, conv, "carol", "carol-old-1", "-400 days").await;
        watermark(conn, conv, "carol", "carol-old-2", "-400 days").await;
        watermark(conn, conv, "mallory", "mallory-1", "-400 days").await;
    }

    // ── The parity assertions ────────────────────────────────────────────────

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

    /// A member whose devices are ALL revoked, alone in the conversation: both
    /// paths must return the EMPTY roster. This is the case where disagreeing is
    /// most expensive — an empty roster disables envelope GC (conservative), but
    /// on the commit-log side it removes Tier-1's lower bound, so the two sides
    /// answering differently means one of them is acting on a device the other
    /// considers gone.
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
        watermark(&conn, "c-dead", "carol", "carol-old-1", "-400 days").await;

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

    // ── The fragment really is the statement ─────────────────────────────────

    /// Factoring the roster out of the DELETE must not have changed the SQL the
    /// DS executes. Pinned against the literal statements as they stood before
    /// the extraction, so the "safe, non-behavioural refactor" claim is checked
    /// rather than asserted.
    #[test]
    fn the_extraction_did_not_change_the_cleanup_sql() {
        assert_eq!(
            CLEANUP_CHANNEL_ENVELOPES,
            "\
DELETE FROM message_envelope
 WHERE conversation_id = ?1
   AND sent_at < (
       SELECT CASE
                WHEN COUNT(ud.device_id) = COUNT(cw.last_fetched_at)
                THEN MIN(cw.last_fetched_at)
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
   AND sent_at < (
       SELECT CASE
                WHEN COUNT(ud.device_id) = COUNT(cw.last_fetched_at)
                THEN MIN(cw.last_fetched_at)
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
    ///
    /// Every device the fragment lists gets a recent cursor and every device it
    /// excludes an ancient one, so an envelope in between is deleted. Then ONE
    /// listed device — and only that one — is moved below the envelope, and the
    /// envelope must survive: the outcome pivots on exactly the roster the
    /// fragment reports.
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
                watermark(&caught_up, conv, &user, d, "-1 day").await;
            }
            // Excluded devices are far behind — they must not be consulted.
            watermark(&caught_up, conv, "bob", "bob-revoked", "-400 days").await;
            watermark(&caught_up, conv, "carol", "carol-old-1", "-400 days").await;
            watermark(&caught_up, conv, "carol", "carol-old-2", "-400 days").await;
            watermark(&caught_up, conv, "mallory", "mallory-1", "-400 days").await;
            envelope(&caught_up, "e1", conv, "-5 days").await;
            // The `watermark` helper leaves `reported_at` NULL (live), so the #720
            // staleness arm excludes no one here — this test isolates the roster.
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
                let offset = if d.as_str() == "bob-silent" {
                    "-400 days"
                } else {
                    "-1 day"
                };
                watermark(&held, conv, &user, d, offset).await;
            }
            watermark(&held, conv, "bob", "bob-revoked", "-1 day").await;
            watermark(&held, conv, "carol", "carol-old-1", "-1 day").await;
            watermark(&held, conv, "carol", "carol-old-2", "-1 day").await;
            watermark(&held, conv, "mallory", "mallory-1", "-1 day").await;
            envelope(&held, "e1", conv, "-5 days").await;
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
    //! #693 / #661 — WS1: does the admin-delete tombstone reliably reach a
    //! caught-up recipient? This module isolates and RULES OUT the issue's
    //! candidate 1 (a read-after-write / `sent_at`-ordering visibility gap
    //! between the DS write connection and the recipient's read connection) at
    //! the envelope/watermark/tombstone layer, using the REAL DS write code
    //! (`apply_send_message`, `apply_advance_watermark`, `apply_delete_message`)
    //! and the EXACT fetch SQL that `pollis_core::commands::messages::ingest`
    //! runs.
    //!
    //! ## The scenario (`sealed_admin_delete_of_other_member_works`, stripped of MLS)
    //!
    //! bob sends; carol fetches (so she holds a copy) and reports her watermark;
    //! alice admin-deletes bob's message (envelope removed + `type='delete'`
    //! tombstone written); carol fetches again. The flaky assertion is that
    //! carol's second fetch *sees the tombstone*. Here we drive exactly that
    //! envelope/watermark/tombstone dance and assert carol's second fetch returns
    //! the tombstone — across every `sent_at` shape a client clock can produce,
    //! and across TWO DISTINCT CONNECTIONS of one shared libsql `Database`.
    //!
    //! ## Why two connections of one `Database` is the faithful model
    //!
    //! In the flows harness the in-process DS holds ONE `Db` — an `Arc`-shared
    //! libsql `Database` on ONE local WAL file (`harness.rs`; a second `Database`
    //! on the same file would NOT see the writer's rows promptly). Since #987 the
    //! clients hold no handle at all: their reads are signed POSTs served by that
    //! same `Db`. Every `Db::conn()` is a pooled `db.connect()` on the shared
    //! handle, so the write connection and the connection serving carol's read are
    //! two connections of ONE `Database` — which is precisely what these tests
    //! use. If libsql gave no read-your-writes guarantee across such connections,
    //! the tombstone SELECT below would intermittently miss the just-written row;
    //! it never does.
    //!
    //! ## What this proves (and what it therefore leaves)
    //!
    //! The tombstone is ALWAYS written and ALWAYS visible to carol's next fetch at
    //! this layer. So #661's residual flake is NOT "the tombstone is never
    //! written / sorts under carol's watermark / isn't yet visible on her
    //! connection". By elimination it lives DOWNSTREAM, in the client's
    //! *application* of the fetched tombstone (`ingest.rs`) — candidate 2,
    //! "fetched but not applied". See the self-diagnosing instrumentation added to
    //! the flows test itself, which reports which case a live failure was.

    use super::*;
    use tempfile::TempDir;


    /// The exact shape a CLIENT writes for `sent_at`
    /// (`chrono::Utc::now().to_rfc3339()`, i.e. `SecondsFormat::AutoSi`), at a
    /// fixed instant plus `nanos`. AutoSi trims to 0/3/6/9 fraction digits, so
    /// `nanos == 0` yields a whole-second stamp with NO fraction — the shape whose
    /// `'+' < '.'` ordering was the #692 regression.
    fn client_stamp(secs: i64, nanos: u32) -> String {
        chrono::DateTime::from_timestamp(secs, nanos)
            .expect("valid instant")
            .to_rfc3339()
    }

    /// One shared libsql `Database` on a WAL file — the faithful model of the
    /// single `Db` the flows harness hands to its in-process DS. Returns the
    /// tempdir (kept alive) so callers can open as many independent connections
    /// on it as they like.
    async fn shared_db() -> (TempDir, libsql::Database) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("main.db");
        let db = libsql::Builder::new_local(&path).build().await.expect("build");
        {
            let conn = db.connect().expect("connect");
            conn.query("PRAGMA journal_mode=WAL", ()).await.expect("wal");
            // Production is Turso, where foreign-key enforcement is off; libsql's LOCAL
        // backend turns it ON by default, so say so explicitly rather than
        // inherit a constraint no deploy has (`Db::connect_local` does the same).
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
        let mut rows = conn
            .query(
                "SELECT id, sender_id, ciphertext, reply_to_id, target_message_id, sent_at, type \
                 FROM message_envelope \
                 WHERE conversation_id = ?1 \
                   AND sent_at > COALESCE( \
                       (SELECT last_fetched_at FROM conversation_watermark \
                        WHERE conversation_id = ?1 AND user_id = ?2 AND device_id = ?3), \
                       '' \
                   ) \
                 ORDER BY sent_at ASC, id ASC",
                libsql::params![
                    conversation_id.to_string(),
                    user_id.to_string(),
                    device_id.to_string()
                ],
            )
            .await
            .expect("ingest fetch");
        let mut out = Vec::new();
        while let Some(row) = rows.next().await.expect("row") {
            out.push((
                row.get::<String>(0).expect("id"),
                row.get::<String>(6).expect("type"),
                row.get::<Option<String>>(4).expect("target"),
            ));
        }
        out
    }

    /// Drive the full #661 envelope-layer dance for a given client `sent_at`
    /// shape and assert carol's SECOND fetch sees the admin tombstone. The writes
    /// go through one connection and carol's reads through a DIFFERENT connection
    /// of the same `Database`, so a read-after-write gap (candidate 1) would show
    /// up as a missing tombstone here.
    async fn assert_tombstone_reaches_carol(msg_sent_at: &str) {
        let (_dir, db) = shared_db().await;
        // Two distinct connections on the SHARED handle: `w` stands in for the
        // DS's write connection, `carol` for carol's read connection.
        let w = db.connect().expect("write conn");
        let carol = db.connect().expect("carol read conn");

        let conv = "chan-general";
        let msg_id = "msg-bobs-post";

        // bob sends (sealed sentinel sender, no-auth path — the envelope columns
        // are what matter here, not the auth gate).
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
                // No push from a unit test — these assert envelope columns.
                push_to: None,
            },
        )
        .await
        .expect("send");

        // carol's FIRST fetch (read connection) sees bob's message, then reports
        // her watermark exactly as ingest does: advanced to the message's
        // `sent_at` (the max over her handled prefix).
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
            },
        )
        .await
        .expect("alice watermark");

        // alice (admin) deletes bob's message: envelope removed + tombstone
        // written. `msg_sender_id = "bob" != actor = "alice"` selects the admin
        // branch; the no-auth path skips the admin role re-check (not what we're
        // testing here). This runs the real `tombstone_floor` / `sent_at_after`.
        let outcome = apply_delete_message(
            &w,
            None,
            &DeleteMessageBody {
                message_id: msg_id.to_string(),
                conversation_id: conv.to_string(),
                msg_sender_id: Some("bob".to_string()),
                actor_id: Some("alice".to_string()),
            },
        )
        .await
        .expect("admin delete");
        assert!(matches!(outcome, WriteOutcome::Ok), "admin delete must succeed");

        // carol's SECOND fetch (read connection again) MUST see the tombstone.
        // This is the assertion that flakes in #661 — proven deterministic here.
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

    /// The headline: across every `sent_at` fraction width a client clock can
    /// emit — including the whole-second (`'+' < '.'`) shape and a stamp in the
    /// DS's FUTURE (client clock ahead) — carol always fetches the tombstone.
    #[tokio::test]
    async fn admin_tombstone_always_reaches_a_caught_up_recipient() {
        // A fixed base second, then the fraction widths AutoSi produces (0/3/6/9
        // digits) plus the max-nanos edge.
        let base = 1_800_000_000;
        for nanos in [0u32, 1_000_000, 1_001_000, 1_001_001, 999_999_999] {
            assert_tombstone_reaches_carol(&client_stamp(base, nanos)).await;
        }
        // Client clock running an hour AHEAD of the DS wall clock: the message —
        // and so carol's watermark — sit in the DS's future. `sent_at_after`'s
        // floor is what keeps the tombstone above them.
        let ahead = (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339();
        assert_tombstone_reaches_carol(&ahead).await;
    }

    /// The other direction of the same ordering question, and the one the
    /// tombstone tests above do not reach: after a DS tombstone lands, does a
    /// message the CLIENT stamps next still get fetched?
    ///
    /// It is the case a "whole-second stamps sort below fractional ones" reading
    /// predicts will break. `chrono::Utc::now().to_rfc3339()` (AutoSi) emits NO
    /// fraction when the instant's nanoseconds are exactly zero, so a client that
    /// sends on a second boundary writes `…T08:00:01+00:00` while the DS tombstone
    /// one nanosecond earlier is `…T08:00:00.999999999+00:00`. If those sorted the
    /// wrong way round the message would land UNDER the recipient's watermark
    /// (advanced to the tombstone) and `sent_at > last_fetched_at` would skip it
    /// permanently — a silently dropped message, which is not one of the three
    /// losses `CLAUDE.md` permits.
    ///
    /// They sort the right way round, and for a reason worth stating: AutoSi never
    /// TRUNCATES, it only omits a fraction that is genuinely zero. A stamp with no
    /// fraction therefore denotes `.000000000` — the EARLIEST instant of its
    /// second — so sorting below every fractional stamp in that same second is
    /// correct, and `'+' < '.'` is what produces it. (The #692 bug was a
    /// hand-rolled formatter that truncated a real fraction away; that is a
    /// different thing, and `now_rfc3339` is what fixed it.)
    ///
    /// What this test pins is the DS half: `sent_at_after`'s floor, and the
    /// mixed-format comparison between a Nanos tombstone and a fraction-less
    /// client stamp. The client half — that pollis-core never starts truncating —
    /// is pinned in `pollis_core::commands::messages::sent_at_format_tests`,
    /// which this crate cannot reach.
    #[tokio::test]
    async fn a_zero_nanosecond_client_message_still_outsorts_the_preceding_tombstone() {
        let (_dir, db) = shared_db().await;
        let w = db.connect().expect("write conn");
        let carol = db.connect().expect("carol read conn");

        let base = 1_800_000_000;
        let conv = "chan-general";

        // An existing message at `…:00.999999999` — the highest fraction there
        // is, so `sent_at_after` must roll the tombstone into the NEXT second.
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
                // No push from a unit test — these assert envelope columns.
                push_to: None,
            },
        )
        .await
        .expect("send");

        // Carol catches up and reports her watermark, then alice admin-deletes.
        // Both stamps are in 2027, i.e. ahead of any real DS clock, so the delete
        // takes `sent_at_after`'s floor branch and the tombstone is stamped
        // exactly one nanosecond past the highest thing in the conversation.
        assert_eq!(ingest_fetch(&carol, conv, "carol", "carol-dev").await.len(), 1);
        apply_advance_watermark(
            &w,
            None,
            &WatermarkBody {
                conversation_id: conv.to_string(),
                user_id: Some("carol".to_string()),
                device_id: "carol-dev".to_string(),
                last_fetched_at: client_stamp(base, 999_999_999),
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
        assert!(
            cursor.starts_with("2027-01-15T08:00:01"),
            "the floor guard must roll the tombstone into the next second, got {cursor}"
        );
        apply_advance_watermark(
            &w,
            None,
            &WatermarkBody {
                conversation_id: conv.to_string(),
                user_id: Some("carol".to_string()),
                device_id: "carol-dev".to_string(),
                last_fetched_at: cursor.clone(),
            },
        )
        .await
        .expect("carol watermark 2");

        // Now the client sends on the second boundary: AutoSi gives it NO
        // fraction, and it must still sort above the tombstone's `.000000000`…
        let boundary = client_stamp(base + 2, 0);
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
                // No push from a unit test — these assert envelope columns.
                push_to: None,
            },
        )
        .await
        .expect("send after tombstone");
        assert!(
            boundary > cursor,
            "a fraction-less client stamp ({boundary}) must sort above the DS \
             tombstone that precedes it ({cursor}) — otherwise it lands under \
             every recipient's watermark and is lost"
        );

        // The negative control, so this test is about the ORDERING RULE and not
        // about two strings happening to differ: what a TRUNCATING client
        // formatter (`SecondsFormat::Secs`) would have written for an instant
        // inside the tombstone's own second. It sorts UNDER the cursor, which is
        // the burial this whole scheme exists to prevent — and the reason
        // `pollis_core::commands::messages::envelope_sent_at` is a chokepoint
        // with `sent_at_never_truncates_a_real_fraction` guarding it.
        let truncated = "2027-01-15T08:00:01+00:00";
        assert!(
            truncated < cursor.as_str(),
            "premise of the guard: a truncated stamp ({truncated}) sorts below the \
             tombstone ({cursor}) and would be lost"
        );

        // …which is what makes carol actually receive it.
        let after = ingest_fetch(&carol, conv, "carol", "carol-dev").await;
        assert!(
            after.iter().any(|(id, _, _)| id == "msg-after-tombstone"),
            "a message sent after an admin delete must reach a caught-up recipient; \
             fetched: {after:?} against watermark {cursor}"
        );
    }

    /// The candidate-1 primitive in isolation: a row written on one connection of
    /// a shared libsql `Database` is IMMEDIATELY visible on another connection of
    /// the same handle — the read-your-writes guarantee the flows harness leans on
    /// (since #987 the clients hold no handle at all — every read is a POST the
    /// DS answers from that one `Database`, which only strengthens this). A second,
    /// INDEPENDENT `Database` opened on the same file is NOT guaranteed to see it
    /// promptly — which is exactly why the harness shares one handle rather than
    /// opening a second, and why candidate 1 cannot occur in that harness.
    #[tokio::test]
    async fn read_your_writes_holds_across_connections_of_one_shared_database() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("main.db");

        let db = libsql::Builder::new_local(&path).build().await.expect("build");
        {
            let c = db.connect().expect("connect");
            c.query("PRAGMA journal_mode=WAL", ()).await.expect("wal");
            // Production is Turso, where foreign-key enforcement is off; libsql's LOCAL
        // backend turns it ON by default, so say so explicitly rather than
        // inherit a constraint no deploy has (`Db::connect_local` does the same).
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
    //! The bug these pin: authz read `body.conversation_id` while the DELETE
    //! acted on `body.message_id` — two independent, attacker-supplied fields
    //! with nothing tying them together. Membership of ANY conversation (a DM
    //! with yourself qualifies) therefore authorised deleting ANY envelope in
    //! the deployment, on both the self and admin branches. Every client holds a
    //! whole-DB read-only Turso token, so the ids are enumerable. That is
    //! undelivered mail destroyed for everyone — invariant I3 and "messages must
    //! work", from an ordinary account.
    //!
    //! Both tests assert the VICTIM row survives. Asserting only that the call
    //! returns Ok would pass against the vulnerable code.

    use super::*;
    use libsql::Connection;


    async fn conn() -> Connection {
        let db = libsql::Builder::new_local(":memory:").build().await.unwrap();
        let c = db.connect().unwrap();
        // Production is Turso, where foreign-key enforcement is off; libsql's LOCAL
        // backend turns it ON by default, so say so explicitly rather than
        // inherit a constraint no deploy has (`Db::connect_local` does the same).
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
        };
        apply_delete_message(&c, Some("mallory"), &body).await.unwrap();

        assert!(
            !exists(&c, "my-envelope").await,
            "the legitimate self-delete must still remove the envelope"
        );
    }
}
