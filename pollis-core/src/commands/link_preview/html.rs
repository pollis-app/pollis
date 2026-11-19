//! Reads a page's preview metadata out of the first part of its HTML (#1987).
//!
//! Deliberately small: a scan for `<meta>` and `<title>` in the bytes the
//! fetcher kept, never a DOM, never a script. Nothing here is trusted; the
//! caller cleans and cuts every string (`wire::clean_text`).

/// What a page says about itself.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PageMeta {
    pub title: Option<String>,
    pub description: Option<String>,
    /// As written in the page; resolve it against the page URL before use.
    pub image: Option<String>,
}

/// Pull preview metadata from `html`, in order of preference: OpenGraph, then
/// Twitter cards, then `<title>` and `<meta name="description">`.
pub fn parse(html: &str) -> PageMeta {
    let mut og = PageMeta::default();
    let mut twitter = PageMeta::default();
    let mut plain = PageMeta::default();
    let lower = html.to_ascii_lowercase();
    let mut at = 0;
    while let Some(rel) = lower[at..].find('<') {
        let start = at + rel;
        let rest = &lower[start..];
        if rest.starts_with("</head") || rest.starts_with("<body") {
            break;
        }
        if rest.starts_with("<!--") {
            at = match lower[start..].find("-->") {
                Some(end) => start + end + 3,
                None => break,
            };
            continue;
        }
        let Some(tag_len) = tag_end(&lower[start..]) else {
            break;
        };
        let tag = &html[start..start + tag_len];
        let tag_lower = &lower[start..start + tag_len];
        if is_tag(tag_lower, "meta") {
            let attrs = attributes(tag);
            let key = attr(&attrs, "property").or_else(|| attr(&attrs, "name"));
            let content = attr(&attrs, "content");
            if let (Some(key), Some(content)) = (key, content) {
                let content = decode_entities(content);
                let slot = match key.to_ascii_lowercase().as_str() {
                    "og:title" => &mut og.title,
                    "og:description" => &mut og.description,
                    "og:image" | "og:image:url" | "og:image:secure_url" => &mut og.image,
                    "twitter:title" => &mut twitter.title,
                    "twitter:description" => &mut twitter.description,
                    "twitter:image" | "twitter:image:src" => &mut twitter.image,
                    "description" => &mut plain.description,
                    _ => {
                        at = start + tag_len;
                        continue;
                    }
                };
                if slot.is_none() && !content.trim().is_empty() {
                    *slot = Some(content);
                }
            }
        } else if is_tag(tag_lower, "title") && plain.title.is_none() {
            let body_start = start + tag_len;
            if let Some(close) = lower[body_start..].find("</title") {
                let text = decode_entities(&html[body_start..body_start + close]);
                if !text.trim().is_empty() {
                    plain.title = Some(text);
                }
                at = body_start + close;
                continue;
            }
        }
        at = start + tag_len;
    }
    PageMeta {
        title: og.title.or(twitter.title).or(plain.title),
        description: og.description.or(twitter.description).or(plain.description),
        image: og.image.or(twitter.image),
    }
}

/// `tag` (lowercased, starting at `<`) opens element `name`.
fn is_tag(tag: &str, name: &str) -> bool {
    tag.strip_prefix('<')
        .and_then(|t| t.strip_prefix(name))
        .and_then(|t| t.chars().next())
        .is_some_and(|c| c.is_whitespace() || c == '>' || c == '/')
}

/// Length of the tag starting at `s[0] == '<'`, through its `>`, skipping `>`
/// inside quoted attribute values.
fn tag_end(s: &str) -> Option<usize> {
    let mut quote: Option<u8> = None;
    for (i, b) in s.bytes().enumerate() {
        match quote {
            Some(q) if b == q => quote = None,
            Some(_) => {}
            None if b == b'"' || b == b'\'' => quote = Some(b),
            None if b == b'>' => return Some(i + 1),
            None => {}
        }
    }
    None
}

/// `name=value` pairs of one tag, names lowercased, values unquoted (raw).
fn attributes(tag: &str) -> Vec<(String, &str)> {
    let inner = tag.trim_start_matches('<').trim_end_matches('>').trim_end_matches('/');
    // Skip the element name.
    let inner = inner.trim_start_matches(|c: char| !c.is_whitespace());
    let bytes = inner.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let name_start = i;
        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'=' {
            i += 1;
        }
        let name = inner[name_start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            if !name.is_empty() {
                out.push((name, ""));
            }
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let value = if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
            let q = bytes[i];
            let v_start = i + 1;
            let v_end = inner[v_start..].bytes().position(|b| b == q).map_or(bytes.len(), |p| v_start + p);
            i = (v_end + 1).min(bytes.len());
            &inner[v_start..v_end]
        } else {
            let v_start = i;
            while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            &inner[v_start..i]
        };
        if !name.is_empty() {
            out.push((name, value));
        }
    }
    out
}

fn attr<'a>(attrs: &'a [(String, &'a str)], name: &str) -> Option<&'a str> {
    attrs.iter().find(|(n, _)| n == name).map(|(_, v)| *v)
}

/// The handful of entities page titles actually use; anything else is left as written.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp..];
        let Some(semi) = after[..after.len().min(12)].find(';') else {
            out.push('&');
            rest = &after[1..];
            continue;
        };
        let entity = &after[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some(' '),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &after[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &after[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opengraph_wins() {
        let html = r#"<html><head>
            <title>Plain title</title>
            <meta name="description" content="plain desc">
            <meta name="twitter:title" content="Tw title">
            <meta property="og:title" content="OG &amp; title">
            <meta property="og:description" content='OG desc with "quotes"'>
            <meta property="og:image" content="/img/card.png">
            </head><body><meta property="og:title" content="in body"></body></html>"#;
        assert_eq!(
            parse(html),
            PageMeta {
                title: Some("OG & title".into()),
                description: Some(r#"OG desc with "quotes""#.into()),
                image: Some("/img/card.png".into()),
            }
        );
    }

    #[test]
    fn falls_back_to_twitter_then_plain() {
        let html = r#"<head><meta name="twitter:image:src" content="https://x/i.jpg"><TITLE>Only &#x2014; title</TITLE><meta content="d" name="DESCRIPTION"/></head>"#;
        let m = parse(html);
        assert_eq!(m.title.as_deref(), Some("Only \u{2014} title"));
        assert_eq!(m.description.as_deref(), Some("d"));
        assert_eq!(m.image.as_deref(), Some("https://x/i.jpg"));
    }

    #[test]
    fn ignores_comments_and_quoted_angle_brackets() {
        let html = r#"<head><!-- <meta property="og:title" content="hidden"> --><meta property="og:title" content="a > b"></head>"#;
        assert_eq!(parse(html).title.as_deref(), Some("a > b"));
    }

    #[test]
    fn nothing_is_nothing() {
        assert_eq!(parse(""), PageMeta::default());
        assert_eq!(parse("<html><body>hi</body>"), PageMeta::default());
        assert_eq!(parse("<head><meta property=\"og:title\""), PageMeta::default(), "a truncated tag");
        assert_eq!(parse("<title>unterminated"), PageMeta::default());
    }
}
