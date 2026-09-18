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
//!   - **axum handlers** — `(State, RawRequest) -> Response`, all identical in
//!     shape. They [`crate::writes::gate_and_parse`] (shared auth over the raw
//!     body, then deserialize), call the matching `apply_*`, and map the outcome
//!     to 200 / 403 / 400 / 500.
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
//!     only advance their own — both halves of the key are bound to the signing
//!     device, the user must be a current member, and the cursor value itself
//!     is admitted only through [`check_cursor_stamp`] (as is every `sent_at`
//!     a send or edit carries): canonical UTC RFC 3339, no further ahead of
//!     the DS clock than the signature window already allows.
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

// ── Client-supplied cursor stamps are BOUNDED, not trusted ───────────────────
//
// `message_envelope.sent_at` and `conversation_watermark.last_fetched_at` are
// both written from values the CLIENT chose, and both are load-bearing for
// delivery: the fetch is `sent_at > last_fetched_at`, the watermark is monotone
// (`MAX`) and never rewinds, and envelope GC deletes `sent_at <
// MIN(last_fetched_at)`. Until this bound existed the DS stored either string
// verbatim, so ONE member posting `sent_at = "9999-…"` was enough to black out a
// conversation for everyone: every recipient fetched it (it sorts above every
// cursor), reported a cursor of `9999-…`, and from then on `sent_at > cursor`
// matched nothing — and once every live device had reported it, the next GC
// sweep deleted every envelope in the conversation, fetched or not. The same
// blackout was reachable from OUTSIDE the conversation through a forged
// watermark row (see [`TOMBSTONE_FLOOR`]). Neither is one of the three losses
// `CLAUDE.md` permits, and undoing either needed operator surgery on
// `conversation_watermark`.
//
// The value stays client-chosen — the sender's own local copy carries the same
// stamp, and cross-device read cursors compare against it (#844), so the DS
// re-stamping it would make one message sort differently on the sender's device
// than on everyone else's. What the DS enforces instead is that the value is
// one it could have produced itself: a canonical UTC RFC 3339 stamp no further
// ahead of the DS clock than the request-signature window already tolerates.
// That is exactly the set of strings whose lexical order matches chronological
// order across every writer of these columns (see [`now_rfc3339`]), and a
// bound the honest client already meets — its `sent_at` is stamped BEFORE the
// request is signed, and a signature more than [`CURSOR_STAMP_SKEW_SECS`]
// ahead of the DS clock is already refused by `auth`.

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

/// The one admission check for a client-chosen `sent_at` or `last_fetched_at`
/// (see the block comment above). `now` is injected so the boundary is testable
/// at chosen instants; the handlers pass `chrono::Utc::now()`.
///
/// Canonical means: `value` parses as RFC 3339 with a UTC offset AND re-renders
/// byte-for-byte as `chrono`'s UTC formatting at one of its four fraction widths
/// — the set `pollis_core::commands::messages::envelope_sent_at` (`AutoSi`) and
/// [`now_rfc3339`] (`Nanos`) between them produce. `Z` suffixes, non-zero
/// offsets, lowercase `t`, a space separator, truncated or padded fractions,
/// and SQLite's `YYYY-MM-DD HH:MM:SS` all fail it: each parses (or nearly does)
/// yet sorts somewhere other than where its instant belongs.
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

// ── The tombstone `sent_at` floor — REMOVED (#1087) ──────────────────────────
//
// A DS-stamped tombstone used to need a computed `sent_at`, strictly above the
// greatest cursor any recipient could already hold, because ingest selected
// `sent_at > last_fetched_at` and the two stamps came from different clocks with
// different precisions. A whole-second DS stamp sorts BELOW a sub-second client
// stamp inside the same second (`+` 0x2B < `.` 0x2E), so the delete was buried
// under the message it redacted and no recipient ever applied it. #692 then
// found that a GC-pruned conversation lost the envelope side of the floor and
// fell back to wall-clock `now`, reintroducing the clock dependency the guard
// existed to remove; the floor became the greater of the envelope and watermark
// sides.
//
// All of that was the cost of ordering by a lexically-compared string that two
// different clocks wrote. With a DS-assigned sequence a tombstone simply takes
// `MAX(seq)+1`, which is above every recipient's cursor by construction — so
// `TOMBSTONE_FLOOR`, `tombstone_floor` and `sent_at_after` are gone, along with
// the class of bug they patched. `sent_at` is display metadata now and nothing
// routes on it.

// ── POST /v1/messages/send ───────────────────────────────────────────────────

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
    let outcome = send_envelope(&conn, &log, authed.as_deref(), &parsed).await?;

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
    // The stamp is the delivery cursor every recipient will adopt — bounded
    // before it can touch the table (see `check_cursor_stamp`).
    if let Err(refused) = admit_sent_at(&body.sent_at) {
        return Ok(refused);
    }
    // Every NEW envelope must carry a deletion capability (#1135). #1086 could
    // only enforce it for rows that HAD one, because no shipped client produced
    // one yet — so anything a pre-#1086 client wrote landed with NULL and kept
    // the membership-only check, leaving the original hole open: any member
    // could remove any member's not-yet-fetched envelope.
    //
    // The NULL fallback stays for rows that already exist (they age out under
    // the #720 retention bound). This only closes the door on new ones, now
    // that clients producing a capability are the floor — #1086 shipped in
    // v1.12.0.
    //
    // `Invalid`, not `Forbidden`: the caller is entitled to send, the body is
    // what is not admissible.
    if body.delete_token_hash.as_deref().unwrap_or("").is_empty() {
        return Ok(WriteOutcome::Invalid(
            "delete_token_hash is required on every new envelope",
        ));
    }
    // The sequence allocation and the row must land together — see
    // `insert_envelope_with_seq`. Edit and delete already run in a transaction;
    // the send path needs its own.
    let tx = conn.transaction().await?;
    insert_envelope_with_seq(
        &tx,
        &body.conversation_id,
        // The per-envelope deletion capability (#1086), stored as the client sent
        // it — the DS never computes it and cannot: it is an HMAC under a key only
        // the author's devices hold. Absent (an older client) → NULL, and deletes
        // of this row fall back to the pre-#1086 membership check.
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

// ── POST /v1/messages/edit ───────────────────────────────────────────────────

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

// ── The epoch gate (#1041) ───────────────────────────────────────────────────
//
// An application envelope is decryptable only by members AT the epoch it was
// sealed at (`max_past_epochs = 0` on the client). If a commit lands between a
// sender's catch-up and its post, the envelope arrives sealed at an epoch every
// member has left or is about to leave: recipients that already merged the
// commit have thrown the keys away, and the committer's own merge advanced
// past it before it could fetch. The message is silently lost — not one of the
// three accepted losses.
//
// So the DS refuses to KEEP an envelope whose asserted `(generation, epoch)` is
// no longer the commit log's head, answering 409 `epoch_behind` with the head
// so the sender can catch up, re-seal, and post again. The assertion is
// optional on the wire (an older client sends none and is admitted ungated);
// every client this repo ships asserts.
//
// Insert-then-verify, not lock-then-insert. The envelope and the commit log
// live on two handles (`state.db` / `state.log_db`) that may be two databases
// in a deployed environment, so there is no transaction spanning both. Instead
// the envelope is inserted first and the head read second; if the head has
// moved the row is deleted again and the post refused. Race against a
// concurrent commit CAS (`commit::submit`), which appends the commit BEFORE its
// winner sweeps the envelopes of the epoch it closes:
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
    apply_at_head(
        main,
        log,
        &body.conversation_id,
        &body.id,
        body.generation.zip(body.epoch),
        || apply_send_message(main, authed, body),
    )
    .await
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
    // Same bound as a send, and checked BEFORE the DELETE below: a refused edit
    // must not have removed the author's pending edit on its way out.
    if let Err(refused) = admit_sent_at(&body.sent_at) {
        return Ok(refused);
    }
    // An edit REPLACES the target's pending edit, so it is a delete, so it needs
    // the target's capability (#1086).
    //
    // This narrows Solution A (#607), which had the DS accept an edit envelope
    // from any member and left authorship to the recipient's ingest check. That
    // was the only option while the DS could not tell an author from anyone
    // else — but it meant any member could clobber another author's
    // not-yet-fetched edit, and `idx_envelope_one_edit_per_message` (a partial
    // UNIQUE on `(conversation_id, target_message_id)`) makes that unavoidable
    // rather than incidental: one pending edit per message is a schema rule, so
    // accepting a second one MUST remove the first. "Accept but do not clobber"
    // is not a state this table can hold.
    //
    // Checking the capability does not undo what #607 bought. The DS still
    // cannot tell whose envelope this is — it verifies possession of a secret,
    // not an identity, so nothing is de-anonymized. The ingest-side author check
    // stays exactly as it was, as defence in depth against a legacy row whose
    // capability is NULL.
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
            edit_capability_hash(conn, &body.conversation_id, &body.target_message_id)
                .await?
                .into(),
        ],
    )
    .await?;
    tx.commit().await?;
    Ok(WriteOutcome::Ok)
}

// ── POST /v1/messages/delete ─────────────────────────────────────────────────

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
    /// or already gone. The caller falls back to the membership check that was
    /// the whole authorization before.
    NotRequired,
    /// The presented token hashes to the stored value.
    Proved,
    /// The row HAS a capability and the caller did not open it.
    Refused,
}

/// Check `token` against the capability stored on `message_id`.
///
/// Sealed sender means the DS cannot tell whose envelope a row is, so it cannot
/// decide who may delete it. It does not try: the sender stored
/// `SHA-256(token)` when the envelope was written, and this compares a hash. The
/// DS can neither compute a token (it is an HMAC under a key only the author's
/// devices hold) nor replay the stored value as one.
///
/// Scoped to `conversation_id` for the same reason every other statement here
/// is: the caller's authorization was checked against that id, so the lookup
/// must be too.
pub(crate) async fn check_delete_capability(
    conn: &Connection,
    conversation_id: &str,
    message_id: &str,
    token: Option<&str>,
) -> anyhow::Result<DeleteCapability> {
    use sha2::{Digest, Sha256};
    use subtle::ConstantTimeEq;

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
        // A row with no hash: the legacy path. A row that does not exist at all:
        // the delete is a no-op, so there is nothing to refuse.
        Some(row) => row.get::<Option<String>>(0).ok().flatten(),
        None => return Ok(DeleteCapability::NotRequired),
    };
    let Some(stored) = stored else {
        return Ok(DeleteCapability::NotRequired);
    };

    // The row demands a capability from here on: no token is a refusal, not a
    // fallback, or presenting nothing would be the easiest way past the check.
    let Some(token) = token else {
        return Ok(DeleteCapability::Refused);
    };
    // Compare the base64 forms, in constant time: both are fixed-length encodings
    // of a 32-byte digest, so a length difference is itself a mismatch rather
    // than something to branch on.
    use base64::Engine as _;
    let digest = base64::engine::general_purpose::STANDARD.encode(Sha256::digest(token.as_bytes()));
    if digest.as_bytes().ct_eq(stored.as_bytes()).into() {
        Ok(DeleteCapability::Proved)
    } else {
        Ok(DeleteCapability::Refused)
    }
}

/// The capability hash stored on `message_id`, for an edit envelope to inherit.
///
/// An edit is a replaceable envelope: the next edit deletes it. Copying the
/// TARGET's hash onto it means that next edit proves the same thing — authorship
/// of the original — rather than needing a capability of the edit's own, which
/// the DS could not check against anything.
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
    Ok(match rows.next().await? {
        Some(row) => row.get::<Option<String>>(0).ok().flatten(),
        None => None,
    })
}

/// Take the next delivery sequence for `conversation_id` (#1087).
///
/// One atomic statement against `conversation_seq`, which is the ONLY durable
/// high-water mark. The obvious alternative — `MAX(seq)+1` over
/// `message_envelope` — is wrong for a reason that is easy to miss and produces
/// silent message loss:
///
/// envelope GC **deletes rows**. Once every member device has read past
/// everything the conversation is emptied, `MAX(seq)` goes NULL, and the next
/// envelope is assigned 1 again — at or below every device's existing cursor, so
/// `seq > last_seq` never selects it and it reaches nobody. That is the #692
/// shape reappearing inside the design meant to retire it;
/// `envelope_retention::a_tombstone_outranks_surviving_cursors_after_gc_emptied_the_conversation`
/// is the test that catches it.
///
/// So the counter outlives the rows it numbers. GC never touches this table;
/// only conversation teardown removes a row, and the cursors pointing into it go
/// at the same time.
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
/// **Takes a `Transaction`, not a `Connection`, and that is the whole point.**
/// Allocating the sequence and inserting the row are two statements; if another
/// writer can slip between them, it allocates `N+1` and makes that row visible
/// while `N` is still missing. A recipient fetching `seq > last_seq` in that
/// window sees `N+1`, advances its cursor to it, and envelope `N` lands
/// *below every cursor* — delivered to nobody and collected by GC. That is a
/// fourth acceptable-loss mode, which we do not get to add.
///
/// Under SQLite/libsql a transaction whose first statement is a write holds the
/// write lock to commit, so writers serialise and the gap is never observable.
/// Requiring the transaction in the *type* is what stops a future caller from
/// re-opening the window by passing a bare autocommit connection.
///
/// A rolled-back insert un-does the counter bump too, so there is not even a
/// burned sequence; `idx_envelope_conv_seq` remains the backstop.
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
        // ...and, since #1086, PROOF rather than a claim. Membership alone let
        // any member remove any envelope in the conversation before slower
        // recipients fetched it — a fourth message loss on top of the three
        // CLAUDE.md allows, and the only attacker-controlled one. A row written
        // before capabilities existed still takes the membership-only path;
        // requiring one outright waits for clients that produce one to reach the
        // fleet.
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
    // #1087: no floor arithmetic any more. A tombstone takes the next delivery
    // sequence like every other envelope, and `MAX(seq)+1` is above every
    // recipient's cursor by construction — that is the whole point of an
    // assigned sequence. `sent_at` is display metadata here, so plain `now` is
    // correct and cannot bury anything.
    //
    // What this replaces: the tombstone used to be stamped `sent_at_after(now,
    // tombstone_floor(..))`, because a whole-second DS stamp sorts BELOW a
    // sub-second client stamp inside the same second (`+` 0x2B < `.` 0x2E) and
    // silently buried the delete under the message it redacted. That was a
    // lexical-format accident, and an integer has no format to get wrong.
    let now = now_rfc3339();
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

// ── POST /v1/reactions/add  &  /v1/reactions/remove ──────────────────────────

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

/// Monotone UPSERT of a `(conversation, user, device)` watermark. Authz: the row
/// belongs to the actor — BOTH halves of the key are bound to the verified
/// signature (`user_id` via [`resolve_actor`], `device_id` to the signing
/// device) — and the actor is a current member of the conversation. The value
/// itself is admitted only through [`check_cursor_stamp`]: a cursor is what
/// every later fetch and the GC floor read, so a far-future or malformed one
/// is refused rather than stored (see the block comment above that function
/// for the blackout it prevents).
///
/// Membership is checked on the SIGNED user, on every call. Without it any
/// account could write a row under any conversation id, and although GC joins
/// the real roster, the tombstone floor used not to — which turned a stray row
/// into a conversation-wide blackout after one admin delete. Rows for
/// conversations the actor does not belong to are now unwritable, so the
/// floor's roster join is defence in depth rather than the only defence.
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
    // A cursor that runs backwards is the whole hazard, so both columns take
    // `MAX(existing, reported)` and neither can rewind. `last_seq` is the one
    // delivery and retention read (#1087); `last_fetched_at` is carried for one
    // release so a device that reports only the old cursor still pins its
    // envelopes — the GC floor requires a `last_seq` from EVERY member device
    // before it collects anything, so a device reporting only the legacy value
    // holds the conversation rather than being counted as caught up.
    //
    // A negative `last_seq` is refused rather than clamped: sequences start at 1,
    // so it can only be a client bug or a probe, and silently coercing it would
    // hide both.
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

// ── POST /v1/envelopes/gc ────────────────────────────────────────────────────

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

/// True when `message_id` names a STILL-EXISTING envelope authored by `user_id`.
///
/// Both halves matter. Existence alone was the pre-#690 story and is what made a
/// forged reference merely inert rather than refused; authorship is what binds
/// the declaration to an account. A deleted or GC'd envelope answers `false`,
/// which is correct — there is no message left to declare an attachment for.
async fn envelope_is_authored_by(
    conn: &Connection,
    message_id: &str,
    user_id: &str,
) -> anyhow::Result<bool> {
    let mut rows = conn
        .query(
            "SELECT 1 FROM message_envelope WHERE id = ?1 AND sender_id = ?2 LIMIT 1",
            libsql::params![message_id.to_string(), user_id.to_string()],
        )
        .await?;
    Ok(rows.next().await?.is_some())
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
    outcome_response::<AttachmentDeleteBody>(apply_delete_attachment(&conn, authed.as_deref(), &parsed).await?)
}

/// Register a convergent-encryption dedup row (`content_hash → r2_key`) and,
/// when a `message_id` is supplied, a `(content_hash, message_id)` REFERENCE
/// declaration (#690).
///
/// ## Authz
///
/// The OBJECT row is content-addressed and identical for every uploader, so any
/// authenticated device may register one; there is no conversation context at
/// upload time and nothing finer to gate on.
///
/// The REFERENCE row is different, and used to be ungated — the handler took
/// `_authed` and discarded it. A reference pins a shared R2 blob against
/// collection (see [`live_ref_exists`]) and, through `/v1/r2/presign`'s delete
/// gate, decides whether the UPLOADER may hard-delete their own bytes. So a
/// reference is now bound to the account that owns the message it names: the
/// actor must be the `message_envelope.sender_id` of `message_id`. Declaring
/// "this file belongs to somebody else's message" is not a claim any account
/// other than that message's author can make.
///
/// The old defence was that a forged reference is *inert* — it counts only while
/// an envelope with that id exists, and the forger does not control that. True
/// for an id they do not own; useless for an id they do, which is the shape the
/// attack actually takes. Binding the declaration is what removes the class:
/// afterwards every `attachment_ref` row joined to a live envelope was written
/// by that envelope's own author, so the liveness predicate can no longer be
/// satisfied by an unrelated account's forgery. (The residual — a member of a
/// conversation pinning a hash they legitimately received onto their OWN live
/// message — is indistinguishable server-side from an honest forward, and is
/// noted against the `attachment_object` ownership migration.)
///
/// Skipped when `authed` is `None`: the no-auth (harness/dev) path has no actor
/// to bind to, exactly as the membership gates elsewhere in this module are.
///
/// The object INSERT stays `OR IGNORE` (the row is convergent — identical for
/// every uploader). The reference INSERT is likewise `OR IGNORE`: the PK
/// `(content_hash, message_id)` makes a retried send idempotent rather than
/// double-counting. Registering the reference here (rather than at upload) is
/// deliberate — a file is uploaded once but referenced by every message that
/// carries it, and the count must track messages, not uploads.
pub async fn apply_register_attachment(
    conn: &Connection,
    authed: Option<&str>,
    body: &AttachmentRegisterBody,
) -> anyhow::Result<WriteOutcome> {
    // Decided BEFORE the transaction opens, so a refused registration writes
    // nothing at all — not even the (harmless) object row, which would otherwise
    // let a refused caller confirm the shape of the refusal.
    if let (Some(actor), Some(message_id)) = (authed, body.message_id()) {
        if !envelope_is_authored_by(conn, message_id, actor).await? {
            return Ok(WriteOutcome::Forbidden);
        }
    }
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

    /// Seed a watermark row whose cursor is the delivery sequence `seq` (#1087).
    /// `reported_at` is left NULL — "report time unknown", treated as live — so
    /// these fixtures keep their pre-#720 meaning.
    ///
    /// `last_fetched_at` is still written, because the column is NOT NULL and is
    /// carried as display/legacy metadata; its VALUE is irrelevant to every
    /// assertion below, which is precisely the change #1087 makes.
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
    /// wall-clock report time (`reported_at`). The two are deliberately of
    /// different KINDS, which is the #720 distinction made structural: `seq` is
    /// how far the device has read (a position), `reported` is when the DS last
    /// heard from it (a time). Conflating them would resurrect F3.
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

    /// Insert one envelope at delivery sequence `seq`. Its `sent_at` is plain
    /// `now` — nothing routes on it since #1087, and writing an offset here would
    /// suggest otherwise.
    async fn add_envelope(conn: &Connection, id: &str, conv: &str, seq: i64) {
        conn.execute(
            "INSERT INTO message_envelope \
                 (id, conversation_id, sent_at, sender_id, ciphertext, seq) \
             VALUES (?1, ?2, datetime('now'), 'sender', 'ct', ?3)",
            libsql::params![id.to_string(), conv.to_string(), seq],
        )
        .await
        .unwrap();
        // Keep the durable counter consistent with the row, as
        // `insert_envelope_with_seq` does. A fixture that inserts an envelope
        // without advancing `conversation_seq` is not simulating the DS.
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
    /// when EVERY device is stale the roster is empty, and — exactly like the
    /// all-revoked case — an empty roster deletes nothing (`MIN` over zero rows is
    /// NULL). A dead conversation retains its (bounded) envelopes; it is not wiped.
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
        // Keep the durable counter consistent with the row, as
        // `insert_envelope_with_seq` does. A fixture that inserts an envelope
        // without advancing `conversation_seq` is not simulating the DS.
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
        watermark(conn, conv, "alice", "alice-1", 4).await;
        watermark(conn, conv, "bob", "bob-live", 3).await;
        // `bob-silent` and `dave-1` deliberately have NO row.
        watermark(conn, conv, "bob", "bob-revoked", 1).await;
        watermark(conn, conv, "carol", "carol-old-1", 1).await;
        watermark(conn, conv, "carol", "carol-old-2", 1).await;
        watermark(conn, conv, "mallory", "mallory-1", 1).await;
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
                watermark(&caught_up, conv, &user, d, 4).await;
            }
            // Excluded devices are far behind — they must not be consulted.
            watermark(&caught_up, conv, "bob", "bob-revoked", 1).await;
            watermark(&caught_up, conv, "carol", "carol-old-1", 1).await;
            watermark(&caught_up, conv, "carol", "carol-old-2", 1).await;
            watermark(&caught_up, conv, "mallory", "mallory-1", 1).await;
            envelope(&caught_up, "e1", conv, 2).await;
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
                // `bob-silent` is BELOW the envelope's sequence (2); everyone
                // else is above it. The envelope must survive on his account
                // alone.
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
        // The PRODUCTION fetch, not a copy of its predicate. This used to
        // reimplement `sent_at > last_fetched_at` inline, and a copy of the
        // delivery rule is exactly the thing that drifts: when #1087 moved the
        // cursor to `seq`, this helper kept fetching by timestamp and the tests
        // failed for a reason that had nothing to do with what they assert.
        crate::directory::envelopes_for_device(conn, conversation_id, user_id, device_id)
            .await
            .expect("ingest fetch")
            .into_iter()
            .map(|e| (e.id, e.kind, e.target_message_id))
            .collect()
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
                // Unasserted lineage: these tests are about envelope columns,
                // not the epoch gate (which has its own tests below).
                generation: None,
                epoch: None,
                // No push from a unit test — these assert envelope columns.
                push_to: None,
                delete_token_hash: Some(FIXTURE_CAPABILITY_HASH.to_string()),
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
                delete_token: None,
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

    /// A whole second in the DS's near future — far enough ahead that a test
    /// running for a while stays "ahead" (so `sent_at_after` takes its floor
    /// branch), yet inside the [`CURSOR_STAMP_SKEW_SECS`] window a client stamp
    /// must sit within to be admitted at all.
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
        // Client clock running AHEAD of the DS wall clock (by as much as the
        // stamp bound admits): the message — and so carol's watermark — sit in
        // the DS's future. `sent_at_after`'s floor is what keeps the tombstone
        // above them.
        let ahead = (chrono::Utc::now() + chrono::Duration::seconds(CURSOR_STAMP_SKEW_SECS - 30))
            .to_rfc3339();
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
    /// **Since #1087 this is pinned by construction rather than by arithmetic.**
    /// The delivery cursor is a DS-assigned sequence, so a tombstone gets
    /// `MAX(seq)+1` and outranks every recipient's cursor no matter how the two
    /// clocks compare. The test keeps the adversarial stamp shape — a client
    /// message at `.999999999` and a tombstone whose `sent_at` sorts BELOW it —
    /// and asserts the tombstone is delivered anyway. That is the strongest form
    /// of the property: the bug is not avoided, it is unrepresentable.
    ///
    /// The client half — that pollis-core never starts truncating a real
    /// fraction, which would corrupt DISPLAY order — is pinned in
    /// `pollis_core::commands::messages::sent_at_format_tests`, which this crate
    /// cannot reach.
    #[tokio::test]
    async fn a_zero_nanosecond_client_message_still_outsorts_the_preceding_tombstone() {
        let (_dir, db) = shared_db().await;
        let w = db.connect().expect("write conn");
        let carol = db.connect().expect("carol read conn");

        let base = base_second_ahead();
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
                // Unasserted lineage: these tests are about envelope columns,
                // not the epoch gate (which has its own tests below).
                generation: None,
                epoch: None,
                // No push from a unit test — these assert envelope columns.
                push_to: None,
                delete_token_hash: Some(FIXTURE_CAPABILITY_HASH.to_string()),
            },
        )
        .await
        .expect("send");

        // Carol catches up and reports her watermark, then alice admin-deletes.
        // Both stamps are ahead of the DS clock, so the delete takes
        // `sent_at_after`'s floor branch and the tombstone is stamped exactly one
        // nanosecond past the highest thing in the conversation.
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
        // #1087: the tombstone's `sent_at` is now plain `now` and is allowed to
        // sort BELOW the message it redacts — that is the whole point. What
        // matters is its SEQUENCE, which is `MAX(seq)+1` by construction and so
        // is above every recipient's cursor no matter what the clocks did.
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
        // And the adversarial stamp ordering the old floor guard existed to
        // paper over is now simply irrelevant: carol fetched the tombstone above
        // (or the `expect` would have fired) even though its `sent_at`
        // ({cursor}) sorts BELOW her reported `last_fetched_at`.
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

        // Now the client sends with a stamp that sorts BELOW the tombstone's —
        // a fraction-less stamp from an EARLIER second, which is what a client
        // clock running behind the DS produces. Under the timestamp cursor this
        // message was unreachable; under a sequence it is simply the next one.
        // Deliberately far in the past and fraction-less — the shape a client
        // whose clock is wrong produces, and unambiguously below the tombstone's
        // `now`. The DS bounds `sent_at` against the FUTURE only, so this is
        // admissible, and since #1087 it is also harmless.
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
                // Unasserted lineage: these tests are about envelope columns,
                // not the epoch gate (which has its own tests below).
                generation: None,
                epoch: None,
                // No push from a unit test — these assert envelope columns.
                push_to: None,
                delete_token_hash: Some(FIXTURE_CAPABILITY_HASH.to_string()),
            },
        )
        .await
        .expect("send after tombstone");
        // The strongest form of the property, and the one #1087 buys. Under the
        // old design delivery turned on `boundary > cursor` — a LEXICAL compare
        // between two clocks' stamps — so a message whose stamp happened to sort
        // below the preceding tombstone was buried under every recipient's
        // watermark and silently lost. Assert here that it is delivered even
        // when it sorts BELOW, because the cursor is a sequence and the stamp is
        // decoration.
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
        // Production is Turso, where foreign-key enforcement is off; libsql's
        // LOCAL backend turns it ON by default, so say so explicitly rather than
        // inherit a constraint no deploy has.
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

    /// Stand-in for what the author's devices derive. The DS never computes
    /// this — it only ever hashes what it is handed — so a fixed string is a
    /// faithful stand-in for an HMAC it cannot produce.
    pub(super) const TOKEN: &str = "the-authors-capability";

    pub(super) fn hash_of(token: &str) -> String {
        use base64::Engine as _;
        use sha2::{Digest, Sha256};
        base64::engine::general_purpose::STANDARD.encode(Sha256::digest(token.as_bytes()))
    }

    /// Write an envelope the way a **pre-#1135 client** did: no capability at
    /// all, NULL `delete_token_hash`.
    ///
    /// A direct insert rather than a `send`, because since #1135 the send
    /// endpoint REFUSES a body without a capability — so the only way such a
    /// row exists now is that it predates the gate. That is exactly the
    /// population the legacy fallback still has to serve, and testing it
    /// through an endpoint that can no longer produce it would quietly assert
    /// nothing (a missing row also reads `NotRequired`).
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
            // The self-branch hint: the caller claims to be the author. Before
            // #1086 that claim WAS the authorization.
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
    /// membership check passed and his `msg_sender_id == bob` hint selected the
    /// self-branch. Sealed sender means the DS cannot see that the envelope is
    /// Alice's — so he could delete it, with no tombstone, before slower
    /// recipients ever fetched it. He cannot compute Alice's capability.
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
    /// hash and keeps the old membership-only behaviour, because the DS cannot
    /// demand a capability for an envelope no client produced one for. This is
    /// the deliberate gap the follow-up closes once clients have shipped.
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
    ///
    /// Without this, a client that simply omits the field gets the pre-#1086
    /// behaviour back on demand — any member able to remove any member's
    /// not-yet-fetched envelope — which would make the capability optional in
    /// the only sense that matters to an attacker.
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
    /// is no "accept but do not clobber" state. Bob's edit is therefore refused,
    /// which narrows #607's "the DS accepts any member's edit" for edits only.
    /// Nothing is de-anonymized by it: the DS checks possession of a secret, not
    /// an identity.
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
    /// Regression for the allocate/insert gap: while a send was mid-flight the
    /// sequence bump used to commit on its own, so a concurrent writer could
    /// take `N+1` and publish *that* row first. A recipient fetching
    /// `seq > last_seq` in the window would see `N+1`, advance its cursor onto
    /// it, and envelope `N` would land below every cursor — delivered to nobody
    /// and then collected. This asserts a reader mid-send sees neither half.
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
    /// The obvious implementation is `MAX(seq)+1` over `message_envelope`. But
    /// envelope GC DELETES rows: once every member device has read past
    /// everything the conversation is emptied, `MAX(seq)` goes NULL, and the next
    /// envelope is assigned 1 again — at or below every device's cursor, so
    /// `seq > last_seq` never selects it and it is delivered to NOBODY.
    ///
    /// That is the #692 shape reappearing inside the design meant to retire it,
    /// and it is invisible unless a test empties a conversation and then posts
    /// into it. `conversation_seq` is not pruned by GC, which is what makes the
    /// sequence monotone for the conversation's lifetime rather than for the
    /// lifetime of its surviving rows.
    #[tokio::test]
    async fn a_sequence_is_never_reused_after_gc_empties_the_conversation() {
        let c = conn().await;
        apply_send_message(&c, None, &send("m1")).await.unwrap();
        let first = seq_of(&c, "m1").await.expect("assigned");

        // The only member device reads past it, so the real cleanup collects it.
        // The legacy cursor column is written the way production writes it — an
        // RFC 3339 stamp from the chokepoint, not `datetime('now')`. Nothing
        // reads it any more, but a fixture that writes the wrong FORMAT there is
        // the thing `watermark_seed_format` exists to catch, and a test that
        // trips its own tripwire teaches nobody anything.
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
                // The target now carries a capability (#1135), and an edit
                // replaces its pending edit — which is a delete — so the edit
                // has to present the preimage.
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
    /// Driven by running the REAL migration SQL, not a re-typed copy — a
    /// hand-rolled backfill in a test proves nothing about the one that ships.
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
    /// An envelope written without a `seq` is invisible: the fetch predicate is
    /// `seq > last_seq`, and `NULL > n` is NULL, so no recipient ever selects it
    /// — and the GC floor `seq <= MIN(last_seq)` never collects it either. It
    /// would sit in the table forever, delivered to nobody. That failure is
    /// silent at every layer, which is exactly the kind that needs a tripwire
    /// rather than a convention.
    ///
    /// A source-shape guard, not a proof: it reads production code only (test
    /// modules seed rows directly on purpose, and keep `conversation_seq` in
    /// step themselves), and it cannot see an INSERT built somewhere exotic. It
    /// closes the case that actually happens — someone adds a fourth envelope
    /// kind next to the three that exist and copies the wrong neighbour.
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
