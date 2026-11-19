//! Plain-text renderings of an [`ImportPlan`]: the English report for the
//! CLI and logs, and the invite sheet (CSV). The apps render the plan
//! natively in the reader's language; this is not their UI.

use std::fmt::Write;

use crate::model::Media;
use crate::plan::{
    ChannelNote, EmojiNote, HistorySummary, ImportPlan, Note, OverwritePlan, Permission, PersonPlan,
    RoleTarget, SkipReason, TargetKind,
};
use crate::snowflake::utc_date;

pub fn render(plan: &ImportPlan) -> String {
    let mut out = String::new();
    let t = &plan.totals;
    let _ = writeln!(out, "Import plan for \"{}\" (dry run, nothing written)", plan.group.name);
    let _ = writeln!(
        out,
        "  {} categories, {} channels, {} emoji to create ({} categories, {} channels and {} emoji already there); {} people to invite",
        t.to_create_categories,
        t.to_create_channels,
        t.to_upload_emoji,
        t.categories - t.to_create_categories,
        t.channels - t.to_create_channels,
        t.emoji - t.to_upload_emoji,
        t.people,
    );

    let _ = writeln!(out, "\nChannels");
    let mut block: Option<Option<&str>> = None;
    for c in &plan.channels {
        if block != Some(c.category.as_deref()) {
            block = Some(c.category.as_deref());
            if let Some(cat) = c.category.as_deref() {
                let exists = plan.categories.iter().any(|k| k.name == cat && k.exists);
                let _ = writeln!(out, "  {cat}{}", if exists { " [exists]" } else { "" });
            }
        }
        let kind = match c.kind {
            TargetKind::Text => "text",
            TargetKind::Voice => "voice",
        };
        let exists = if c.exists { " [exists, skipped]" } else { "" };
        let private = if c.private { " [private on Discord; every member will see it]" } else { "" };
        let _ = writeln!(out, "    #{} ({kind}){exists}{private}", c.name);
        for n in &c.notes {
            let _ = writeln!(out, "      note: {}", channel_note(n));
        }
        for o in &c.overwrites {
            let _ = writeln!(out, "      permission dropped: {}", overwrite(o));
        }
        if c.history.messages > 0 {
            let _ = writeln!(out, "      history left on Discord: {}", history(&c.history));
        }
    }
    for k in &plan.categories {
        if !plan.channels.iter().any(|c| c.category.as_deref() == Some(k.name.as_str())) {
            let _ = writeln!(out, "  {} (empty){}", k.name, if k.exists { " [exists]" } else { "" });
        }
    }

    if !plan.skipped_channels.is_empty() {
        let _ = writeln!(out, "\nNot imported");
        for s in &plan.skipped_channels {
            let reason = match &s.reason {
                SkipReason::OrphanThread => "thread whose parent channel is not in the export".to_string(),
                SkipReason::Direct => "direct message; only server channels are imported".to_string(),
                SkipReason::Unsupported { source_type } => format!("unsupported channel type \"{source_type}\""),
            };
            let _ = writeln!(out, "  {}: {reason}", s.name);
        }
    }

    if !plan.emoji.is_empty() {
        let _ = writeln!(out, "\nCustom emoji");
        for e in &plan.emoji {
            let target = match (&e.shortcode, e.exists) {
                (Some(s), true) => format!(":{s}: [exists]"),
                (Some(s), false) => format!(":{s}:"),
                (None, _) => "skipped".into(),
            };
            let anim = if e.animated { ", animated" } else { "" };
            let note = e.note.as_ref().map(|n| format!(" ({})", emoji_note(n))).unwrap_or_default();
            let _ = writeln!(out, "  {} -> {target}, used {}{anim}{note}", e.source_name, e.uses);
        }
    }

    if !plan.roles.is_empty() {
        let _ = writeln!(out, "\nRoles (Pollis has member and admin)");
        if !plan.permissions_known {
            let _ = writeln!(out, "  permissions unknown without a guild snapshot; nobody is mapped to admin");
        }
        for r in &plan.roles {
            let to = match r.maps_to {
                RoleTarget::Admin => "admin",
                RoleTarget::Member => "member",
                RoleTarget::Discarded => "dropped; holders join as members",
                RoleTarget::Bot => "a bot's role; dropped",
            };
            let _ = writeln!(out, "  {} ({} people) -> {to}", r.name, r.holders);
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
        let _ = writeln!(out, "\nBots (not brought over)");
        for p in bots {
            let _ = writeln!(out, "  {}", person(p));
        }
    }

    let _ = writeln!(out, "\nNotes");
    for n in &plan.notes {
        let _ = writeln!(out, "  - {}", note(n));
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

/// The invite sheet: one row per person (bots left out), for the admin to
/// work through once people join. CSV, so it opens in any spreadsheet.
pub fn invite_sheet(plan: &ImportPlan) -> String {
    let mut out = String::from("username,display_name,pollis_role,discord_roles,channels,messages\n");
    for p in plan.people.iter().filter(|p| !p.is_bot) {
        let row = [
            p.username.clone(),
            p.display_name.clone(),
            if p.admin { "admin" } else { "member" }.to_string(),
            p.roles.join("; "),
            p.channels.join("; "),
            p.messages.to_string(),
        ];
        let cells: Vec<String> = row.iter().map(|c| csv_cell(c)).collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    out
}

/// Quote a CSV cell, and defuse a leading formula character so a name
/// like `=HYPERLINK(...)` is text in a spreadsheet, not a formula.
fn csv_cell(s: &str) -> String {
    let defused = if s.starts_with(['=', '+', '-', '@', '\t', '\r']) {
        format!("'{s}")
    } else {
        s.to_string()
    };
    if defused.contains([',', '"', '\n', '\r']) || defused != s {
        format!("\"{}\"", defused.replace('"', "\"\""))
    } else {
        defused
    }
}

fn note(n: &Note) -> String {
    match n {
        Note::NoEmails => "Discord exposes no email addresses, so nobody is invited automatically: share the group's invite link with the people listed.".into(),
        Note::NoSnapshot => "No guild snapshot (discord-guild.json): channel order follows creation date, role permissions and private channels are unknown, and empty categories are missing.".into(),
        Note::PrivateChannels { count } => format!("{count} channel(s) were private on Discord. Every member of a Pollis group can see every channel."),
        Note::RolesCollapsed { discarded } => format!("{discarded} role(s) have no Pollis equivalent and are dropped; their holders join as members."),
        Note::HistoryNotImported { messages } => format!("Structure only: {messages} message(s) stay on Discord."),
    }
}

fn channel_note(n: &ChannelNote) -> String {
    match n {
        ChannelNote::Announcement => "announcement channel imported as a text channel".into(),
        ChannelNote::Forum => "forum imported as a text channel".into(),
        ChannelNote::Stage => "stage imported as a voice channel".into(),
        ChannelNote::Renamed { from } => format!("renamed from \"{from}\""),
    }
}

fn emoji_note(n: &EmojiNote) -> String {
    match n {
        EmojiNote::Renamed { collided_with } => format!("\"{collided_with}\" was taken; renamed"),
        EmojiNote::NoValidShortcode => "no valid shortcode (needs 2 to 32 of a-z, 0-9, _)".into(),
        EmojiNote::ImageRemote => "image is only a CDN link; re-export with --media".into(),
        EmojiNote::ImageMissing => "image not found in the export".into(),
    }
}

fn overwrite(o: &OverwritePlan) -> String {
    let names = |ps: &[Permission]| ps.iter().map(|p| permission(*p)).collect::<Vec<_>>().join(", ");
    let mut parts = Vec::new();
    if !o.allow.is_empty() {
        parts.push(format!("allowed {}", names(&o.allow)));
    }
    if !o.deny.is_empty() {
        parts.push(format!("denied {}", names(&o.deny)));
    }
    if o.other_bits > 0 {
        parts.push(format!("{} other permission(s)", o.other_bits));
    }
    let who = if o.member { format!("member {}", o.target) } else { o.target.clone() };
    format!("{who}: {}", parts.join("; "))
}

fn permission(p: Permission) -> &'static str {
    match p {
        Permission::View => "view",
        Permission::SendMessages => "send messages",
        Permission::ReadHistory => "read history",
        Permission::Connect => "join voice",
        Permission::Speak => "speak",
        Permission::ManageChannel => "manage the channel",
        Permission::ManageMessages => "manage messages",
        Permission::ManagePermissions => "manage permissions",
        Permission::AttachFiles => "attach files",
        Permission::AddReactions => "add reactions",
        Permission::MentionEveryone => "mention everyone",
        Permission::CreateThreads => "create threads",
    }
}

fn person(p: &PersonPlan) -> String {
    let name = if p.display_name == p.username {
        p.username.clone()
    } else {
        format!("{} ({})", p.display_name, p.username)
    };
    let admin = if p.admin { ", make admin" } else { "" };
    let active = range(p.first_ms, p.last_ms).map(|r| format!(", {r}")).unwrap_or_default();
    format!("{name}{admin}: {} messages{active}", p.messages)
}

fn history(h: &HistorySummary) -> String {
    let mut parts = vec![format!("{} messages", h.messages)];
    if let Some(r) = range(h.first_ms, h.last_ms) {
        parts.push(r);
    }
    if h.media.files > 0 {
        parts.push(format!("{} files ({})", h.media.files, bytes(h.media.bytes)));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_cells_quote_and_defuse_formulas() {
        assert_eq!(csv_cell("ash"), "ash");
        assert_eq!(csv_cell("a, b"), "\"a, b\"");
        assert_eq!(csv_cell("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_cell("=1+1"), "\"'=1+1\"");
    }
}
