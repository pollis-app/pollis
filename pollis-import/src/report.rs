//! A plain-text rendering of an [`ImportPlan`], for the CLI and for logs.
//! The apps render the same plan natively; this is not their UI.

use std::fmt::Write;

use crate::model::Media;
use crate::plan::{HistorySummary, ImportPlan, PersonPlan, TargetKind};
use crate::snowflake::utc_date;

pub fn render(plan: &ImportPlan) -> String {
    let mut out = String::new();
    let t = &plan.totals;
    let _ = writeln!(out, "Import plan for \"{}\" (dry run, nothing written)", plan.group.name);
    let _ = writeln!(
        out,
        "  {} channels, {} threads, {} messages{}, {} files ({}), {} emoji, {} people",
        t.channels,
        t.threads,
        t.messages,
        range(t.first_ms, t.last_ms).map(|r| format!(" from {r}")).unwrap_or_default(),
        t.media_files,
        bytes(t.media_bytes),
        t.emoji,
        t.people,
    );
    if t.media_missing > 0 || t.media_remote_only > 0 {
        let _ = writeln!(
            out,
            "  media problems: {} missing on disk, {} only a CDN link",
            t.media_missing, t.media_remote_only
        );
    }

    let _ = writeln!(out, "\nChannels");
    for c in &plan.channels {
        let kind = match c.kind {
            TargetKind::Text => "text",
            TargetKind::Voice => "voice",
        };
        let exists = if c.exists { " [exists, skipped]" } else { "" };
        let _ = writeln!(out, "  #{} ({kind}){exists}: {}", c.name, history(&c.history));
        for th in &c.threads {
            let anchor = if th.root_message_id.is_some() { "thread" } else { "thread, folded with heading" };
            let _ = writeln!(out, "      ↳ {} ({anchor}): {}", th.name, history(&th.history));
        }
        for n in &c.notes {
            let _ = writeln!(out, "      note: {n}");
        }
    }

    if !plan.skipped_channels.is_empty() {
        let _ = writeln!(out, "\nNot imported");
        for s in &plan.skipped_channels {
            let _ = writeln!(out, "  {}: {}", s.name, s.reason);
        }
    }

    if !plan.emoji.is_empty() {
        let _ = writeln!(out, "\nCustom emoji");
        for e in &plan.emoji {
            let target = e.shortcode.as_deref().map(|s| format!(":{s}:")).unwrap_or_else(|| "skipped".into());
            let anim = if e.animated { ", animated" } else { "" };
            let note = e.note.as_deref().map(|n| format!(" ({n})")).unwrap_or_default();
            let _ = writeln!(out, "  {} -> {target}, used {}{anim}{note}", e.source_name, e.uses);
        }
    }

    let (bots, humans): (Vec<_>, Vec<_>) = plan.people.iter().partition(|p| p.is_bot);
    if !humans.is_empty() {
        let _ = writeln!(out, "\nPeople to invite (share the group's invite link)");
        for p in humans {
            let _ = writeln!(out, "  {}", person(p));
        }
    }
    if !bots.is_empty() {
        let _ = writeln!(out, "\nBots (not brought over; they would need to be rebuilt as Pollis bots)");
        for p in bots {
            let _ = writeln!(out, "  {}", person(p));
        }
    }

    if !plan.notes.is_empty() {
        let _ = writeln!(out, "\nNotes");
        for n in &plan.notes {
            let _ = writeln!(out, "  - {n}");
        }
    }
    if !plan.warnings.is_empty() {
        let _ = writeln!(out, "\nWarnings");
        for w in &plan.warnings {
            let _ = writeln!(out, "  - {w}");
        }
    }
    if let Some(Media::Remote(_)) = &plan.group.icon {
        let _ = writeln!(out, "\n(The server icon is only a CDN link and may have expired.)");
    }
    out
}

fn person(p: &PersonPlan) -> String {
    let name = if p.display_name == p.username {
        p.username.clone()
    } else {
        format!("{} ({})", p.display_name, p.username)
    };
    let active = range(p.first_ms, p.last_ms).map(|r| format!(", {r}")).unwrap_or_default();
    format!("{name}: {} messages{active}", p.messages)
}

fn history(h: &HistorySummary) -> String {
    let mut parts = vec![format!("{} messages", h.messages)];
    if let Some(r) = range(h.first_ms, h.last_ms) {
        parts.push(r);
    }
    if h.media.files > 0 {
        parts.push(format!("{} files ({})", h.media.files, bytes(h.media.bytes)));
    }
    if h.pinned > 0 {
        parts.push(format!("{} pinned", h.pinned));
    }
    if h.system_messages > 0 {
        parts.push(format!("{} system notices dropped", h.system_messages));
    }
    parts.join(", ")
}

fn range(first: Option<u64>, last: Option<u64>) -> Option<String> {
    match (first, last) {
        (Some(f), Some(l)) if utc_date(f) == utc_date(l) => Some(utc_date(f)),
        (Some(f), Some(l)) => Some(format!("{} to {}", utc_date(f), utc_date(l))),
        _ => None,
    }
}

fn bytes(n: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[unit])
    }
}
