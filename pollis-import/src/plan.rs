//! Turning an [`Export`] into a dry-run [`ImportPlan`]: what would be created,
//! what would be dropped, and why, before anything is written.
//!
//! The plan is deterministic (same export and options, same plan), which is
//! what makes a re-run idempotent: the options describe what the target group
//! already has, and everything already there is marked `exists` and skipped.
//!
//! Everything the plan says to a person is structured ([`Note`],
//! [`ChannelNote`], [`EmojiNote`], [`SkipReason`]) so each app renders it in
//! the reader's language; [`crate::report`] renders English for the CLI.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use pollis_api::profile::{is_refused_display_char, DISPLAY_NAME_MAX_LEN};
use serde::Serialize;

use crate::model::{
    Channel, ChannelKind, Export, Media, MessageKind, OverwriteTarget, Role, VIEW_CHANNEL,
};
use crate::shortcode::{Allocator, Fitted};

/// What the target group already has. Empty for a new group.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ImportOptions {
    /// Category names in the group.
    pub existing_categories: Vec<String>,
    /// Channels in the group, with their category's name.
    pub existing_channels: Vec<ExistingChannel>,
    /// Shortcodes the group already has (for collision handling).
    pub existing_shortcodes: Vec<String>,
    /// Source emoji (by source id) the group already holds, matched by
    /// content: a re-run keeps their shortcode instead of uploading a copy.
    pub imported_emoji: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ExistingChannel {
    pub category: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportPlan {
    pub group: GroupPlan,
    /// In display order.
    pub categories: Vec<CategoryPlan>,
    /// In display order: the uncategorised block first, then each category's.
    pub channels: Vec<ChannelPlan>,
    pub skipped_channels: Vec<Skipped>,
    pub emoji: Vec<EmojiPlan>,
    pub roles: Vec<RolePlan>,
    /// Whether role permissions were known (a guild snapshot); without them
    /// nobody can be mapped to admin.
    pub permissions_known: bool,
    pub people: Vec<PersonPlan>,
    pub totals: Totals,
    /// Things the importer must tell the admin plainly.
    pub notes: Vec<Note>,
    /// Parse warnings carried through from the export (file paths; not translated).
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GroupPlan {
    pub name: String,
    pub icon: Option<Media>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CategoryPlan {
    pub source_id: String,
    pub name: String,
    /// The group already has a category of this name: a re-run reuses it.
    pub exists: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    Text,
    Voice,
}

impl TargetKind {
    /// The value `create_channel` takes.
    pub fn as_str(self) -> &'static str {
        match self {
            TargetKind::Text => "text",
            TargetKind::Voice => "voice",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ChannelPlan {
    pub source_id: String,
    pub source_name: String,
    pub name: String,
    pub kind: TargetKind,
    /// The category's name ([`CategoryPlan::name`]), or `None` for the
    /// uncategorised block.
    pub category: Option<String>,
    pub description: Option<String>,
    /// Already in the target group under this name and category: a re-run skips it.
    pub exists: bool,
    /// `@everyone` could not see it on Discord. Every Pollis group member will.
    pub private: bool,
    /// Every per-channel permission overwrite, none of which survives.
    pub overwrites: Vec<OverwritePlan>,
    /// Not imported (structure only); measured so the admin sees what stays behind.
    pub history: HistorySummary,
    /// Source threads under this channel (history, not imported).
    pub threads: Vec<ThreadPlan>,
    pub notes: Vec<ChannelNote>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OverwritePlan {
    /// The role's name or the member's username (`@everyone` for the guild role).
    pub target: String,
    pub member: bool,
    pub allow: Vec<Permission>,
    pub deny: Vec<Permission>,
    /// Bits outside [`Permission`]'s list.
    pub other_bits: u32,
}

/// The permissions an overwrite report names. Others are counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    View,
    SendMessages,
    ReadHistory,
    Connect,
    Speak,
    ManageChannel,
    ManageMessages,
    ManagePermissions,
    AttachFiles,
    AddReactions,
    MentionEveryone,
    CreateThreads,
}

const NAMED: [(u64, Permission); 12] = [
    (VIEW_CHANNEL, Permission::View),
    (1 << 11, Permission::SendMessages),
    (1 << 16, Permission::ReadHistory),
    (1 << 20, Permission::Connect),
    (1 << 21, Permission::Speak),
    (1 << 4, Permission::ManageChannel),
    (1 << 13, Permission::ManageMessages),
    (1 << 28, Permission::ManagePermissions),
    (1 << 15, Permission::AttachFiles),
    (1 << 6, Permission::AddReactions),
    (1 << 17, Permission::MentionEveryone),
    ((1 << 35) | (1 << 36), Permission::CreateThreads),
];

fn name_bits(bits: u64) -> (Vec<Permission>, u32) {
    let mut named = Vec::new();
    let mut rest = bits;
    for (mask, p) in NAMED {
        if bits & mask != 0 {
            named.push(p);
        }
        rest &= !mask;
    }
    (named, rest.count_ones())
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChannelNote {
    Announcement,
    Forum,
    Stage,
    Renamed { from: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadPlan {
    pub source_id: String,
    pub name: String,
    /// The parent-channel message the thread hangs off, if it was exported.
    pub root_message_id: Option<String>,
    pub history: HistorySummary,
}

/// What a channel's (or thread's) history amounts to.
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
    pub reason: SkipReason,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkipReason {
    /// A thread whose parent channel is not in the export.
    OrphanThread,
    /// A DM or group DM.
    Direct,
    Unsupported { source_type: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct EmojiPlan {
    pub source_id: String,
    pub source_name: String,
    /// The shortcode it gets (or already has); `None` = not imported.
    pub shortcode: Option<String>,
    /// Already in the group (same picture): a re-run skips it.
    pub exists: bool,
    pub animated: bool,
    pub uses: u64,
    pub image: Option<Media>,
    pub note: Option<EmojiNote>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EmojiNote {
    /// The fitted name was taken; it got a suffix.
    Renamed { collided_with: String },
    /// No 2 to 32 characters of a-z, 0-9, _ survive.
    NoValidShortcode,
    /// Only a CDN link; re-export with `--media`.
    ImageRemote,
    /// No picture in the export.
    ImageMissing,
}

#[derive(Debug, Clone, Serialize)]
pub struct RolePlan {
    pub source_id: String,
    pub name: String,
    pub maps_to: RoleTarget,
    /// People known to hold it.
    pub holders: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleTarget {
    /// Holders become admins (once they join).
    Admin,
    /// `@everyone`: every member.
    Member,
    /// A role with no Pollis equivalent: dropped, its holders join as members.
    Discarded,
    /// A bot's own role.
    Bot,
}

#[derive(Debug, Clone, Serialize)]
pub struct PersonPlan {
    pub source_id: String,
    pub username: String,
    pub display_name: String,
    pub is_bot: bool,
    /// Owner, or holds a role that runs the server: make them an admin once
    /// they join.
    pub admin: bool,
    /// Their Discord role names (not `@everyone`), for the sheet.
    pub roles: Vec<String>,
    /// The (Pollis) channel names they wrote in, in plan order.
    pub channels: Vec<String>,
    pub messages: u64,
    pub first_ms: Option<u64>,
    pub last_ms: Option<u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub categories: u64,
    pub channels: u64,
    pub to_create_categories: u64,
    pub to_create_channels: u64,
    pub to_upload_emoji: u64,
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
    pub overwrites: u64,
    pub private_channels: u64,
}

/// A plain statement for the admin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Note {
    /// Discord exposes no email addresses: share the invite link.
    NoEmails,
    /// No guild snapshot: order follows creation date, roles have no
    /// permissions, overwrites and empty categories are unknown.
    NoSnapshot,
    /// Channels hidden from `@everyone` on Discord; every member sees them here.
    PrivateChannels { count: u64 },
    /// Roles beyond member/admin, dropped.
    RolesCollapsed { discarded: u64 },
    /// Message history stays on Discord.
    HistoryNotImported { messages: u64 },
}

/// Build the plan. Reads the filesystem only to check that images are present.
pub fn plan(export: &Export, options: &ImportOptions) -> ImportPlan {
    let mut skipped_channels = Vec::new();

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
                    reason: SkipReason::OrphanThread,
                }),
            }
        }
    }

    // Categories in Discord's order: position, then creation.
    let mut source_categories = export.categories.clone();
    source_categories.sort_by(|a, b| {
        a.position
            .unwrap_or(i64::MAX)
            .cmp(&b.position.unwrap_or(i64::MAX))
            .then_with(|| snowflake_cmp(&a.id, &b.id))
    });
    let existing_categories: BTreeSet<String> =
        options.existing_categories.iter().map(|n| n.to_lowercase()).collect();
    let mut category_names: BTreeMap<String, String> = BTreeMap::new();
    let mut taken_category_names = BTreeSet::new();
    let categories: Vec<CategoryPlan> = source_categories
        .iter()
        .map(|c| {
            let name = unique_name(fit_name(&c.name, "category", &c.id), &mut taken_category_names);
            category_names.insert(c.id.clone(), name.clone());
            CategoryPlan {
                source_id: c.id.clone(),
                exists: existing_categories.contains(&name.to_lowercase()),
                name,
            }
        })
        .collect();

    // Channels: the uncategorised block, then each category's; within a
    // block text-like channels before voice-like ones (as Discord draws
    // them), each by position, then creation.
    let block_of = |ch: &Channel| -> usize {
        ch.category
            .as_ref()
            .and_then(|cat| categories.iter().position(|k| k.source_id == cat.id))
            .map_or(0, |i| i + 1)
    };
    let mut ordered: Vec<&Channel> = export.channels.iter().collect();
    ordered.sort_by(|a, b| {
        block_of(a)
            .cmp(&block_of(b))
            .then_with(|| is_voice_like(&a.kind).cmp(&is_voice_like(&b.kind)))
            .then_with(|| a.position.unwrap_or(i64::MAX).cmp(&b.position.unwrap_or(i64::MAX)))
            .then_with(|| snowflake_cmp(&a.id, &b.id))
    });

    let existing: BTreeSet<(Option<String>, String)> = options
        .existing_channels
        .iter()
        .map(|c| (c.category.as_ref().map(|n| n.to_lowercase()), c.name.to_lowercase()))
        .collect();
    let role_names: BTreeMap<&str, &Role> = export.roles.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut names_taken: BTreeMap<Option<String>, BTreeSet<String>> = BTreeMap::new();
    let mut channels = Vec::new();
    for ch in ordered {
        let kind = match &ch.kind {
            ChannelKind::Text | ChannelKind::Announcement | ChannelKind::Forum => TargetKind::Text,
            ChannelKind::Voice | ChannelKind::Stage => TargetKind::Voice,
            ChannelKind::Thread => continue,
            ChannelKind::Direct => {
                skipped_channels.push(Skipped {
                    name: ch.name.clone(),
                    reason: SkipReason::Direct,
                });
                continue;
            }
            ChannelKind::Other(t) => {
                skipped_channels.push(Skipped {
                    name: ch.name.clone(),
                    reason: SkipReason::Unsupported { source_type: t.clone() },
                });
                continue;
            }
        };

        let mut notes = Vec::new();
        match &ch.kind {
            ChannelKind::Announcement => notes.push(ChannelNote::Announcement),
            ChannelKind::Forum => notes.push(ChannelNote::Forum),
            ChannelKind::Stage => notes.push(ChannelNote::Stage),
            _ => {}
        }

        let category = ch.category.as_ref().and_then(|c| category_names.get(&c.id).cloned());
        let taken = names_taken.entry(category.clone()).or_default();
        let name = unique_name(fit_name(&ch.name, "channel", &ch.id), taken);
        if name != ch.name {
            notes.push(ChannelNote::Renamed { from: ch.name.clone() });
        }

        let overwrites: Vec<OverwritePlan> = ch
            .overwrites
            .iter()
            .map(|o| {
                let (target, member) = match &o.target {
                    OverwriteTarget::Role(id) => (
                        role_names.get(id.as_str()).map_or_else(|| format!("role {id}"), |r| r.name.clone()),
                        false,
                    ),
                    OverwriteTarget::Member(id) => (
                        export.people.get(id).map_or_else(|| format!("user {id}"), |p| p.username.clone()),
                        true,
                    ),
                };
                let (allow, allow_rest) = name_bits(o.allow);
                let (deny, deny_rest) = name_bits(o.deny);
                OverwritePlan { target, member, allow, deny, other_bits: allow_rest + deny_rest }
            })
            .collect();
        let private = ch.overwrites.iter().any(|o| {
            o.target == OverwriteTarget::Role(export.community.id.clone()) && o.deny & VIEW_CHANNEL != 0
        });

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
            exists: existing.contains(&(category.as_ref().map(|n| n.to_lowercase()), name.to_lowercase())),
            name,
            kind,
            category,
            description: ch.topic.as_deref().map(fit_description).filter(|d| !d.is_empty()),
            private,
            overwrites,
            history: summarise(ch),
            threads,
            notes,
        });
    }

    let emoji = plan_emoji(export, options);
    let people = plan_people(export, &channels);
    let roles = plan_roles(export);
    let permissions_known = export.roles.iter().any(|r| r.permissions.is_some());
    let totals = totals(&categories, &channels, &emoji, &people);

    let mut notes = vec![Note::NoEmails];
    if !export.has_snapshot {
        notes.push(Note::NoSnapshot);
    }
    if totals.private_channels > 0 {
        notes.push(Note::PrivateChannels { count: totals.private_channels });
    }
    let discarded = roles.iter().filter(|r| r.maps_to == RoleTarget::Discarded).count() as u64;
    if discarded > 0 {
        notes.push(Note::RolesCollapsed { discarded });
    }
    if totals.messages > 0 {
        notes.push(Note::HistoryNotImported { messages: totals.messages });
    }

    ImportPlan {
        group: GroupPlan {
            name: fit_name(&export.community.name, "server", &export.community.id),
            icon: export.community.icon.clone(),
        },
        categories,
        channels,
        skipped_channels,
        emoji,
        roles,
        permissions_known,
        people,
        totals,
        notes,
        warnings: export.warnings.clone(),
    }
}

fn is_voice_like(kind: &ChannelKind) -> bool {
    matches!(kind, ChannelKind::Voice | ChannelKind::Stage)
}

/// Snowflakes are decimal strings; a shorter one is older.
fn snowflake_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// Fit a name to Pollis's display-name rule, falling back to the source id.
fn fit_name(raw: &str, what: &str, id: &str) -> String {
    let cleaned: String = raw.chars().filter(|c| !is_refused_display_char(*c)).collect();
    let trimmed = cleaned.trim();
    if trimmed.is_empty() {
        return format!("{what} {id}");
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
    // Shortcodes that a re-run keeps are not free for anything else.
    let mut alloc = Allocator::with_existing(options.existing_shortcodes.iter().cloned());
    let mut ordered: Vec<_> = export.emoji.values().collect();
    // Most used first, so the popular one keeps the plain name in a collision.
    ordered.sort_by(|a, b| b.uses.cmp(&a.uses).then_with(|| snowflake_cmp(&a.id, &b.id)));
    ordered
        .into_iter()
        .map(|e| {
            let base = EmojiPlan {
                source_id: e.id.clone(),
                source_name: e.name.clone(),
                shortcode: None,
                exists: false,
                animated: e.animated,
                uses: e.uses,
                image: e.image.clone(),
                note: None,
            };
            if let Some(code) = options.imported_emoji.get(&e.id) {
                return EmojiPlan { shortcode: Some(code.clone()), exists: true, ..base };
            }
            let image_ok = matches!(&e.image, Some(Media::Local(p)) if p.is_file());
            if !image_ok {
                let note = match &e.image {
                    Some(Media::Remote(_)) => EmojiNote::ImageRemote,
                    _ => EmojiNote::ImageMissing,
                };
                return EmojiPlan { note: Some(note), ..base };
            }
            match alloc.fit(&e.name) {
                Fitted::Ok(s) => EmojiPlan { shortcode: Some(s), ..base },
                Fitted::Renamed { shortcode, collided_with } => EmojiPlan {
                    shortcode: Some(shortcode),
                    note: Some(EmojiNote::Renamed { collided_with }),
                    ..base
                },
                Fitted::Skipped { .. } => EmojiPlan { note: Some(EmojiNote::NoValidShortcode), ..base },
            }
        })
        .collect()
}

fn plan_roles(export: &Export) -> Vec<RolePlan> {
    let mut roles: Vec<&Role> = export.roles.iter().collect();
    // Most senior first, as Discord lists them.
    roles.sort_by(|a, b| b.position.cmp(&a.position).then_with(|| snowflake_cmp(&a.id, &b.id)));
    roles
        .into_iter()
        .map(|r| RolePlan {
            source_id: r.id.clone(),
            name: r.name.clone(),
            maps_to: if r.everyone {
                RoleTarget::Member
            } else if r.managed {
                RoleTarget::Bot
            } else if r.grants_admin() {
                RoleTarget::Admin
            } else {
                RoleTarget::Discarded
            },
            holders: if r.everyone {
                export.people.values().filter(|p| !p.is_bot).count() as u64
            } else {
                export.people.values().filter(|p| p.role_ids.contains(&r.id)).count() as u64
            },
        })
        .collect()
}

/// Everyone the snapshot lists as a member, plus everyone who wrote in a
/// channel being created.
fn plan_people(export: &Export, channels: &[ChannelPlan]) -> Vec<PersonPlan> {
    // Source channel (or thread) id → the Pollis channel's place in the plan
    // and its label ("Category / name", since names repeat across categories).
    let mut target: BTreeMap<&str, (usize, String)> = BTreeMap::new();
    for (i, c) in channels.iter().enumerate() {
        let label = c.category.as_ref().map_or_else(|| c.name.clone(), |k| format!("{k} / {}", c.name));
        target.insert(c.source_id.as_str(), (i, label.clone()));
        for t in &c.threads {
            target.insert(t.source_id.as_str(), (i, label.clone()));
        }
    }

    struct Stat<'a> {
        messages: u64,
        first: Option<u64>,
        last: Option<u64>,
        channels: BTreeSet<&'a (usize, String)>,
    }
    let mut stats: BTreeMap<&str, Stat> = BTreeMap::new();
    for ch in &export.channels {
        let Some(place) = target.get(ch.id.as_str()) else {
            continue;
        };
        for m in &ch.messages {
            if m.kind != MessageKind::Normal {
                continue;
            }
            let s = stats.entry(m.author_id.as_str()).or_insert(Stat {
                messages: 0,
                first: None,
                last: None,
                channels: BTreeSet::new(),
            });
            s.messages += 1;
            s.first = Some(s.first.map_or(m.sent_at_ms, |f| f.min(m.sent_at_ms)));
            s.last = Some(s.last.map_or(m.sent_at_ms, |l| l.max(m.sent_at_ms)));
            s.channels.insert(place);
        }
    }

    let roles: BTreeMap<&str, &Role> = export.roles.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut people: Vec<PersonPlan> = export
        .people
        .values()
        .filter_map(|p| {
            let stat = stats.get(p.id.as_str());
            if stat.is_none() && !p.listed_member {
                return None;
            }
            let held: Vec<&Role> = p.role_ids.iter().filter_map(|id| roles.get(id.as_str()).copied()).collect();
            let owner = export.owner_id.as_deref() == Some(p.id.as_str());
            Some(PersonPlan {
                source_id: p.id.clone(),
                username: p.username.clone(),
                display_name: p.display_name().to_string(),
                is_bot: p.is_bot,
                admin: !p.is_bot && (owner || held.iter().any(|r| r.grants_admin())),
                roles: held.iter().filter(|r| !r.everyone).map(|r| r.name.clone()).collect(),
                channels: stat.map(|s| s.channels.iter().map(|(_, n)| n.clone()).collect()).unwrap_or_default(),
                messages: stat.map_or(0, |s| s.messages),
                first_ms: stat.and_then(|s| s.first),
                last_ms: stat.and_then(|s| s.last),
            })
        })
        .collect();
    people.sort_by(|a, b| {
        b.admin
            .cmp(&a.admin)
            .then_with(|| b.messages.cmp(&a.messages))
            .then_with(|| a.username.to_lowercase().cmp(&b.username.to_lowercase()))
    });
    people
}

fn totals(categories: &[CategoryPlan], channels: &[ChannelPlan], emoji: &[EmojiPlan], people: &[PersonPlan]) -> Totals {
    let mut t = Totals {
        categories: categories.len() as u64,
        channels: channels.len() as u64,
        to_create_categories: categories.iter().filter(|c| !c.exists).count() as u64,
        to_create_channels: channels.iter().filter(|c| !c.exists).count() as u64,
        to_upload_emoji: emoji.iter().filter(|e| e.shortcode.is_some() && !e.exists).count() as u64,
        emoji: emoji.iter().filter(|e| e.shortcode.is_some()).count() as u64,
        people: people.iter().filter(|p| !p.is_bot).count() as u64,
        overwrites: channels.iter().map(|c| c.overwrites.len() as u64).sum(),
        private_channels: channels.iter().filter(|c| c.private).count() as u64,
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
