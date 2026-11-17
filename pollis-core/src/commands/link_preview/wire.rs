//! The link preview as it travels inside the MLS ciphertext (#1987), and the
//! one place that decides whether a received preview may be shown.
//!
//! The bytes are the `0xF9` trailer of a text frame (`messages::framing`). They
//! are JSON so later fields can be added the way `_att` grew `bh`/`sens`/`pv`:
//! unknown keys are ignored, every optional key is omitted when unset.
//!
//! Everything here is untrusted: the sender's client wrote it and could have
//! written anything. [`decode`] is the chokepoint every received preview passes
//! before it is stored, so no other layer has to re-check what it guarantees.

use serde::{Deserialize, Serialize};

/// Longest title kept, in characters.
pub const MAX_TITLE_CHARS: usize = 200;
/// Longest description kept, in characters.
pub const MAX_DESCRIPTION_CHARS: usize = 300;
/// Longest preview URL, as `MAX_URL_LENGTH` in the renderers' link rules.
pub const MAX_URL_LEN: usize = 2048;
/// A trailer larger than this is not a preview; it is dropped unread.
pub const MAX_TRAILER_BYTES: usize = 8 * 1024;

/// A preview, as stored in `message.link_preview` and sent to the renderers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkPreview {
    /// The previewed link, exactly as it appears in the message text.
    pub url: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The thumbnail, an encrypted object in the conversation's media store.
    /// Absent when the page had none, and while the sender's upload is pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<PreviewImage>,
}

/// The thumbnail reference. Same field names as an `_att` entry, so every
/// client fetches and decrypts it through its existing attachment path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviewImage {
    pub key: String,
    pub url: String,
    /// The object's random file id (not a content hash, #1382).
    pub hash: String,
    /// Which key of the conversation's file key ring sealed it.
    pub kv: i64,
    /// SHA-256 of the plaintext JPEG, checked after decrypt.
    pub sha: String,
    pub ct: String,
    pub size: usize,
    pub w: u32,
    pub h: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bh: Option<String>,
}

/// The trailer bytes for `preview`.
pub fn encode(preview: &LinkPreview) -> Vec<u8> {
    serde_json::to_vec(preview).expect("a link preview always serializes")
}

/// The stored JSON for `preview`.
pub fn to_stored(preview: &LinkPreview) -> String {
    serde_json::to_string(preview).expect("a link preview always serializes")
}

/// A stored preview, re-checked against the text it belongs to.
pub fn from_stored(json: &str, body: &str) -> Option<LinkPreview> {
    let preview: LinkPreview = serde_json::from_str(json).ok()?;
    validated(preview, body)
}

/// Decode a received trailer. `None` unless it is a well-formed preview of a
/// link that is in `body`; the strings come back cleaned and cut to length.
pub fn decode(bytes: &[u8], body: &str) -> Option<LinkPreview> {
    if bytes.len() > MAX_TRAILER_BYTES {
        return None;
    }
    let preview: LinkPreview = serde_json::from_slice(bytes).ok()?;
    validated(preview, body)
}

fn validated(preview: LinkPreview, body: &str) -> Option<LinkPreview> {
    if !is_previewable_url(&preview.url) || !body_contains_link(body, &preview.url) {
        return None;
    }
    let title = clean_text(&preview.title, MAX_TITLE_CHARS);
    if title.is_empty() {
        return None;
    }
    let description = preview
        .description
        .as_deref()
        .map(|d| clean_text(d, MAX_DESCRIPTION_CHARS))
        .filter(|d| !d.is_empty());
    // A malformed thumbnail costs the thumbnail, never the card.
    let image = preview.image.filter(is_valid_image);
    Some(LinkPreview {
        url: preview.url,
        title,
        description,
        image,
    })
}

/// An absolute https URL of sane length with a host.
pub fn is_previewable_url(url: &str) -> bool {
    if url.len() > MAX_URL_LEN {
        return false;
    }
    match url::Url::parse(url) {
        Ok(u) => u.scheme() == "https" && u.host_str().is_some_and(|h| !h.is_empty()),
        Err(_) => false,
    }
}

/// `url` appears in `body` as a whole token: not a prefix of a longer link, not
/// glued to the word before it. The token may differ in what URL parsing
/// normalizes (`https://GitHub.com` for `https://github.com/`), because the
/// composer previews the parsed link, not the typed text. The renderers then
/// require an exact match with a link their own parser finds, so this is the
/// floor, not the whole rule.
pub fn body_contains_link(body: &str, url: &str) -> bool {
    if url.is_empty() {
        return false;
    }
    let exact = body.match_indices(url).any(|(at, _)| {
        let before = body[..at].chars().next_back();
        let after = body[at + url.len()..].chars().next();
        let clean_before = before.is_none_or(|c| c.is_whitespace() || LEADING.contains(c));
        let clean_after = after.is_none_or(|c| c.is_whitespace() || TRAILING.contains(c));
        clean_before && clean_after
    });
    exact || body_has_normalized(body, url)
}

/// Characters a link may sit inside, before and after it.
const LEADING: &str = "([<\"'";
const TRAILING: &str = ".,;:!?)]>\"'";

/// Some whitespace-separated token of `body`, without the punctuation around
/// it, parses to the same URL as `url`.
fn body_has_normalized(body: &str, url: &str) -> bool {
    let Ok(want) = url::Url::parse(url) else {
        return false;
    };
    body.split_whitespace().any(|token| {
        let token = token
            .trim_start_matches(|c| LEADING.contains(c))
            .trim_end_matches(|c| TRAILING.contains(c));
        token.len() <= MAX_URL_LEN
            && token.get(..8).is_some_and(|s| s.eq_ignore_ascii_case("https://"))
            && url::Url::parse(token).is_ok_and(|got| got == want)
    })
}

/// Plain one-paragraph text: control and bidi-override characters removed,
/// whitespace runs collapsed, trimmed, then cut to `max` characters.
pub fn clean_text(raw: &str, max: usize) -> String {
    let mut out = String::with_capacity(raw.len().min(max * 4));
    let mut last_space = true;
    for c in raw.chars() {
        if is_bidi_control(c) || (c.is_control() && !c.is_whitespace()) {
            continue;
        }
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
    }
    let trimmed = out.trim_end();
    let mut cut: String = trimmed.chars().take(max).collect();
    if trimmed.chars().count() > max {
        // Leave room for the ellipsis inside the limit.
        cut = cut.chars().take(max.saturating_sub(1)).collect::<String>().trim_end().to_string();
        cut.push('…');
    }
    cut
}

/// Characters that reorder how text displays (U+202A..E, U+2066..9, LRM/RLM/ALM),
/// which could make a title read as something it is not.
fn is_bidi_control(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}' | '\u{200E}' | '\u{200F}' | '\u{061C}'
    )
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The same shape every client checks an `_att` image entry against: a
/// `media/<file id>.enc` object, a JPEG, with a plausible size.
fn is_valid_image(image: &PreviewImage) -> bool {
    is_hex64(&image.hash)
        && image.key == format!("media/{}.enc", image.hash)
        && is_hex64(&image.sha)
        && image.ct == "image/jpeg"
        && image.kv >= 0
        && image.size > 0
        && image.size <= super::MAX_THUMBNAIL_BYTES
        && image.w > 0
        && image.h > 0
        && image.w <= super::THUMBNAIL_MAX_EDGE
        && image.h <= super::THUMBNAIL_MAX_EDGE
        && image.bh.as_deref().is_none_or(|b| b.len() <= 128)
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://example.com/article";

    fn sample() -> LinkPreview {
        LinkPreview {
            url: URL.into(),
            title: "An article".into(),
            description: Some("About things".into()),
            image: Some(PreviewImage {
                key: format!("media/{}.enc", "a".repeat(64)),
                url: String::new(),
                hash: "a".repeat(64),
                kv: 1,
                sha: "b".repeat(64),
                ct: "image/jpeg".into(),
                size: 12_000,
                w: 600,
                h: 315,
                bh: Some("LEHV6nWB2yk8pyo0adR*.7kCMdnj".into()),
            }),
        }
    }

    #[test]
    fn roundtrips_through_the_trailer() {
        let body = format!("look {URL} here");
        assert_eq!(decode(&encode(&sample()), &body), Some(sample()));
        assert_eq!(from_stored(&to_stored(&sample()), &body), Some(sample()));
    }

    /// A card may only describe a link the reader can see in the message.
    #[test]
    fn a_preview_of_a_link_not_in_the_body_is_dropped() {
        let bytes = encode(&sample());
        assert_eq!(decode(&bytes, "no link here"), None);
        assert_eq!(decode(&bytes, "https://example.com/article-two"), None, "a prefix of a longer link");
        assert_eq!(decode(&bytes, "xhttps://example.com/article"), None, "glued to a word");
        assert!(decode(&bytes, "(https://example.com/article).").is_some());
    }

    /// The composer previews the parsed link; a keyboard may capitalise the
    /// host, and a person may leave off the trailing slash.
    #[test]
    fn a_link_typed_differently_but_parsing_the_same_matches() {
        assert!(body_contains_link("Look https://GitHub.com/", "https://github.com/"));
        assert!(body_contains_link("see https://github.com", "https://github.com/"));
        assert!(body_contains_link("(HTTPS://Example.COM:443/article).", URL));
        assert!(!body_contains_link("https://github.com.evil.example/", "https://github.com/"));
        assert!(!body_contains_link("https://github.com/x", "https://github.com/"));
        assert!(!body_contains_link("xhttps://github.com/", "https://github.com/"));
        assert!(!body_contains_link("http://github.com/", "https://github.com/"));
    }

    #[test]
    fn only_https_urls() {
        for url in ["http://example.com/", "javascript:alert(1)", "file:///etc/passwd", "https://", "not a url"] {
            let mut p = sample();
            p.url = url.into();
            assert_eq!(decode(&encode(&p), url), None, "{url}");
        }
    }

    #[test]
    fn text_is_cleaned_and_cut() {
        let mut p = sample();
        p.title = format!("\u{202E}evil\u{0007}  title\n\n{}", "x".repeat(400));
        p.description = Some("  \u{2066}desc\u{2069} ".into());
        let got = decode(&encode(&p), URL).unwrap();
        assert!(got.title.starts_with("evil title "));
        assert_eq!(got.title.chars().count(), MAX_TITLE_CHARS);
        assert!(got.title.ends_with('…'));
        assert_eq!(got.description.as_deref(), Some("desc"));
    }

    #[test]
    fn an_empty_title_is_no_preview() {
        let mut p = sample();
        p.title = " \u{202E} ".into();
        assert_eq!(decode(&encode(&p), URL), None);
    }

    /// A bad thumbnail reference costs the thumbnail, not the card.
    #[test]
    fn a_malformed_image_is_dropped_alone() {
        let cases: Vec<Box<dyn Fn(&mut PreviewImage)>> = vec![
            Box::new(|i| i.key = "avatars/x/y.png".into()),
            Box::new(|i| i.hash = "zz".into()),
            Box::new(|i| i.ct = "image/svg+xml".into()),
            Box::new(|i| i.size = 50 * 1024 * 1024),
            Box::new(|i| i.w = 10_000),
            Box::new(|i| i.kv = -1),
        ];
        for break_it in cases {
            let mut p = sample();
            break_it(p.image.as_mut().unwrap());
            let got = decode(&encode(&p), URL).expect("the card survives");
            assert_eq!(got.image, None);
        }
    }

    #[test]
    fn oversized_or_garbage_trailers_are_ignored() {
        assert_eq!(decode(&vec![b' '; MAX_TRAILER_BYTES + 1], URL), None);
        assert_eq!(decode(b"\xFF\xFE", URL), None);
        assert_eq!(decode(b"[]", URL), None);
    }

    /// Unknown keys from a later client are ignored, not fatal.
    #[test]
    fn unknown_fields_are_ignored() {
        let json = format!(r#"{{"url":"{URL}","title":"T","future":{{"x":1}}}}"#);
        assert_eq!(decode(json.as_bytes(), URL).unwrap().title, "T");
    }
}
