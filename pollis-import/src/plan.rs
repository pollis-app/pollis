//! Turning an [`Export`] into a dry-run [`ImportPlan`]: what would be created,
//! what would be dropped, and why, before anything is written.
//!
//! The plan is deterministic (same export and options, same plan), which is
//! what makes a re-run idempotent: the apply step matches planned channels and
//! emoji by name against what the group already has.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use pollis_api::profile::{is_refused_display_char, DISPLAY_NAME_MAX_LEN};
use serde::Serialize;

use crate::model::{Channel, ChannelKind, Export, Media, MessageKind};
use crate::shortcode::{Allocator, Fitted};

/// How Discord categories (channel folders) land, since Pollis has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CategoryStrategy {
    /// `Campaign / session-notes`. Keeps the grouping visible in one flat list.
    Prefix,
    /// `session-notes`. Drops the grouping.
    Drop,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportOptions {
    pub categories: CategoryStrategy,
    /// Shortcodes the target group already has (for collision handling).
    pub existing_shortcodes: Vec<String>,
    /// Channel names the target group already has (a re-run skips them).
    pub existing_channels: Vec<String>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            categories: CategoryStrategy::Prefix,
            existing_shortcodes: Vec::new(),
            existing_channels: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportPlan {
    pub group: GroupPlan,
    pub channels: Vec<ChannelPlan>,
    pub skipped_channels: Vec<Skipped>,
    pub emoji: Vec<EmojiPlan>,
    pub people: Vec<PersonPlan>,
    pub totals: Totals,
    /// Things the importer must tell the admin plainly.
    pub notes: Vec<String>,
    /// Parse warnings carried through from the export.
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupPlan {
    pub name: String,
    pub icon: Option<Media>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum TargetKind {
    Text,
    Voice,
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelPlan {
    pub source_id: String,
    pub source_name: String,
    pub name: String,
    pub kind: TargetKind,
    pub description: Option<String>,
    /// Already in the target group under this name: a re-run skips creating it.
    pub exists: bool,
    pub history: HistorySummary,
    /// Source threads folded into this channel.
    pub threads: Vec<ThreadPlan>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadPlan {
    pub source_id: String,
    pub name: String,
    /// The parent-channel message the thread hangs off, if it was exported.
    /// `None` means the thread is folded in with a heading instead (forum
    /// posts, or threads whose starter was deleted).
    pub root_message_id: Option<String>,
    pub history: HistorySummary,
}

/// What a channel's (or thread's) history amounts to. Measured for every
/// channel even when history is not being imported, so the admin sees the
/// scale of what stays behind.
#[derive(Debug, Clone, Default, Serialize)]
pub struct HistorySummary {
    pub messages: u64,
    pub system_messages: u64,
    pub first_ms: Option<u64>,
    pub last_ms: Option<u64>,
    pub replies: u64,
    pub pinned: u64,
    pub edited: u64,
    pub reactions: u64,
    pub embeds: u64,
    pub stickers: u64,
    pub media: MediaSummary,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct MediaSummary {
    pub files: u64,
    pub bytes: u64,
    /// Downloaded beside the export and present on disk.
    pub local_present: u64,
    /// Referenced as downloaded but not found on disk.
    pub local_missing: Vec<PathBuf>,
    /// Only a CDN URL (export made without `--media`); almost certainly expired.
    pub remote_only: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Skipped {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EmojiPlan {
    pub source_name: String,
    pub shortcode: Option<String>,
    pub animated: bool,
    pub uses: u64,
    pub image: Option<Media>,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersonPlan {
    pub source_id: String,
    pub username: String,
    pub display_name: String,
    pub is_bot: bool,
    pub messages: u64,
    pub first_ms: Option<u64>,
    pub last_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub channels: u64,
    pub threads: u64,
    pub messages: u64,
    pub system_messages: u64,
    pub first_ms: Option<u64>,
    pub last_ms: Option<u64>,
    pub media_files: u64,
    pub media_bytes: u64,
    pub media_missing: u64,
    pub media_remote_only: u64,
    pub emoji: u64,
    pub people: u64,
}

/// Build the plan. Reads the filesystem only to check that downloaded media
/// is present.
pub fn plan(export: &Export, options: &ImportOptions) -> ImportPlan {
    let mut notes = Vec::new();
    let mut skipped_channels = Vec::new();
    let mut names_taken: BTreeSet<String> = BTreeSet::new();
    let existing: BTreeSet<String> = options.existing_channels.iter().map(|n| n.to_lowercase()).collect();

    // Threads hang off their parent; index them first.
    let mut threads_by_parent: BTreeMap<&str, Vec<&Channel>> = BTreeMap::new();
    for ch in &export.channels {
        if ch.kind == ChannelKind::Thread {
            match ch.parent_id.as_deref() {
                Some(parent) if export.channels.iter().any(|c| c.id == parent) => {
                    threads_by_parent.entry(parent).or_default().push(ch);
                }
                _ => skipped_channels.push(Skipped {
                    name: ch.name.clone(),
                    reason: "thread whose parent channel is not in the export; export the parent too".into(),
                }),
            }
        }
    }

    let mut channels = Vec::new();
    for ch in &export.channels {
        let kind = match &ch.kind {
            ChannelKind::Text | ChannelKind::Announcement | ChannelKind::Forum => TargetKind::Text,
            ChannelKind::Voice | ChannelKind::Stage => TargetKind::Voice,
            ChannelKind::Thread => continue,
            ChannelKind::Direct => {
                skipped_channels.push(Skipped {
                    name: ch.name.clone(),
                    reason: "direct message; only server channels are imported into a group".into(),
                });
                continue;
            }
            ChannelKind::Other(t) => {
                skipped_channels.push(Skipped {
                    name: ch.name.clone(),
                    reason: format!("unsupported channel type \"{t}\""),
                });
                continue;
            }
        };

        let mut channel_notes = Vec::new();
        match &ch.kind {
            ChannelKind::Announcement => channel_notes.push("announcement channel imported as a text channel".into()),
            ChannelKind::Forum => channel_notes.push("forum imported as a text channel; each post becomes a thread with a heading".into()),
            ChannelKind::Stage => channel_notes.push("stage imported as a voice channel".into()),
            _ => {}
        }

        let raw_name = match (options.categories, &ch.category) {
            (CategoryStrategy::Prefix, Some(cat)) if !is_default_category(&cat.name) => {
                format!("{} / {}", cat.name, ch.name)
            }
            _ => ch.name.clone(),
        };
        let name = unique_name(fit_name(&raw_name, &ch.id), &mut names_taken);
        if name != raw_name {
            channel_notes.push(format!("renamed from \"{raw_name}\""));
        }

        let history = summarise(ch);
        let ids: BTreeSet<&str> = ch.messages.iter().map(|m| m.id.as_str()).collect();
        let threads = threads_by_parent
            .get(ch.id.as_str())
            .map(|ts| {
                ts.iter()
                    .map(|t| ThreadPlan {
                        source_id: t.id.clone(),
                        name: t.name.clone(),
                        // A thread started from a message shares that message's id.
                        root_message_id: ids.contains(t.id.as_str()).then(|| t.id.clone()),
                        history: summarise(t),
                    })
                    .collect()
            })
            .unwrap_or_default();

        channels.push(ChannelPlan {
            source_id: ch.id.clone(),
            source_name: ch.name.clone(),
            exists: existing.contains(&name.to_lowercase()),
            name,
            kind,
            description: ch.topic.as_deref().map(fit_description),
            history,
            threads,
            notes: channel_notes,
        });
    }

    let emoji = plan_emoji(export, options);
    let imported: BTreeSet<&str> = channels
        .iter()
        .flat_map(|c| std::iter::once(c.source_id.as_str()).chain(c.threads.iter().map(|t| t.source_id.as_str())))
        .collect();
    let people = plan_people(export, &imported);
    let totals = totals(&channels, &emoji, &people);

    if export
        .channels
        .iter()
        .any(|c| c.category.as_ref().is_some_and(|cat| !is_default_category(&cat.name)))
    {
        notes.push(match options.categories {
            CategoryStrategy::Prefix => "Pollis has no channel categories; category names are kept as a prefix (\"Category / channel\").".into(),
            CategoryStrategy::Drop => "Pollis has no channel categories; they are dropped.".into(),
        });
    }
    notes.push("Channel order is not kept: Pollis lists channels in its own order.".into());
    notes.push("Roles are not imported. The person running the import owns the new group; everyone else joins as a member through an invite link.".into());
    notes.push("Discord exposes no email addresses, so nobody can be invited automatically: share the group's invite link with the people listed below.".into());
    if totals.media_remote_only > 0 {
        notes.push(format!(
            "{} attachment(s) have only a Discord CDN link, which expire. Re-export with DiscordChatExporter's --media option to keep them.",
            totals.media_remote_only
        ));
    }

    ImportPlan {
        group: GroupPlan {
            name: fit_name(&export.community.name, &export.community.id),
            icon: export.community.icon.clone(),
        },
        channels,
        skipped_channels,
        emoji,
        people,
        totals,
        notes,
        warnings: export.warnings.clone(),
    }
}

/// Fit a name to Pollis's display-name rule, falling back to the source id.
fn fit_name(raw: &str, id: &str) -> String {
    let cleaned: String = raw.chars().filter(|c| !is_refused_display_char(*c)).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return format!("channel {id}");
    }
    trimmed.chars().take(DISPLAY_NAME_MAX_LEN).collect::<String>().trim_end().to_string()
}

fn fit_description(raw: &str) -> String {
    raw.chars().filter(|c| *c == '\n' || !c.is_control()).collect::<String>().trim().to_string()
}

fn unique_name(name: String, taken: &mut BTreeSet<String>) -> String {
    if taken.insert(name.to_lowercase()) {
        return name;
    }
    for n in 2u32.. {
        let suffix = format!(" ({n})");
        let stem: String = name.chars().take(DISPLAY_NAME_MAX_LEN - suffix.len()).collect();
        let candidate = format!("{}{suffix}", stem.trim_end());
        if taken.insert(candidate.to_lowercase()) {
            return candidate;
        }
    }
    unreachable!("an unbounded suffix search always finds a free name")
}

fn summarise(ch: &Channel) -> HistorySummary {
    let mut s = HistorySummary::default();
    for m in &ch.messages {
        if let MessageKind::System(_) = m.kind {
            s.system_messages += 1;
            continue;
        }
        s.messages += 1;
        s.first_ms = Some(s.first_ms.map_or(m.sent_at_ms, |f| f.min(m.sent_at_ms)));
        s.last_ms = Some(s.last_ms.map_or(m.sent_at_ms, |l| l.max(m.sent_at_ms)));
        s.replies += u64::from(m.reply_to.is_some());
        s.pinned += u64::from(m.pinned);
        s.edited += u64::from(m.edited_at.is_some());
        s.reactions += m.reactions.iter().map(|r| r.count).sum::<u64>();
        s.embeds += m.embed_count as u64;
        s.stickers += m.stickers.len() as u64;
        for a in &m.attachments {
            s.media.files += 1;
            s.media.bytes += a.size_bytes;
            match &a.media {
                Media::Local(p) if p.is_file() => s.media.local_present += 1,
                Media::Local(p) => s.media.local_missing.push(p.clone()),
                Media::Remote(_) => s.media.remote_only += 1,
            }
        }
    }
    s
}

fn plan_emoji(export: &Export, options: &ImportOptions) -> Vec<EmojiPlan> {
    let mut alloc = Allocator::with_existing(options.existing_shortcodes.iter().cloned());
    let mut ordered: Vec<_> = export.emoji.values().collect();
    // Most used first, so the popular one keeps the plain name in a collision.
    ordered.sort_by(|a, b| b.uses.cmp(&a.uses).then_with(|| a.id.cmp(&b.id)));
    ordered
        .into_iter()
        .map(|e| {
            let (shortcode, mut note) = match alloc.fit(&e.name) {
                Fitted::Ok(s) => (Some(s), None),
                Fitted::Renamed { shortcode, collided_with } => {
                    let note = format!("\"{collided_with}\" was taken; renamed");
                    (Some(shortcode), Some(note))
                }
                Fitted::Skipped { reason } => (None, Some(reason)),
            };
            let image_ok = matches!(&e.image, Some(Media::Local(p)) if p.is_file());
            if shortcode.is_some() && !image_ok {
                note = Some(match &e.image {
                    Some(Media::Remote(_)) => "image is only a CDN link; re-export with --media".into(),
                    _ => "image not found in the export".into(),
                });
            }
            EmojiPlan {
                source_name: e.name.clone(),
                shortcode: if image_ok { shortcode } else { None },
                animated: e.animated,
                uses: e.uses,
                image: e.image.clone(),
                note,
            }
        })
        .collect()
}

/// Discord names the categories every new server starts with "Text Channels"
/// and "Voice Channels". As a prefix they say nothing, so they are dropped.
fn is_default_category(name: &str) -> bool {
    name.eq_ignore_ascii_case("Text Channels") || name.eq_ignore_ascii_case("Voice Channels")
}

/// People who wrote at least one message in a channel being imported.
fn plan_people(export: &Export, imported: &BTreeSet<&str>) -> Vec<PersonPlan> {
    let mut stats: BTreeMap<&str, (u64, Option<u64>, Option<u64>)> = BTreeMap::new();
    for ch in export.channels.iter().filter(|c| imported.contains(c.id.as_str())) {
        for m in &ch.messages {
            if m.kind != MessageKind::Normal {
                continue;
            }
            let entry = stats.entry(m.author_id.as_str()).or_insert((0, None, None));
            entry.0 += 1;
            entry.1 = Some(entry.1.map_or(m.sent_at_ms, |f| f.min(m.sent_at_ms)));
            entry.2 = Some(entry.2.map_or(m.sent_at_ms, |l| l.max(m.sent_at_ms)));
        }
    }
    let mut people: Vec<PersonPlan> = export
        .people
        .values()
        .filter_map(|p| {
            let (messages, first_ms, last_ms) = stats.get(p.id.as_str()).copied()?;
            Some(PersonPlan {
                source_id: p.id.clone(),
                username: p.username.clone(),
                display_name: p.display_name().to_string(),
                is_bot: p.is_bot,
                messages,
                first_ms,
                last_ms,
            })
        })
        .collect();
    people.sort_by(|a, b| b.messages.cmp(&a.messages).then_with(|| a.source_id.cmp(&b.source_id)));
    people
}

fn totals(channels: &[ChannelPlan], emoji: &[EmojiPlan], people: &[PersonPlan]) -> Totals {
    let mut t = Totals {
        channels: channels.len() as u64,
        emoji: emoji.iter().filter(|e| e.shortcode.is_some()).count() as u64,
        people: people.iter().filter(|p| !p.is_bot).count() as u64,
        ..Totals::default()
    };
    let mut add = |h: &HistorySummary| {
        t.messages += h.messages;
        t.system_messages += h.system_messages;
        t.media_files += h.media.files;
        t.media_bytes += h.media.bytes;
        t.media_missing += h.media.local_missing.len() as u64;
        t.media_remote_only += h.media.remote_only;
        if let Some(f) = h.first_ms {
            t.first_ms = Some(t.first_ms.map_or(f, |x| x.min(f)));
        }
        if let Some(l) = h.last_ms {
            t.last_ms = Some(t.last_ms.map_or(l, |x| x.max(l)));
        }
    };
    for c in channels {
        add(&c.history);
        for th in &c.threads {
            add(&th.history);
        }
    }
    t.threads = channels.iter().map(|c| c.threads.len() as u64).sum();
    t
}
