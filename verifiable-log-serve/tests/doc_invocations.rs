//! Every `pollis-verify` command we publish must parse against the CLI we ship, and every
//! subcommand and flag the CLI has must be shown somewhere public (#1721).
//!
//! Commands are taken only from code: fenced blocks and inline code in Markdown, `<code>` /
//! `<pre>` in HTML, and whole files elsewhere (scripts, workflows, string literals). Prose that
//! merely names the tool is not a command. Placeholders (`<id>`, `$TAG`, `{tag}`) parse as dummy
//! values; invocations without any are concrete and are listed in `doc_invocations.live.txt`,
//! which the pre-release sweep (`.claude/skills/pre-release-sweeps`) runs against the live log.
//!
//! Not part of CI: these read the docs, so they run before a full release with
//! `cargo test -p verifiable-log-serve --test doc_invocations -- --include-ignored`.
//! Regenerate the list with `UPDATE_DOC_INVOCATIONS=1 cargo test -p verifiable-log-serve --test doc_invocations`.

use clap::{CommandFactory, Parser};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use verifiable_log_serve::cli::Cli;

const BIN: &str = "pollis-verify";
const LIVE_LIST: &str = "tests/doc_invocations.live.txt";
const LIVE_BASE: &str = "https://verify.pollis.com";
/// Well under today's count, so a broken extractor fails instead of passing on nothing.
const MIN_INVOCATIONS: usize = 40;
/// Where a reader learns to use the tool; the reverse check wants every subcommand and flag here.
const PUBLIC: &[&str] = &["website/", "docs/verify-transparency-log.md", "docs/transparency.md", "README.md"];
const SCANNED_EXT: &[&str] = &["md", "html", "js", "py", "yml", "yaml", "sh"];

#[derive(Debug)]
struct Invocation {
    file: String,
    line: usize,
    /// Arguments after the binary name, placeholders already substituted.
    args: Vec<String>,
    concrete: bool,
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn tracked_files(root: &Path) -> Vec<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z"])
        .output()
        .expect("git ls-files");
    assert!(out.status.success(), "git ls-files failed");
    String::from_utf8(out.stdout)
        .expect("utf8 paths")
        .split('\0')
        .filter(|p| !p.is_empty())
        .filter(|p| {
            let ext = Path::new(p).extension().and_then(|e| e.to_str()).unwrap_or("");
            SCANNED_EXT.contains(&ext)
                && !p.starts_with("verifiable-log-serve/src/")
                && !p.contains("node_modules/")
        })
        .map(str::to_string)
        .collect()
}

/// The code regions of a file as (1-based start line, text). Newlines are kept so line numbers
/// can be recovered and shell continuations joined.
fn code_regions(path: &str, text: &str) -> Vec<(usize, String)> {
    let ext = Path::new(path).extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "md" => markdown_code(text),
        "html" => html_code(text),
        // Script files carry HTML in string literals; their placeholders are entity-encoded.
        "js" => vec![(1, decode_html(&strip_tags(text)))],
        _ => vec![(1, text.to_string())],
    }
}

fn markdown_code(text: &str) -> Vec<(usize, String)> {
    let mut regions = Vec::new();
    let mut fence: Option<(usize, String)> = None;
    for (i, line) in text.lines().enumerate() {
        let n = i + 1;
        if line.trim_start().starts_with("```") {
            match fence.take() {
                Some(block) => regions.push(block),
                None => fence = Some((n + 1, String::new())),
            }
            continue;
        }
        if let Some((_, body)) = fence.as_mut() {
            body.push_str(line);
            body.push('\n');
            continue;
        }
        let mut rest = line;
        while let Some(open) = rest.find('`') {
            let after = &rest[open + 1..];
            let Some(close) = after.find('`') else { break };
            regions.push((n, after[..close].to_string()));
            rest = &after[close + 1..];
        }
    }
    regions
}

fn html_code(text: &str) -> Vec<(usize, String)> {
    let mut regions = Vec::new();
    for tag in ["code", "pre"] {
        let open_tag = format!("<{tag}");
        let close_tag = format!("</{tag}>");
        let mut from = 0;
        while let Some(rel) = text[from..].find(&open_tag) {
            let start = from + rel;
            let Some(gt) = text[start..].find('>') else { break };
            let body_start = start + gt + 1;
            let Some(end_rel) = text[body_start..].find(&close_tag) else { break };
            let body = &text[body_start..body_start + end_rel];
            let line = text[..body_start].matches('\n').count() + 1;
            regions.push((line, decode_html(&strip_tags(body))));
            from = body_start + end_rel + close_tag.len();
        }
    }
    regions
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out
}

fn decode_html(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'").replace("&nbsp;", " ").replace("&amp;", "&")
}

/// True when what precedes the binary name on its line makes it a command, not prose.
fn starts_a_command(before: &str) -> bool {
    let trimmed = before.trim_end();
    let without_path = trimmed.trim_end_matches(|c: char| c.is_ascii_alphanumeric() || "./_-~".contains(c));
    if without_path.len() != trimmed.len() && !trimmed[without_path.len()..].ends_with('/') {
        return false;
    }
    let t = without_path.trim_end();
    t.is_empty()
        || t.ends_with(['$', '&', ';', '|', '(', '`', '"', '\'', '=', '{'])
        || t.ends_with("run:")
        || t.ends_with('-')
        || t.ends_with('>')
}

/// Substitute placeholders and cut the command at shell syntax. `None` = nothing parseable.
fn normalise(raw: &str) -> (Vec<String>, bool) {
    let mut s = raw.to_string();
    let mut substituted = false;
    let mut replace_spans = |s: &mut String, open: char, close: char, keep_inner: bool| {
        while let Some(a) = s.find(open) {
            let Some(b_rel) = s[a..].find(close) else { break };
            let inner = s[a + 1..a + b_rel].to_string();
            let with = if keep_inner { inner } else { "X".to_string() };
            s.replace_range(a..a + b_rel + close.len_utf8(), &with);
            if !keep_inner {
                substituted = true;
            }
        }
    };
    replace_spans(&mut s, '<', '>', false);
    replace_spans(&mut s, '{', '}', false);
    replace_spans(&mut s, '[', ']', true);
    let mut tokens = Vec::new();
    for word in s.split_whitespace() {
        if word.starts_with(['|', ';', '&', '#', '>', '"', '\'', '`', ')', '(', '…', '\\'])
            || word.starts_with("2>")
            || word.starts_with("...")
        {
            break;
        }
        let end = word.find(['"', '\'', '`', ')', ';', '|', ',']).unwrap_or(word.len());
        let w = &word[..end];
        if w.is_empty() {
            break;
        }
        // A sentence's full stop or colon is not part of the argument.
        let trimmed = w.trim_end_matches(['.', ':']);
        let w = if trimmed.len() < w.len() { trimmed } else { w };
        if w.is_empty() {
            break;
        }
        let stop_after = trimmed.len() < word[..end].len();
        // `vX`, `vX.Y.Z` are placeholder tags in prose.
        let w = if w.starts_with('$') || matches!(w, "vX" | "vX.Y" | "vX.Y.Z" | "X") {
            substituted = true;
            "X".to_string()
        } else {
            w.to_string()
        };
        tokens.push(w);
        if end < word.len() || stop_after {
            break;
        }
    }
    (tokens, !substituted)
}

fn extract(root: &Path) -> Vec<Invocation> {
    let mut found = Vec::new();
    for file in tracked_files(root) {
        let Ok(text) = std::fs::read_to_string(root.join(&file)) else { continue };
        if !text.contains(BIN) {
            continue;
        }
        for (start_line, region) in code_regions(&file, &text) {
            for (offset, line) in logical_lines(&region) {
                let mut from = 0;
                while let Some(rel) = line[from..].find(BIN) {
                    let at = from + rel;
                    from = at + BIN.len();
                    // Followed by an argument or nothing; `pollis-verify:` or `pollis-verify-v1` is prose.
                    if !matches!(line[from..].chars().next(), None | Some(' ' | '\t' | '"' | '\'' | '`')) {
                        continue;
                    }
                    if !starts_a_command(&line[..at]) {
                        continue;
                    }
                    let (args, concrete) = normalise(&line[from..]);
                    found.push(Invocation {
                        file: file.clone(),
                        line: start_line + offset,
                        args,
                        concrete,
                    });
                }
            }
        }
    }
    found
}

/// Lines with shell continuations (`\` at the end) joined, each tagged with its first line's offset.
fn logical_lines(region: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut pending: Option<(usize, String)> = None;
    for (i, line) in region.lines().enumerate() {
        let (start, mut acc) = pending.take().unwrap_or((i, String::new()));
        match line.strip_suffix('\\') {
            Some(head) => {
                acc.push_str(head);
                acc.push(' ');
                pending = Some((start, acc));
            }
            None => {
                acc.push_str(line);
                out.push((start, acc));
            }
        }
    }
    if let Some(rest) = pending {
        out.push(rest);
    }
    out
}

/// A subcommand named with only flags (`release --json`) is a reference: the flags must exist.
fn reference_ok(args: &[String]) -> bool {
    let Some((sub, flags)) = args.split_first() else { return true };
    let cmd = Cli::command();
    let Some(sc) = cmd.get_subcommands().find(|c| c.get_name() == sub) else { return false };
    flags.iter().all(|f| {
        f.starts_with("--") && sc.get_arguments().any(|a| a.get_long() == Some(&f[2..]))
    })
}

fn parses(args: &[String]) -> Result<(), clap::Error> {
    let argv = std::iter::once(BIN.to_string()).chain(args.iter().cloned());
    match Cli::try_parse_from(argv) {
        Ok(_) => Ok(()),
        Err(e) if matches!(e.kind(), clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion) => Ok(()),
        Err(e) => Err(e),
    }
}

fn subcommands() -> BTreeSet<String> {
    Cli::command().get_subcommands().map(|c| c.get_name().to_string()).collect()
}

#[test]
#[ignore = "pre-release sweep (#1721), not CI"]
fn every_published_invocation_parses_against_the_shipped_cli() {
    let root = repo_root();
    let invocations = extract(&root);
    assert!(
        invocations.len() >= MIN_INVOCATIONS,
        "only {} pollis-verify invocations extracted (expected at least {MIN_INVOCATIONS}); the extractor broke",
        invocations.len()
    );
    let subs = subcommands();
    let mut failures = Vec::new();
    for inv in &invocations {
        match inv.args.as_slice() {
            // The bare name, or a subcommand named on its own as a reference.
            [] => {}
            [only] if subs.contains(only) => {}
            [sub, rest @ ..] if subs.contains(sub) && rest.iter().all(|a| a.starts_with('-')) && reference_ok(&inv.args) => {}
            args => {
                if let Err(e) = parses(args) {
                    let first = e.to_string().lines().next().unwrap_or_default().to_string();
                    failures.push(format!("{}:{}: `{BIN} {}`: {first}", inv.file, inv.line, args.join(" ")));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "published pollis-verify commands the CLI rejects (fix the doc, or the CLI if it regressed):\n  {}",
        failures.join("\n  ")
    );
}

#[test]
#[ignore = "pre-release sweep (#1721), not CI"]
fn every_subcommand_and_flag_is_shown_in_public_docs() {
    let root = repo_root();
    let public: Vec<Invocation> = extract(&root)
        .into_iter()
        .filter(|i| PUBLIC.iter().any(|p| i.file.starts_with(p)))
        .collect();
    let mut missing = Vec::new();
    if !public.iter().any(|i| i.args.first().map(String::as_str) == Some("--version")) {
        missing.push(format!("`{BIN} --version`"));
    }
    for sub in Cli::command().get_subcommands() {
        let name = sub.get_name();
        let uses: Vec<&Invocation> = public.iter().filter(|i| i.args.first().map(String::as_str) == Some(name)).collect();
        if uses.is_empty() {
            missing.push(format!("`{BIN} {name}`"));
            continue;
        }
        for arg in sub.get_arguments() {
            let Some(long) = arg.get_long() else { continue };
            if long == "help" {
                continue;
            }
            let flag = format!("--{long}");
            if !uses.iter().any(|i| i.args.contains(&flag)) {
                missing.push(format!("`{BIN} {name} {flag}`"));
            }
        }
    }
    assert!(
        missing.is_empty(),
        "real CLI surface no public doc shows ({}): {}",
        PUBLIC.join(", "),
        missing.join(", ")
    );
}

#[test]
#[ignore = "pre-release sweep (#1721), not CI"]
fn the_live_list_matches_the_docs() {
    let root = repo_root();
    let mut live: BTreeSet<String> = BTreeSet::new();
    for inv in extract(&root) {
        if inv.concrete && inv.args.get(1).map(String::as_str) == Some(LIVE_BASE) && parses(&inv.args).is_ok() {
            live.insert(inv.args.join(" "));
        }
    }
    let rendered: String = live.iter().map(|l| format!("{l}\n")).collect();
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(LIVE_LIST);
    if std::env::var_os("UPDATE_DOC_INVOCATIONS").is_some() {
        std::fs::write(&path, &rendered).expect("write the live list");
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        committed, rendered,
        "{LIVE_LIST} is stale; regenerate with UPDATE_DOC_INVOCATIONS=1 cargo test -p verifiable-log-serve --test doc_invocations"
    );
}

#[test]
fn prose_is_not_a_command_and_placeholders_parse() {
    assert!(!starts_a_command("your "));
    assert!(starts_a_command(""));
    assert!(starts_a_command("$ "));
    assert!(starts_a_command("./target/release/"));
    assert!(starts_a_command("  run: "));
    let (args, concrete) = normalise(" group https://verify.pollis.com <conversation-id> [--json] | jq");
    assert_eq!(args, ["group", "https://verify.pollis.com", "X", "--json"]);
    assert!(!concrete);
    let (args, concrete) = normalise(" release https://verify.pollis.com v1.5.3");
    assert_eq!(args, ["release", "https://verify.pollis.com", "v1.5.3"]);
    assert!(concrete);
    let (args, _) = normalise(" release \" + tag");
    assert_eq!(args, ["release"]);
}
