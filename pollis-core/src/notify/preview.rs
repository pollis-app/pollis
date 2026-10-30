//! What a notification shows of a stored message body (#1599). Pure.
//!
//! The body is at most [`MAX_BODY_CHARS`] characters with markdown markers removed. An
//! attachment never shows its media or file name: it becomes a fixed English placeholder plus a
//! `body_key` the host may localise instead.

/// Longest body a preview carries, in characters.
pub const MAX_BODY_CHARS: usize = 200;

/// Why a body is a placeholder rather than text. Serialized as the contract's `body_key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKey {
    Attachment,
    Image,
    Video,
    Voice,
    Sensitive,
}

impl BodyKey {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Attachment => "attachment",
            Self::Image => "image",
            Self::Video => "video",
            Self::Voice => "voice",
            Self::Sensitive => "sensitive",
        }
    }

    /// The English fallback text; hosts may localise by [`BodyKey::as_str`].
    pub fn placeholder(self) -> &'static str {
        match self {
            Self::Attachment => "📎 Attachment",
            Self::Image => "📷 Photo",
            Self::Video => "🎥 Video",
            Self::Voice => "🎤 Voice message",
            Self::Sensitive => "Sensitive media",
        }
    }
}

/// The preview of one stored `message.content`: `(body, body_key)`.
///
/// An attachment message with a caption shows the caption and still carries the key, so a host
/// can add an icon; without a caption the body is the placeholder.
pub fn preview(content: &str) -> (String, Option<BodyKey>) {
    if let Some((caption, key)) = attachment_envelope(content) {
        let text = clean(&caption);
        if text.is_empty() {
            return (key.placeholder().to_string(), Some(key));
        }
        return (text, Some(key));
    }
    (clean(content), None)
}

/// `{"_att":[…],"_txt":…}` → `(caption, key)`; `None` for a plain body.
fn attachment_envelope(content: &str) -> Option<(String, BodyKey)> {
    let trimmed = content.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(trimmed).ok()?;
    let obj = value.as_object()?;
    let list = obj.get("_att")?.as_array()?;
    if list.is_empty() {
        return None;
    }
    let caption = obj
        .get("_txt")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let sensitive = list
        .iter()
        .any(|a| a.get("sens").and_then(|v| v.as_bool()).unwrap_or(false));
    let key = if sensitive {
        BodyKey::Sensitive
    } else {
        let ct = list[0].get("ct").and_then(|v| v.as_str()).unwrap_or_default();
        if ct.starts_with("image/") {
            BodyKey::Image
        } else if ct.starts_with("video/") {
            BodyKey::Video
        } else if ct.starts_with("audio/") {
            BodyKey::Voice
        } else {
            BodyKey::Attachment
        }
    };
    Some((caption, key))
}

/// Markdown markers out, whitespace collapsed, cut to [`MAX_BODY_CHARS`].
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(MAX_BODY_CHARS * 4));
    for line in text.lines() {
        let line = line.trim_start();
        // Block markers at the start of a line: quotes, headings, list bullets.
        let line = line.trim_start_matches('>').trim_start();
        let line = line.trim_start_matches('#').trim_start();
        let line = line
            .strip_prefix("- ")
            .or_else(|| line.strip_prefix("* "))
            .unwrap_or(line);
        if line.starts_with("```") {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(&strip_inline(line));
    }
    let collapsed: String = out.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate(&collapsed, MAX_BODY_CHARS)
}

/// `**b**`, `__b__`, `*i*`, `~~s~~`, `` `c` `` lose their markers; `[text](url)` keeps the text.
fn strip_inline(line: &str) -> String {
    let chars: Vec<char> = line.chars().collect();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            '*' | '`' | '~' => {
                i += 1;
            }
            '_' if chars.get(i + 1) == Some(&'_') => {
                i += 2;
            }
            '[' => {
                // A link: `[text](url)` → `text`. Anything else stays as typed.
                if let Some(close) = chars[i + 1..].iter().position(|&ch| ch == ']') {
                    let close = i + 1 + close;
                    if chars.get(close + 1) == Some(&'(') {
                        if let Some(end) = chars[close + 2..].iter().position(|&ch| ch == ')') {
                            out.extend(&chars[i + 1..close]);
                            i = close + 2 + end + 1;
                            continue;
                        }
                    }
                }
                out.push(c);
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let mut cut: String = text.chars().take(max - 1).collect();
    cut = cut.trim_end().to_string();
    cut.push('…');
    cut
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_passes_through() {
        assert_eq!(preview("hello there"), ("hello there".into(), None));
    }

    #[test]
    fn markdown_markers_are_stripped() {
        let (body, _) = preview("**bold** and *it* and ~~gone~~ `code` [site](https://x.example)");
        assert_eq!(body, "bold and it and gone code site");
        let (body, _) = preview("> quoted\n# Heading\n- item\n```\nfn x() {}\n```");
        assert_eq!(body, "quoted Heading item fn x() {}");
    }

    #[test]
    fn usernames_keep_single_underscores() {
        assert_eq!(preview("hi @ann_marie").0, "hi @ann_marie");
    }

    #[test]
    fn long_bodies_stop_at_200_characters() {
        let long = "é".repeat(500);
        let (body, _) = preview(&long);
        assert_eq!(body.chars().count(), MAX_BODY_CHARS);
        assert!(body.ends_with('…'));
    }

    #[test]
    fn attachments_become_placeholders_and_never_leak_names() {
        let img = r#"{"_att":[{"key":"k","url":"https://r2.example/k","name":"secret-plan.png","ct":"image/png","size":1}]}"#;
        assert_eq!(preview(img), ("📷 Photo".into(), Some(BodyKey::Image)));
        let vid = r#"{"_att":[{"name":"a.mp4","ct":"video/mp4","size":1}]}"#;
        assert_eq!(preview(vid).1, Some(BodyKey::Video));
        let voice = r#"{"_att":[{"name":"v.m4a","ct":"audio/mp4","size":1}]}"#;
        assert_eq!(preview(voice).1, Some(BodyKey::Voice));
        let file = r#"{"_att":[{"name":"q3.xlsx","ct":"application/vnd.ms-excel","size":1}]}"#;
        let (body, key) = preview(file);
        assert_eq!(key, Some(BodyKey::Attachment));
        assert!(!body.contains("q3"), "{body}");
    }

    #[test]
    fn sensitive_media_wins_over_its_type_and_a_caption_shows() {
        let s = r#"{"_att":[{"name":"x.jpg","ct":"image/jpeg","size":1,"sens":true}],"_txt":"look **now**"}"#;
        assert_eq!(preview(s), ("look now".into(), Some(BodyKey::Sensitive)));
        let bare = r#"{"_att":[{"name":"x.jpg","ct":"image/jpeg","size":1,"sens":true}]}"#;
        assert_eq!(preview(bare), ("Sensitive media".into(), Some(BodyKey::Sensitive)));
    }

    #[test]
    fn json_that_is_not_an_envelope_is_text() {
        assert_eq!(preview(r#"{"a":1}"#).1, None);
    }
}
