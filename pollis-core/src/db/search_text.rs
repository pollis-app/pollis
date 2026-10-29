//! `pollis_search_text` — the one transform between a stored message body and
//! the tokens the full-text index holds (#850).
//!
//! A deterministic SQLite function, so the FTS5 triggers in `local_schema.sql` maintain the index.
//!
//! It does three things, and each one is load-bearing:
//!
//! 1. **Unwraps the attachment envelope**: only `_txt` and attachment `name`s are indexed, not keys, URLs or hashes.
//! 2. **Segments CJK into overlapping bigrams**, applied identically to queries, so two-character words are findable.
//! 3. **Emits sentinel tokens** for `has:attachment` and `has:link`.
//!
//! **Determinism is required**: the contentless delete trigger re-runs this over `old.content`, so changed output drifts the index.

/// "Carries an attachment" sentinel; namespaced, and stripped from user input.
pub const ATTACHMENT_SENTINEL: &str = "zzpollisatt";

/// Sentinel token meaning "this message contains a link".
pub const LINK_SENTINEL: &str = "zzpollislnk";

/// Every sentinel, for the query parser to strip out of free text.
pub const SENTINELS: [&str; 2] = [ATTACHMENT_SENTINEL, LINK_SENTINEL];

/// Is `c` in a script `unicode61` runs together into one token?
///
/// Kana included: an unsegmented kana run fails the same way.
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
/// `今天天气很好` → `今天 天天 天气 气很 很好`; a lone `好` stays findable.
///
/// Runs over query terms too, so both sides agree.
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
    // Collapse double spaces: the query side compares segmented strings for equality.
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
    // A boundary so the run doesn't fuse with what follows.
    out.push(' ');
    run.clear();
}

/// The human-readable text of a message body: the caption plus any attachment
/// filenames, with the transport metadata dropped.
///
/// Also what the snippet is cut from.
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
/// Not run over attachment `url`s, or every attachment would match `has:link`.
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
/// Cheap-rejects non-`{` bodies; this runs on every insert.
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

/// Strip typed sentinels so a user cannot fake `has:attachment`.
pub fn strip_sentinels(text: &str) -> String {
    let mut out = text.to_string();
    for sentinel in SENTINELS {
        let lower = out.to_lowercase();
        if !lower.contains(sentinel) {
            continue;
        }
        // Case-insensitive without regex; positions recomputed because lowercasing can change byte length.
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
/// Must run before ANY write to `message`: the triggers call it.
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
        assert_eq!(segment_cjk("今天").trim(), "今天");
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

    /// Same input, same output, forever, or the index drifts.
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
