//! `pollis_search_text` — the one transform between a stored message body and
//! the tokens the full-text index holds (#850).
//!
//! Registered as a deterministic SQLite scalar function on every local
//! connection, because the FTS5 triggers in `local_schema.sql` call it. That is
//! deliberate: putting the transform in SQL-reachable code is what lets the
//! index be maintained by triggers instead of by ten Rust write sites that each
//! have to remember (CLAUDE.md ranks code discipline last).
//!
//! It does three things, and each one is load-bearing:
//!
//! 1. **Unwraps the attachment envelope.** A message body is either raw text or
//!    `{"_att":[{"key":…,"url":…,"name":…,"hash":…}],"_txt":…}`. Only `_txt`
//!    and each attachment's `name` are indexed; `key`, `url`, `ct`, `hash`,
//!    `bh` and `size` are dropped. Without this, searching for `media` or a hex
//!    fragment matches every message that carries an attachment, because the R2
//!    object key and the content hash are sitting in the body.
//! 2. **Segments CJK into overlapping bigrams.** `unicode61` tokenises
//!    `今天天气很好` as ONE token, so `今天` finds nothing; `trigram` needs three
//!    characters, and most Chinese words are two. Emitting bigrams at index
//!    time and applying the identical transform to the query is what makes
//!    Mandarin searchable at all.
//! 3. **Emits sentinel tokens** for `has:attachment` and `has:link`, so those
//!    filters are ordinary MATCH terms rather than an extra column, an extra
//!    index, or a schema change.
//!
//! **Determinism is a correctness requirement, not a nicety.** A contentless
//! FTS5 table is told what to delete by being handed the body it indexed, so
//! the delete trigger re-runs this function over `old.content`. If the output
//! for a given input ever changes, previously indexed rows can no longer be
//! deleted and the index drifts — which is what `rebuild_search_index` exists
//! to repair.

/// Sentinel token meaning "this message carries an attachment". Namespaced so
/// it cannot collide with a real word; stripped from user input by the query
/// parser so nobody can inject it by typing it.
pub const ATTACHMENT_SENTINEL: &str = "zzpollisatt";

/// Sentinel token meaning "this message contains a link".
pub const LINK_SENTINEL: &str = "zzpollislnk";

/// Every sentinel, for the query parser to strip out of free text.
pub const SENTINELS: [&str; 2] = [ATTACHMENT_SENTINEL, LINK_SENTINEL];

/// Is `c` in a script whose words are not space-separated, i.e. one that
/// `unicode61` will run together into a single token?
///
/// Kana is included even though Japanese has its own segmentation rules: an
/// unsegmented kana run has exactly the same failure mode as a Han one.
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF     // Hiragana + Katakana
        | 0x3400..=0x4DBF   // CJK Ext A
        | 0x4E00..=0x9FFF   // CJK Unified
        | 0xF900..=0xFAFF   // CJK Compatibility
        | 0xAC00..=0xD7AF   // Hangul syllables
        | 0x20000..=0x2FA1F // CJK Ext B..F + Compatibility Supplement
    )
}

/// Does this string contain a character that needs bigram segmentation?
pub fn has_cjk(s: &str) -> bool {
    s.chars().any(is_cjk)
}

/// Rewrite `text` so every CJK run becomes space-separated overlapping
/// bigrams, leaving everything else untouched.
///
/// `今天天气很好` becomes `今天 天天 天气 气很 很好`. A one-character run is
/// emitted as itself, so a lone `好` is still findable.
///
/// The same function runs over query terms, which is what makes the two sides
/// agree: `今天` is one bigram token, `会议明天` is the ordered bigram sequence
/// `会议 议明 明天` and is issued as a phrase.
pub fn segment_cjk(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut run: Vec<char> = Vec::new();

    for c in text.chars() {
        if is_cjk(c) {
            run.push(c);
            continue;
        }
        flush_cjk_run(&mut run, &mut out);
        out.push(c);
    }
    flush_cjk_run(&mut run, &mut out);
    // Flushing a run always emits a trailing separator, so a run followed by
    // real whitespace leaves a double space. Tokenisation would not care, but
    // the query side compares segmented strings for equality — normalising here
    // is what keeps the two sides byte-identical.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn flush_cjk_run(run: &mut Vec<char>, out: &mut String) {
    if run.is_empty() {
        return;
    }
    if run.len() == 1 {
        out.push(run[0]);
    } else {
        for (i, pair) in run.windows(2).enumerate() {
            if i > 0 {
                out.push(' ');
            }
            out.push(pair[0]);
            out.push(pair[1]);
        }
    }
    // Bigrams are their own tokens; without a boundary the run would fuse into
    // whatever follows it.
    out.push(' ');
    run.clear();
}

/// The human-readable text of a message body: the caption plus any attachment
/// filenames, with the transport metadata dropped.
///
/// This is also what the snippet is cut from, so the two cannot disagree about
/// what the message "says".
pub fn plain_text(content: &str) -> String {
    match parse_envelope(content) {
        Some(parsed) => {
            let mut parts: Vec<String> = Vec::new();
            if !parsed.text.trim().is_empty() {
                parts.push(parsed.text);
            }
            parts.extend(parsed.attachment_names);
            parts.join(" ")
        }
        None => content.to_string(),
    }
}

/// Whether a body carries at least one attachment.
pub fn has_attachment(content: &str) -> bool {
    parse_envelope(content)
        .map(|p| p.has_attachment)
        .unwrap_or(false)
}

/// Whether a body's human-readable text contains a link.
///
/// Deliberately naive, and deliberately NOT run over the attachment envelope's
/// `url` field — every attachment message would otherwise answer yes and
/// `has:link` would stop meaning anything.
pub fn has_link(content: &str) -> bool {
    let text = plain_text(content).to_lowercase();
    text.contains("http://") || text.contains("https://") || text.contains("www.")
}

struct Envelope {
    text: String,
    attachment_names: Vec<String>,
    has_attachment: bool,
}

/// Parse the `{"_att":[…],"_txt":…}` envelope, or `None` for a plain body.
///
/// Cheap-rejects anything that does not start with `{` before handing bytes to
/// serde: the overwhelming majority of messages are plain text and this runs on
/// every insert.
fn parse_envelope(content: &str) -> Option<Envelope> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let obj = value.as_object()?;
    if !obj.contains_key("_att") && !obj.contains_key("_txt") {
        return None;
    }

    let text = obj
        .get("_txt")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let mut attachment_names = Vec::new();
    let mut has_attachment = false;
    if let Some(list) = obj.get("_att").and_then(|v| v.as_array()) {
        has_attachment = !list.is_empty();
        for att in list {
            if let Some(name) = att.get("name").and_then(|v| v.as_str()) {
                if !name.trim().is_empty() {
                    attachment_names.push(name.to_string());
                }
            }
        }
    }

    Some(Envelope {
        text,
        attachment_names,
        has_attachment,
    })
}

/// The indexed body for a stored message body. This is the SQL function.
pub fn search_text(content: &str) -> String {
    let mut body = strip_sentinels(&plain_text(content));
    body = segment_cjk(&body);

    if has_attachment(content) {
        body.push(' ');
        body.push_str(ATTACHMENT_SENTINEL);
    }
    if has_link(content) {
        body.push(' ');
        body.push_str(LINK_SENTINEL);
    }
    body
}

/// Remove sentinel tokens that happen to appear in real text, so a user cannot
/// make a message answer `has:attachment` by typing `zzpollisatt`.
pub fn strip_sentinels(text: &str) -> String {
    let mut out = text.to_string();
    for sentinel in SENTINELS {
        let lower = out.to_lowercase();
        if !lower.contains(sentinel) {
            continue;
        }
        // Case-insensitive removal, without pulling in a regex engine. Safe to
        // slice `out` at offsets found in `lower` only because `to_lowercase`
        // is length-preserving for ASCII, and the sentinels are ASCII: a
        // non-ASCII character's lowercase form can change byte length, so the
        // match position is recomputed from the rebuilt prefix instead.
        let mut rebuilt = String::with_capacity(out.len());
        let mut cursor = 0usize;
        while let Some(at) = lower[cursor..].find(sentinel) {
            let start = cursor + at;
            if !out.is_char_boundary(start) || !out.is_char_boundary(start + sentinel.len()) {
                break;
            }
            rebuilt.push_str(&out[cursor..start]);
            cursor = start + sentinel.len();
        }
        rebuilt.push_str(&out[cursor..]);
        out = rebuilt;
    }
    out
}

/// Register `pollis_search_text` on a connection.
///
/// Must run before ANY write to `message`, because the triggers call it and an
/// unregistered function fails the whole statement with `no such function`.
pub fn register(conn: &rusqlite::Connection) -> rusqlite::Result<()> {
    use rusqlite::functions::FunctionFlags;
    conn.create_scalar_function(
        "pollis_search_text",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| {
            let content: Option<String> = ctx.get(0)?;
            Ok(content.map(|c| search_text(&c)))
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ATTACHMENT_BODY: &str = r#"{"_att":[{"key":"media/9f3c1a2b4d5e6f70","url":"https://r2.example/media/9f3c1a2b4d5e6f70","name":"Q3-budget-final.xlsx","hash":"deadbeefcafef00d","ct":"application/vnd.ms-excel","size":91234}],"_txt":"here is the deck"}"#;

    #[test]
    fn attachment_filenames_are_indexed_and_transport_metadata_is_not() {
        let indexed = search_text(ATTACHMENT_BODY);
        assert!(indexed.contains("Q3-budget-final.xlsx"), "{indexed}");
        assert!(indexed.contains("here is the deck"), "{indexed}");
        assert!(
            !indexed.contains("9f3c1a2b4d5e6f70"),
            "R2 key leaked into the index: {indexed}"
        );
        assert!(
            !indexed.contains("deadbeefcafef00d"),
            "content hash leaked into the index: {indexed}"
        );
        assert!(indexed.contains(ATTACHMENT_SENTINEL));
    }

    #[test]
    fn a_plain_body_is_left_alone() {
        assert_eq!(search_text("hello world"), "hello world");
    }

    #[test]
    fn cjk_runs_become_overlapping_bigrams() {
        assert_eq!(segment_cjk("今天天气很好").trim(), "今天 天天 天气 气很 很好");
        // A query for a two-character word is exactly one of those bigrams.
        assert_eq!(segment_cjk("今天").trim(), "今天");
        // Mixed scripts keep their Latin half verbatim.
        assert_eq!(segment_cjk("meet 明天 ok").trim(), "meet 明天 ok");
    }

    #[test]
    fn a_lone_cjk_character_survives() {
        assert_eq!(segment_cjk("好").trim(), "好");
    }

    #[test]
    fn link_sentinel_tracks_the_visible_text_not_the_attachment_url() {
        assert!(has_link("see https://example.com"));
        assert!(
            !has_link(ATTACHMENT_BODY),
            "an attachment's own URL must not answer has:link"
        );
    }

    #[test]
    fn sentinels_cannot_be_typed_into_a_message() {
        let indexed = search_text("zzpollisatt look at me");
        assert!(!indexed.contains(ATTACHMENT_SENTINEL), "{indexed}");
    }

    /// The delete trigger hands the index `pollis_search_text(old.content)` and
    /// expects the exact bytes the insert put there. Same input, same output —
    /// forever, or the index drifts.
    #[test]
    fn the_transform_is_deterministic() {
        for body in [
            ATTACHMENT_BODY,
            "hello world",
            "今天天气很好",
            "see https://example.com",
        ] {
            assert_eq!(search_text(body), search_text(body));
        }
    }
}
