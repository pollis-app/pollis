//! Message send / receive / edit / delete / reactions commands — split into
//! cohesive submodules. Public surface is preserved via the `pub use`
//! re-exports below so every external caller (Tauri shims, sibling
//! `commands::*` modules, integration tests) keeps resolving names at
//! `pollis_core::commands::messages::*`.

mod edit_delete;
pub(crate) mod framing;
mod ingest;
mod reactions;
mod read;
mod receipts;
mod retention;
mod send;
mod types;
// `watermark` (the `next_watermark` pure fn + `EnvKind`) is `pub` — not because
// any runtime caller needs the module path (they go through `pub use` below), but
// so the out-of-workspace `fuzz/` crate (Track B, #481) can fuzz the SAME
// production function Kani proves. Only `next_watermark` / `EnvKind` are `pub`
// inside it; `is_handled` stays private, so this widens no real API surface.
pub mod watermark;

/// The `sent_at` stamp for a `message_envelope` row this device writes — the ONE
/// place the client's envelope timestamp format is decided.
///
/// **Why a chokepoint for a one-liner.** `message_envelope.sent_at` is compared
/// **lexically**, not as a date: ingest fetches `sent_at > last_fetched_at`,
/// envelope GC deletes `sent_at < MIN(last_fetched_at)`, and the DS stamps
/// tombstones relative to `MAX(sent_at)`. The column holds both client stamps and
/// DS ones, so their two formats have to sort against each other correctly — and
/// a value that sorts below a recipient's cursor is not delayed, it is **lost**
/// (the cursor never rewinds), which is not one of the three losses `CLAUDE.md`
/// allows. That is a property of the *format*, so it belongs to one function
/// rather than to five call sites that each look like an innocent `now()`.
///
/// **The format, and why it is right.** `to_rfc3339()` is
/// `SecondsFormat::AutoSi`: 0, 3, 6 or 9 fraction digits, omitting the fraction
/// only when the nanoseconds are genuinely zero. It never TRUNCATES. So a
/// fraction-less stamp denotes `.000000000` — the earliest instant of its second
/// — and sorting below every fractional stamp in that second (`'+'` 0x2B <
/// `'.'` 0x2E) is the correct answer, not a bug. Across all four widths lexical
/// order equals chronological order, and it matches the DS's
/// `SecondsFormat::Nanos` stamps (`pollis_delivery::messages::now_rfc3339`) the
/// same way.
///
/// **What must never happen here** is truncation — writing `…T09:00:00+00:00`
/// for an instant that was really `…T09:00:00.789`. That value sorts below its
/// own second's traffic and buries the envelope under recipients' watermarks;
/// it is exactly the #692 bug, on the DS side, and the reason that side had to
/// move to explicit nanoseconds. `sent_at_never_truncates_a_real_fraction` below
/// is what stops it reappearing on this side.
pub(crate) fn envelope_sent_at() -> String {
    stamp_envelope_sent_at(chrono::Utc::now())
}

/// [`envelope_sent_at`] with the clock lifted out, so the format itself is
/// testable at chosen instants (`envelope_sent_at` can only ever be called at
/// "now", whose nanoseconds are not ours to pick). This is the function the
/// format tests below actually exercise — keeping them from asserting against a
/// re-implementation that could drift from the shipped one.
fn stamp_envelope_sent_at(at: chrono::DateTime<chrono::Utc>) -> String {
    at.to_rfc3339()
}

#[cfg(test)]
mod sent_at_format_tests {
    use super::{envelope_sent_at, stamp_envelope_sent_at};

    /// The shipped formatter, at a chosen instant.
    fn stamp(nanos: u32) -> String {
        stamp_envelope_sent_at(
            chrono::DateTime::from_timestamp(1_800_000_000, nanos).expect("valid instant"),
        )
    }

    /// The invariant the chokepoint exists for: a real sub-second fraction is
    /// never thrown away. A truncating formatter (`SecondsFormat::Secs`) writes
    /// `…T08:00:00+00:00` for `.789`, which sorts BELOW everything else in that
    /// second and so under every recipient's watermark — a silently lost message.
    #[test]
    fn sent_at_never_truncates_a_real_fraction() {
        for nanos in [1u32, 1_000, 1_000_000, 123_456_789, 999_999_999] {
            let s = stamp(nanos);
            assert!(
                s.contains('.'),
                "a nonzero fraction ({nanos}ns) must survive into the stamp, got {s}"
            );
        }
        // ...and a genuinely-zero fraction is OMITTED, not padded. That is what
        // makes the no-fraction shape mean `.000000000` rather than "unknown".
        assert!(!stamp(0).contains('.'), "got {}", stamp(0));
    }

    /// Lexical order — the only order the watermark ever applies — must match
    /// chronological order across every width AutoSi emits, including the
    /// fraction-less one.
    #[test]
    fn lexical_order_matches_chronological_order() {
        let stamps = [
            stamp(0),
            stamp(1),
            stamp(1_000),
            stamp(1_000_000),
            stamp(123_456_789),
            stamp(999_999_999),
        ];
        for pair in stamps.windows(2) {
            assert!(pair[0] < pair[1], "{} must sort below {}", pair[0], pair[1]);
        }
    }

    /// A fraction-less client stamp must also outsort a DS stamp
    /// (`SecondsFormat::Nanos`) from the PREVIOUS second — the mixed-format
    /// comparison that actually happens when a message follows an admin
    /// tombstone. The DS half of this lives in
    /// `pollis_delivery::messages::admin_delete_visibility_tests`.
    #[test]
    fn a_fraction_less_stamp_outsorts_the_previous_seconds_ds_stamp() {
        let ds = chrono::DateTime::from_timestamp(1_800_000_000, 999_999_999)
            .expect("valid instant")
            .to_rfc3339_opts(chrono::SecondsFormat::Nanos, false);
        let client = stamp(0);
        assert!(!client.contains('.'), "premise: no fraction, got {client}");
        assert!(
            stamp_envelope_sent_at(
                chrono::DateTime::from_timestamp(1_800_000_001, 0).expect("valid instant")
            ) > ds,
            "a fraction-less stamp one second later must sort above {ds}"
        );
        assert!(client < ds, "{client} is earlier and must sort below {ds}");
    }

    /// `envelope_sent_at` really is [`stamp_envelope_sent_at`] at `now` —
    /// otherwise every assertion above is about a formatter the shipped call
    /// sites do not use.
    #[test]
    fn envelope_sent_at_delegates_to_the_tested_formatter() {
        let live = envelope_sent_at();
        let parsed = chrono::DateTime::parse_from_rfc3339(&live)
            .expect("RFC3339")
            .to_utc();
        assert_eq!(stamp_envelope_sent_at(parsed), live);
        assert!(live.ends_with("+00:00"), "must be UTC-offset form, got {live}");
    }
}

// ── Types ────────────────────────────────────────────────────────────────────
pub use types::{
    ChannelMessage, ChannelPreview, Message, MessageCursor, MessagePage, MessageWithContext,
    SearchResult, ThreadSummary,
};

// ── Send ─────────────────────────────────────────────────────────────────────
pub use send::send_message;

// ── Read / list / search ─────────────────────────────────────────────────────
pub use read::{
    get_channel_messages, get_dm_messages, list_channel_previews, list_messages,
    list_messages_by_sender, list_thread_summaries, read_channel_messages, read_dm_messages,
    read_last_messages, read_thread_messages, search_messages,
};

// ── Ingest (envelope pull + watermark + cleanup) ─────────────────────────────
pub use ingest::{
    catch_up_mls_group_interleaved, ingest_channel_envelopes, ingest_channel_envelopes_inner,
    ingest_dm_envelopes, ingest_dm_envelopes_inner,
};

// ── Edit / delete ────────────────────────────────────────────────────────────
pub use edit_delete::{delete_message, edit_message};
#[cfg(feature = "test-harness")]
pub use edit_delete::{delete_message_body, edit_message_as, send_redaction_as};

// ── Receipts (delivery / read, DMs only — #857) ──────────────────────────────
pub use receipts::{get_conversation_receipts, mark_messages_read, MessageReceipts};

// ── Reactions ────────────────────────────────────────────────────────────────
pub use reactions::{add_reaction, get_reactions, remove_reaction, Reaction};

// ── Retention / local eviction ───────────────────────────────────────────────
pub use retention::{get_message_retention, run_message_eviction, set_message_retention};

#[cfg(test)]
mod tests;
