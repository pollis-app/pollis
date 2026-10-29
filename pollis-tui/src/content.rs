//! The message content string, as the other clients write it (#1337): plain
//! text or the envelope
//! `{"_att":[{"key":"media/…","name":"…","ct":"…","size":N,…}],"_txt":"caption"}`.
//!
//! [`parse_content`] is a line-for-line port of `parseContent` in
//! `mobile/lib/messageContent.ts` (itself in lockstep with desktop's
//! `frontend/src/hooks/queries/useMessages.ts`); keep all three in step.

use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub name: String,
    pub content_type: String,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Content {
    pub text: String,
    pub attachments: Vec<Attachment>,
}

/// Every field optional, so a malformed entry still renders as an attachment
/// rather than discarding the message.
#[derive(Deserialize)]
struct AttachmentWire {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    ct: Option<String>,
    #[serde(default)]
    size: Option<serde_json::Value>,
}

/// Plain text and anything that is not a valid envelope pass through as-is.
pub fn parse_content(raw: &str) -> Content {
    let plain = || Content {
        text: raw.to_string(),
        attachments: Vec::new(),
    };
    if !raw.starts_with('{') {
        return plain();
    }
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) else {
        return plain();
    };
    let caption = parsed.get("_txt").and_then(|v| v.as_str());
    let Some(list) = parsed.get("_att").and_then(|v| v.as_array()) else {
        // Legacy `_txt`-only envelopes still reduce to their caption.
        return match caption {
            Some(text) => Content {
                text: text.to_string(),
                attachments: Vec::new(),
            },
            None => plain(),
        };
    };
    let attachments = list
        .iter()
        .map(|entry| {
            let wire: AttachmentWire =
                serde_json::from_value(entry.clone()).unwrap_or(AttachmentWire {
                    name: None,
                    ct: None,
                    size: None,
                });
            Attachment {
                name: wire.name.unwrap_or_else(|| "attachment".to_string()),
                content_type: wire.ct.unwrap_or_default(),
                size: wire
                    .size
                    .and_then(|v| v.as_f64())
                    .map_or(0, |n| n.max(0.0) as u64),
            }
        })
        .collect();
    Content {
        text: caption.unwrap_or("").to_string(),
        attachments,
    }
}

/// `[image: photo.png, 2 KB]` — the line an attachment renders as.
pub fn attachment_label(a: &Attachment) -> String {
    let kind = attachment_kind(&a.content_type);
    let size = format_file_size(a.size);
    if size.is_empty() {
        format!("[{kind}: {}]", a.name)
    } else {
        format!("[{kind}: {}, {size}]", a.name)
    }
}

fn attachment_kind(content_type: &str) -> &'static str {
    let major = content_type.split('/').next().unwrap_or("");
    match major {
        "image" => "image",
        "video" => "video",
        "audio" => "audio",
        _ => "file",
    }
}

/// Same rule as desktop's `formatFileSize` (`frontend/src/utils/format.ts`);
/// empty for zero.
pub fn format_file_size(bytes: u64) -> String {
    if bytes == 0 {
        return String::new();
    }
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    let rounded = (value * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{} {}", rounded as u64, UNITS[unit])
    } else {
        format!("{rounded:.1} {}", UNITS[unit])
    }
}

/// A quoted message's snippet: its caption (else the first attachment's label),
/// whitespace collapsed, cut to `max_chars`.
pub fn preview(raw: &str, max_chars: usize) -> String {
    let content = parse_content(raw);
    let source = if content.text.trim().is_empty() {
        content
            .attachments
            .first()
            .map(attachment_label)
            .unwrap_or_default()
    } else {
        content.text
    };
    let collapsed = source.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_chars(&collapsed, max_chars)
}

/// Cut `s` to at most `max` characters, ending in `…` when it was cut.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// Fit `s` into `cols` terminal columns, ending in `…` when it had to be cut.
pub fn fit_width(s: &str, cols: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if UnicodeWidthStr::width(s) <= cols {
        return s.to_string();
    }
    if cols == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = UnicodeWidthChar::width(c).unwrap_or(0);
        // Leave one column for the ellipsis.
        if used + w > cols - 1 {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Byte ranges of the `@mentions` in `text` that address `username` (or
/// `@all`). Must match `mention_tokens` in pollis-core's `send.rs` and
/// `findMentions` in `frontend/src/utils/mentions.ts`.
pub fn mention_ranges(text: &str, username: &str) -> Vec<(usize, usize)> {
    let me = username.to_lowercase();
    let mut out = Vec::new();
    let mut prev: Option<char> = None;
    let mut iter = text.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        let opens = c == '@' && prev.is_none_or(char::is_whitespace);
        prev = Some(c);
        if !opens {
            continue;
        }
        let body_start = i + c.len_utf8();
        let mut body_end = body_start;
        while let Some(&(j, d)) = iter.peek() {
            if d.is_alphanumeric() || d == '_' || d == '-' || d == '.' {
                body_end = j + d.len_utf8();
                prev = Some(d);
                iter.next();
            } else {
                break;
            }
        }
        let body = text[body_start..body_end].trim_end_matches(['.', '-', '_']);
        if body.is_empty() {
            continue;
        }
        let name = body.to_lowercase();
        if name == "all" || (!me.is_empty() && name == me) {
            out.push((i, body_start + body.len()));
        }
    }
    out
}

/// The body an edit should store: editing a captioned attachment replaces only
/// its caption.
pub fn replace_caption(raw: &str, new_text: &str) -> String {
    if raw.starts_with('{') {
        if let Ok(serde_json::Value::Object(mut map)) = serde_json::from_str(raw) {
            if map.get("_att").is_some_and(|v| v.is_array()) {
                if new_text.is_empty() {
                    map.remove("_txt");
                } else {
                    map.insert("_txt".into(), serde_json::Value::String(new_text.into()));
                }
                return serde_json::Value::Object(map).to_string();
            }
        }
    }
    new_text.to_string()
}

/// Desktop's shortcode table (gemoji v4.1.0, MIT), so `:joy:` means the same
/// emoji everywhere.
const SHORTCODES_JSON: &str = include_str!("../../scripts/emoji-shortcodes.json");

fn shortcodes() -> &'static std::collections::HashMap<String, String> {
    static TABLE: std::sync::OnceLock<std::collections::HashMap<String, String>> =
        std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        #[derive(Deserialize)]
        struct File {
            shortcodes: std::collections::HashMap<String, String>,
        }
        serde_json::from_str::<File>(SHORTCODES_JSON)
            .map(|f| f.shortcodes)
            .unwrap_or_default()
    })
}

/// The react prompt's input as an emoji: a shortcode (`:joy:`, `joy`, `+1`) or
/// a literal. U+FE0F is stripped because desktop and mobile store the bare form
/// and reactions match by exact string (`❤` vs `❤️`).
pub fn resolve_reaction(input: &str) -> Option<String> {
    let trimmed = input.trim();
    let name = trimmed.trim_matches(':').to_lowercase();
    if name.is_empty() {
        return None;
    }
    if let Some(codepoints) = shortcodes().get(&name) {
        let emoji: Option<String> = codepoints
            .split('-')
            .map(|cp| u32::from_str_radix(cp, 16).ok().and_then(char::from_u32))
            .collect();
        return emoji;
    }
    if trimmed.chars().any(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let bare: String = trimmed.chars().filter(|&c| c != '\u{FE0F}').collect();
    if bare.is_empty() {
        None
    } else {
        Some(bare)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reactions_resolve_from_shortcodes_and_literals_in_bare_form() {
        assert_eq!(resolve_reaction(":+1:").as_deref(), Some("👍"));
        assert_eq!(resolve_reaction("joy").as_deref(), Some("😂"));
        assert_eq!(resolve_reaction(" :Tada: ").as_deref(), Some("🎉"));
        // The heart is stored without its variation selector, as desktop does.
        assert_eq!(resolve_reaction(":heart:").as_deref(), Some("\u{2764}"));
        assert_eq!(
            resolve_reaction("\u{2764}\u{FE0F}").as_deref(),
            Some("\u{2764}")
        );
        assert_eq!(resolve_reaction("🔥").as_deref(), Some("🔥"));
        assert_eq!(resolve_reaction("not-an-emoji"), None);
        assert_eq!(resolve_reaction("::"), None);
    }

    #[test]
    fn editing_an_attachment_message_keeps_the_attachment() {
        let raw =
            r#"{"_att":[{"key":"media/k","name":"p.png","ct":"image/png","size":1}],"_txt":"old"}"#;
        let edited = replace_caption(raw, "new caption");
        let c = parse_content(&edited);
        assert_eq!(c.text, "new caption");
        assert_eq!(c.attachments.len(), 1);
        assert!(edited.contains("media/k"), "{edited}");
        // Clearing the caption leaves just the attachment.
        assert_eq!(parse_content(&replace_caption(raw, "")).text, "");
        // Plain text is simply replaced.
        assert_eq!(replace_caption("old", "new"), "new");
    }

    #[test]
    fn long_names_are_ellipsized_to_the_column_budget() {
        assert_eq!(fit_width("@ bob", 10), "@ bob");
        assert_eq!(fit_width("@ pollis-e2e_peer1619_f8", 12), "@ pollis-e2…");
        assert_eq!(
            fit_width("@ pollis-e2e_peer1619_f8", 12).chars().count(),
            12
        );
        // Wide characters count as two columns.
        assert_eq!(fit_width("日本語テキスト", 5), "日本…");
        assert_eq!(fit_width("abc", 0), "");
    }

    #[test]
    fn plain_text_passes_through_untouched() {
        let c = parse_content("hello\nworld");
        assert_eq!(c.text, "hello\nworld");
        assert!(c.attachments.is_empty());
    }

    /// The exact shape desktop and mobile write for a captioned photo.
    #[test]
    fn an_attachment_envelope_yields_its_caption_and_attachments() {
        let raw = r#"{"_att":[{"key":"media/ab12","hash":"ab12","name":"photo.png","ct":"image/png","size":2048,"w":10,"h":10}],"_txt":"look at this"}"#;
        let c = parse_content(raw);
        assert_eq!(c.text, "look at this");
        assert_eq!(
            c.attachments,
            vec![Attachment {
                name: "photo.png".into(),
                content_type: "image/png".into(),
                size: 2048,
            }]
        );
        assert_eq!(
            attachment_label(&c.attachments[0]),
            "[image: photo.png, 2 KB]"
        );
    }

    #[test]
    fn an_envelope_without_a_caption_has_empty_text() {
        let c = parse_content(
            r#"{"_att":[{"key":"k","name":"a.pdf","ct":"application/pdf","size":1536}]}"#,
        );
        assert_eq!(c.text, "");
        assert_eq!(attachment_label(&c.attachments[0]), "[file: a.pdf, 1.5 KB]");
    }

    #[test]
    fn a_legacy_txt_only_envelope_reduces_to_its_caption() {
        assert_eq!(parse_content(r#"{"_txt":"just words"}"#).text, "just words");
    }

    /// Anything that merely looks like JSON stays the user's text, exactly as
    /// `parseContent` keeps it.
    #[test]
    fn text_that_is_not_an_envelope_is_kept_verbatim() {
        for raw in ["{not json", r#"{"a":1}"#, "{}", r#"{"_att":"nope"}"#] {
            assert_eq!(parse_content(raw).text, raw, "{raw}");
        }
    }

    #[test]
    fn file_sizes_read_like_desktop() {
        assert_eq!(format_file_size(0), "");
        assert_eq!(format_file_size(512), "512 B");
        assert_eq!(format_file_size(2048), "2 KB");
        assert_eq!(format_file_size(1536), "1.5 KB");
        assert_eq!(format_file_size(5 * 1024 * 1024), "5 MB");
    }

    #[test]
    fn attachment_kinds_follow_the_mime_type() {
        let a = |ct: &str| Attachment {
            name: "x".into(),
            content_type: ct.into(),
            size: 0,
        };
        assert_eq!(attachment_label(&a("video/mp4")), "[video: x]");
        assert_eq!(attachment_label(&a("audio/ogg")), "[audio: x]");
        assert_eq!(attachment_label(&a("")), "[file: x]");
    }

    #[test]
    fn preview_collapses_whitespace_and_falls_back_to_the_attachment() {
        assert_eq!(preview("one\ntwo   three", 40), "one two three");
        assert_eq!(preview("abcdefghij", 5), "abcd…");
        assert_eq!(
            preview(
                r#"{"_att":[{"name":"p.png","ct":"image/png","size":2048}]}"#,
                40
            ),
            "[image: p.png, 2 KB]"
        );
    }

    #[test]
    fn mentions_of_me_and_all_are_found_and_nothing_else() {
        let text = "hey @Dana, ping @all — not email@dana.com or @danae";
        let found: Vec<&str> = mention_ranges(text, "dana")
            .into_iter()
            .map(|(s, e)| &text[s..e])
            .collect();
        assert_eq!(found, vec!["@Dana", "@all"]);
        // Trailing sentence punctuation is not part of the name.
        assert_eq!(mention_ranges("thanks @dana.", "dana"), vec![(7, 12)]);
        assert!(mention_ranges("@ dana", "dana").is_empty());
    }
}
