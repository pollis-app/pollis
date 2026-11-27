//! Message formatting (#2142): the Rust twin of `design/messageFormat.ts`. Pure.
//!
//! Exactly five styles: `**bold**`, `*italic*` / `_italic_`, `__underline__` (Discord's meaning),
//! `~~strikethrough~~`, and ``` fenced code blocks. Used for the previews the core builds
//! (notifications, search snippets) and by the terminal client's renderer. Both parsers run the
//! case table in `design/messageFormat.cases.json`; change them together. The rules are written
//! out at the top of the TypeScript file.

/// One node of a parsed message body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Text(String),
    Bold(Vec<Node>),
    Italic(Vec<Node>),
    Underline(Vec<Node>),
    Strike(Vec<Node>),
    Code { lang: Option<String>, text: String },
}

/// Nesting deeper than this stays literal.
const MAX_DEPTH: usize = 16;

fn escapable(c: char) -> bool {
    matches!(c, '\\' | '*' | '_' | '~' | '`')
}

fn glue(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '.' | '@' | '/' | '+' | '-')
}

fn mention_body(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '.' | '-')
}

/// The token tree of a message body. Plain text is one text node; empty text is none.
pub fn parse(text: &str) -> Vec<Node> {
    if text.is_empty() {
        return Vec::new();
    }
    if !text.contains(['*', '_', '~', '\\', '`']) {
        return vec![Node::Text(text.to_string())];
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = Vec::new();
    let mut from = 0;
    let mut i = 0;
    while i < lines.len() {
        let Some(lang) = fence_lang(lines[i]) else {
            i += 1;
            continue;
        };
        let Some(close) = (i + 1..lines.len()).find(|&j| fence_lang(lines[j]) == Some("")) else {
            // Every later fence would only close this one, so the rest is text.
            break;
        };
        if i > from {
            out.extend(parse_inline_text(&lines[from..i].join("\n")));
        }
        out.push(Node::Code {
            lang: (!lang.is_empty()).then(|| lang.to_string()),
            text: lines[i + 1..close].join("\n"),
        });
        from = close + 1;
        i = close + 1;
    }
    if from < lines.len() {
        out.extend(parse_inline_text(&lines[from..].join("\n")));
    }
    out
}

/// The words without the markers, for previews: code keeps its text, set off by line breaks.
pub fn strip(text: &str) -> String {
    plain_text(&parse(text))
}

/// [`strip`] over a tree already parsed.
pub fn plain_text(nodes: &[Node]) -> String {
    let mut out = String::new();
    let mut after_code = false;
    for node in nodes {
        if let Node::Code { text, .. } = node {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
            after_code = true;
            continue;
        }
        if after_code {
            out.push('\n');
            after_code = false;
        }
        match node {
            Node::Text(t) => out.push_str(t),
            Node::Bold(c) | Node::Italic(c) | Node::Underline(c) | Node::Strike(c) => {
                out.push_str(&plain_text(c))
            }
            Node::Code { .. } => {}
        }
    }
    out
}

/// The text outside code blocks, one line per formatted run: what mention detection reads, so a
/// mention counts exactly where a client draws one, and never inside code.
pub fn scan_text(text: &str) -> String {
    let mut runs: Vec<&str> = Vec::new();
    let nodes = parse(text);
    collect_runs(&nodes, &mut runs);
    runs.join("\n")
}

fn collect_runs<'a>(nodes: &'a [Node], runs: &mut Vec<&'a str>) {
    for node in nodes {
        match node {
            Node::Text(t) => runs.push(t),
            Node::Bold(c) | Node::Italic(c) | Node::Underline(c) | Node::Strike(c) => {
                collect_runs(c, runs)
            }
            Node::Code { .. } => {}
        }
    }
}

/// The language word of a fence line (`""` for none), or `None` when the line is not a fence.
fn fence_lang(line: &str) -> Option<&str> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    let trimmed = line.trim_matches([' ', '\t']);
    let lang = trimmed.strip_prefix("```")?;
    lang.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '+' | '#' | '.' | '-'))
        .then_some(lang)
}

fn parse_inline_text(text: &str) -> Vec<Node> {
    let cs: Vec<char> = text.chars().collect();
    parse_inline(&cs, 0, cs.len(), 0)
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct MarkerKey(char, usize);

fn parse_inline(cs: &[char], lo: usize, hi: usize, depth: usize) -> Vec<Node> {
    let mut out = Vec::new();
    // Per marker, the lowest start a closer search already failed from: a later one fails too.
    let mut failed: std::collections::HashMap<MarkerKey, usize> = std::collections::HashMap::new();
    let mut buf = String::new();
    let mut i = lo;
    while i < hi {
        let c = cs[i];
        if c == '\\' && i + 1 < hi && escapable(cs[i + 1]) {
            buf.push(cs[i + 1]);
            i += 2;
            continue;
        }
        let atom_end = atom(cs, i, lo, hi);
        if atom_end > i {
            buf.extend(&cs[i..atom_end]);
            i = atom_end;
            continue;
        }
        if depth < MAX_DEPTH && matches!(c, '*' | '_' | '~') {
            if let Some((node, next)) = delimited(cs, i, lo, hi, depth, &mut failed) {
                if !buf.is_empty() {
                    out.push(Node::Text(std::mem::take(&mut buf)));
                }
                out.push(node);
                i = next;
                continue;
            }
        }
        buf.push(c);
        i += 1;
    }
    if !buf.is_empty() {
        out.push(Node::Text(buf));
    }
    out
}

fn delimited(
    cs: &[char],
    i: usize,
    lo: usize,
    hi: usize,
    depth: usize,
    failed: &mut std::collections::HashMap<MarkerKey, usize>,
) -> Option<(Node, usize)> {
    let c = cs[i];
    let double = i + 1 < hi && cs[i + 1] == c;
    if c == '~' && !double {
        return None;
    }
    let len = if double { 2 } else { 1 };
    let prev = (i > lo).then(|| cs[i - 1]);
    if c == '*' && prev.is_some_and(char::is_numeric) {
        return None;
    }
    // An underscore after an underscore is still inside the word (`x__y_`).
    if c == '_' && prev.is_some_and(|p| p.is_alphanumeric() || p == '_') {
        return None;
    }
    let start = i + len;
    if start >= hi || cs[start].is_whitespace() {
        return None;
    }
    let key = MarkerKey(c, len);
    let failed_from = failed.get(&key).copied();
    if failed_from.is_some_and(|f| start >= f) {
        return None;
    }
    let Some(close) = find_closer(cs, c, len, start, lo, hi) else {
        failed.insert(key, failed_from.map_or(start, |f| f.min(start)));
        return None;
    };
    let children = parse_inline(cs, start, close, depth + 1);
    let node = match (c, double) {
        ('~', _) => Node::Strike(children),
        ('*', true) => Node::Bold(children),
        ('_', true) => Node::Underline(children),
        _ => Node::Italic(children),
    };
    Some((node, close + len))
}

fn find_closer(cs: &[char], c: char, len: usize, from: usize, lo: usize, hi: usize) -> Option<usize> {
    let mut k = from;
    while k < hi {
        let ch = cs[k];
        if ch == '\\' && k + 1 < hi && escapable(cs[k + 1]) {
            k += 2;
            continue;
        }
        let atom_end = atom(cs, k, lo, hi);
        if atom_end > k {
            k = atom_end;
            continue;
        }
        if ch != c {
            k += 1;
            continue;
        }
        let pair = k + 1 < hi && cs[k + 1] == c;
        if len == 2 {
            if pair
                && k > from
                && !cs[k - 1].is_whitespace()
                && !(k + 2 < hi && cs[k + 2] == c)
                && close_flank(c, cs, k + 2, hi)
            {
                return Some(k);
            }
            k += 1;
            continue;
        }
        if pair {
            // A double marker inside single-marker text is its own span, never half a closer.
            k += 2;
            continue;
        }
        if k > from && !cs[k - 1].is_whitespace() && close_flank(c, cs, k + 1, hi) {
            return Some(k);
        }
        k += 1;
    }
    None
}

fn close_flank(c: char, cs: &[char], after: usize, hi: usize) -> bool {
    let Some(&next) = (after < hi).then(|| &cs[after]) else {
        return true;
    };
    match c {
        '*' => !next.is_numeric(),
        '_' => !next.is_alphanumeric(),
        _ => true,
    }
}

/// The end of a URL, mention or emoji token starting at `k`, or `k` when none starts there.
fn atom(cs: &[char], k: usize, lo: usize, hi: usize) -> usize {
    let c = cs[k];
    let prev = (k > lo).then(|| cs[k - 1]);
    if matches!(c, 'h' | 'H' | 'w' | 'W') && !prev.is_some_and(glue) {
        let prefix = url_prefix(cs, k, hi);
        if prefix == 0 {
            return k;
        }
        let mut end = k + prefix;
        while end < hi && !cs[end].is_whitespace() && !matches!(cs[end], '<' | '>' | '"' | '\'' | '`') {
            end += 1;
        }
        while end > k + prefix && matches!(cs[end - 1], '*' | '_' | '~') {
            end -= 1;
        }
        return if end > k + prefix { end } else { k };
    }
    if c == '@' && !prev.is_some_and(glue) {
        let mut end = k + 1;
        while end < hi && mention_body(cs[end]) {
            end += 1;
        }
        while end > k + 1 && matches!(cs[end - 1], '.' | '-' | '_') {
            end -= 1;
        }
        return if end > k + 1 { end } else { k };
    }
    if c == '<' && k + 1 < hi && cs[k + 1] == ':' {
        let mut end = k + 2;
        while end < hi && end - (k + 2) < 32 && (cs[end].is_ascii_lowercase() || cs[end].is_ascii_digit() || cs[end] == '_') {
            end += 1;
        }
        if end - (k + 2) < 2 || end >= hi || cs[end] != ':' {
            return k;
        }
        let mut h = end + 1;
        while h < hi && h - (end + 1) < 64 && matches!(cs[h], '0'..='9' | 'a'..='f') {
            h += 1;
        }
        if h - (end + 1) != 64 || h >= hi || cs[h] != '>' {
            return k;
        }
        return h + 1;
    }
    if c == ':' {
        let mut end = k + 1;
        while end < hi
            && end - (k + 1) < 64
            && (cs[end].is_ascii_lowercase() || cs[end].is_ascii_digit() || matches!(cs[end], '_' | '+' | '-'))
        {
            end += 1;
        }
        if end > k + 1 && end < hi && cs[end] == ':' {
            return end + 1;
        }
        return k;
    }
    k
}

/// Length of `http://`, `https://` or `www.` at `k` (ASCII case-insensitive), or 0.
fn url_prefix(cs: &[char], k: usize, hi: usize) -> usize {
    for p in ["https://", "http://", "www."] {
        let n = p.len();
        if k + n <= hi && cs[k..k + n].iter().zip(p.chars()).all(|(a, b)| a.eq_ignore_ascii_case(&b)) {
            return n;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    const CASES: &str = include_str!("../../design/messageFormat.cases.json");

    fn compact(nodes: &[Node]) -> Value {
        Value::Array(
            nodes
                .iter()
                .map(|n| match n {
                    Node::Text(t) => json!(t),
                    Node::Bold(c) => json!({ "b": compact(c) }),
                    Node::Italic(c) => json!({ "i": compact(c) }),
                    Node::Underline(c) => json!({ "u": compact(c) }),
                    Node::Strike(c) => json!({ "s": compact(c) }),
                    Node::Code { lang, text } => json!({ "code": text, "lang": lang }),
                })
                .collect(),
        )
    }

    #[test]
    fn the_shared_case_table_passes() {
        let table: Value = serde_json::from_str(CASES).expect("case table parses");
        let cases = table["cases"].as_array().expect("cases");
        assert!(cases.len() >= 50);
        let mut failures = Vec::new();
        for case in cases {
            let name = case["name"].as_str().unwrap();
            let input = case["input"].as_str().unwrap();
            let nodes = parse(input);
            if compact(&nodes) != case["tokens"] {
                failures.push(format!("{name}: tokens {} != {}", compact(&nodes), case["tokens"]));
            }
            if strip(input) != case["plain"].as_str().unwrap() {
                failures.push(format!("{name}: plain {:?}", strip(input)));
            }
            if let Some(scan) = case.get("scan").and_then(Value::as_str) {
                if scan_text(input) != scan {
                    failures.push(format!("{name}: scan {:?}", scan_text(input)));
                }
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn a_pathological_run_of_openers_stays_fast() {
        let text = "*a _b ~~c **d __e ".repeat(4000);
        let started = std::time::Instant::now();
        assert_eq!(strip(&text), text);
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
    }

    #[test]
    fn deep_nesting_keeps_every_character() {
        let text = format!("{}x{}", "*_".repeat(40), "_*".repeat(40));
        assert!(strip(&text).contains('x'));
    }
}
