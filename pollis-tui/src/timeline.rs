//! The message pane's layout: messages → wrapped, styled terminal lines (#1337).
//!
//! The pane used to render one `Line` per message, so a multi-line message ran
//! its lines together, a long one was cut off at the pane edge, and an
//! attachment showed as its raw JSON envelope. This module turns the messages
//! the pane displays into [`Block`]s of display lines that fit a given width:
//!
//! ```text
//! ── Today ─────────────────────────────
//! 14:05 alice  ↳ bob: are we still on for…      (quote-reply context)
//! 14:05 alice  sure — see you at
//!       eight, I'll bring the                   (newlines kept, words wrapped)
//!       [image: map.png, 2 KB]                  (attachment line)
//!       👍 2  🎉 1                               (reactions)
//!       ↪ 3 replies · t to open                  (thread root)
//! ```
//!
//! Everything is a pure function of its inputs (no clock read, no I/O), so
//! the shapes are unit-tested here and the renderer and the key handler — which
//! needs line counts to keep the selected message on screen — share one
//! layout. Lines own their text: a frame lays out at most a few hundred
//! messages, and owning keeps the borrow of the message buffer out of the
//! render signature.

use std::collections::HashMap;

use chrono::{DateTime, Local, NaiveDate};
use pollis_core::commands::messages::{ChannelMessage, Reaction};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use crate::content::{attachment_label, mention_ranges, parse_content, preview};
use crate::home::OpenConversation;

/// Columns a continuation line is indented by: the width of the `HH:MM `
/// timestamp, so wrapped text lines up under the sender.
pub const INDENT: usize = 6;

/// How much of a quoted message the `↳` context line shows.
const QUOTE_CHARS: usize = 48;

/// The placeholder for a lost message — the same words desktop and mobile use.
pub const UNDECRYPTABLE_TEXT: &str = "This message couldn't be decrypted on this device";

/// A solid selection background (no glow — repo rule).
pub const SELECTED_BG: Color = Color::Rgb(45, 55, 75);

/// One laid-out item of the pane: a message (selectable, carries its id) or a
/// date separator / lost-message run (not selectable).
#[derive(Debug, Clone)]
pub struct Block {
    pub message_id: Option<String>,
    pub lines: Vec<Line<'static>>,
}

/// Everything a layout depends on besides the messages themselves.
pub struct Context<'a> {
    /// The signed-in user — whose messages are "own", whose reactions are
    /// highlighted.
    pub me_id: &'a str,
    /// The signed-in user's username — whose `@mentions` are highlighted.
    pub me_name: &'a str,
    pub reactions: &'a HashMap<String, Vec<Reaction>>,
    /// Thread root id → reply count (`list_thread_summaries`).
    pub thread_counts: &'a HashMap<String, i64>,
    /// Resolves a `reply_to_id` to the quoted message, when it is loaded.
    pub lookup: &'a dyn Fn(&str) -> Option<&'a ChannelMessage>,
    pub selected: Option<&'a str>,
    /// In a thread view the root shows no "N replies" line — the replies are
    /// right below it.
    pub in_thread: bool,
    /// The viewer's local date, for "Today" / "Yesterday" separators.
    pub today: NaiveDate,
}

/// Lay `messages` (display order, oldest first) out at `width` columns.
pub fn build(messages: &[&ChannelMessage], ctx: &Context<'_>, width: u16) -> Vec<Block> {
    let width = (width as usize).max(INDENT + 8);
    let mut blocks = Vec::with_capacity(messages.len() + 4);
    let mut last_day: Option<NaiveDate> = None;
    let mut i = 0;
    while i < messages.len() {
        let m = messages[i];
        if let Some(day) = local_time(&m.sent_at).map(|t| t.date_naive()) {
            if last_day != Some(day) {
                blocks.push(date_separator(day, ctx.today, width));
                last_day = Some(day);
            }
        }
        // A run of lost messages (#1312) is one line, as on desktop and mobile.
        if m.undecryptable {
            let run = messages[i..].iter().take_while(|m| m.undecryptable).count();
            blocks.push(Block {
                message_id: None,
                lines: vec![undecryptable_line(run)],
            });
            i += run;
            continue;
        }
        blocks.push(message_block(m, ctx, width));
        i += 1;
    }
    blocks
}

/// Lay out what an open conversation's pane displays (the channel, or the
/// open thread) — the one entry point the renderer and the key handler share.
pub fn layout_open(open: &OpenConversation, me_id: &str, me_name: &str, width: u16) -> Vec<Block> {
    let displayed = open.displayed();
    let lookup = |id: &str| open.find(id);
    let ctx = Context {
        me_id,
        me_name,
        reactions: &open.reactions,
        thread_counts: &open.thread_counts,
        lookup: &lookup,
        selected: open.selected.as_deref(),
        in_thread: open.thread.is_some(),
        today: Local::now().date_naive(),
    };
    build(&displayed, &ctx, width)
}

/// Total display lines across `blocks`.
pub fn line_count(blocks: &[Block]) -> usize {
    blocks.iter().map(|b| b.lines.len()).sum()
}

/// The scroll offset (lines up from the bottom) that keeps the block for
/// `message_id` fully on screen, starting from `scroll`: unchanged when it is
/// already visible, else the smallest move that shows it (its top, if it is
/// taller than the viewport).
pub fn scroll_to_show(blocks: &[Block], message_id: &str, viewport: usize, scroll: usize) -> usize {
    let Some(idx) = blocks
        .iter()
        .position(|b| b.message_id.as_deref() == Some(message_id))
    else {
        return scroll;
    };
    let below: usize = blocks[idx + 1..].iter().map(|b| b.lines.len()).sum();
    let height = blocks[idx].lines.len();
    if viewport == 0 {
        return below;
    }
    if below < scroll {
        return below;
    }
    if below + height > scroll + viewport {
        return (below + height).saturating_sub(viewport);
    }
    scroll
}

/// Parse a stored `sent_at` (RFC 3339, written by `envelope_sent_at`) into the
/// viewer's local time.
pub fn local_time(sent_at: &str) -> Option<DateTime<Local>> {
    DateTime::parse_from_rfc3339(sent_at)
        .ok()
        .map(|t| t.with_timezone(&Local))
}

fn date_separator(day: NaiveDate, today: NaiveDate, width: usize) -> Block {
    let label = if day == today {
        "Today".to_string()
    } else if today.pred_opt() == Some(day) {
        "Yesterday".to_string()
    } else {
        day.format("%a, %b %-d, %Y").to_string()
    };
    let head = format!("── {label} ");
    let used: usize = head.chars().filter_map(UnicodeWidthChar::width).sum();
    let rule = "─".repeat(width.saturating_sub(used));
    Block {
        message_id: None,
        lines: vec![Line::from(Span::styled(
            format!("{head}{rule}"),
            Style::default().fg(Color::DarkGray),
        ))],
    }
}

fn undecryptable_line(count: usize) -> Line<'static> {
    let text = if count == 1 {
        UNDECRYPTABLE_TEXT.to_string()
    } else {
        format!("{count} messages couldn't be decrypted on this device")
    };
    Line::from(vec![
        Span::styled("?     ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            text,
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::ITALIC),
        ),
    ])
}

fn sender_of(m: &ChannelMessage) -> &str {
    m.sender_username.as_deref().unwrap_or(&m.sender_id)
}

/// One message: optional quote context, the header line with the first run of
/// text, the wrapped remainder, attachments, reactions, and the thread count.
fn message_block(m: &ChannelMessage, ctx: &Context<'_>, width: usize) -> Block {
    let dim = Style::default().fg(Color::DarkGray);
    let own = m.sender_id == ctx.me_id;
    let sender_style = Style::default()
        .fg(if own { Color::LightGreen } else { Color::Cyan })
        .add_modifier(Modifier::BOLD);
    let time = local_time(&m.sent_at)
        .map(|t| t.format("%H:%M").to_string())
        .unwrap_or_else(|| "--:--".to_string());

    let mut lines: Vec<Line<'static>> = Vec::new();

    // The header: `HH:MM sender  ` then the body runs on from there.
    let mut head: Vec<(String, Style)> = vec![
        (format!("{time} "), dim),
        (sender_of(m).to_string(), sender_style),
        ("  ".to_string(), Style::default()),
    ];

    // Quote-reply context, on its own line above the message.
    if let Some(reply_to) = &m.reply_to_id {
        let quoted = match (ctx.lookup)(reply_to) {
            Some(q) if q.deleted_at.is_some() => format!("↳ {}: (deleted)", sender_of(q)),
            Some(q) if q.undecryptable => "↳ (a message that couldn't be decrypted)".to_string(),
            Some(q) => format!(
                "↳ {}: {}",
                sender_of(q),
                preview(q.content.as_deref().unwrap_or(""), QUOTE_CHARS)
            ),
            None => "↳ (reply to an earlier message)".to_string(),
        };
        let quote_style = Style::default()
            .fg(Color::Gray)
            .add_modifier(Modifier::ITALIC);
        lines.extend(wrap(
            vec![
                (" ".repeat(INDENT), Style::default()),
                (quoted, quote_style),
            ],
            width,
            INDENT,
        ));
    }

    if m.deleted_at.is_some() {
        head.push(("(deleted)".to_string(), dim.add_modifier(Modifier::ITALIC)));
        lines.extend(wrap(head, width, INDENT));
        return finish(m, ctx, lines, width);
    }
    let Some(raw) = m.content.as_deref() else {
        head.push((
            "(unable to decrypt)".to_string(),
            Style::default().fg(Color::Red),
        ));
        lines.extend(wrap(head, width, INDENT));
        return finish(m, ctx, lines, width);
    };

    let content = parse_content(raw);
    let mut text_lines: Vec<&str> = if content.text.is_empty() {
        Vec::new()
    } else {
        // `\r\n` from a Windows client reads as one break.
        content
            .text
            .split('\n')
            .map(|l| l.trim_end_matches('\r'))
            .collect()
    };
    let edited = m.edited_at.is_some();
    // An edit of ours the DS has not confirmed (#1555): a pending one reads as
    // an ordinary edit, a failed one says it did not go out.
    let edited_label = if matches!(m.edit_state.as_deref(), Some("failed") | Some("rejected")) {
        " (edit not sent)"
    } else {
        " (edited)"
    };

    // Attachments-only message: the first attachment rides the header line.
    let mut attachments = content.attachments.iter();
    if text_lines.is_empty() {
        let first = attachments.next().map(attachment_label).unwrap_or_default();
        head.push((first, attachment_style()));
        if edited && content.attachments.len() <= 1 {
            head.push((edited_label.to_string(), dim));
        }
        lines.extend(wrap(head, width, INDENT));
    } else {
        let last = text_lines.len() - 1;
        let first_text = text_lines.remove(0);
        let mut segments = head;
        segments.extend(highlight_mentions(first_text, ctx.me_name));
        if edited && last == 0 && content.attachments.is_empty() {
            segments.push((edited_label.to_string(), dim));
        }
        lines.extend(wrap(segments, width, INDENT));
        for (n, text) in text_lines.iter().enumerate() {
            let mut segments = vec![(" ".repeat(INDENT), Style::default())];
            segments.extend(highlight_mentions(text, ctx.me_name));
            if edited && n + 1 == last && content.attachments.is_empty() {
                segments.push((edited_label.to_string(), dim));
            }
            lines.extend(wrap(segments, width, INDENT));
        }
    }
    let rest: Vec<_> = attachments.collect();
    let rest_len = rest.len();
    for (n, a) in rest.into_iter().enumerate() {
        let mut segments = vec![
            (" ".repeat(INDENT), Style::default()),
            (attachment_label(a), attachment_style()),
        ];
        if edited && n + 1 == rest_len {
            segments.push((edited_label.to_string(), dim));
        }
        lines.extend(wrap(segments, width, INDENT));
    }
    finish(m, ctx, lines, width)
}

/// What a message the durable outbox could not deliver says under itself
/// (#1335) — the apps' "failed" label with their Retry / Discard, as keys. A
/// `pending` one is drawn like a sent one, as in the apps; `rejected` (no
/// retry can fix it, so the outbox stopped trying) reads the same as `failed`.
pub const UNSENT_MARK: &str = "✗ not sent — R retry · x discard";

/// Whether `m` is one of ours the outbox failed to deliver.
pub fn is_unsent_failure(m: &ChannelMessage) -> bool {
    matches!(m.send_state.as_deref(), Some("failed") | Some("rejected"))
}

/// Reactions + thread count + selection highlight — the tail every message
/// block gets, deleted or not.
fn finish(
    m: &ChannelMessage,
    ctx: &Context<'_>,
    mut lines: Vec<Line<'static>>,
    width: usize,
) -> Block {
    if is_unsent_failure(m) {
        lines.extend(wrap(
            vec![
                (" ".repeat(INDENT), Style::default()),
                (UNSENT_MARK.to_string(), Style::default().fg(Color::Red)),
            ],
            width,
            INDENT,
        ));
    }
    if let Some(reactions) = ctx.reactions.get(&m.id) {
        let mut segments = vec![(" ".repeat(INDENT), Style::default())];
        for (n, r) in reactions.iter().filter(|r| r.count > 0).enumerate() {
            if n > 0 {
                segments.push(("  ".to_string(), Style::default()));
            }
            let mine = r.user_ids.iter().any(|u| u == ctx.me_id);
            let style = if mine {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Rgb(120, 170, 230))
            } else {
                Style::default().fg(Color::Gray).bg(Color::Rgb(40, 40, 46))
            };
            segments.push((format!(" {} {} ", r.emoji, r.count), style));
        }
        if segments.len() > 1 {
            lines.extend(wrap(segments, width, INDENT));
        }
    }
    if !ctx.in_thread {
        if let Some(&count) = ctx.thread_counts.get(&m.id) {
            if count > 0 {
                let label = if count == 1 {
                    "↪ 1 reply".to_string()
                } else {
                    format!("↪ {count} replies")
                };
                lines.push(Line::from(vec![
                    Span::raw(" ".repeat(INDENT)),
                    Span::styled(label, Style::default().fg(Color::LightBlue)),
                    Span::styled(" · t to open", Style::default().fg(Color::DarkGray)),
                ]));
            }
        }
    }
    if ctx.selected == Some(m.id.as_str()) {
        for line in &mut lines {
            *line = std::mem::take(line).style(Style::default().bg(SELECTED_BG));
        }
    }
    Block {
        message_id: Some(m.id.clone()),
        lines,
    }
}

fn attachment_style() -> Style {
    Style::default().fg(Color::Magenta)
}

/// `text` split into plain and highlighted runs, highlighting the mentions
/// that address `me` (and `@all`).
fn highlight_mentions(text: &str, me: &str) -> Vec<(String, Style)> {
    let hl = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);
    let mut out = Vec::new();
    let mut at = 0;
    for (start, end) in mention_ranges(text, me) {
        if start > at {
            out.push((text[at..start].to_string(), Style::default()));
        }
        out.push((text[start..end].to_string(), hl));
        at = end;
    }
    if at < text.len() {
        out.push((text[at..].to_string(), Style::default()));
    }
    out
}

/// Word-wrap styled `segments` to `width` columns. The first line takes the
/// segments as given (including any leading indent they carry); continuation
/// lines are indented by `indent`. Breaks after a space where one exists on
/// the line, else mid-word (a URL longer than the pane still shows in full,
/// over several lines). Tabs become spaces; other control characters are
/// dropped so a message cannot move the terminal's cursor.
pub fn wrap(segments: Vec<(String, Style)>, width: usize, indent: usize) -> Vec<Line<'static>> {
    let indent = indent.min(width.saturating_sub(1));
    let mut out: Vec<Vec<(char, Style)>> = Vec::new();
    let mut line: Vec<(char, Style)> = Vec::new();
    let mut used = 0usize;
    // Index into `line` just past the last space — the preferred break.
    let mut break_at: Option<usize> = None;
    // Characters at or before this index are the continuation indent, never a
    // break point.
    let mut floor = 0usize;

    let width_of = |c: char| UnicodeWidthChar::width(c).unwrap_or(0);

    for (text, style) in segments {
        for c in text.chars() {
            let c = if c == '\t' { ' ' } else { c };
            if c.is_control() {
                continue;
            }
            let w = width_of(c);
            if used + w > width && line.len() > floor {
                // Carry the partial word to the next line when there is a space
                // to break at; otherwise hard-break here.
                let carry: Vec<(char, Style)> = match break_at {
                    Some(b) if b > floor => line.split_off(b),
                    _ => Vec::new(),
                };
                while line.len() > floor && line.last().is_some_and(|(c, _)| *c == ' ') {
                    line.pop();
                }
                out.push(std::mem::take(&mut line));
                line.extend(std::iter::repeat_n((' ', Style::default()), indent));
                floor = indent;
                used = indent;
                break_at = None;
                for (cc, ss) in carry {
                    line.push((cc, ss));
                    used += width_of(cc);
                }
                // A carried word never starts with a space; drop one leading
                // space left by the break.
                if c == ' ' && line.len() == floor {
                    continue;
                }
            }
            line.push((c, style));
            used += w;
            if c == ' ' {
                break_at = Some(line.len());
            }
        }
    }
    out.push(line);
    out.into_iter().map(to_line).collect()
}

/// Group a run of styled characters back into spans.
fn to_line(chars: Vec<(char, Style)>) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut current = String::new();
    let mut style: Option<Style> = None;
    for (c, s) in chars {
        if style != Some(s) && !current.is_empty() {
            spans.push(Span::styled(
                std::mem::take(&mut current),
                style.unwrap_or_default(),
            ));
        }
        style = Some(s);
        current.push(c);
    }
    if !current.is_empty() {
        spans.push(Span::styled(current, style.unwrap_or_default()));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn all_text(blocks: &[Block]) -> Vec<String> {
        blocks
            .iter()
            .flat_map(|b| b.lines.iter().map(text))
            .collect()
    }

    fn msg(id: &str, sender: &str, content: &str) -> ChannelMessage {
        ChannelMessage {
            id: id.to_string(),
            conversation_id: "c".to_string(),
            sender_id: format!("u-{sender}"),
            sender_username: Some(sender.to_string()),
            ciphertext: String::new(),
            content: Some(content.to_string()),
            reply_to_id: None,
            thread_id: None,
            // Noon UTC: the same local date in every timezone a CI runner uses.
            sent_at: "2026-10-15T12:00:00+00:00".to_string(),
            edited_at: None,
            deleted_at: None,
            undecryptable: false,
            send_state: None,
            edit_state: None,
        }
    }

    struct Fixture {
        reactions: HashMap<String, Vec<Reaction>>,
        threads: HashMap<String, i64>,
        pool: Vec<ChannelMessage>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                reactions: HashMap::new(),
                threads: HashMap::new(),
                pool: Vec::new(),
            }
        }

        fn layout(
            &self,
            messages: &[&ChannelMessage],
            selected: Option<&str>,
            width: u16,
        ) -> Vec<Block> {
            let pool = &self.pool;
            let lookup = move |id: &str| pool.iter().find(|m| m.id == id);
            let today = local_time("2026-10-15T12:00:00+00:00")
                .unwrap()
                .date_naive();
            let ctx = Context {
                me_id: "u-me",
                me_name: "me",
                reactions: &self.reactions,
                thread_counts: &self.threads,
                lookup: &lookup,
                selected,
                in_thread: false,
                today,
            };
            build(messages, &ctx, width)
        }
    }

    #[test]
    fn a_day_starts_with_a_separator_and_messages_carry_their_time() {
        let f = Fixture::new();
        let m = msg("1", "alice", "hello");
        let lines = all_text(&f.layout(&[&m], None, 60));
        assert!(lines[0].starts_with("── Today "), "{lines:?}");
        let expected_time = local_time(&m.sent_at).unwrap().format("%H:%M").to_string();
        assert_eq!(lines[1], format!("{expected_time} alice  hello"));
    }

    #[test]
    fn a_new_day_gets_its_own_separator() {
        let f = Fixture::new();
        let mut older = msg("1", "alice", "a");
        older.sent_at = "2026-10-13T12:00:00+00:00".to_string();
        let newer = msg("2", "alice", "b");
        let lines = all_text(&f.layout(&[&older, &newer], None, 60));
        let separators: Vec<&String> = lines.iter().filter(|l| l.starts_with("── ")).collect();
        assert_eq!(separators.len(), 2, "{lines:?}");
        assert!(separators[0].contains("Tue, Oct 13, 2026"), "{lines:?}");
        assert!(separators[1].contains("Today"), "{lines:?}");
    }

    /// `multi\nline\nmessage` used to render as `multilinemessage`.
    #[test]
    fn newlines_are_kept() {
        let f = Fixture::new();
        let m = msg("1", "alice", "multi\nline\nmessage");
        let lines = all_text(&f.layout(&[&m], None, 60));
        assert!(lines[1].ends_with("alice  multi"), "{lines:?}");
        assert_eq!(lines[2], "      line");
        assert_eq!(lines[3], "      message");
    }

    /// A long message wraps at word boundaries instead of being cut off.
    #[test]
    fn long_lines_wrap_at_words_and_nothing_is_lost() {
        let f = Fixture::new();
        let body = "the quick brown fox jumps over the lazy dog and keeps on running";
        let m = msg("1", "al", body);
        let blocks = f.layout(&[&m], None, 30);
        let lines = all_text(&blocks[1..]);
        assert!(lines.len() > 2, "{lines:?}");
        for l in &lines {
            assert!(l.chars().count() <= 30, "too wide: {l:?}");
        }
        let rejoined: Vec<&str> = lines
            .iter()
            .flat_map(|l| l.split_whitespace())
            .skip(2)
            .collect();
        assert_eq!(rejoined.join(" "), body);
    }

    #[test]
    fn a_word_longer_than_the_pane_is_hard_broken_not_dropped() {
        let f = Fixture::new();
        let url = "https://example.com/".to_string() + &"x".repeat(60);
        let m = msg("1", "al", &url);
        let lines = all_text(&f.layout(&[&m], None, 30)[1..]);
        let joined: String = lines.iter().map(|l| l.trim()).collect();
        assert!(joined.contains(&url), "{lines:?}");
    }

    /// The raw `{"_att":…}` envelope used to be the whole message.
    #[test]
    fn an_attachment_shows_its_caption_and_a_readable_line() {
        let f = Fixture::new();
        let m = msg(
            "1",
            "alice",
            r#"{"_att":[{"key":"media/k","hash":"h","name":"photo.png","ct":"image/png","size":2048}],"_txt":"look"}"#,
        );
        let lines = all_text(&f.layout(&[&m], None, 60));
        assert!(lines[1].ends_with("alice  look"), "{lines:?}");
        assert_eq!(lines[2], "      [image: photo.png, 2 KB]");
        assert!(!lines.join("\n").contains("_att"), "{lines:?}");
    }

    #[test]
    fn an_attachment_without_caption_rides_the_header() {
        let f = Fixture::new();
        let m = msg(
            "1",
            "alice",
            r#"{"_att":[{"name":"a.pdf","ct":"application/pdf","size":1024}]}"#,
        );
        let lines = all_text(&f.layout(&[&m], None, 60));
        assert!(
            lines[1].ends_with("alice  [file: a.pdf, 1 KB]"),
            "{lines:?}"
        );
    }

    #[test]
    fn a_quote_reply_shows_who_and_what_it_answers() {
        let mut f = Fixture::new();
        f.pool.push(msg(
            "q",
            "bob",
            "are we still on for dinner tonight at eight o'clock or not",
        ));
        let mut reply = msg("r", "alice", "yes");
        reply.reply_to_id = Some("q".into());
        let lines = all_text(&f.layout(&[&reply], None, 80));
        assert!(
            lines[1].trim_start().starts_with("↳ bob: are we still on"),
            "{lines:?}"
        );
        assert!(lines[1].ends_with('…'), "{lines:?}");
        assert!(lines[2].ends_with("alice  yes"), "{lines:?}");

        // Quoting something not loaded still says it is a reply.
        let mut orphan = msg("o", "alice", "hm");
        orphan.reply_to_id = Some("gone".into());
        let lines = all_text(&f.layout(&[&orphan], None, 80));
        assert!(
            lines[1].contains("↳ (reply to an earlier message)"),
            "{lines:?}"
        );
    }

    #[test]
    fn reactions_and_thread_counts_render_under_the_message() {
        let mut f = Fixture::new();
        f.reactions.insert(
            "1".into(),
            vec![
                Reaction {
                    emoji: "👍".into(),
                    user_ids: vec!["u-me".into(), "u-bob".into()],
                    count: 2,
                },
                Reaction {
                    emoji: "🎉".into(),
                    user_ids: vec!["u-bob".into()],
                    count: 1,
                },
            ],
        );
        f.threads.insert("1".into(), 3);
        let m = msg("1", "alice", "launch");
        let lines = all_text(&f.layout(&[&m], None, 60));
        assert!(
            lines[2].contains("👍 2") && lines[2].contains("🎉 1"),
            "{lines:?}"
        );
        assert!(lines[3].contains("↪ 3 replies"), "{lines:?}");
    }

    /// A message the outbox failed to deliver says so, with its keys; one
    /// still on its way (pending) and a delivered one do not (#1335).
    #[test]
    fn an_unsent_failure_is_marked_with_its_keys() {
        let f = Fixture::new();
        let mut failed = msg("1", "alice", "offline");
        failed.send_state = Some("failed".into());
        let mut rejected = msg("2", "alice", "not a member");
        rejected.send_state = Some("rejected".into());
        let mut pending = msg("3", "alice", "on its way");
        pending.send_state = Some("pending".into());
        let sent = msg("4", "alice", "delivered");
        let lines = all_text(&f.layout(&[&failed, &rejected, &pending, &sent], None, 80));
        let marks = lines.iter().filter(|l| l.contains(UNSENT_MARK)).count();
        assert_eq!(marks, 2, "{lines:?}");
        let after = |text: &str| {
            let at = lines.iter().position(|l| l.contains(text)).expect(text);
            lines.get(at + 1).cloned().unwrap_or_default()
        };
        assert!(after("offline").contains(UNSENT_MARK), "{lines:?}");
        assert!(after("not a member").contains(UNSENT_MARK), "{lines:?}");
        assert!(!after("on its way").contains(UNSENT_MARK), "{lines:?}");
    }

    #[test]
    fn edited_and_deleted_are_marked() {
        let f = Fixture::new();
        let mut edited = msg("1", "alice", "fixed");
        edited.edited_at = Some("t".into());
        let mut deleted = msg("2", "alice", "gone");
        deleted.deleted_at = Some("t".into());
        let lines = all_text(&f.layout(&[&edited, &deleted], None, 60));
        assert!(lines[1].ends_with("fixed (edited)"), "{lines:?}");
        assert!(
            lines[2].ends_with("(deleted)") && !lines[2].contains("gone"),
            "{lines:?}"
        );
    }

    /// An edit the outbox could not deliver says so (#1555); a pending one
    /// reads as an ordinary edit.
    #[test]
    fn an_unsent_edit_is_marked() {
        let f = Fixture::new();
        let mut failed = msg("1", "alice", "fixed");
        failed.edited_at = Some("t".into());
        failed.edit_state = Some("failed".into());
        let mut pending = msg("2", "alice", "fixing");
        pending.edited_at = Some("t".into());
        pending.edit_state = Some("pending".into());
        let lines = all_text(&f.layout(&[&failed, &pending], None, 60));
        assert!(lines[1].ends_with("fixed (edit not sent)"), "{lines:?}");
        assert!(lines[2].ends_with("fixing (edited)"), "{lines:?}");
    }

    #[test]
    fn a_run_of_lost_messages_is_one_line() {
        let f = Fixture::new();
        let lost_one = |id: &str| {
            let mut m = msg(id, "x", "");
            m.undecryptable = true;
            m.content = None;
            m
        };
        let lost = lost_one("1");
        let lost2 = lost_one("2");
        let ok = msg("3", "alice", "fine");
        let lines = all_text(&f.layout(&[&lost, &lost2, &ok], None, 80));
        assert!(
            lines[1].contains("2 messages couldn't be decrypted"),
            "{lines:?}"
        );
        assert!(lines[2].ends_with("alice  fine"), "{lines:?}");
    }

    #[test]
    fn mentions_of_me_are_highlighted() {
        let f = Fixture::new();
        let m = msg("1", "alice", "hi @me and @bob");
        let blocks = f.layout(&[&m], None, 60);
        let spans = &blocks[1].lines[0].spans;
        let highlighted: Vec<&str> = spans
            .iter()
            .filter(|s| s.style.bg == Some(Color::Yellow))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(highlighted, vec!["@me"]);
    }

    #[test]
    fn the_selected_message_is_highlighted_and_scrolled_into_view() {
        let f = Fixture::new();
        let a = msg("a", "alice", "one");
        let b = msg("b", "alice", "two\nlines");
        let c = msg("c", "alice", "three");
        let blocks = f.layout(&[&a, &b, &c], Some("b"), 60);
        assert_eq!(blocks[2].lines[0].style.bg, Some(SELECTED_BG));
        assert_eq!(blocks[1].lines[0].style.bg, None);
        // Viewport of 1 line, pinned to the bottom: "b" sits 1 line up and is
        // 2 tall, so the view moves up to show its top.
        assert_eq!(scroll_to_show(&blocks, "b", 1, 0), 2);
        // Already visible: unchanged.
        assert_eq!(scroll_to_show(&blocks, "b", 10, 0), 0);
        // Scrolled past it upward: comes back down to it.
        assert_eq!(scroll_to_show(&blocks, "c", 2, 3), 0);
    }
}
