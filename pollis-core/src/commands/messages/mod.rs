//! Message send / receive / edit / delete / reactions commands.

mod delete_capability;
mod edit_delete;
mod edit_outbox;
pub(crate) mod framing;
#[cfg(feature = "fuzzing")]
pub mod fuzz_surface;
mod ingest;
pub(crate) mod loss;
mod outbox;
pub mod persisted;
mod reactions;
mod read;
mod read_state;
mod receipts;
mod retention;
mod seal;
mod search;
mod send;
mod types;
// `pub` so the out-of-workspace `fuzz/` crate can fuzz `next_watermark` (#481).
pub mod watermark;

/// The `sent_at` stamp for a `message_envelope` row this device writes — the ONE
/// place the client's envelope timestamp format is decided.
/// `sent_at` is compared lexically, so this must never truncate a real fraction.
pub(crate) fn envelope_sent_at() -> String {
    stamp_envelope_sent_at(chrono::Utc::now())
}

/// [`envelope_sent_at`] with the clock lifted out, so tests can pick the instant.
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

    /// A real sub-second fraction is never thrown away.
    #[test]
    fn sent_at_never_truncates_a_real_fraction() {
        for nanos in [1u32, 1_000, 1_000_000, 123_456_789, 999_999_999] {
            let s = stamp(nanos);
            assert!(
                s.contains('.'),
                "a nonzero fraction ({nanos}ns) must survive into the stamp, got {s}"
            );
        }
        // ...and a genuinely-zero fraction is omitted, not padded.
        assert!(!stamp(0).contains('.'), "got {}", stamp(0));
    }

    /// Lexical order must match chronological order across every AutoSi width.
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

    /// A fraction-less client stamp must outsort a DS `Nanos` stamp from the previous second.
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

    /// `envelope_sent_at` really is [`stamp_envelope_sent_at`] at `now`.
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

pub use types::{
    ChannelMessage, ChannelPreview, Message, MessageCursor, MessagePage, MessageWithContext,
    SearchCorpus, SearchCursor, SearchPage, SearchResult, SearchSnippet, SearchSort, ThreadSummary,
};

pub use send::{send_message, send_message_with_id};
pub use send::test_hooks;
pub(crate) use send::direct_mentions;

pub use outbox::{
    discard_unsent_message, flush_outbox, new_message_id, retry_message, OutboxReport,
};
pub(crate) use outbox::{spawn_outbox_flush, sweep_orphan_attachments_on};
pub use outbox::OutgoingAttachment;

pub use read::{
    get_channel_messages, get_dm_messages, list_messages, list_thread_summaries,
    read_channel_messages, read_dm_messages, read_last_messages, read_messages_after,
    read_messages_around, read_thread_messages,
};

pub use search::{
    cache_conversations, invalidate_corpus_cache, rebuild_search_index, search_messages,
    CachedConversation,
};

pub use ingest::{
    catch_up_mls_group_interleaved, ingest_channel_envelopes, ingest_channel_envelopes_inner,
    ingest_dm_envelopes, ingest_dm_envelopes_inner,
};
// The committer's pre-merge sweep (#1041), for the MLS publish path only.
pub(crate) use ingest::{sweep_current_epoch, SweptEnvelopes};

pub use edit_delete::{delete_message, edit_message};
// Shared with the vault (#107): one attachment-ref parser.
pub(crate) use edit_delete::{parse_attachment_refs, spawn_attachment_ref_backfill, AttachmentRef};
#[cfg(feature = "test-harness")]
pub use edit_delete::{delete_message_body, edit_message_as, send_redaction_as};

pub use read_state::{
    get_unread_counts, mark_conversation_read, sync_read_cursors, ReadCursor, UnreadCount,
};
pub use receipts::{get_conversation_receipts, mark_messages_read, MessageReceipts};

pub use reactions::{
    add_reaction, get_reactions, get_reactions_for_messages, remove_reaction, Reaction,
};

pub use retention::{get_message_retention, run_message_eviction, set_message_retention};

#[cfg(test)]
mod tests;
